//! redb commit metadata: the history graph and snapshot trees.
//!
//! Recording a commit uses one transaction; blob durability is established
//! before the metadata commit by the caller. redb admits one writer at a
//! time, so every mutation is a single exclusive write transaction.
//!
//! Layout (all tables in one `meta.redb` file):
//! - `root_tree` / `dim_tree` / `region_tree`: 32-byte node hash -> canonical
//!   node bytes (the codec lives in `sekai_storage::tree`).
//! - `commits`: record sequence (`u64`, from 1) -> 72-byte record
//!   `[id:32][created_at_ms i64 LE:8][root_hash:32]`.
//! - `commit_ids`: commit id -> record sequence.
//! - `commit_parents`: multimap child id -> parent ids, sorted and deduped
//!   by redb.
//! - `refs`: name -> `[created_at_ms:8][commit:0|32]`. The active flag is
//!   not stored per ref; it is derived from the state's `active_ref` key,
//!   the single source of truth for which ref new commits hang from.
//! - `scopes`: name -> `[created_at_ms:8][n x 20-byte areas]`, each area
//!   `[dim, x0, z0, x1, z1]` as `i32` LE.
//! - `__sekai_state`: `schema` (`i64` LE), `next_seq` (`u64` LE), and
//!   `active_ref` (ref name bytes).
//!
//! redb is a synchronous embedded database, so every operation runs on the
//! runtime's blocking pool, never inline on an async worker.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;

use redb::{
    Database, MultimapTableDefinition, ReadTransaction, ReadableDatabase, ReadableMultimapTable,
    ReadableTable, TableDefinition, WriteTransaction,
};
use sekai_core::{
    ApplyOutcome, ChunkCoord, Commit, CommitEntry, CommitId, CommitRef, DEFAULT_REF_NAME, DimHash,
    Dimension, NamedScope, NamedScopeError, Rect, RefName, RegionHash, RootHash, ScopeName,
    commit_hash,
};
use sekai_storage::{DimNode, MetaStore, RegionNode, RootNode, StorageError, TreeNodes, io_error};

/// Managed schema version (the state table's `schema` key).
///
/// Version 18 moved the metadata from SQLite to redb. There is no migration
/// path from version 17: old stores are rejected via `UnsupportedSchema`
/// (pre-release policy recreates, it never migrates).
pub const SCHEMA_VERSION: i64 = 18;

const ROOT_TREE: TableDefinition<&[u8; 32], &[u8]> = TableDefinition::new("root_tree");
const DIM_TREE: TableDefinition<&[u8; 32], &[u8]> = TableDefinition::new("dim_tree");
const REGION_TREE: TableDefinition<&[u8; 32], &[u8]> = TableDefinition::new("region_tree");
const COMMITS: TableDefinition<u64, &[u8]> = TableDefinition::new("commits");
const COMMIT_IDS: TableDefinition<&[u8; 32], u64> = TableDefinition::new("commit_ids");
const COMMIT_PARENTS: MultimapTableDefinition<&[u8; 32], &[u8; 32]> =
    MultimapTableDefinition::new("commit_parents");
const REFS: TableDefinition<&str, &[u8]> = TableDefinition::new("refs");
const SCOPES: TableDefinition<&str, &[u8]> = TableDefinition::new("scopes");
const STATE: TableDefinition<&str, &[u8]> = TableDefinition::new("__sekai_state");

const STATE_SCHEMA: &str = "schema";
const STATE_NEXT_SEQ: &str = "next_seq";
const STATE_ACTIVE_REF: &str = "active_ref";

/// Map a backend failure into the shared storage error.
fn db_err(source: impl Into<redb::Error>) -> StorageError {
    StorageError::backend("redb", source.into())
}

/// Map a blocking-task failure (cancellation or panic) into the shared error.
fn join_err(source: tokio::task::JoinError) -> StorageError {
    StorageError::backend("redb", source)
}

/// A stored record failed its layout check: corruption, never a value to
/// reinterpret.
fn corrupt(message: impl Into<String>) -> StorageError {
    db_err(redb::StorageError::Corrupted(message.into()))
}

/// Decode a 32-byte hash from stored bytes.
fn hash32(bytes: &[u8]) -> Result<[u8; 32], StorageError> {
    bytes
        .try_into()
        .map_err(|_| StorageError::InvalidHashLength { len: bytes.len() })
}

/// Decode a millisecond timestamp stored as `i64` LE. Negative values are
/// out of the stored domain, reported loudly.
fn millis_of(bytes: &[u8]) -> Result<u64, StorageError> {
    let octets: [u8; 8] = bytes.try_into().map_err(|_| {
        corrupt(format!(
            "timestamp has invalid length: {} bytes",
            bytes.len()
        ))
    })?;
    let value = i64::from_le_bytes(octets);
    u64::try_from(value).map_err(|_| StorageError::InvalidHistoryValue {
        column: "created_at_ms",
        value,
    })
}

/// Millis as a stored `i64`. Genuinely fallible: callers compute the clock,
/// so an absurd value is their bug, surfaced loudly.
fn millis_param(ms: u64) -> Result<i64, StorageError> {
    i64::try_from(ms).map_err(|_| StorageError::InvalidTimestamp(ms))
}

/// The current time in Unix millis, for the rows a store creates for
/// itself.
///
/// Kept local rather than shared with the app's clock helper: failures
/// here are `StorageError`s, and an out-of-range value is rejected
/// instead of saturating. The app does not read the clock for a move
/// that keeps an existing timestamp.
fn now_ms() -> Result<i64, StorageError> {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| StorageError::ClockBeforeEpoch)?
        .as_millis();
    // `as_millis` is `u128`: a value above `u64::MAX` has no
    // representation in the stored range, so it gets its own error rather
    // than a sentinel timestamp.
    let millis = u64::try_from(millis).map_err(|_| StorageError::ClockOutOfRange)?;
    i64::try_from(millis).map_err(|_| StorageError::InvalidTimestamp(millis))
}

/// Map a tree codec failure into the storage error surface.
const fn tree_err(err: sekai_storage::TreeError) -> StorageError {
    match err {
        sekai_storage::TreeError::UnknownKind { kind } => StorageError::UnknownRegionKind { kind },
        other => StorageError::Tree(other),
    }
}

/// One commit's fixed-width record: `[id:32][created_at_ms:8][root_hash:32]`.
const COMMIT_RECORD_LEN: usize = 72;

fn encode_commit(id: CommitId, created_at_ms: i64, root: RootHash) -> [u8; COMMIT_RECORD_LEN] {
    let mut record = [0u8; COMMIT_RECORD_LEN];
    record[..32].copy_from_slice(&id.0);
    record[32..40].copy_from_slice(&created_at_ms.to_le_bytes());
    record[40..].copy_from_slice(root.as_bytes());
    record
}

struct CommitRecord {
    id: CommitId,
    created_at_ms: u64,
    root: RootHash,
}

fn decode_commit_record(bytes: &[u8]) -> Result<CommitRecord, StorageError> {
    if bytes.len() != COMMIT_RECORD_LEN {
        return Err(corrupt(format!(
            "commit record has invalid length: {} bytes, expected {COMMIT_RECORD_LEN}",
            bytes.len()
        )));
    }
    Ok(CommitRecord {
        id: CommitId(hash32(&bytes[..32])?),
        created_at_ms: millis_of(&bytes[32..40])?,
        root: RootHash::new(hash32(&bytes[40..])?),
    })
}

/// One ref's record: `[created_at_ms:8][commit:0|32]`.
fn encode_ref(created_at_ms: i64, commit: Option<CommitId>) -> Vec<u8> {
    let mut record = created_at_ms.to_le_bytes().to_vec();
    if let Some(id) = commit {
        record.extend_from_slice(&id.0);
    }
    record
}

