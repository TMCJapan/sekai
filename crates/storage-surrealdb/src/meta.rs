//! `SurrealDB` commit metadata: the history graph and per-chunk history.

use std::collections::{BTreeMap, BTreeSet};

use sekai_core::{
    ApplyOutcome, ChunkCoord, ChunkHash, ChunkHistoryEntry, Commit, CommitEntry, CommitId,
    CommitRef, FoldOutcome, MetaStore, NamedScope, RefName, ScopeName, commit_hash,
};
use sekai_storage::StorageError;
use surrealdb::Connection;
use surrealdb::method::Transaction;
use surrealdb::types::{Bytes, RecordId};

use crate::SurrealdbStore;
use crate::row::{
    CommitParentsPatch, CommitRow, CoordRow, HistoryMove, HistoryRow, RefActivePatch,
    RefCommitPatch, RefRow, ScopeRow, StoredCommit, StoredHistory, StoredHistoryRow, commit_bytes,
    commit_record, db_err, ref_record, ref_record_name, scope_record,
};

/// Managed schema version stored in `meta:store`.
pub const SCHEMA_VERSION: i64 = 1;

fn normalized_parents(parents: &[CommitId]) -> Vec<CommitId> {
    let mut parents = parents.to_vec();
    parents.sort_unstable();
    parents.dedup();
    parents
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

pub(crate) async fn finish_tx<C: Connection, T>(
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
        let existing: Option<CommitRow> =
            tx.select(commit_record(*parent)).await.map_err(db_err)?;
        if existing.is_none() {
            return Err(StorageError::UnknownCommit { id: *parent });
        }
    }
    let existing: Option<CommitRow> = tx.select(commit_record(id)).await.map_err(db_err)?;
    if existing.is_some() {
        return Ok(None);
    }

    let mut response = tx
        .query("RETURN sequence::nextval('commit_seq');")
        .await
        .map_err(db_err)?
        .check()
        .map_err(db_err)?;
    let seq: Option<i64> = response.take(0).map_err(db_err)?;
    let seq = seq.ok_or_else(|| crate::row::corrupt("commit sequence returned no value"))?;

    let created: Option<CommitRow> = tx
        .create(commit_record(id))
        .content(CommitRow::new(id, seq, created_at_ms, parents)?)
        .await
        .map_err(db_err)?;
    Ok(created.map(|_| seq))
}

async fn insert_history<C: Connection>(
    tx: &Transaction<C>,
    commit: CommitId,
    seq: i64,
    coord: ChunkCoord,
    blob: Option<&ChunkHash>,
) -> Result<(), StorageError> {
    let _: Option<StoredHistoryRow> = tx
        .create("chunk_history")
        .content(HistoryRow::new(commit, seq, coord, blob))
        .await
        .map_err(db_err)?;
    Ok(())
}

async fn record_chunk_tx<C: Connection>(
    tx: &Transaction<C>,
    commit: CommitId,
    coord: &ChunkCoord,
    blob: Option<&ChunkHash>,
) -> Result<(), StorageError> {
    let stored: Option<CommitRow> = tx.select(commit_record(commit)).await.map_err(db_err)?;
    let stored = stored.ok_or(StorageError::UnknownCommit { id: commit })?;
    insert_history(tx, commit, stored.seq, *coord, blob).await
}

