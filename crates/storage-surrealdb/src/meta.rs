//! `SurrealDB` commit metadata: the history graph and per-chunk history.

use std::collections::{BTreeMap, BTreeSet};

use sekai_core::{
    ApplyOutcome, ChunkCoord, ChunkHash, ChunkHistoryEntry, Commit, CommitEntry, CommitId,
    CommitRef, FoldOutcome, MetaStore, NamedScope, RefName, ScopeName, commit_hash,
};
use sekai_storage::StorageError;
use surrealdb::method::Transaction;
use surrealdb::types::{Bytes, Object, RecordId, Value};
use surrealdb::{Connection, Surreal};

use crate::SurrealdbStore;
use crate::row::{
    StoredCommit, StoredHistory, commit_bytes, commit_record, db_err, decode_commit,
    decode_commit_ref, decode_coord_fields, decode_created_at_ms, decode_history, decode_int,
    decode_scope, ref_record, scope_record,
};

/// Managed schema version stored in `meta:store`.
pub const SCHEMA_VERSION: i64 = 1;

fn millis_param(ms: u64) -> Result<i64, StorageError> {
    i64::try_from(ms).map_err(|_| StorageError::InvalidTimestamp(ms))
}

fn normalized_parents(parents: &[CommitId]) -> Vec<CommitId> {
    let mut parents = parents.to_vec();
    parents.sort_unstable();
    parents.dedup();
    parents
}

fn parent_bytes(parents: &[CommitId]) -> Vec<Bytes> {
    parents.iter().copied().map(commit_bytes).collect()
}

fn commit_id_set(ids: &BTreeSet<CommitId>) -> Vec<Bytes> {
    ids.iter().copied().map(commit_bytes).collect()
}

fn coord_key(coord: ChunkCoord) -> (i32, i32, i32, i32) {
    (
        coord.dim.raw(),
        i32::from(coord.kind.raw()),
        coord.x,
        coord.z,
    )
}

/// Ancestors of `seed`, including `seed` itself. Unknown commits simply have
/// no parents to follow.
fn ancestor_ids(commits: &[StoredCommit], seed: CommitId) -> BTreeSet<CommitId> {
    let parents: BTreeMap<CommitId, &[CommitId]> = commits
        .iter()
        .map(|commit| (commit.id, commit.parents.as_slice()))
        .collect();
    let mut ancestors = BTreeSet::new();
    let mut pending = vec![seed];
    while let Some(id) = pending.pop() {
        if ancestors.insert(id)
            && let Some(parents) = parents.get(&id)
        {
            pending.extend(parents.iter().copied());
        }
    }
    ancestors
}

async fn selected_value<C: Connection>(
    db: &Surreal<C>,
    record: RecordId,
) -> Result<Option<Value>, StorageError> {
    let mut response = db
        .query("SELECT * FROM $record;")
        .bind(("record", record))
        .await
        .map_err(db_err)?
        .check()
        .map_err(db_err)?;
    let rows: Vec<Value> = response.take(0).map_err(db_err)?;
    Ok(rows.into_iter().next())
}

async fn tx_selected_value<C: Connection>(
    tx: &Transaction<C>,
    record: RecordId,
) -> Result<Option<Value>, StorageError> {
    let mut response = tx
        .query("SELECT * FROM $record;")
        .bind(("record", record))
        .await
        .map_err(db_err)?
        .check()
        .map_err(db_err)?;
    let rows: Vec<Value> = response.take(0).map_err(db_err)?;
    Ok(rows.into_iter().next())
}

async fn finish_commit<C: Connection, T>(
    tx: Transaction<C>,
    result: Result<T, StorageError>,
) -> Result<T, StorageError> {
    match result {
        Ok(value) => {
            tx.commit().await.map_err(db_err)?;
            Ok(value)
        }
        Err(error) => {
            let _ = tx.cancel().await;
            Err(error)
        }
    }
}