/// Split a stored ref record into its creation-time bytes and target.
fn split_ref(record: &[u8]) -> Result<(&[u8], Option<CommitId>), StorageError> {
    match record.len() {
        8 => Ok((&record[..8], None)),
        40 => Ok((&record[..8], Some(CommitId(hash32(&record[8..])?)))),
        len => Err(corrupt(format!(
            "ref record has invalid length: {len} bytes, expected 8 or 40"
        ))),
    }
}

/// Decode one stored ref. Stored names always re-validate: anything else
/// is corruption, never silent reinterpretation.
fn decode_ref(name: &str, record: &[u8], active: bool) -> Result<CommitRef, StorageError> {
    let parsed = RefName::parse(name).map_err(|_| StorageError::InvalidRefName {
        name: name.to_owned(),
    })?;
    let (created, commit) = split_ref(record)?;
    Ok(CommitRef::new(parsed, commit, active, millis_of(created)?))
}

/// One scope area: `[dim, x0, z0, x1, z1]` as `i32` LE.
const AREA_LEN: usize = 20;

fn encode_scope(created_at_ms: i64, areas: &[(Dimension, Rect)]) -> Vec<u8> {
    let mut record = created_at_ms.to_le_bytes().to_vec();
    for (dim, rect) in areas {
        for value in [dim.raw(), rect.x0, rect.z0, rect.x1, rect.z1] {
            record.extend_from_slice(&value.to_le_bytes());
        }
    }
    record
}

/// Validate a stored scope record's layout, returning its creation-time
/// bytes.
fn scope_header(record: &[u8]) -> Result<[u8; 8], StorageError> {
    if record.len() < 8 || !(record.len() - 8).is_multiple_of(AREA_LEN) {
        return Err(corrupt(format!(
            "scope record has invalid length: {} bytes",
            record.len()
        )));
    }
    record[..8]
        .try_into()
        .map_err(|_| corrupt("malformed scope header"))
}

const fn area_i32(area: &[u8], offset: usize) -> i32 {
    i32::from_le_bytes([
        area[offset],
        area[offset + 1],
        area[offset + 2],
        area[offset + 3],
    ])
}

/// Decode one stored scope. Stored names re-validate and a scope must hold
/// an area: anything else is corruption, never silent reinterpretation.
fn decode_scope(name: &str, record: &[u8]) -> Result<NamedScope, StorageError> {
    let parsed = ScopeName::parse(name).map_err(|_| StorageError::InvalidScopeName {
        name: name.to_owned(),
    })?;
    let created = scope_header(record)?;
    let mut areas = Vec::with_capacity((record.len() - 8) / AREA_LEN);
    for area in record[8..].as_chunks::<AREA_LEN>().0 {
        areas.push((
            Dimension::new(area_i32(area, 0)),
            Rect::new(
                area_i32(area, 4),
                area_i32(area, 8),
                area_i32(area, 12),
                area_i32(area, 16),
            ),
        ));
    }
    NamedScope::new(parsed, areas, millis_of(&created)?).map_err(|NamedScopeError::NoAreas| {
        StorageError::EmptyScope {
            name: name.to_owned(),
        }
    })
}

/// Decode the stored version marker; anything but eight bytes is
/// corruption, never a version to reinterpret.
fn version_of(bytes: &[u8]) -> Result<i64, StorageError> {
    let octets: [u8; 8] = bytes.try_into().map_err(|_| {
        corrupt(format!(
            "malformed schema version: {} bytes, expected 8",
            bytes.len()
        ))
    })?;
    Ok(i64::from_le_bytes(octets))
}

/// Run one read transaction; the snapshot is released when it returns.
fn with_read<T>(
    db: &Database,
    f: impl FnOnce(&ReadTransaction) -> Result<T, StorageError>,
) -> Result<T, StorageError> {
    let tx = db.begin_read().map_err(db_err)?;
    f(&tx)
}

/// Run one write transaction, committing on success. An error drops the
/// transaction, which aborts it: a rejected call writes nothing.
fn with_write<T>(
    db: &Database,
    f: impl FnOnce(&WriteTransaction) -> Result<T, StorageError>,
) -> Result<T, StorageError> {
    let tx = db.begin_write().map_err(db_err)?;
    let value = f(&tx)?;
    tx.commit().map_err(db_err)?;
    Ok(value)
}

/// Run one blocking redb operation on the runtime's blocking pool.
async fn blocking<T, F>(db: Arc<Database>, f: F) -> Result<T, StorageError>
where
    F: FnOnce(&Database) -> Result<T, StorageError> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(move || f(&db))
        .await
        .map_err(join_err)?
}

/// The tree table a node operation targets. An enum keeps an unknown table
/// unrepresentable: no string fallback can quietly read the wrong level.
#[derive(Clone, Copy)]
enum TreeTable {
    Root,
    Dim,
    Region,
}

impl TreeTable {
    const fn definition(self) -> TableDefinition<'static, &'static [u8; 32], &'static [u8]> {
        match self {
            Self::Root => ROOT_TREE,
            Self::Dim => DIM_TREE,
            Self::Region => REGION_TREE,
        }
    }

    const fn level(self) -> &'static str {
        match self {
            Self::Root => "root",
            Self::Dim => "dim",
            Self::Region => "region",
        }
    }
}

/// Read one node's stored bytes, if present.
fn node_bytes(
    tx: &ReadTransaction,
    table: TreeTable,
    hash: &[u8; 32],
) -> Result<Option<Vec<u8>>, StorageError> {
    let nodes = tx.open_table(table.definition()).map_err(db_err)?;
    Ok(nodes
        .get(hash)
        .map_err(db_err)?
        .map(|node| node.value().to_vec()))
}

/// Read many nodes by hash in one snapshot.
fn node_bytes_many(
    tx: &ReadTransaction,
    table: TreeTable,
    hashes: &[[u8; 32]],
) -> Result<BTreeMap<[u8; 32], Vec<u8>>, StorageError> {
    let nodes = tx.open_table(table.definition()).map_err(db_err)?;
    let mut found = BTreeMap::new();
    for hash in hashes {
        if let Some(node) = nodes.get(hash).map_err(db_err)? {
            found.insert(*hash, node.value().to_vec());
        }
    }
    Ok(found)
}

/// One commit's stored record, by id.
fn commit_record(
    ids: &impl ReadableTable<&'static [u8; 32], u64>,
    commits: &impl ReadableTable<u64, &'static [u8]>,
    id: CommitId,
) -> Result<Option<CommitRecord>, StorageError> {
    let Some(seq) = ids.get(&id.0).map_err(db_err)? else {
        return Ok(None);
    };
    let seq = seq.value();
    let record = commits
        .get(seq)
        .map_err(db_err)?
        .ok_or_else(|| corrupt(format!("commit sequence {seq} names no record")))?;
    decode_commit_record(record.value()).map(Some)
}

/// Direct parents of a commit, in ascending hash order (the multimap's
/// stored order). An unknown commit yields an empty list.
fn commit_parents_sync(tx: &ReadTransaction, id: CommitId) -> Result<Vec<CommitId>, StorageError> {
    let edges = tx.open_multimap_table(COMMIT_PARENTS).map_err(db_err)?;
    edges
        .get(&id.0)
        .map_err(db_err)?
        .map(|parent| Ok(CommitId(*parent.map_err(db_err)?.value())))
        .collect()
}

/// The active ref's stored name, if the store has one.
fn active_name(
    state: &impl ReadableTable<&'static str, &'static [u8]>,
) -> Result<Option<String>, StorageError> {
    match state.get(STATE_ACTIVE_REF).map_err(db_err)? {
        Some(name) => {
            let name = std::str::from_utf8(name.value())
                .map_err(|_| corrupt("active ref name is not valid UTF-8"))?;
            Ok(Some(name.to_owned()))
        }
        None => Ok(None),
    }
}