async fn apply_commit_tx<C: Connection>(
    tx: &Transaction<C>,
    id: CommitId,
    created_at_ms: u64,
    parents: &[CommitId],
    entries: &[CommitEntry],
) -> Result<ApplyOutcome, StorageError> {
    let refs: Vec<RefRow> = tx.select("ref").await.map_err(db_err)?;
    let active = refs
        .into_iter()
        .find(|row| row.active)
        .ok_or(StorageError::NoActiveRef)?
        .into_domain()?;

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

    let moved: Option<RefRow> = tx
        .update(ref_record(&active.name))
        .merge(RefCommitPatch::new(Some(id)))
        .await
        .map_err(db_err)?;
    if moved.is_none() {
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
    if let Some(id) = commit {
        let existing: Option<CommitRow> = tx.select(commit_record(id)).await.map_err(db_err)?;
        if existing.is_none() {
            return Err(StorageError::UnknownCommit { id });
        }
    }

    let updated: Option<RefRow> = tx
        .update(ref_record(name))
        .merge(RefCommitPatch::new(commit))
        .await
        .map_err(db_err)?;
    if updated.is_some() {
        return Ok(());
    }

    let created: Option<RefRow> = tx
        .create(ref_record(name))
        .content(RefRow::new(name, commit, false, created_at_ms)?)
        .await
        .map_err(db_err)?;
    if created.is_none() {
        return Err(crate::row::corrupt("failed to create ref row"));
    }
    Ok(())
}

async fn delete_ref_tx<C: Connection>(
    tx: &Transaction<C>,
    name: &RefName,
) -> Result<bool, StorageError> {
    let existing: Option<RefRow> = tx.select(ref_record(name)).await.map_err(db_err)?;
    let Some(row) = existing else {
        return Ok(false);
    };
    if row.active {
        return Err(StorageError::ActiveRefProtected {
            name: name.as_str().to_owned(),
        });
    }
    let _: Option<RefRow> = tx.delete(ref_record(name)).await.map_err(db_err)?;
    Ok(true)
}

async fn activate_ref_tx<C: Connection>(
    tx: &Transaction<C>,
    name: &RefName,
) -> Result<(), StorageError> {
    let existing: Option<RefRow> = tx.select(ref_record(name)).await.map_err(db_err)?;
    let record = existing
        .ok_or_else(|| StorageError::UnknownRef {
            name: name.as_str().to_owned(),
        })?
        .into_domain()?;
    if let Some(commit) = record.commit {
        let target: Option<CommitRow> = tx.select(commit_record(commit)).await.map_err(db_err)?;
        if target.is_none() {
            return Err(StorageError::UnknownCommit { id: commit });
        }
    }

    let refs: Vec<RefRow> = tx.select("ref").await.map_err(db_err)?;
    for row in refs.into_iter().filter(|row| row.active) {
        let _: Option<RefRow> = tx
            .update(ref_record_name(&row.name))
            .merge(RefActivePatch { active: false })
            .await
            .map_err(db_err)?;
    }

    let activated: Option<RefRow> = tx
        .update(ref_record(name))
        .merge(RefActivePatch { active: true })
        .await
        .map_err(db_err)?;
    if activated.is_none() {
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
    let existing: Option<ScopeRow> = tx.select(scope_record(&scope.name)).await.map_err(db_err)?;
    let created_at_ms = existing.map_or(scope.created_at_ms, |row| row.created_at_ms);
    let _: Option<ScopeRow> = tx
        .upsert(scope_record(&scope.name))
        .content(ScopeRow::new(scope, created_at_ms)?)
        .await
        .map_err(db_err)?;
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
    folded: &[RecordId],
) -> Result<FoldOutcome, StorageError> {
    for record in dropped {
        let _: Option<StoredHistoryRow> = tx.delete(record.clone()).await.map_err(db_err)?;
    }
    for record in folded {
        let _: Option<StoredHistoryRow> = tx
            .update(record.clone())
            .merge(HistoryMove::new(into, into_seq))
            .await
            .map_err(db_err)?;
    }

    let from_bytes = commit_bytes(from);
    let refs: Vec<RefRow> = tx.select("ref").await.map_err(db_err)?;
    for row in refs {
        if row.commit.as_ref() == Some(&from_bytes) {
            let _: Option<RefRow> = tx
                .update(ref_record_name(&row.name))
                .merge(RefCommitPatch::new(Some(into)))
                .await
                .map_err(db_err)?;
        }
    }

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
        let _: Option<CommitRow> = tx
            .update(commit_record(commit.id))
            .merge(CommitParentsPatch::new(&parents))
            .await
            .map_err(db_err)?;
    }

    let _: Option<CommitRow> = tx.delete(commit_record(from)).await.map_err(db_err)?;

    Ok(FoldOutcome {
        folded: folded.len(),
        dropped: dropped.len(),
    })
}

impl<C: Connection> SurrealdbStore<C> {
    async fn load_commits(&self) -> Result<Vec<StoredCommit>, StorageError> {
        let rows: Vec<CommitRow> = self.db.select("commit").await.map_err(db_err)?;
        let mut commits = rows
            .into_iter()
            .map(CommitRow::into_stored)
            .collect::<Result<Vec<_>, _>>()?;
        commits.sort_unstable_by_key(|commit| commit.seq);
        Ok(commits)
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
        let rows: Vec<StoredHistoryRow> = response.take(0).map_err(db_err)?;
        rows.into_iter()
            .map(StoredHistoryRow::into_stored)
            .collect()
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
        let rows: Vec<CoordRow> = response.take(0).map_err(db_err)?;
        Ok(rows.into_iter().map(CoordRow::into_key).collect())
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
        finish_tx(tx, result).await?;
        Ok(id)
    }

    async fn commit_parents(&self, commit: CommitId) -> Result<Vec<CommitId>, StorageError> {
        let row: Option<CommitRow> = self
            .db
            .select(commit_record(commit))
            .await
            .map_err(db_err)?;
        match row {
            Some(row) => Ok(row.into_stored()?.parents),
            None => Ok(Vec::new()),
        }
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
        finish_tx(tx, result).await
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
            .bind(("dim", coord.dim.raw()))
            .bind(("kind", i32::from(coord.kind.raw())))
            .bind(("cx", coord.x))
            .bind(("cz", coord.z))
            .await
            .map_err(db_err)?
            .check()
            .map_err(db_err)?;
        let rows: Vec<StoredHistoryRow> = response.take(0).map_err(db_err)?;
        rows.into_iter()
            .next()
            .map(StoredHistoryRow::into_stored)
            .transpose()
            .map(|stored| stored.map(|stored| stored.entry))
    }

    async fn lookup_commit(&self, id: CommitId) -> Result<Option<Commit>, StorageError> {
        let row: Option<CommitRow> = self.db.select(commit_record(id)).await.map_err(db_err)?;
        Ok(row
            .map(CommitRow::into_stored)
            .transpose()?
            .map(StoredCommit::into_commit))
    }

    async fn latest_commit(&self) -> Result<Option<Commit>, StorageError> {
        let commits = self.load_commits().await?;
        Ok(commits
            .into_iter()
            .max_by_key(|commit| commit.seq)
            .map(StoredCommit::into_commit))
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
        let rows: Vec<StoredHistoryRow> = response.take(0).map_err(db_err)?;
        let mut current: Option<ChunkCoord> = None;
        for row in rows {
            let entry = row.into_stored()?.entry;
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
        for row in self.load_history(commit).await? {
            if !visit(&row.entry) {
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
        finish_tx(tx, result).await
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
        let mut folded = Vec::new();
        for row in self.load_history(from).await? {
            if superseding.contains(&coord_key(row.entry.coord)) {
                dropped.push(row.record);
            } else {
                folded.push(row.record);
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
            &folded,
        )
        .await;
        finish_tx(tx, result).await
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
        finish_tx(tx, result).await
    }

    async fn lookup_ref(&self, name: &RefName) -> Result<Option<CommitRef>, StorageError> {
        let row: Option<RefRow> = self.db.select(ref_record(name)).await.map_err(db_err)?;
        row.map(RefRow::into_domain).transpose()
    }

    async fn visit_refs<F>(&self, mut visit: F) -> Result<(), StorageError>
    where
        F: FnMut(&CommitRef) -> bool + Send,
    {
        let rows: Vec<RefRow> = self.db.select("ref").await.map_err(db_err)?;
        let mut refs = rows
            .into_iter()
            .map(RefRow::into_domain)
            .collect::<Result<Vec<_>, _>>()?;
        refs.sort_by(|left, right| left.name.as_str().cmp(right.name.as_str()));
        for record in refs {
            if !visit(&record) {
                break;
            }
        }
        Ok(())
    }

    async fn delete_ref(&mut self, name: &RefName) -> Result<bool, StorageError> {
        let _write = self.write_lock.lock().await;
        let tx = self.db.clone().begin().await.map_err(db_err)?;
        let result = delete_ref_tx(&tx, name).await;
        finish_tx(tx, result).await
    }

    async fn active_ref(&self) -> Result<Option<CommitRef>, StorageError> {
        let rows: Vec<RefRow> = self.db.select("ref").await.map_err(db_err)?;
        let Some(row) = rows.into_iter().find(|row| row.active) else {
            return Ok(None);
        };
        let record = row.into_domain()?;
        if let Some(commit) = record.commit {
            let target: Option<CommitRow> = self
                .db
                .select(commit_record(commit))
                .await
                .map_err(db_err)?;
            if target.is_none() {
                return Err(StorageError::UnknownCommit { id: commit });
            }
        }
        Ok(Some(record))
    }

    async fn activate_ref(&mut self, name: &RefName) -> Result<(), StorageError> {
        let _write = self.write_lock.lock().await;
        let tx = self.db.clone().begin().await.map_err(db_err)?;
        let result = activate_ref_tx(&tx, name).await;
        finish_tx(tx, result).await
    }

    async fn set_scope(&mut self, scope: &NamedScope) -> Result<(), StorageError> {
        let _write = self.write_lock.lock().await;
        let tx = self.db.clone().begin().await.map_err(db_err)?;
        let result = set_scope_tx(&tx, scope).await;
        finish_tx(tx, result).await
    }

    async fn lookup_scope(&self, name: &ScopeName) -> Result<Option<NamedScope>, StorageError> {
        let row: Option<ScopeRow> = self.db.select(scope_record(name)).await.map_err(db_err)?;
        row.map(ScopeRow::into_domain).transpose()
    }

    async fn visit_scopes<F>(&self, mut visit: F) -> Result<(), StorageError>
    where
        F: FnMut(&NamedScope) -> bool + Send,
    {
        let rows: Vec<ScopeRow> = self.db.select("scope").await.map_err(db_err)?;
        let mut scopes = rows
            .into_iter()
            .map(ScopeRow::into_domain)
            .collect::<Result<Vec<_>, _>>()?;
        scopes.sort_by(|left, right| left.name.as_str().cmp(right.name.as_str()));
        for scope in scopes {
            if !visit(&scope) {
                break;
            }
        }
        Ok(())
    }

    async fn delete_scope(&mut self, name: &ScopeName) -> Result<bool, StorageError> {
        let _write = self.write_lock.lock().await;
        let removed: Option<ScopeRow> = self.db.delete(scope_record(name)).await.map_err(db_err)?;
        Ok(removed.is_some())
    }
}