async fn insert_commit<C: Connection>(
    tx: &Transaction<C>,
    id: CommitId,
    created_at_ms: u64,
    parents: &[CommitId],
) -> Result<Option<i64>, StorageError> {
    for parent in parents {
        if tx_selected_value(tx, commit_record(*parent))
            .await?
            .is_none()
        {
            return Err(StorageError::UnknownCommit { id: *parent });
        }
    }
    if tx_selected_value(tx, commit_record(id)).await?.is_some() {
        return Ok(None);
    }

    let mut response = tx
        .query("RETURN sequence::nextval('commit_seq');")
        .await
        .map_err(db_err)?
        .check()
        .map_err(db_err)?;
    let seq_value: Value = response.take(0).map_err(db_err)?;
    let seq = decode_int(&seq_value, "seq")?;

    let mut response = tx
        .query(
            "INSERT IGNORE INTO commit {
                 id: $record, hash: $hash, seq: $seq,
                 created_at_ms: $created_at_ms, parents: $parents
             }
             RETURN AFTER;",
        )
        .bind(("record", commit_record(id)))
        .bind(("hash", commit_bytes(id)))
        .bind(("seq", seq))
        .bind(("created_at_ms", millis_param(created_at_ms)?))
        .bind(("parents", parent_bytes(parents)))
        .await
        .map_err(db_err)?
        .check()
        .map_err(db_err)?;
    let inserted: Vec<Value> = response.take(0).map_err(db_err)?;
    Ok((!inserted.is_empty()).then_some(seq))
}

async fn insert_history<C: Connection>(
    tx: &Transaction<C>,
    commit: CommitId,
    seq: i64,
    coord: ChunkCoord,
    blob: Option<&ChunkHash>,
) -> Result<(), StorageError> {
    let blob = blob.map(|hash| Bytes::from(hash.as_bytes().to_vec()));
    tx.query(
        "INSERT INTO chunk_history {
             commit: $commit, seq: $seq, dim: $dim, kind: $kind,
             cx: $cx, cz: $cz, blob: $blob
         }
         RETURN NONE;",
    )
    .bind(("commit", commit_bytes(commit)))
    .bind(("seq", seq))
    .bind(("dim", i64::from(coord.dim.raw())))
    .bind(("kind", i64::from(coord.kind.raw())))
    .bind(("cx", i64::from(coord.x)))
    .bind(("cz", i64::from(coord.z)))
    .bind(("blob", blob))
    .await
    .map_err(db_err)?
    .check()
    .map_err(db_err)?;
    Ok(())
}

async fn record_chunk_tx<C: Connection>(
    tx: &Transaction<C>,
    commit: CommitId,
    coord: &ChunkCoord,
    blob: Option<&ChunkHash>,
) -> Result<(), StorageError> {
    let value = tx_selected_value(tx, commit_record(commit))
        .await?
        .ok_or(StorageError::UnknownCommit { id: commit })?;
    let stored = decode_commit(value)?;
    insert_history(tx, commit, stored.seq, *coord, blob).await
}

async fn apply_commit_tx<C: Connection>(
    tx: &Transaction<C>,
    id: CommitId,
    created_at_ms: u64,
    parents: &[CommitId],
    entries: &[CommitEntry],
) -> Result<ApplyOutcome, StorageError> {
    let mut response = tx
        .query("SELECT * FROM ref WHERE active = true LIMIT 1;")
        .await
        .map_err(db_err)?
        .check()
        .map_err(db_err)?;
    let refs: Vec<Value> = response.take(0).map_err(db_err)?;
    let active = refs
        .into_iter()
        .next()
        .ok_or(StorageError::NoActiveRef)
        .and_then(decode_commit_ref)?;

    if let Some(base) = active.commit
        && !parents.contains(&base)
    {
        return Err(StorageError::NotContinuing { base });
    }

    let inserted = insert_commit(tx, id, created_at_ms, parents).await?;
    if let Some(seq) = inserted {
        for entry in entries {
            insert_history(tx, id, seq, entry.coord, entry.blob.as_ref()).await?;
        }
    }

    let mut response = tx
        .query("UPDATE $record SET commit = $commit RETURN AFTER;")
        .bind(("record", ref_record(&active.name)))
        .bind(("commit", commit_bytes(id)))
        .await
        .map_err(db_err)?
        .check()
        .map_err(db_err)?;
    let moved: Vec<Value> = response.take(0).map_err(db_err)?;
    if moved.is_empty() {
        return Err(StorageError::NoActiveRef);
    }

    Ok(ApplyOutcome { id })
}