/// Insert tree nodes children-first, skipping hashes the store already
/// has: unchanged subtrees are shared, never rewritten.
fn insert_tree_nodes(tx: &WriteTransaction, nodes: &TreeNodes) -> Result<(), StorageError> {
    insert_nodes(tx, TreeTable::Region, &nodes.region, RegionHash::as_bytes)?;
    insert_nodes(tx, TreeTable::Dim, &nodes.dim, DimHash::as_bytes)?;
    insert_nodes(tx, TreeTable::Root, &nodes.root, RootHash::as_bytes)?;
    Ok(())
}

fn insert_nodes<H: Ord>(
    tx: &WriteTransaction,
    table: TreeTable,
    nodes: &BTreeMap<H, Vec<u8>>,
    key: fn(&H) -> &[u8; 32],
) -> Result<(), StorageError> {
    let mut tree = tx.open_table(table.definition()).map_err(db_err)?;
    for (hash, node) in nodes {
        if tree.get(key(hash)).map_err(db_err)?.is_none() {
            tree.insert(key(hash), &node[..]).map_err(db_err)?;
        }
    }
    Ok(())
}

/// Insert a commit record and the edges naming its parents.
///
/// Every parent must exist; a parent listed twice is recorded once (the
/// multimap deduplicates). The checks run in the transaction, so a rejected
/// commit leaves nothing behind. An identical commit (same hash) is
/// idempotent: its record and edges already exist, and `inserted` reports
/// `false`.
fn insert_commit(
    tx: &WriteTransaction,
    id: CommitId,
    created_at_ms: u64,
    parents: &[CommitId],
    root: RootHash,
) -> Result<(u64, bool), StorageError> {
    let created_at_ms = millis_param(created_at_ms)?;
    let mut ids = tx.open_table(COMMIT_IDS).map_err(db_err)?;
    for parent in parents {
        if ids.get(&parent.0).map_err(db_err)?.is_none() {
            return Err(StorageError::UnknownCommit { id: *parent });
        }
    }
    let existing = ids.get(&id.0).map_err(db_err)?.map(|guard| guard.value());
    let (seq, inserted) = if let Some(seq) = existing {
        (seq, false)
    } else {
        let mut state = tx.open_table(STATE).map_err(db_err)?;
        let next = state
            .get(STATE_NEXT_SEQ)
            .map_err(db_err)?
            .ok_or_else(|| corrupt("state is missing next_seq"))?
            .value()
            .to_vec();
        let seq = u64::from_le_bytes(next.try_into().map_err(|_| corrupt("malformed next_seq"))?);
        state
            .insert(STATE_NEXT_SEQ, &(seq + 1).to_le_bytes()[..])
            .map_err(db_err)?;
        drop(state);
        let mut commits = tx.open_table(COMMITS).map_err(db_err)?;
        commits
            .insert(seq, &encode_commit(id, created_at_ms, root)[..])
            .map_err(db_err)?;
        drop(commits);
        ids.insert(&id.0, seq).map_err(db_err)?;
        (seq, true)
    };
    drop(ids);
    let mut edges = tx.open_multimap_table(COMMIT_PARENTS).map_err(db_err)?;
    for parent in parents {
        edges.insert(&id.0, &parent.0).map_err(db_err)?;
    }
    Ok((seq, inserted))
}

/// Record a commit with no snapshot changes.
fn create_commit_sync(
    db: &Database,
    created_at_ms: u64,
    parents: &[CommitId],
) -> Result<CommitId, StorageError> {
    // No snapshot changes: continue the lowest-hash parent's tree, or
    // start with the canonical empty root.
    let mut canonical_parents = parents.to_vec();
    canonical_parents.sort();
    canonical_parents.dedup();
    with_write(db, |tx| {
        let parent_root = match canonical_parents.first() {
            Some(parent) => {
                let ids = tx.open_table(COMMIT_IDS).map_err(db_err)?;
                let commits = tx.open_table(COMMITS).map_err(db_err)?;
                commit_record(&ids, &commits, *parent)?.map(|record| record.root)
            }
            None => None,
        };
        let (root, nodes) = parent_root.map_or_else(sekai_storage::tree::empty_tree, |root| {
            (root, TreeNodes::default())
        });
        let id = commit_hash(parents, created_at_ms, root);
        insert_tree_nodes(tx, &nodes)?;
        insert_commit(tx, id, created_at_ms, parents, root)?;
        Ok(id)
    })
}

/// Record a commit whose tree is already built, moving the active ref onto
/// it in the same transaction.
fn commit_tree_sync(
    db: &Database,
    created_at_ms: u64,
    parents: &[CommitId],
    root: RootHash,
    nodes: &TreeNodes,
) -> Result<ApplyOutcome, StorageError> {
    let commit = commit_hash(parents, created_at_ms, root);
    with_write(db, |tx| {
        // The new commit must continue the active ref's target: moving the
        // ref onto an unrelated history would fork it. Read the target
        // before inserting, so a rejected call writes nothing.
        let name = {
            let state = tx.open_table(STATE).map_err(db_err)?;
            active_name(&state)?.ok_or(StorageError::NoActiveRef)?
        };
        {
            let refs = tx.open_table(REFS).map_err(db_err)?;
            let record = refs
                .get(name.as_str())
                .map_err(db_err)?
                .ok_or_else(|| StorageError::UnknownRef { name: name.clone() })?;
            if let Some(base) = split_ref(record.value())?.1
                && !parents.contains(&base)
            {
                return Err(StorageError::NotContinuing { base });
            }
        }
        insert_tree_nodes(tx, nodes)?;
        insert_commit(tx, commit, created_at_ms, parents, root)?;
        // The active ref moves onto the new commit in the same transaction:
        // a ref lagging behind its history would fork the next commit. Its
        // creation time is kept verbatim.
        let mut refs = tx.open_table(REFS).map_err(db_err)?;
        let mut moved = {
            let record = refs
                .get(name.as_str())
                .map_err(db_err)?
                .ok_or_else(|| StorageError::UnknownRef { name: name.clone() })?;
            split_ref(record.value())?.0.to_vec()
        };
        moved.extend_from_slice(&commit.0);
        refs.insert(name.as_str(), &moved[..]).map_err(db_err)?;
        Ok(ApplyOutcome { id: commit })
    })
}

/// Enforce the schema version, creating the schema on a fresh file.
///
/// The version stamp and the schema commit together: a crash between them
/// would leave a half-created schema that no later open can repair. redb's
/// file lock makes the gate race-free: a second opener fails before it can
/// observe a half-created store.
fn gate(db: &Database) -> Result<(), StorageError> {
    let tx = db.begin_write().map_err(db_err)?;
    let found = {
        let state = tx.open_table(STATE).map_err(db_err)?;
        match state.get(STATE_SCHEMA).map_err(db_err)? {
            Some(bytes) => Some(version_of(bytes.value())?),
            None => None,
        }
    };
    match found {
        Some(version) if version != SCHEMA_VERSION => {
            return Err(StorageError::UnsupportedSchema {
                found: version,
                supported: SCHEMA_VERSION,
            });
        }
        Some(_) => {}
        None => {
            create_schema(&tx)?;
            tx.commit().map_err(db_err)?;
        }
    }
    Ok(())
}