async fn set_ref_tx<C: Connection>(
    tx: &Transaction<C>,
    name: &RefName,
    commit: Option<CommitId>,
    created_at_ms: u64,
) -> Result<(), StorageError> {
    if let Some(id) = commit
        && tx_selected_value(tx, commit_record(id)).await?.is_none()
    {
        return Err(StorageError::UnknownCommit { id });
    }
    let stored_commit = commit.map(commit_bytes);
    let created_at_ms = millis_param(created_at_ms)?;
    let record = ref_record(name);
    if tx_selected_value(tx, record.clone()).await?.is_some() {
        tx.query("UPDATE $record SET commit = $commit RETURN NONE;")
            .bind(("record", record))
            .bind(("commit", stored_commit))
            .await
            .map_err(db_err)?
            .check()
            .map_err(db_err)?;
    } else {
        tx.query(
            "CREATE $record SET name = $name, commit = $commit,
                 active = false, created_at_ms = $created_at_ms RETURN NONE;",
        )
        .bind(("record", record))
        .bind(("name", name.as_str().to_owned()))
        .bind(("commit", stored_commit))
        .bind(("created_at_ms", created_at_ms))
        .await
        .map_err(db_err)?
        .check()
        .map_err(db_err)?;
    }
    Ok(())
}

async fn delete_ref_tx<C: Connection>(
    tx: &Transaction<C>,
    name: &RefName,
) -> Result<bool, StorageError> {
    let Some(value) = tx_selected_value(tx, ref_record(name)).await? else {
        return Ok(false);
    };
    let record = decode_commit_ref(value)?;
    if record.active {
        return Err(StorageError::ActiveRefProtected {
            name: name.as_str().to_owned(),
        });
    }
    tx.query("DELETE $record RETURN NONE;")
        .bind(("record", ref_record(name)))
        .await
        .map_err(db_err)?
        .check()
        .map_err(db_err)?;
    Ok(true)
}

async fn activate_ref_tx<C: Connection>(
    tx: &Transaction<C>,
    name: &RefName,
) -> Result<(), StorageError> {
    let value = tx_selected_value(tx, ref_record(name))
        .await?
        .ok_or_else(|| StorageError::UnknownRef {
            name: name.as_str().to_owned(),
        })?;
    let record = decode_commit_ref(value)?;
    if let Some(commit) = record.commit
        && tx_selected_value(tx, commit_record(commit))
            .await?
            .is_none()
    {
        return Err(StorageError::UnknownCommit { id: commit });
    }

    tx.query("UPDATE ref SET active = false WHERE active = true RETURN NONE;")
        .await
        .map_err(db_err)?
        .check()
        .map_err(db_err)?;
    let mut response = tx
        .query("UPDATE $record SET active = true RETURN AFTER;")
        .bind(("record", ref_record(name)))
        .await
        .map_err(db_err)?
        .check()
        .map_err(db_err)?;
    let activated: Vec<Value> = response.take(0).map_err(db_err)?;
    if activated.is_empty() {
        return Err(StorageError::UnknownRef {
            name: name.as_str().to_owned(),
        });
    }
    Ok(())
}