/// Materialize every table and seed the state of a fresh store.
fn create_schema(tx: &WriteTransaction) -> Result<(), StorageError> {
    // Opening a table in a write transaction materializes it on commit.
    tx.open_table(ROOT_TREE).map_err(db_err)?;
    tx.open_table(DIM_TREE).map_err(db_err)?;
    tx.open_table(REGION_TREE).map_err(db_err)?;
    tx.open_table(COMMITS).map_err(db_err)?;
    tx.open_table(COMMIT_IDS).map_err(db_err)?;
    tx.open_multimap_table(COMMIT_PARENTS).map_err(db_err)?;
    tx.open_table(SCOPES).map_err(db_err)?;
    let mut refs = tx.open_table(REFS).map_err(db_err)?;
    // The store starts with one active ref, unborn: new commits hang from
    // it until the first one lands.
    refs.insert(DEFAULT_REF_NAME, &encode_ref(now_ms()?, None)[..])
        .map_err(db_err)?;
    drop(refs);
    let mut state = tx.open_table(STATE).map_err(db_err)?;
    state
        .insert(STATE_SCHEMA, &SCHEMA_VERSION.to_le_bytes()[..])
        .map_err(db_err)?;
    state
        .insert(STATE_NEXT_SEQ, &1u64.to_le_bytes()[..])
        .map_err(db_err)?;
    state
        .insert(STATE_ACTIVE_REF, DEFAULT_REF_NAME.as_bytes())
        .map_err(db_err)?;
    Ok(())
}

/// redb-backed commit metadata: the history graph and snapshot trees.
#[derive(Debug, Clone)]
pub struct RedbMeta {
    db: Arc<Database>,
}

impl RedbMeta {
    /// Open (creating and version-gating) the database at `path`.
    ///
    /// The parent directory must already exist; this type never creates it
    /// (directory layout is the store's concern). redb admits one opener
    /// per file, so opening the same path again fails while the first
    /// handle lives instead of racing.
    pub async fn open(path: impl AsRef<Path>) -> Result<Self, StorageError> {
        let path = path.as_ref().to_path_buf();
        if !path.parent().is_some_and(Path::is_dir) {
            return Err(io_error(
                path.display().to_string(),
                std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "parent directory does not exist",
                ),
            ));
        }
        let opened = path.clone();
        let db = tokio::task::spawn_blocking(move || {
            let db = Database::create(&opened).map_err(db_err)?;
            gate(&db)?;
            Ok::<_, StorageError>(db)
        })
        .await
        .map_err(join_err)??;
        Ok(Self { db: Arc::new(db) })
    }

    /// Record a whole commit atomically; returns its new ID.
    ///
    /// Blobs must already be flushed to the blob store before calling.
    pub async fn apply_commit(
        &mut self,
        created_at_ms: u64,
        parents: &[CommitId],
        entries: &[CommitEntry],
    ) -> Result<CommitId, StorageError> {
        Ok(self
            .apply_commit_incremental(created_at_ms, parents, entries)
            .await?
            .id)
    }
}

impl MetaStore for RedbMeta {
    type Error = StorageError;

    async fn create_commit(
        &mut self,
        created_at_ms: u64,
        parents: &[CommitId],
    ) -> Result<CommitId, StorageError> {
        let parents = parents.to_vec();
        blocking(self.db.clone(), move |db| {
            create_commit_sync(db, created_at_ms, &parents)
        })
        .await
    }

    async fn commit_parents(&self, commit: CommitId) -> Result<Vec<CommitId>, StorageError> {
        blocking(self.db.clone(), move |db| {
            with_read(db, |tx| commit_parents_sync(tx, commit))
        })
        .await
    }

    async fn lookup_chunk(
        &self,
        commit: CommitId,
        coord: &ChunkCoord,
    ) -> Result<Option<CommitEntry>, StorageError> {
        sekai_core::tree::lookup_chunk(self, commit, coord).await
    }

    async fn visit_commit_chunks<F>(&self, commit: CommitId, visit: F) -> Result<(), StorageError>
    where
        F: FnMut(&CommitEntry) -> bool + Send,
    {
        sekai_core::tree::visit_chunks(self, commit, visit).await
    }

    async fn read_root_node(&self, hash: RootHash) -> Result<RootNode, StorageError> {
        let bytes = blocking(self.db.clone(), move |db| {
            with_read(db, |tx| node_bytes(tx, TreeTable::Root, hash.as_bytes()))
        })
        .await?
        .ok_or_else(|| StorageError::MissingTree {
            level: TreeTable::Root.level(),
            hash: hash.to_string(),
        })?;
        RootNode::decode(&bytes, hash).map_err(tree_err)
    }

    async fn read_dim_node(&self, hash: DimHash) -> Result<DimNode, StorageError> {
        let bytes = blocking(self.db.clone(), move |db| {
            with_read(db, |tx| node_bytes(tx, TreeTable::Dim, hash.as_bytes()))
        })
        .await?
        .ok_or_else(|| StorageError::MissingTree {
            level: TreeTable::Dim.level(),
            hash: hash.to_string(),
        })?;
        DimNode::decode(&bytes, hash).map_err(tree_err)
    }

    async fn read_region_node(&self, hash: RegionHash) -> Result<RegionNode, StorageError> {
        let bytes = blocking(self.db.clone(), move |db| {
            with_read(db, |tx| node_bytes(tx, TreeTable::Region, hash.as_bytes()))
        })
        .await?
        .ok_or_else(|| StorageError::MissingTree {
            level: TreeTable::Region.level(),
            hash: hash.to_string(),
        })?;
        RegionNode::decode(&bytes, hash).map_err(tree_err)
    }

    async fn read_dim_nodes(&self, hashes: &[DimHash]) -> Result<Vec<DimNode>, StorageError> {
        if hashes.is_empty() {
            return Ok(Vec::new());
        }
        let keys: Vec<[u8; 32]> = hashes.iter().map(|hash| *hash.as_bytes()).collect();
        let nodes = blocking(self.db.clone(), move |db| {
            with_read(db, |tx| node_bytes_many(tx, TreeTable::Dim, &keys))
        })
        .await?;
        hashes
            .iter()
            .map(|hash| {
                let bytes =
                    nodes
                        .get(hash.as_bytes())
                        .ok_or_else(|| StorageError::MissingTree {
                            level: TreeTable::Dim.level(),
                            hash: hash.to_string(),
                        })?;
                DimNode::decode(bytes, *hash).map_err(tree_err)
            })
            .collect()
    }

    async fn read_region_nodes(
        &self,
        hashes: &[RegionHash],
    ) -> Result<Vec<RegionNode>, StorageError> {
        if hashes.is_empty() {
            return Ok(Vec::new());
        }
        let keys: Vec<[u8; 32]> = hashes.iter().map(|hash| *hash.as_bytes()).collect();
        let nodes = blocking(self.db.clone(), move |db| {
            with_read(db, |tx| node_bytes_many(tx, TreeTable::Region, &keys))
        })
        .await?;
        hashes
            .iter()
            .map(|hash| {
                let bytes =
                    nodes
                        .get(hash.as_bytes())
                        .ok_or_else(|| StorageError::MissingTree {
                            level: TreeTable::Region.level(),
                            hash: hash.to_string(),
                        })?;
                RegionNode::decode(bytes, *hash).map_err(tree_err)
            })
            .collect()
    }

    async fn lookup_commit(&self, id: CommitId) -> Result<Option<Commit>, StorageError> {
        blocking(self.db.clone(), move |db| {
            with_read(db, |tx| {
                let ids = tx.open_table(COMMIT_IDS).map_err(db_err)?;
                let commits = tx.open_table(COMMITS).map_err(db_err)?;
                let Some(record) = commit_record(&ids, &commits, id)? else {
                    return Ok(None);
                };
                drop(commits);
                drop(ids);
                let parents = commit_parents_sync(tx, record.id)?;
                Ok(Some(Commit::new(
                    record.id,
                    record.created_at_ms,
                    parents,
                    record.root,
                )))
            })
        })
        .await
    }