async fn set_scope_tx<C: Connection>(
    tx: &Transaction<C>,
    scope: &NamedScope,
) -> Result<(), StorageError> {
    let record = scope_record(&scope.name);
    let existing = tx_selected_value(tx, record.clone()).await?;
    let created_at_ms = match &existing {
        Some(value) => decode_created_at_ms(value.clone())?,
        None => scope.created_at_ms,
    };
    let areas: Vec<Object> = scope
        .areas()
        .iter()
        .map(|(dim, rect)| {
            let mut area = Object::new();
            area.insert("dim", i64::from(dim.raw()));
            area.insert("x0", i64::from(rect.x0));
            area.insert("z0", i64::from(rect.z0));
            area.insert("x1", i64::from(rect.x1));
            area.insert("z1", i64::from(rect.z1));
            area
        })
        .collect();

    if existing.is_some() {
        tx.query("UPDATE $record SET name = $name, areas = $areas RETURN NONE;")
            .bind(("record", record))
            .bind(("name", scope.name.as_str().to_owned()))
            .bind(("areas", areas))
            .await
            .map_err(db_err)?
            .check()
            .map_err(db_err)?;
    } else {
        tx.query(
            "CREATE $record SET name = $name, created_at_ms = $created_at_ms,
                 areas = $areas RETURN NONE;",
        )
        .bind(("record", record))
        .bind(("name", scope.name.as_str().to_owned()))
        .bind(("created_at_ms", millis_param(created_at_ms)?))
        .bind(("areas", areas))
        .await
        .map_err(db_err)?
        .check()
        .map_err(db_err)?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn retire_writes<C: Connection>(
    tx: &Transaction<C>,
    from: CommitId,
    into: CommitId,
    from_parents: &[CommitId],
    into_seq: i64,
    commits: &[StoredCommit],
    dropped: &[RecordId],
    folded: usize,
) -> Result<FoldOutcome, StorageError> {
    if !dropped.is_empty() {
        tx.query("DELETE chunk_history WHERE id IN $records RETURN NONE;")
            .bind(("records", dropped.to_vec()))
            .await
            .map_err(db_err)?
            .check()
            .map_err(db_err)?;
    }
    if folded > 0 {
        tx.query(
            "UPDATE chunk_history SET commit = $into, seq = $into_seq
             WHERE commit = $from RETURN NONE;",
        )
        .bind(("from", commit_bytes(from)))
        .bind(("into", commit_bytes(into)))
        .bind(("into_seq", into_seq))
        .await
        .map_err(db_err)?
        .check()
        .map_err(db_err)?;
    }
    tx.query("UPDATE ref SET commit = $into WHERE commit = $from RETURN NONE;")
        .bind(("from", commit_bytes(from)))
        .bind(("into", commit_bytes(into)))
        .await
        .map_err(db_err)?
        .check()
        .map_err(db_err)?;

    for commit in commits {
        if commit.id == from || !commit.parents.contains(&from) {
            continue;
        }
        let mut parents: Vec<CommitId> = commit
            .parents
            .iter()
            .copied()
            .filter(|parent| *parent != from)
            .collect();
        parents.extend(from_parents.iter().copied());
        parents.sort_unstable();
        parents.dedup();
        tx.query("UPDATE $record SET parents = $parents RETURN NONE;")
            .bind(("record", commit_record(commit.id)))
            .bind(("parents", parent_bytes(&parents)))
            .await
            .map_err(db_err)?
            .check()
            .map_err(db_err)?;
    }

    tx.query("DELETE $record RETURN NONE;")
        .bind(("record", commit_record(from)))
        .await
        .map_err(db_err)?
        .check()
        .map_err(db_err)?;

    Ok(FoldOutcome {
        folded,
        dropped: dropped.len(),
    })
}

impl<C: Connection> SurrealdbStore<C> {
    async fn load_commits(&self) -> Result<Vec<StoredCommit>, StorageError> {
        let mut response = self
            .db
            .query("SELECT * FROM commit ORDER BY seq;")
            .await
            .map_err(db_err)?
            .check()
            .map_err(db_err)?;
        let rows: Vec<Value> = response.take(0).map_err(db_err)?;
        rows.into_iter().map(decode_commit).collect()
    }

    async fn load_history(&self, commit: CommitId) -> Result<Vec<StoredHistory>, StorageError> {
        let mut response = self
            .db
            .query("SELECT * FROM chunk_history WHERE commit = $commit;")
            .bind(("commit", commit_bytes(commit)))
            .await
            .map_err(db_err)?
            .check()
            .map_err(db_err)?;
        let rows: Vec<Value> = response.take(0).map_err(db_err)?;
        rows.into_iter().map(decode_history).collect()
    }

    async fn superseding_coords(
        &self,
        ancestors: &BTreeSet<CommitId>,
        from_seq: i64,
    ) -> Result<BTreeSet<(i32, i32, i32, i32)>, StorageError> {
        let mut response = self
            .db
            .query(
                "SELECT dim, kind, cx, cz FROM chunk_history
                 WHERE seq > $from_seq AND commit IN $commits;",
            )
            .bind(("from_seq", from_seq))
            .bind(("commits", commit_id_set(ancestors)))
            .await
            .map_err(db_err)?
            .check()
            .map_err(db_err)?;
        let rows: Vec<Value> = response.take(0).map_err(db_err)?;
        rows.into_iter()
            .map(decode_coord_fields)
            .collect::<Result<_, _>>()
    }
}

impl<C: Connection> MetaStore for SurrealdbStore<C> {
    type Error = StorageError;

    async fn create_commit(
        &mut self,
        created_at_ms: u64,
        parents: &[CommitId],
    ) -> Result<CommitId, StorageError> {
        let _write = self.write_lock.lock().await;
        let id = commit_hash(parents, created_at_ms, &[]);
        let parents = normalized_parents(parents);
        let tx = self.db.clone().begin().await.map_err(db_err)?;
        let result = insert_commit(&tx, id, created_at_ms, &parents).await;
        finish_commit(tx, result).await?;
        Ok(id)
    }

    async fn commit_parents(&self, commit: CommitId) -> Result<Vec<CommitId>, StorageError> {
        let Some(value) = selected_value(&self.db, commit_record(commit)).await? else {
            return Ok(Vec::new());
        };
        Ok(decode_commit(value)?.parents)
    }

    async fn record_chunk(
        &mut self,
        commit: CommitId,
        coord: &ChunkCoord,
        blob: Option<&ChunkHash>,
    ) -> Result<(), StorageError> {
        let _write = self.write_lock.lock().await;
        let tx = self.db.clone().begin().await.map_err(db_err)?;
        let result = record_chunk_tx(&tx, commit, coord, blob).await;
        finish_commit(tx, result).await
    }

    async fn lookup_chunk(
        &self,
        commit: CommitId,
        coord: &ChunkCoord,
    ) -> Result<Option<ChunkHistoryEntry>, StorageError> {
        let commits = self.load_commits().await?;
        let ancestors = ancestor_ids(&commits, commit);
        let mut response = self
            .db
            .query(
                "SELECT * FROM chunk_history
                 WHERE commit IN $commits
                   AND dim = $dim AND kind = $kind AND cx = $cx AND cz = $cz
                 ORDER BY seq DESC LIMIT 1;",
            )
            .bind(("commits", commit_id_set(&ancestors)))
            .bind(("dim", i64::from(coord.dim.raw())))
            .bind(("kind", i64::from(coord.kind.raw())))
            .bind(("cx", i64::from(coord.x)))
            .bind(("cz", i64::from(coord.z)))
            .await
            .map_err(db_err)?
            .check()
            .map_err(db_err)?;
        let rows: Vec<Value> = response.take(0).map_err(db_err)?;
        rows.into_iter()
            .next()
            .map(|value| decode_history(value).map(|stored| stored.entry))
            .transpose()
    }

    async fn lookup_commit(&self, id: CommitId) -> Result<Option<Commit>, StorageError> {
        selected_value(&self.db, commit_record(id))
            .await?
            .map(|value| decode_commit(value).map(StoredCommit::into_commit))
            .transpose()
    }

    async fn latest_commit(&self) -> Result<Option<Commit>, StorageError> {
        let mut response = self
            .db
            .query("SELECT * FROM commit ORDER BY seq DESC LIMIT 1;")
            .await
            .map_err(db_err)?
            .check()
            .map_err(db_err)?;
        let rows: Vec<Value> = response.take(0).map_err(db_err)?;
        rows.into_iter()
            .next()
            .map(|value| decode_commit(value).map(StoredCommit::into_commit))
            .transpose()
    }

    async fn visit_commit_chunks<F>(
        &self,
        commit: CommitId,
        mut visit: F,
    ) -> Result<(), StorageError>
    where
        F: FnMut(&ChunkHistoryEntry) -> bool + Send,
    {
        let commits = self.load_commits().await?;
        let ancestors = ancestor_ids(&commits, commit);
        let mut response = self
            .db
            .query(
                "SELECT * FROM chunk_history WHERE commit IN $commits
                 ORDER BY dim, kind, cx, cz, seq DESC;",
            )
            .bind(("commits", commit_id_set(&ancestors)))
            .await
            .map_err(db_err)?
            .check()
            .map_err(db_err)?;
        let rows: Vec<Value> = response.take(0).map_err(db_err)?;
        let mut current: Option<ChunkCoord> = None;
        for value in rows {
            let entry = decode_history(value)?.entry;
            if current == Some(entry.coord) {
                continue;
            }
            current = Some(entry.coord);
            if !visit(&entry) {
                break;
            }
        }
        Ok(())
    }

    async fn visit_fresh_rows<F>(&self, commit: CommitId, mut visit: F) -> Result<(), StorageError>
    where
        F: FnMut(&ChunkHistoryEntry) -> bool + Send,
    {
        let mut response = self
            .db
            .query("SELECT * FROM chunk_history WHERE commit = $commit;")
            .bind(("commit", commit_bytes(commit)))
            .await
            .map_err(db_err)?
            .check()
            .map_err(db_err)?;
        let rows: Vec<Value> = response.take(0).map_err(db_err)?;
        for value in rows {
            if !visit(&decode_history(value)?.entry) {
                break;
            }
        }
        Ok(())
    }

    async fn visit_commits<F>(&self, mut visit: F) -> Result<(), StorageError>
    where
        F: FnMut(&Commit) -> bool + Send,
    {
        for commit in self.load_commits().await? {
            if !visit(&commit.into_commit()) {
                break;
            }
        }
        Ok(())
    }

    async fn apply_commit_incremental(
        &mut self,
        created_at_ms: u64,
        parents: &[CommitId],
        entries: &[CommitEntry],
    ) -> Result<ApplyOutcome, StorageError> {
        let _write = self.write_lock.lock().await;
        let id = commit_hash(parents, created_at_ms, entries);
        let parents = normalized_parents(parents);
        let tx = self.db.clone().begin().await.map_err(db_err)?;
        let result = apply_commit_tx(&tx, id, created_at_ms, &parents, entries).await;
        finish_commit(tx, result).await
    }

    async fn retire_commit(
        &mut self,
        from: CommitId,
        into: CommitId,
    ) -> Result<FoldOutcome, StorageError> {
        let _write = self.write_lock.lock().await;
        let commits = self.load_commits().await?;
        let from_row = commits
            .iter()
            .find(|commit| commit.id == from)
            .ok_or(StorageError::UnknownCommit { id: from })?;
        let into_row = commits
            .iter()
            .find(|commit| commit.id == into)
            .ok_or(StorageError::UnknownCommit { id: into })?;
        if into_row.seq <= from_row.seq {
            return Err(StorageError::InvalidRetirement { from, into });
        }
        let ancestors = ancestor_ids(&commits, into);
        if !ancestors.contains(&from) {
            return Err(StorageError::UnrelatedRetirement { from, into });
        }

        let superseding = self.superseding_coords(&ancestors, from_row.seq).await?;
        let from_parents = from_row.parents.clone();
        let into_seq = into_row.seq;
        let mut dropped = Vec::new();
        let mut folded = 0usize;
        for row in self.load_history(from).await? {
            if superseding.contains(&coord_key(row.entry.coord)) {
                let Some(record) = row.record else {
                    return Err(crate::row::corrupt("chunk-history row has no record ID"));
                };
                dropped.push(record);
            } else {
                folded += 1;
            }
        }

        let tx = self.db.clone().begin().await.map_err(db_err)?;
        let result = retire_writes(
            &tx,
            from,
            into,
            &from_parents,
            into_seq,
            &commits,
            &dropped,
            folded,
        )
        .await;
        finish_commit(tx, result).await
    }

    async fn set_ref(
        &mut self,
        name: &RefName,
        commit: Option<CommitId>,
        created_at_ms: u64,
    ) -> Result<(), StorageError> {
        let _write = self.write_lock.lock().await;
        let tx = self.db.clone().begin().await.map_err(db_err)?;
        let result = set_ref_tx(&tx, name, commit, created_at_ms).await;
        finish_commit(tx, result).await
    }

    async fn lookup_ref(&self, name: &RefName) -> Result<Option<CommitRef>, StorageError> {
        selected_value(&self.db, ref_record(name))
            .await?
            .map(decode_commit_ref)
            .transpose()
    }

    async fn visit_refs<F>(&self, mut visit: F) -> Result<(), StorageError>
    where
        F: FnMut(&CommitRef) -> bool + Send,
    {
        let mut response = self
            .db
            .query("SELECT * FROM ref ORDER BY name;")
            .await
            .map_err(db_err)?
            .check()
            .map_err(db_err)?;
        let rows: Vec<Value> = response.take(0).map_err(db_err)?;
        for value in rows {
            if !visit(&decode_commit_ref(value)?) {
                break;
            }
        }
        Ok(())
    }

    async fn delete_ref(&mut self, name: &RefName) -> Result<bool, StorageError> {
        let _write = self.write_lock.lock().await;
        let tx = self.db.clone().begin().await.map_err(db_err)?;
        let result = delete_ref_tx(&tx, name).await;
        finish_commit(tx, result).await
    }

    async fn active_ref(&self) -> Result<Option<CommitRef>, StorageError> {
        let mut response = self
            .db
            .query("SELECT * FROM ref WHERE active = true LIMIT 1;")
            .await
            .map_err(db_err)?
            .check()
            .map_err(db_err)?;
        let rows: Vec<Value> = response.take(0).map_err(db_err)?;
        let Some(value) = rows.into_iter().next() else {
            return Ok(None);
        };
        let record = decode_commit_ref(value)?;
        if let Some(commit) = record.commit
            && selected_value(&self.db, commit_record(commit))
                .await?
                .is_none()
        {
            return Err(StorageError::UnknownCommit { id: commit });
        }
        Ok(Some(record))
    }

    async fn activate_ref(&mut self, name: &RefName) -> Result<(), StorageError> {
        let _write = self.write_lock.lock().await;
        let tx = self.db.clone().begin().await.map_err(db_err)?;
        let result = activate_ref_tx(&tx, name).await;
        finish_commit(tx, result).await
    }

    async fn set_scope(&mut self, scope: &NamedScope) -> Result<(), StorageError> {
        let _write = self.write_lock.lock().await;
        let tx = self.db.clone().begin().await.map_err(db_err)?;
        let result = set_scope_tx(&tx, scope).await;
        finish_commit(tx, result).await
    }

    async fn lookup_scope(&self, name: &ScopeName) -> Result<Option<NamedScope>, StorageError> {
        selected_value(&self.db, scope_record(name))
            .await?
            .map(decode_scope)
            .transpose()
    }

    async fn visit_scopes<F>(&self, mut visit: F) -> Result<(), StorageError>
    where
        F: FnMut(&NamedScope) -> bool + Send,
    {
        let mut response = self
            .db
            .query("SELECT * FROM scope ORDER BY name;")
            .await
            .map_err(db_err)?
            .check()
            .map_err(db_err)?;
        let rows: Vec<Value> = response.take(0).map_err(db_err)?;
        for value in rows {
            if !visit(&decode_scope(value)?) {
                break;
            }
        }
        Ok(())
    }

    async fn delete_scope(&mut self, name: &ScopeName) -> Result<bool, StorageError> {
        let _write = self.write_lock.lock().await;
        let mut response = self
            .db
            .query("DELETE $record RETURN BEFORE;")
            .bind(("record", scope_record(name)))
            .await
            .map_err(db_err)?
            .check()
            .map_err(db_err)?;
        let removed: Vec<Value> = response.take(0).map_err(db_err)?;
        Ok(!removed.is_empty())
    }
}