    async fn latest_commit(&self) -> Result<Option<Commit>, StorageError> {
        blocking(self.db.clone(), |db| {
            with_read(db, |tx| {
                let commits = tx.open_table(COMMITS).map_err(db_err)?;
                let Some((_, raw)) = commits.last().map_err(db_err)? else {
                    return Ok(None);
                };
                let record = decode_commit_record(raw.value())?;
                drop(raw);
                drop(commits);
                let parents = commit_parents_sync(tx, record.id)?;
                Ok(Some(Commit::new(
                    record.id,
                    record.created_at_ms,
                    parents,
                    record.root,
                )))
            })
        })
        .await
    }

    async fn visit_commits<F>(&self, mut visit: F) -> Result<(), StorageError>
    where
        F: FnMut(&Commit) -> bool + Send,
    {
        // The visitor is not `'static`, so it cannot cross into the blocking
        // task: materialize the commits there (record order is the sequence
        // order) and visit them on this side.
        let commits = blocking(self.db.clone(), |db| {
            with_read(db, |tx| {
                let edges = tx.open_multimap_table(COMMIT_PARENTS).map_err(db_err)?;
                let mut parents: BTreeMap<CommitId, Vec<CommitId>> = BTreeMap::new();
                for edge in edges.iter().map_err(db_err)? {
                    let (child, values) = edge.map_err(db_err)?;
                    let list = values
                        .map(|parent| Ok(CommitId(*parent.map_err(db_err)?.value())))
                        .collect::<Result<Vec<_>, StorageError>>()?;
                    parents.insert(CommitId(*child.value()), list);
                }
                drop(edges);
                let commits = tx.open_table(COMMITS).map_err(db_err)?;
                let mut records = Vec::new();
                for entry in commits.iter().map_err(db_err)? {
                    let (_, record) = entry.map_err(db_err)?;
                    records.push(decode_commit_record(record.value())?);
                }
                // The map holds every edge, so a commit without an entry is
                // a root; an edge naming a commit that is gone is corruption.
                let known: BTreeSet<CommitId> = records.iter().map(|record| record.id).collect();
                debug_assert!(
                    parents.keys().all(|id| known.contains(id)),
                    "parent map names a commit that does not exist"
                );
                records
                    .into_iter()
                    .map(|record| {
                        let parents = parents.get(&record.id).cloned().unwrap_or_default();
                        Ok(Commit::new(
                            record.id,
                            record.created_at_ms,
                            parents,
                            record.root,
                        ))
                    })
                    .collect::<Result<Vec<Commit>, StorageError>>()
            })
        })
        .await?;
        for commit in &commits {
            if !visit(commit) {
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
        sekai_core::tree::apply_entries(self, created_at_ms, parents, entries).await
    }

    async fn commit_tree(
        &mut self,
        created_at_ms: u64,
        parents: &[CommitId],
        root: RootHash,
        nodes: &TreeNodes,
    ) -> Result<ApplyOutcome, StorageError> {
        let parents = parents.to_vec();
        let nodes = nodes.clone();
        blocking(self.db.clone(), move |db| {
            commit_tree_sync(db, created_at_ms, &parents, root, &nodes)
        })
        .await
    }

    async fn set_ref(
        &mut self,
        name: &RefName,
        commit: Option<CommitId>,
        created_at_ms: u64,
    ) -> Result<(), StorageError> {
        let name = name.as_str().to_owned();
        blocking(self.db.clone(), move |db| {
            with_write(db, |tx| {
                let created_at_ms = millis_param(created_at_ms)?;
                if let Some(id) = commit {
                    // Pointing at a commit that does not exist is
                    // corruption, not a move: report it as such.
                    let ids = tx.open_table(COMMIT_IDS).map_err(db_err)?;
                    if ids.get(&id.0).map_err(db_err)?.is_none() {
                        return Err(StorageError::UnknownCommit { id });
                    }
                }
                let mut refs = tx.open_table(REFS).map_err(db_err)?;
                // A moved ref keeps its creation time; a new ref starts
                // inactive (the state's active_ref key does not move).
                // `commit = None` is the documented unborn state, so moving
                // a ref back to unborn is allowed.
                let mut record = match refs.get(name.as_str()).map_err(db_err)? {
                    Some(existing) => split_ref(existing.value())?.0.to_vec(),
                    None => created_at_ms.to_le_bytes().to_vec(),
                };
                if let Some(id) = commit {
                    record.extend_from_slice(&id.0);
                }
                refs.insert(name.as_str(), &record[..]).map_err(db_err)?;
                Ok(())
            })
        })
        .await
    }

    async fn lookup_ref(&self, name: &RefName) -> Result<Option<CommitRef>, StorageError> {
        let name = name.as_str().to_owned();
        blocking(self.db.clone(), move |db| {
            with_read(db, |tx| {
                let refs = tx.open_table(REFS).map_err(db_err)?;
                let Some(raw) = refs.get(name.as_str()).map_err(db_err)? else {
                    return Ok(None);
                };
                let record = raw.value().to_vec();
                drop(raw);
                drop(refs);
                let state = tx.open_table(STATE).map_err(db_err)?;
                let active = active_name(&state)?.is_some_and(|active| active == name);
                decode_ref(&name, &record, active).map(Some)
            })
        })
        .await
    }

    async fn visit_refs<F>(&self, mut visit: F) -> Result<(), StorageError>
    where
        F: FnMut(&CommitRef) -> bool + Send,
    {
        // The visitor is not `'static`: materialize the raw records in name
        // order (redb orders `&str` keys byte-wise) and decode them on this
        // side, so an early stop never decodes the rows it skipped.
        let (active, rows) = blocking(self.db.clone(), |db| {
            with_read(db, |tx| {
                let state = tx.open_table(STATE).map_err(db_err)?;
                let active = active_name(&state)?;
                drop(state);
                let refs = tx.open_table(REFS).map_err(db_err)?;
                let mut rows = Vec::new();
                for entry in refs.iter().map_err(db_err)? {
                    let (name, record) = entry.map_err(db_err)?;
                    rows.push((name.value().to_owned(), record.value().to_vec()));
                }
                Ok((active, rows))
            })
        })
        .await?;
        for (name, record) in &rows {
            let is_active = active.as_deref() == Some(name.as_str());
            if !visit(&decode_ref(name, record, is_active)?) {
                break;
            }
        }
        Ok(())
    }

    async fn delete_ref(&mut self, name: &RefName) -> Result<bool, StorageError> {
        let name = name.as_str().to_owned();
        blocking(self.db.clone(), move |db| {
            with_write(db, |tx| {
                let state = tx.open_table(STATE).map_err(db_err)?;
                if active_name(&state)?.is_some_and(|active| active == name) {
                    return Err(StorageError::ActiveRefProtected { name });
                }
                drop(state);
                let mut refs = tx.open_table(REFS).map_err(db_err)?;
                Ok(refs.remove(name.as_str()).map_err(db_err)?.is_some())
            })
        })
        .await
    }

    async fn active_ref(&self) -> Result<Option<CommitRef>, StorageError> {
        blocking(self.db.clone(), |db| {
            with_read(db, |tx| {
                let state = tx.open_table(STATE).map_err(db_err)?;
                let Some(name) = active_name(&state)? else {
                    return Ok(None);
                };
                drop(state);
                let refs = tx.open_table(REFS).map_err(db_err)?;
                let raw = refs
                    .get(name.as_str())
                    .map_err(db_err)?
                    .ok_or_else(|| StorageError::UnknownRef { name: name.clone() })?;
                let record = decode_ref(&name, raw.value(), true)?;
                drop(raw);
                drop(refs);
                // The target commit must exist: a dangling target is
                // corruption, not an unborn ref.
                if let Some(commit) = record.commit {
                    let ids = tx.open_table(COMMIT_IDS).map_err(db_err)?;
                    if ids.get(&commit.0).map_err(db_err)?.is_none() {
                        return Err(StorageError::UnknownCommit { id: commit });
                    }
                }
                Ok(Some(record))
            })
        })
        .await
    }

    async fn activate_ref(&mut self, name: &RefName) -> Result<(), StorageError> {
        let name = name.as_str().to_owned();
        blocking(self.db.clone(), move |db| {
            with_write(db, |tx| {
                let refs = tx.open_table(REFS).map_err(db_err)?;
                let raw = refs
                    .get(name.as_str())
                    .map_err(db_err)?
                    .ok_or_else(|| StorageError::UnknownRef { name: name.clone() })?;
                let target = split_ref(raw.value())?.1;
                drop(raw);
                drop(refs);
                if let Some(commit) = target {
                    let ids = tx.open_table(COMMIT_IDS).map_err(db_err)?;
                    if ids.get(&commit.0).map_err(db_err)?.is_none() {
                        return Err(StorageError::UnknownCommit { id: commit });
                    }
                    drop(ids);
                }
                // Moving the state key is the whole activation: the flag is
                // derived, so the previous active ref demotes itself.
                let mut state = tx.open_table(STATE).map_err(db_err)?;
                state
                    .insert(STATE_ACTIVE_REF, name.as_bytes())
                    .map_err(db_err)?;
                Ok(())
            })
        })
        .await
    }

    async fn set_scope(&mut self, scope: &NamedScope) -> Result<(), StorageError> {
        let scope = scope.clone();
        blocking(self.db.clone(), move |db| {
            with_write(db, |tx| {
                let created_at_ms = millis_param(scope.created_at_ms)?;
                let mut scopes = tx.open_table(SCOPES).map_err(db_err)?;
                // A replacement keeps the creation time; only the whole
                // area set is swapped, in one atomic insert.
                let created = match scopes.get(scope.name.as_str()).map_err(db_err)? {
                    Some(existing) => i64::from_le_bytes(scope_header(existing.value())?),
                    None => created_at_ms,
                };
                let record = encode_scope(created, scope.areas());
                scopes
                    .insert(scope.name.as_str(), &record[..])
                    .map_err(db_err)?;
                Ok(())
            })
        })
        .await
    }

    async fn lookup_scope(&self, name: &ScopeName) -> Result<Option<NamedScope>, StorageError> {
        let name = name.as_str().to_owned();
        blocking(self.db.clone(), move |db| {
            with_read(db, |tx| {
                let scopes = tx.open_table(SCOPES).map_err(db_err)?;
                let Some(record) = scopes.get(name.as_str()).map_err(db_err)? else {
                    return Ok(None);
                };
                decode_scope(&name, record.value()).map(Some)
            })
        })
        .await
    }

    async fn visit_scopes<F>(&self, mut visit: F) -> Result<(), StorageError>
    where
        F: FnMut(&NamedScope) -> bool + Send,
    {
        // The visitor is not `'static`: materialize the raw records in name
        // order and decode them on this side, so an early stop never decodes
        // the rows it skipped.
        let rows = blocking(self.db.clone(), |db| {
            with_read(db, |tx| {
                let scopes = tx.open_table(SCOPES).map_err(db_err)?;
                let mut rows = Vec::new();
                for entry in scopes.iter().map_err(db_err)? {
                    let (name, record) = entry.map_err(db_err)?;
                    rows.push((name.value().to_owned(), record.value().to_vec()));
                }
                Ok(rows)
            })
        })
        .await?;
        for (name, record) in &rows {
            if !visit(&decode_scope(name, record)?) {
                break;
            }
        }
        Ok(())
    }

    async fn delete_scope(&mut self, name: &ScopeName) -> Result<bool, StorageError> {
        let name = name.as_str().to_owned();
        blocking(self.db.clone(), move |db| {
            with_write(db, |tx| {
                // Areas live inside the scope record, so one remove drops
                // the scope with all its areas.
                let mut scopes = tx.open_table(SCOPES).map_err(db_err)?;
                Ok(scopes.remove(name.as_str()).map_err(db_err)?.is_some())
            })
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use redb::ReadableTableMetadata as _;
    use redb::backends::InMemoryBackend;

    /// A temp dir whose name starts with the calling test's name
    /// (tempfile appends a random suffix), removed when its guard is
    /// dropped at the end of the test, panics included.
    fn tempdir(name: &str) -> (std::path::PathBuf, tempfile::TempDir) {
        let guard = tempfile::Builder::new()
            .prefix(&format!("sekai-{name}-"))
            .tempdir()
            .unwrap();
        (guard.path().to_path_buf(), guard)
    }

    fn memory_meta() -> RedbMeta {
        let db = Database::builder()
            .create_with_backend(InMemoryBackend::new())
            .unwrap();
        gate(&db).unwrap();
        RedbMeta { db: Arc::new(db) }
    }

    /// Write raw records straight into the store, bypassing the trait's
    /// validation, to simulate corruption.
    fn corrupt_with(meta: &RedbMeta, f: impl FnOnce(&WriteTransaction)) {
        let tx = meta.db.begin_write().unwrap();
        f(&tx);
        tx.commit().unwrap();
    }

    fn raw_state(meta: &RedbMeta, key: &str) -> Option<Vec<u8>> {
        let tx = meta.db.begin_read().unwrap();
        let state = tx.open_table(STATE).unwrap();
        state.get(key).unwrap().map(|value| value.value().to_vec())
    }

    fn raw_node(meta: &RedbMeta, table: TreeTable, hash: &[u8; 32]) -> Vec<u8> {
        let tx = meta.db.begin_read().unwrap();
        let nodes = tx.open_table(table.definition()).unwrap();
        nodes.get(hash).unwrap().unwrap().value().to_vec()
    }

    fn edge_count(meta: &RedbMeta) -> u64 {
        let tx = meta.db.begin_read().unwrap();
        let edges = tx.open_multimap_table(COMMIT_PARENTS).unwrap();
        edges.len().unwrap()
    }

    #[tokio::test]
    async fn fresh_store_carries_schema_version() {
        let meta = memory_meta();
        let bytes = raw_state(&meta, STATE_SCHEMA).unwrap();
        let version = i64::from_le_bytes(bytes.try_into().unwrap());
        assert_eq!(version, SCHEMA_VERSION);
    }

    #[tokio::test]
    async fn a_fresh_store_starts_with_one_active_ref() {
        let meta = memory_meta();
        let active = meta.active_ref().await.unwrap().unwrap();
        assert_eq!(active.name.as_str(), DEFAULT_REF_NAME);
        assert!(active.active);
        // Unborn: no commit yet.
        assert!(active.commit.is_none());

        let mut refs = Vec::new();
        meta.visit_refs(|record| {
            refs.push(record.clone());
            true
        })
        .await
        .unwrap();
        assert_eq!(refs.len(), 1);
    }

    #[tokio::test]
    async fn activating_a_ref_moves_the_active_flag() {
        let mut meta = memory_meta();
        let main = RefName::parse(DEFAULT_REF_NAME).unwrap();
        let other = RefName::parse("feature").unwrap();
        meta.set_ref(&other, None, 1_000).await.unwrap();

        meta.activate_ref(&other).await.unwrap();
        assert_eq!(meta.active_ref().await.unwrap().unwrap().name, other);
        assert!(
            !meta.lookup_ref(&main).await.unwrap().unwrap().active,
            "the previous active ref is no longer active"
        );

        // Unknown refs are refused, and the flag stays where it was.
        assert!(matches!(
            meta.activate_ref(&RefName::parse("missing").unwrap()).await,
            Err(StorageError::UnknownRef { .. })
        ));
        assert_eq!(meta.active_ref().await.unwrap().unwrap().name, other);
    }

    #[tokio::test]
    async fn activating_a_ref_refuses_a_dangling_target() {
        let (dir, _guard) = tempdir("dangling-target");
        let path = dir.join("meta.redb");
        let mut meta = RedbMeta::open(&path).await.unwrap();
        let main = RefName::parse(DEFAULT_REF_NAME).unwrap();
        let feature = RefName::parse("feature").unwrap();
        let root = meta
            .apply_commit_incremental(1_000, &[], &[])
            .await
            .unwrap()
            .id;
        meta.set_ref(&feature, Some(root), 2_000).await.unwrap();

        // Simulate corruption that bypassed the existence check.
        let missing = CommitId([0xEE; 32]);
        corrupt_with(&meta, |tx| {
            let mut refs = tx.open_table(REFS).unwrap();
            refs.insert("feature", &encode_ref(2_000, Some(missing))[..])
                .unwrap();
        });

        let err = meta.activate_ref(&feature).await.unwrap_err();
        assert!(matches!(err, StorageError::UnknownCommit { id } if id == missing));
        assert_eq!(meta.active_ref().await.unwrap().unwrap().name, main);
    }

    #[tokio::test]
    async fn an_unborn_active_ref_starts_a_new_root() {
        let mut meta = memory_meta();
        let main = RefName::parse(DEFAULT_REF_NAME).unwrap();
        let first = meta
            .apply_commit_incremental(1_000, &[], &[])
            .await
            .unwrap()
            .id;

        // Moving the active ref to unborn deliberately drops the parent
        // constraint: the next commit is a fresh root.
        meta.set_ref(&main, None, 2_000).await.unwrap();
        let second = meta
            .apply_commit_incremental(3_000, &[], &[])
            .await
            .unwrap()
            .id;
        assert_ne!(first, second);
        assert_eq!(
            meta.active_ref().await.unwrap().unwrap().commit,
            Some(second)
        );
        assert_eq!(meta.commit_parents(second).await.unwrap(), Vec::new());
    }

    #[tokio::test]
    async fn a_ref_cannot_name_a_missing_commit() {
        let mut meta = memory_meta();
        let feature = RefName::parse("feature").unwrap();

        // A stale target is a typed error, and the ref does not appear.
        let missing = CommitId([0xEE; 32]);
        let err = meta
            .set_ref(&feature, Some(missing), 1_000)
            .await
            .unwrap_err();
        assert!(matches!(err, StorageError::UnknownCommit { id } if id == missing));
        assert!(meta.lookup_ref(&feature).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn moving_a_born_active_ref_back_to_unborn_keeps_its_state() {
        let mut meta = memory_meta();
        let main = RefName::parse(DEFAULT_REF_NAME).unwrap();
        let first = meta
            .apply_commit_incremental(1_000, &[], &[])
            .await
            .unwrap()
            .id;
        let before = meta.lookup_ref(&main).await.unwrap().unwrap();
        assert_eq!(before.commit, Some(first));

        meta.set_ref(&main, None, 2_000).await.unwrap();
        let after = meta.lookup_ref(&main).await.unwrap().unwrap();
        assert_eq!(after.commit, None, "the target moved back to unborn");
        assert!(after.active, "the move kept the active flag");
        assert_eq!(
            after.created_at_ms, before.created_at_ms,
            "the move kept the creation time"
        );
    }

    #[tokio::test]
    async fn recording_the_same_commit_twice_is_idempotent() {
        let mut meta = memory_meta();
        let first = meta.create_commit(1_000, &[]).await.unwrap();
        let again = meta.create_commit(1_000, &[]).await.unwrap();
        assert_eq!(first, again);
        let mut commits = Vec::new();
        meta.visit_commits(|commit| {
            commits.push(commit.id);
            true
        })
        .await
        .unwrap();
        assert_eq!(commits, [first]);
    }

    #[tokio::test]
    async fn a_commit_must_continue_the_active_ref() {
        let mut meta = memory_meta();
        let root = meta
            .apply_commit_incremental(1_000, &[], &[])
            .await
            .unwrap()
            .id;

        let err = meta
            .apply_commit_incremental(2_000, &[], &[])
            .await
            .unwrap_err();
        assert!(matches!(err, StorageError::NotContinuing { base } if base == root));

        // The rejected call wrote nothing.
        assert_eq!(meta.latest_commit().await.unwrap().unwrap().id, root);
        assert_eq!(meta.active_ref().await.unwrap().unwrap().commit, Some(root));

        let next = meta
            .apply_commit_incremental(3_000, &[root], &[])
            .await
            .unwrap()
            .id;
        assert_ne!(next, root);
    }

    #[tokio::test]
    async fn a_corrupt_ref_row_fails_loudly() {
        let meta = memory_meta();
        // Names are re-validated on decode: a record the trait would never
        // write is corruption, never a new ref name.
        corrupt_with(&meta, |tx| {
            let mut refs = tx.open_table(REFS).unwrap();
            refs.insert("bad name", &encode_ref(0, None)[..]).unwrap();
        });
        let err = meta.visit_refs(|_| true).await.unwrap_err();
        assert!(matches!(err, StorageError::InvalidRefName { name } if name == "bad name"));

        // A negative creation time is outside the stored domain.
        corrupt_with(&meta, |tx| {
            let mut refs = tx.open_table(REFS).unwrap();
            refs.remove("bad name").unwrap();
            refs.insert(DEFAULT_REF_NAME, &encode_ref(-1, None)[..])
                .unwrap();
        });
        let main = RefName::parse(DEFAULT_REF_NAME).unwrap();
        assert!(matches!(
            meta.lookup_ref(&main).await,
            Err(StorageError::InvalidHistoryValue {
                column: "created_at_ms",
                value: -1
            })
        ));
    }

    #[tokio::test]
    async fn a_commit_without_an_active_ref_writes_nothing() {
        let mut meta = memory_meta();
        // A first commit gives the store a history.
        let first = meta
            .apply_commit_incremental(1_000, &[], &[])
            .await
            .unwrap()
            .id;
        // A store whose active ref was lost (corruption), leaving the
        // history intact but with no head.
        corrupt_with(&meta, |tx| {
            let mut state = tx.open_table(STATE).unwrap();
            state.remove(STATE_ACTIVE_REF).unwrap();
        });

        let err = meta
            .apply_commit_incremental(2_000, &[], &[])
            .await
            .unwrap_err();
        assert!(matches!(err, StorageError::NoActiveRef));

        let mut commits = Vec::new();
        meta.visit_commits(|commit| {
            commits.push(commit.clone());
            true
        })
        .await
        .unwrap();
        assert_eq!(commits.len(), 1, "the failed commit left a row behind");
        assert_eq!(commits[0].id, first);
    }

    #[tokio::test]
    async fn the_active_ref_survives_a_delete_attempt() {
        let mut meta = memory_meta();
        let main = RefName::parse(DEFAULT_REF_NAME).unwrap();
        let other = RefName::parse("feature").unwrap();
        meta.set_ref(&other, None, 1_000).await.unwrap();

        assert!(matches!(
            meta.delete_ref(&main).await,
            Err(StorageError::ActiveRefProtected { .. })
        ));
        assert!(meta.lookup_ref(&main).await.unwrap().is_some());
        assert!(meta.delete_ref(&other).await.unwrap());
        assert!(!meta.delete_ref(&other).await.unwrap());
    }

    #[tokio::test]
    async fn a_commit_cannot_name_a_missing_commit_as_parent() {
        let mut meta = memory_meta();
        let missing = CommitId([9; 32]);
        assert!(matches!(
            meta.create_commit(1_000, &[missing]).await,
            Err(StorageError::UnknownCommit { id }) if id == missing
        ));
        // The rejected attempt left nothing behind.
        assert!(meta.latest_commit().await.unwrap().is_none());
        assert_eq!(edge_count(&meta), 0);

        // A rejection on a store that already holds a history leaves that
        // history alone.
        let root = meta.create_commit(2_000, &[]).await.unwrap();
        assert!(matches!(
            meta.create_commit(3_000, &[missing]).await,
            Err(StorageError::UnknownCommit { id }) if id == missing
        ));
        assert_eq!(meta.latest_commit().await.unwrap().unwrap().id, root);
        assert_eq!(edge_count(&meta), 0);
    }

    /// Stamp a foreign schema version into a fresh file, then close it so
    /// the gate can reopen it.
    fn seed_version(path: impl AsRef<Path>, version: i64) {
        let seed = Database::create(path).unwrap();
        let tx = seed.begin_write().unwrap();
        let mut state = tx.open_table(STATE).unwrap();
        state
            .insert(STATE_SCHEMA, &version.to_le_bytes()[..])
            .unwrap();
        drop(state);
        tx.commit().unwrap();
    }

    #[tokio::test]
    async fn version_gate_rejects_foreign_schema() {
        let (dir, _guard) = tempdir("version");
        let path = dir.join("meta.redb");
        seed_version(&path, 999);
        assert!(matches!(
            RedbMeta::open(&path).await,
            Err(StorageError::UnsupportedSchema {
                found: 999,
                supported: SCHEMA_VERSION,
            })
        ));
    }

    /// The version gate rejects any store whose `schema` stamp is not
    /// `SCHEMA_VERSION` (18) before any table is touched. A store from the
    /// immediately previous version (17: the SQLite layout) is refused with
    /// `UnsupportedSchema { found: 17, supported: SCHEMA_VERSION }`, never
    /// opened and failed on the first query.
    #[tokio::test]
    async fn a_store_from_the_previous_schema_version_is_refused() {
        let (dir, _guard) = tempdir("version-17");
        let path = dir.join("meta.redb");
        seed_version(&path, SCHEMA_VERSION - 1);
        assert!(matches!(
            RedbMeta::open(&path).await,
            Err(StorageError::UnsupportedSchema {
                found: 17,
                supported: SCHEMA_VERSION,
            })
        ));
    }

    #[tokio::test]
    async fn commit_rows_store_canonical_tree_bytes() {
        use sekai_core::{ChunkHash, RegionKind};
        let mut meta = memory_meta();
        let coord = ChunkCoord::new(Dimension::OVERWORLD, RegionKind::Region, 5, 5);
        let id = meta
            .apply_commit_incremental(
                1_000,
                &[],
                &[CommitEntry::new(coord, Some(ChunkHash::new([1; 32])))],
            )
            .await
            .unwrap()
            .id;
        let record = meta.lookup_commit(id).await.unwrap().unwrap();

        let root_bytes = raw_node(&meta, TreeTable::Root, record.root.as_bytes());
        let root = RootNode::decode(&root_bytes, record.root).unwrap();
        let dim_hash = root.get(Dimension::OVERWORLD.raw()).unwrap();
        let dim_bytes = raw_node(&meta, TreeTable::Dim, dim_hash.as_bytes());
        let dim = DimNode::decode(&dim_bytes, dim_hash).unwrap();
        let region_hash = dim.get(RegionKind::Region, 0, 0).unwrap();
        let region_bytes = raw_node(&meta, TreeTable::Region, region_hash.as_bytes());
        let region = RegionNode::decode(&region_bytes, region_hash).unwrap();
        assert_eq!(region.get(5, 5), Some(ChunkHash::new([1; 32])));
    }

    #[tokio::test]
    async fn stored_tree_corruption_fails_loudly() {
        let meta = memory_meta();
        let hash = RootHash::new([9; 32]);
        corrupt_with(&meta, |tx| {
            let mut roots = tx.open_table(ROOT_TREE).unwrap();
            roots.insert(hash.as_bytes(), &[][..]).unwrap();
        });
        let err = meta.read_root_node(hash).await.unwrap_err();
        assert!(
            matches!(
                err,
                StorageError::Tree(sekai_storage::TreeError::HashMismatch { .. })
            ),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn a_large_commit_records_every_region() {
        use sekai_core::{ChunkHash, RegionKind};
        let mut meta = memory_meta();
        // One chunk per region, so the commit spans thousands of region
        // nodes. Region nodes are content-addressed: a repeating blob would
        // collapse same-slot regions into one node, so every blob must be
        // distinct.
        let rows: usize = 2051;
        let entries: Vec<CommitEntry> = (0..rows)
            .map(|i| {
                let x = i32::try_from(i).unwrap() * 32;
                let coord = ChunkCoord::new(Dimension::OVERWORLD, RegionKind::Region, x, -x);
                let mut hash = [0u8; 32];
                hash[..2].copy_from_slice(&u16::try_from(i).unwrap().to_le_bytes());
                CommitEntry::new(coord, Some(ChunkHash::new(hash)))
            })
            .collect();
        let commit = meta.apply_commit(1_000, &[], &entries).await.unwrap();

        let mut recorded = Vec::new();
        meta.visit_commit_chunks(commit, |entry| {
            recorded.push(*entry);
            true
        })
        .await
        .unwrap();
        recorded.sort();
        let mut expected = entries;
        expected.sort();
        assert_eq!(recorded, expected);
    }

    fn scope(name: &str) -> NamedScope {
        NamedScope::new(
            ScopeName::parse(name).unwrap(),
            vec![(Dimension::OVERWORLD, Rect::new(0, 0, 1, 1))],
            1_000,
        )
        .unwrap()
    }

    #[tokio::test]
    async fn deleting_a_scope_drops_its_areas() {
        let mut meta = memory_meta();
        let record = scope("spawn");
        meta.set_scope(&record).await.unwrap();
        assert!(meta.delete_scope(&record.name).await.unwrap());
        // Areas live inside the scope record, so the delete dropped them
        // with it; nothing is left to orphan.
        assert!(meta.lookup_scope(&record.name).await.unwrap().is_none());
        let tx = meta.db.begin_read().unwrap();
        let scopes = tx.open_table(SCOPES).unwrap();
        assert_eq!(scopes.len().unwrap(), 0);
    }

    #[tokio::test]
    async fn a_corrupt_scope_fails_loudly() {
        let meta = memory_meta();

        // Names re-validate on decode.
        corrupt_with(&meta, |tx| {
            let mut scopes = tx.open_table(SCOPES).unwrap();
            let record = encode_scope(0, &[(Dimension::OVERWORLD, Rect::new(0, 0, 0, 0))]);
            scopes.insert("bad name", &record[..]).unwrap();
        });
        let err = meta.visit_scopes(|_| true).await.unwrap_err();
        assert!(matches!(err, StorageError::InvalidScopeName { name } if name == "bad name"));

        // A scope without areas would select nothing: `NamedScope::new`
        // refuses one, so a stored one is corruption.
        corrupt_with(&meta, |tx| {
            let mut scopes = tx.open_table(SCOPES).unwrap();
            scopes.remove("bad name").unwrap();
            scopes.insert("empty", &0i64.to_le_bytes()[..]).unwrap();
        });
        let empty = ScopeName::parse("empty").unwrap();
        let err = meta.lookup_scope(&empty).await.unwrap_err();
        assert!(matches!(err, StorageError::EmptyScope { name } if name == "empty"));
    }

    /// redb admits a single opener per file: a second open fails while the
    /// first handle lives, and succeeds once it is gone.
    #[tokio::test]
    async fn a_live_database_refuses_a_second_opener() {
        let (dir, _guard) = tempdir("exclusive-meta");
        let path = dir.join("meta.redb");
        let meta = RedbMeta::open(&path).await.unwrap();
        let err = RedbMeta::open(&path).await.unwrap_err();
        assert!(
            matches!(
                err,
                StorageError::Backend {
                    backend: "redb",
                    ..
                }
            ),
            "unexpected error: {err}"
        );
        drop(meta);
        let _meta = RedbMeta::open(&path).await.unwrap();
    }
}
