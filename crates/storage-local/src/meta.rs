//! SQLite commit metadata: the history graph and snapshot trees.
//!
//! Recording a commit uses one transaction; blob durability is established
//! before the metadata commit by the caller. The pool has one connection.

use std::collections::{BTreeMap, BTreeSet, btree_map::Entry};
use std::path::Path;
use std::time::{Duration, Instant};

use sekai_storage::{
    DimNode, MetaStore, RegionNode, RootNode, StorageError, TreeNodes, commit_hash,
};
use sekai_util::{
    ApplyOutcome, ChunkCoord, Commit, CommitEntry, CommitId, CommitRef, DimHash, Dimension, Head,
    HeadState, NamedScope, NamedScopeError, Rect, Ref, RefKind, RefName, RegionHash, RootHash,
    ScopeName, Worktree,
};
use sqlx::Row as _;
use sqlx::SqliteConnection;
use sqlx::sqlite::{
    SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions, SqliteSynchronous,
};

const SCHEMA: &str = include_str!("../schema/sqlite.sql");

/// Map a backend failure into the shared storage error.
fn db_err(source: sqlx::Error) -> StorageError {
    StorageError::backend("sqlite", source)
}

/// SQLite-backed commit metadata: the history graph and snapshot trees.
#[derive(Debug, Clone)]
pub struct SqliteMeta {
    pool: SqlitePool,
}

/// How long a connect keeps retrying `SQLITE_BUSY`, matching sqlx's
/// default `busy_timeout`.
const CONNECT_BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// Connect the single-connection pool.
///
/// Switching a fresh database into WAL needs an exclusive lock that
/// SQLite's busy handler cannot wait on, so a connect racing another
/// opener's switch fails with `SQLITE_BUSY` at once. Retry it as long as
/// `busy_timeout` would have waited. Extended codes (`SQLITE_BUSY_RECOVERY`
/// while another opener replays the WAL, ...) share the primary code in
/// their low byte.
async fn connect_pool(options: SqliteConnectOptions) -> Result<SqlitePool, sqlx::Error> {
    const SQLITE_BUSY: i32 = 5;
    let is_busy = |err: &dyn sqlx::error::DatabaseError| {
        err.code()
            .and_then(|code| code.parse::<i32>().ok())
            .is_some_and(|code| code & 0xff == SQLITE_BUSY)
    };
    let deadline = Instant::now() + CONNECT_BUSY_TIMEOUT;
    loop {
        match SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options.clone())
            .await
        {
            Err(sqlx::Error::Database(err)) if is_busy(&*err) && Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            connected => return connected,
        }
    }
}

async fn has_schema<'c>(executor: impl sqlx::SqliteExecutor<'c>) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table')")
        .fetch_one(executor)
        .await
}

impl SqliteMeta {
    /// Open the database at `path`, creating the schema when missing.
    ///
    /// The parent directory must already exist; this type never creates it
    /// (directory layout is the store's concern). Opening while another
    /// opener creates the schema waits for it, up to the busy timeout.
    pub async fn open(path: impl AsRef<Path>) -> Result<Self, StorageError> {
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Full)
            .foreign_keys(true);
        let pool = connect_pool(options).await.map_err(db_err)?;
        if !has_schema(&pool).await.map_err(db_err)? {
            let mut tx = pool.begin_with("BEGIN IMMEDIATE").await.map_err(db_err)?;
            if !has_schema(&mut *tx).await.map_err(db_err)? {
                sqlx::query(SCHEMA)
                    .execute(&mut *tx)
                    .await
                    .map_err(db_err)?;
            }
            tx.commit().await.map_err(db_err)?;
        }
        Ok(Self { pool })
    }

    /// Record a whole commit atomically; returns its new ID.
    ///
    /// Blobs must already be flushed to the blob store before calling.
    pub async fn apply_commit(
        &mut self,
        path: &str,
        created_at_ms: u64,
        parents: &[CommitId],
        entries: &[CommitEntry],
    ) -> Result<CommitId, StorageError> {
        Ok(self
            .apply_commit_incremental(path, created_at_ms, parents, entries)
            .await?
            .id)
    }

    /// Parent edges of every commit, grouped by child.
    ///
    /// The graph is small (one edge per commit in a linear history), so
    /// reading it whole beats a query per commit.
    async fn parent_map(&self) -> Result<BTreeMap<CommitId, Vec<CommitId>>, StorageError> {
        let rows = sqlx::query(
            "SELECT child_id, parent_id FROM commit_parents ORDER BY child_id, parent_id",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?;
        let mut map: BTreeMap<CommitId, Vec<CommitId>> = BTreeMap::new();
        for row in &rows {
            let child = commit_id_from_blob(row.try_get("child_id").map_err(db_err)?)?;
            let parent = commit_id_from_blob(row.try_get("parent_id").map_err(db_err)?)?;
            map.entry(child).or_default().push(parent);
        }
        Ok(map)
    }
}

/// Fallible `i64` -> `i32` narrowing for SQLite integer columns.
fn i64_to_i32(value: i64, column: &'static str) -> Result<i32, StorageError> {
    i32::try_from(value).map_err(|_| StorageError::InvalidHistoryValue { column, value })
}

/// Fallible `i64` -> `u64` widening for SQLite timestamp/size columns.
fn i64_to_u64(value: i64, column: &'static str) -> Result<u64, StorageError> {
    u64::try_from(value).map_err(|_| StorageError::InvalidHistoryValue { column, value })
}

/// Decode a 32-byte commit-ID BLOB.
fn commit_id_from_blob(bytes: Vec<u8>) -> Result<CommitId, StorageError> {
    hash32(bytes).map(CommitId)
}

/// Read an `i32` column with a loud error on out-of-range values.
fn i32_col(row: &sqlx::sqlite::SqliteRow, column: &'static str) -> Result<i32, StorageError> {
    let value: i64 = row.try_get(column).map_err(db_err)?;
    i64_to_i32(value, column)
}

/// Read a 32-byte hash column.
fn hash32(bytes: Vec<u8>) -> Result<[u8; 32], StorageError> {
    bytes
        .try_into()
        .map_err(|bytes: Vec<u8>| StorageError::InvalidHashLength { len: bytes.len() })
}

/// Decode a 32-byte root hash BLOB.
fn root_hash_from_blob(bytes: Vec<u8>) -> Result<RootHash, StorageError> {
    hash32(bytes).map(RootHash::new)
}

/// Map a tree codec failure into the storage error surface.
const fn tree_err(err: sekai_storage::TreeError) -> StorageError {
    match err {
        sekai_storage::TreeError::UnknownKind { kind } => StorageError::UnknownRegionKind { kind },
        other => StorageError::Tree(other),
    }
}

/// Read a millisecond timestamp column.
fn millis_col(row: &sqlx::sqlite::SqliteRow, column: &'static str) -> Result<u64, StorageError> {
    let value: i64 = row.try_get(column).map_err(db_err)?;
    i64_to_u64(value, column)
}

/// Decode one `refs` row. Stored names always re-validate: anything
/// else is corruption, never silent reinterpretation.
fn decode_ref(row: &sqlx::sqlite::SqliteRow) -> Result<CommitRef, StorageError> {
    let name_raw: String = row.try_get("name").map_err(db_err)?;
    let name =
        Ref::from_full(&name_raw).map_err(|_| StorageError::InvalidRefName { name: name_raw })?;
    Ok(CommitRef::new(
        name,
        commit_id_from_blob(row.try_get("commit_id").map_err(db_err)?)?,
    ))
}

/// Decode a stored `head_ref`, which must name a branch. Stored names
/// always re-validate: anything else is corruption, never silent
/// reinterpretation.
fn branch_from_column(raw: String) -> Result<RefName, StorageError> {
    match Ref::from_full(&raw) {
        Ok(Ref {
            kind: RefKind::Branch,
            name,
        }) => Ok(name),
        _ => Err(StorageError::InvalidRefName { name: raw }),
    }
}

/// Decode one `worktrees` row.
fn decode_worktree(row: &sqlx::sqlite::SqliteRow) -> Result<Worktree, StorageError> {
    let head_ref: Option<String> = row.try_get("head_ref").map_err(db_err)?;
    let head_commit: Option<Vec<u8>> = row.try_get("head_commit").map_err(db_err)?;
    let path: String = row.try_get("path").map_err(db_err)?;
    // The CHECK constraint keeps exactly one of the two columns set.
    let head = match (head_ref, head_commit) {
        (Some(branch), None) => Head::Branch(branch_from_column(branch)?),
        (None, Some(commit)) => Head::Detached(commit_id_from_blob(commit)?),
        _ => return Err(StorageError::CorruptWorktreeHead { path }),
    };
    Ok(Worktree {
        path,
        main: row.try_get("main").map_err(db_err)?,
        head,
    })
}

/// The HEAD of the worktree at `path` resolved against the refs, or
/// `None` when no worktree is registered there. A branch without a row is
/// unborn.
async fn head_state(
    conn: &mut SqliteConnection,
    path: &str,
) -> Result<Option<HeadState>, StorageError> {
    let row = sqlx::query(
        "SELECT worktrees.path, worktrees.main, worktrees.head_ref, worktrees.head_commit,
                refs.commit_id AS branch_commit
         FROM worktrees LEFT JOIN refs ON refs.name = worktrees.head_ref
         WHERE worktrees.path = ?",
    )
    .bind(path)
    .fetch_optional(conn)
    .await
    .map_err(db_err)?;
    let Some(row) = row else {
        return Ok(None);
    };
    let branch_commit: Option<Vec<u8>> = row.try_get("branch_commit").map_err(db_err)?;
    let commit = branch_commit.map(commit_id_from_blob).transpose()?;
    Ok(Some(decode_worktree(&row)?.head.resolve(commit)))
}

/// One scope's columns, gathered from its joined area rows.
struct ScopeColumns {
    created_at_ms: u64,
    areas: Vec<(Dimension, Rect)>,
}

/// Group rows of [`SCOPE_QUERY`] by scope name. The map iterates in
/// byte-wise name order, the order `ScopeName` compares by.
fn group_scope_rows(
    rows: &[sqlx::sqlite::SqliteRow],
) -> Result<BTreeMap<String, ScopeColumns>, StorageError> {
    let mut grouped = BTreeMap::new();
    for row in rows {
        let name: String = row.try_get("name").map_err(db_err)?;
        let columns = match grouped.entry(name) {
            Entry::Occupied(entry) => entry.into_mut(),
            Entry::Vacant(entry) => entry.insert(ScopeColumns {
                created_at_ms: millis_col(row, "created_at_ms")?,
                areas: Vec::new(),
            }),
        };
        // The LEFT JOIN yields one all-NULL area for a scope without rows.
        let dim: Option<i64> = row.try_get("dim").map_err(db_err)?;
        if let Some(dim) = dim {
            columns.areas.push((
                Dimension::new(i64_to_i32(dim, "dim")?),
                Rect::new(
                    i32_col(row, "x0")?,
                    i32_col(row, "z0")?,
                    i32_col(row, "x1")?,
                    i32_col(row, "z1")?,
                ),
            ));
        }
    }
    Ok(grouped)
}

/// Decode one grouped scope. Stored names re-validate and a scope must
/// hold an area: anything else is corruption, never silent
/// reinterpretation.
fn decode_scope(name: String, columns: ScopeColumns) -> Result<NamedScope, StorageError> {
    let Ok(parsed) = ScopeName::parse(&name) else {
        return Err(StorageError::InvalidScopeName { name });
    };
    NamedScope::new(parsed, columns.areas, columns.created_at_ms)
        .map_err(|NamedScopeError::NoAreas| StorageError::EmptyScope { name })
}

/// Every scope with its areas, one row per area. A single statement reads
/// one snapshot, so a concurrent replacement is seen whole or not at all.
const SCOPE_QUERY: &str = "SELECT scopes.name, scopes.created_at_ms,
            scope_areas.dim, scope_areas.x0, scope_areas.z0, scope_areas.x1, scope_areas.z1
     FROM scopes LEFT JOIN scope_areas ON scope_areas.scope = scopes.id";

/// `CommitId` as a BLOB bind parameter.
fn commit_param(id: CommitId) -> Vec<u8> {
    id.0.to_vec()
}

/// Millis as an `INTEGER` parameter. Genuinely fallible: callers compute the
/// clock, so an absurd value is their bug, surfaced loudly.
fn millis_param(ms: u64) -> Result<i64, StorageError> {
    i64::try_from(ms).map_err(|_| StorageError::InvalidTimestamp(ms))
}

/// Decode one `commits` row.
fn decode_commit(
    row: &sqlx::sqlite::SqliteRow,
    parents: &[CommitId],
) -> Result<Commit, StorageError> {
    let id: Vec<u8> = row.try_get("id").map_err(db_err)?;
    let root: Vec<u8> = row.try_get("root_hash").map_err(db_err)?;
    Ok(Commit::new(
        commit_id_from_blob(id)?,
        millis_col(row, "created_at_ms")?,
        parents.to_vec(),
        root_hash_from_blob(root)?,
    ))
}

/// Insert a commit row and the edges naming its parents.
///
/// Every parent must exist and none of them may be the commit itself; a
/// parent listed twice is recorded once. The checks run in the
/// transaction, so a rejected commit leaves nothing behind. An identical
/// commit (same hash) is idempotent: its row and edges already exist, and
/// `inserted` reports `false`.
async fn insert_commit(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    id: CommitId,
    created_at_ms: u64,
    parents: &[CommitId],
    root: RootHash,
) -> Result<(i64, bool), StorageError> {
    // One query for the whole list, so a merge does not cost a round
    // trip per parent.
    let existing: BTreeSet<CommitId> = if parents.is_empty() {
        BTreeSet::new()
    } else {
        let mut qb = sqlx::QueryBuilder::new("SELECT id FROM commits WHERE id IN (");
        let mut separated = qb.separated(", ");
        for parent in parents {
            separated.push_bind(commit_param(*parent));
        }
        separated.push_unseparated(")");
        qb.build_query_scalar()
            .fetch_all(&mut **tx)
            .await
            .map_err(db_err)?
            .into_iter()
            .map(commit_id_from_blob)
            .collect::<Result<_, _>>()?
    };
    for parent in parents {
        if !existing.contains(parent) {
            return Err(StorageError::UnknownCommit { id: *parent });
        }
    }
    let inserted = sqlx::query(
        "INSERT OR IGNORE INTO commits (id, created_at_ms, root_hash) VALUES (?, ?, ?)",
    )
    .bind(commit_param(id))
    .bind(millis_param(created_at_ms)?)
    .bind(&root.as_bytes()[..])
    .execute(&mut **tx)
    .await
    .map_err(db_err)?
    .rows_affected()
        > 0;
    let seq: i64 = sqlx::query_scalar("SELECT seq FROM commits WHERE id = ?")
        .bind(commit_param(id))
        .fetch_one(&mut **tx)
        .await
        .map_err(db_err)?;
    for parent in parents {
        sqlx::query("INSERT OR IGNORE INTO commit_parents (child_id, parent_id) VALUES (?, ?)")
            .bind(commit_param(id))
            .bind(commit_param(*parent))
            .execute(&mut **tx)
            .await
            .map_err(db_err)?;
    }
    Ok((seq, inserted))
}

/// Rows per bulk node insert.
///
/// Each row binds two parameters and SQLite caps a statement's parameter
/// count (32766 by default), so large inserts bind in batches. Every batch
/// is one round trip to sqlx's SQLite worker thread, which costs far more
/// than SQLite's own insert.
const NODE_INSERT_BATCH: usize = 1024;

/// Insert tree nodes children-first, ignoring hashes the store already has.
async fn insert_tree_nodes(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    nodes: &TreeNodes,
) -> Result<(), StorageError> {
    insert_node_rows(
        tx,
        "INSERT OR IGNORE INTO region_tree (hash, node) ",
        &nodes.region,
        RegionHash::as_bytes,
    )
    .await?;
    insert_node_rows(
        tx,
        "INSERT OR IGNORE INTO dim_tree (hash, node) ",
        &nodes.dim,
        DimHash::as_bytes,
    )
    .await?;
    insert_node_rows(
        tx,
        "INSERT OR IGNORE INTO root_tree (hash, node) ",
        &nodes.root,
        RootHash::as_bytes,
    )
    .await?;
    Ok(())
}

/// Insert one table's nodes in `NODE_INSERT_BATCH`-row statements.
async fn insert_node_rows<H: Ord>(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    prefix: &'static str,
    nodes: &BTreeMap<H, Vec<u8>>,
    key: fn(&H) -> &[u8; 32],
) -> Result<(), StorageError> {
    let mut insert = sqlx::QueryBuilder::new(prefix);
    let mut rows = nodes.iter().peekable();
    while rows.peek().is_some() {
        insert.reset().push_values(
            rows.by_ref().take(NODE_INSERT_BATCH),
            |mut row, (hash, node)| {
                row.push_bind(&key(hash)[..]).push_bind(node);
            },
        );
        insert.build().execute(&mut **tx).await.map_err(db_err)?;
    }
    Ok(())
}

/// Hashes per bulk node read.
///
/// Each hash binds one parameter and SQLite caps a statement's parameter
/// count (32766 by default), so large reads bind in batches. Every batch
/// is one round trip to sqlx's SQLite worker thread, which costs far more
/// than SQLite's own lookup.
const NODE_READ_BATCH: usize = 1024;

/// The tree table a node read targets. An enum keeps an unknown table
/// unrepresentable: no string fallback can quietly read the wrong level.
#[derive(Clone, Copy)]
enum TreeTable {
    Root,
    Dim,
    Region,
}

impl TreeTable {
    const fn select_node(self) -> &'static str {
        match self {
            Self::Root => "SELECT node FROM root_tree WHERE hash = ?",
            Self::Dim => "SELECT node FROM dim_tree WHERE hash = ?",
            Self::Region => "SELECT node FROM region_tree WHERE hash = ?",
        }
    }

    const fn select_nodes(self) -> &'static str {
        match self {
            Self::Root => "SELECT hash, node FROM root_tree WHERE hash IN (",
            Self::Dim => "SELECT hash, node FROM dim_tree WHERE hash IN (",
            Self::Region => "SELECT hash, node FROM region_tree WHERE hash IN (",
        }
    }
}

impl SqliteMeta {
    async fn node_bytes(
        &self,
        table: TreeTable,
        hash: &[u8; 32],
    ) -> Result<Option<Vec<u8>>, StorageError> {
        sqlx::query(table.select_node())
            .bind(&hash[..])
            .fetch_optional(&self.pool)
            .await
            .map_err(db_err)?
            .map(|row| row.try_get("node").map_err(db_err))
            .transpose()
    }

    /// Fetch many nodes by hash, one `IN (...)` query per batch.
    async fn node_bytes_many(
        &self,
        table: TreeTable,
        hashes: &[[u8; 32]],
    ) -> Result<BTreeMap<[u8; 32], Vec<u8>>, StorageError> {
        let select = table.select_nodes();
        let mut nodes = BTreeMap::new();
        for batch in hashes.chunks(NODE_READ_BATCH) {
            let mut query = sqlx::QueryBuilder::new(select);
            let mut separated = query.separated(", ");
            for hash in batch {
                separated.push_bind(&hash[..]);
            }
            separated.push_unseparated(")");
            let rows = query.build().fetch_all(&self.pool).await.map_err(db_err)?;
            for row in &rows {
                let hash: Vec<u8> = row.try_get("hash").map_err(db_err)?;
                let node: Vec<u8> = row.try_get("node").map_err(db_err)?;
                nodes.insert(hash32(hash)?, node);
            }
        }
        Ok(nodes)
    }
}
impl sekai_storage::MetaStore for SqliteMeta {
    type Error = StorageError;

    async fn create_commit(
        &mut self,
        created_at_ms: u64,
        parents: &[CommitId],
    ) -> Result<CommitId, StorageError> {
        // No snapshot changes: continue the lowest-hash parent's tree, or
        // start with the canonical empty root.
        let mut canonical_parents = parents.to_vec();
        canonical_parents.sort();
        canonical_parents.dedup();
        let parent_root = match canonical_parents.first() {
            Some(parent) => sqlx::query_scalar("SELECT root_hash FROM commits WHERE id = ?")
                .bind(commit_param(*parent))
                .fetch_optional(&self.pool)
                .await
                .map_err(db_err)?
                .map(root_hash_from_blob)
                .transpose()?,
            None => None,
        };
        let (root, nodes) = parent_root.map_or_else(sekai_storage::tree::empty_tree, |root| {
            (root, TreeNodes::default())
        });
        let id = commit_hash(parents, created_at_ms, root);
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        insert_tree_nodes(&mut tx, &nodes).await?;
        insert_commit(&mut tx, id, created_at_ms, parents, root).await?;
        tx.commit().await.map_err(db_err)?;
        Ok(id)
    }

    async fn commit_parents(&self, commit: CommitId) -> Result<Vec<CommitId>, StorageError> {
        let rows = sqlx::query(
            "SELECT parent_id FROM commit_parents WHERE child_id = ? ORDER BY parent_id",
        )
        .bind(commit_param(commit))
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?;
        rows.iter()
            .map(|row| commit_id_from_blob(row.try_get("parent_id").map_err(db_err)?))
            .collect()
    }

    async fn lookup_chunk(
        &self,
        commit: CommitId,
        coord: &ChunkCoord,
    ) -> Result<Option<CommitEntry>, StorageError> {
        sekai_storage::tree::lookup_chunk(self, commit, coord).await
    }

    async fn visit_commit_chunks<F>(&self, commit: CommitId, visit: F) -> Result<(), StorageError>
    where
        F: FnMut(&CommitEntry) -> bool + Send,
    {
        sekai_storage::tree::visit_chunks(self, commit, visit).await
    }

    async fn read_root_node(&self, hash: RootHash) -> Result<RootNode, StorageError> {
        let bytes = self
            .node_bytes(TreeTable::Root, hash.as_bytes())
            .await?
            .ok_or_else(|| StorageError::MissingTree {
                level: "root",
                hash: hash.to_string(),
            })?;
        RootNode::decode(&bytes, hash).map_err(tree_err)
    }

    async fn read_dim_node(&self, hash: DimHash) -> Result<DimNode, StorageError> {
        let bytes = self
            .node_bytes(TreeTable::Dim, hash.as_bytes())
            .await?
            .ok_or_else(|| StorageError::MissingTree {
                level: "dim",
                hash: hash.to_string(),
            })?;
        DimNode::decode(&bytes, hash).map_err(tree_err)
    }

    async fn read_region_node(&self, hash: RegionHash) -> Result<RegionNode, StorageError> {
        let bytes = self
            .node_bytes(TreeTable::Region, hash.as_bytes())
            .await?
            .ok_or_else(|| StorageError::MissingTree {
                level: "region",
                hash: hash.to_string(),
            })?;
        RegionNode::decode(&bytes, hash).map_err(tree_err)
    }

    async fn read_dim_nodes(&self, hashes: &[DimHash]) -> Result<Vec<DimNode>, StorageError> {
        let keys: Vec<[u8; 32]> = hashes.iter().map(|hash| *hash.as_bytes()).collect();
        let nodes = self.node_bytes_many(TreeTable::Dim, &keys).await?;
        hashes
            .iter()
            .map(|hash| {
                let bytes =
                    nodes
                        .get(hash.as_bytes())
                        .ok_or_else(|| StorageError::MissingTree {
                            level: "dim",
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
        let keys: Vec<[u8; 32]> = hashes.iter().map(|hash| *hash.as_bytes()).collect();
        let nodes = self.node_bytes_many(TreeTable::Region, &keys).await?;
        hashes
            .iter()
            .map(|hash| {
                let bytes =
                    nodes
                        .get(hash.as_bytes())
                        .ok_or_else(|| StorageError::MissingTree {
                            level: "region",
                            hash: hash.to_string(),
                        })?;
                RegionNode::decode(bytes, *hash).map_err(tree_err)
            })
            .collect()
    }

    async fn lookup_commit(&self, id: CommitId) -> Result<Option<Commit>, StorageError> {
        let row = sqlx::query("SELECT id, created_at_ms, root_hash FROM commits WHERE id = ?")
            .bind(commit_param(id))
            .fetch_optional(&self.pool)
            .await
            .map_err(db_err)?;
        let Some(row) = row else {
            return Ok(None);
        };
        let parents = self.commit_parents(id).await?;
        Ok(Some(decode_commit(&row, &parents)?))
    }

    async fn latest_commit(&self) -> Result<Option<Commit>, StorageError> {
        let row = sqlx::query(
            "SELECT id, created_at_ms, root_hash FROM commits ORDER BY seq DESC LIMIT 1",
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(db_err)?;
        let Some(row) = row else {
            return Ok(None);
        };
        let id = commit_id_from_blob(row.try_get("id").map_err(db_err)?)?;
        let parents = self.commit_parents(id).await?;
        Ok(Some(decode_commit(&row, &parents)?))
    }

    async fn visit_commits<F>(&self, mut visit: F) -> Result<(), StorageError>
    where
        F: FnMut(&Commit) -> bool + Send,
    {
        let rows = sqlx::query("SELECT id, created_at_ms, root_hash FROM commits ORDER BY seq")
            .fetch_all(&self.pool)
            .await
            .map_err(db_err)?;
        let parents = self.parent_map().await?;
        let mut ids: Vec<CommitId> = Vec::with_capacity(rows.len());
        for row in &rows {
            ids.push(commit_id_from_blob(row.try_get("id").map_err(db_err)?)?);
        }
        // The map holds every edge, so a commit without an entry is a
        // root; an edge naming a commit that is gone is corruption.
        let known: BTreeSet<CommitId> = ids.iter().copied().collect();
        debug_assert!(
            parents.keys().all(|id| known.contains(id)),
            "parent map names a commit that does not exist"
        );
        for (row, id) in rows.iter().zip(ids) {
            let commit = decode_commit(row, parents.get(&id).map_or(&[], Vec::as_slice))?;
            if !visit(&commit) {
                break;
            }
        }
        Ok(())
    }

    async fn apply_commit_incremental(
        &mut self,
        path: &str,
        created_at_ms: u64,
        parents: &[CommitId],
        entries: &[CommitEntry],
    ) -> Result<ApplyOutcome, StorageError> {
        sekai_storage::tree::apply_entries(self, path, created_at_ms, parents, entries).await
    }

    async fn commit_tree(
        &mut self,
        path: &str,
        created_at_ms: u64,
        parents: &[CommitId],
        root: RootHash,
        nodes: &TreeNodes,
    ) -> Result<ApplyOutcome, StorageError> {
        let commit = commit_hash(parents, created_at_ms, root);
        // Dropping the transaction on an early return rolls it back, so a
        // rejected call writes nothing.
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        let state =
            head_state(&mut tx, path)
                .await?
                .ok_or_else(|| StorageError::UnknownWorktree {
                    path: path.to_owned(),
                })?;
        // The new commit must continue HEAD's commit: moving HEAD onto an
        // unrelated history would fork it.
        if let Some(base) = state.commit()
            && !parents.contains(&base)
        {
            return Err(StorageError::NotContinuing { base });
        }
        insert_tree_nodes(&mut tx, nodes).await?;
        insert_commit(&mut tx, commit, created_at_ms, parents, root).await?;
        // HEAD moves onto the new commit in the same transaction: a HEAD
        // lagging behind its history would fork the next commit.
        match &state {
            HeadState::Attached { branch, .. } | HeadState::Unborn { branch } => {
                sqlx::query(
                    "INSERT INTO refs (name, commit_id) VALUES (?, ?)
                     ON CONFLICT(name) DO UPDATE SET commit_id = excluded.commit_id",
                )
                .bind(Ref::branch(branch.clone()).full_name())
                .bind(commit_param(commit))
                .execute(&mut *tx)
                .await
                .map_err(db_err)?;
            }
            HeadState::Detached { .. } => {
                sqlx::query("UPDATE worktrees SET head_commit = ? WHERE path = ?")
                    .bind(commit_param(commit))
                    .bind(path)
                    .execute(&mut *tx)
                    .await
                    .map_err(db_err)?;
            }
        }
        tx.commit().await.map_err(db_err)?;
        Ok(ApplyOutcome { id: commit })
    }

    async fn set_ref(&mut self, name: &Ref, commit: CommitId) -> Result<(), StorageError> {
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        // Pointing at a commit that does not exist is corruption, not a
        // move: report it as such instead of the foreign-key violation the
        // upsert would raise.
        let found: Option<i64> = sqlx::query_scalar("SELECT 1 FROM commits WHERE id = ?")
            .bind(commit_param(commit))
            .fetch_optional(&mut *tx)
            .await
            .map_err(db_err)?;
        if found.is_none() {
            return Err(StorageError::UnknownCommit { id: commit });
        }
        sqlx::query(
            "INSERT INTO refs (name, commit_id) VALUES (?, ?)
             ON CONFLICT(name) DO UPDATE SET commit_id = excluded.commit_id",
        )
        .bind(name.full_name())
        .bind(commit_param(commit))
        .execute(&mut *tx)
        .await
        .map_err(db_err)?;
        tx.commit().await.map_err(db_err)?;
        Ok(())
    }

    async fn lookup_ref(&self, name: &Ref) -> Result<Option<CommitRef>, StorageError> {
        sqlx::query("SELECT name, commit_id FROM refs WHERE name = ?")
            .bind(name.full_name())
            .fetch_optional(&self.pool)
            .await
            .map_err(db_err)?
            .map(|row| decode_ref(&row))
            .transpose()
    }

    async fn visit_refs<F>(&self, mut visit: F) -> Result<(), StorageError>
    where
        F: FnMut(&CommitRef) -> bool + Send,
    {
        let rows = sqlx::query("SELECT name, commit_id FROM refs ORDER BY name COLLATE BINARY")
            .fetch_all(&self.pool)
            .await
            .map_err(db_err)?;
        for row in &rows {
            if !visit(&decode_ref(row)?) {
                break;
            }
        }
        Ok(())
    }

    async fn delete_ref(&mut self, name: &Ref) -> Result<bool, StorageError> {
        let deleted = sqlx::query("DELETE FROM refs WHERE name = ?")
            .bind(name.full_name())
            .execute(&self.pool)
            .await
            .map_err(db_err)?;
        Ok(deleted.rows_affected() > 0)
    }

    async fn add_worktree(&mut self, worktree: &Worktree) -> Result<(), StorageError> {
        let (head_ref, head_commit) = match &worktree.head {
            Head::Branch(branch) => (Some(Ref::branch(branch.clone()).full_name()), None),
            Head::Detached(commit) => (None, Some(commit_param(*commit))),
        };
        sqlx::query(
            "INSERT INTO worktrees (path, main, head_ref, head_commit) VALUES (?, ?, ?, ?)",
        )
        .bind(&worktree.path)
        .bind(worktree.main)
        .bind(head_ref)
        .bind(head_commit)
        .execute(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(())
    }

    async fn visit_worktrees<F>(&self, mut visit: F) -> Result<(), StorageError>
    where
        F: FnMut(&Worktree) -> bool + Send,
    {
        let rows = sqlx::query(
            "SELECT path, main, head_ref, head_commit FROM worktrees ORDER BY path COLLATE BINARY",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?;
        for row in &rows {
            if !visit(&decode_worktree(row)?) {
                break;
            }
        }
        Ok(())
    }

    async fn head(&self, path: &str) -> Result<Option<HeadState>, StorageError> {
        let mut conn = self.pool.acquire().await.map_err(db_err)?;
        head_state(&mut conn, path).await
    }

    async fn set_scope(&mut self, scope: &NamedScope) -> Result<(), StorageError> {
        let created_at_ms = millis_param(scope.created_at_ms)?;
        let name = scope.name.as_str();
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        // A replacement keeps the scope row, and with it the id and the
        // creation time; only the whole area set is swapped.
        sqlx::query(
            "INSERT INTO scopes (name, created_at_ms) VALUES (?, ?)
             ON CONFLICT(name) DO NOTHING",
        )
        .bind(name)
        .bind(created_at_ms)
        .execute(&mut *tx)
        .await
        .map_err(db_err)?;
        let scope_id: i64 = sqlx::query_scalar("SELECT id FROM scopes WHERE name = ?")
            .bind(name)
            .fetch_one(&mut *tx)
            .await
            .map_err(db_err)?;
        sqlx::query("DELETE FROM scope_areas WHERE scope = ?")
            .bind(scope_id)
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
        for (dim, rect) in scope.areas() {
            sqlx::query(
                "INSERT INTO scope_areas (scope, dim, x0, z0, x1, z1) VALUES (?, ?, ?, ?, ?, ?)",
            )
            .bind(scope_id)
            .bind(i64::from(dim.raw()))
            .bind(i64::from(rect.x0))
            .bind(i64::from(rect.z0))
            .bind(i64::from(rect.x1))
            .bind(i64::from(rect.z1))
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
        }
        tx.commit().await.map_err(db_err)?;
        Ok(())
    }

    async fn lookup_scope(&self, name: &ScopeName) -> Result<Option<NamedScope>, StorageError> {
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "{SCOPE_QUERY} WHERE scopes.name = ?"
        )))
        .bind(name.as_str())
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?;
        group_scope_rows(&rows)?
            .into_iter()
            .next()
            .map(|(name, columns)| decode_scope(name, columns))
            .transpose()
    }

    async fn visit_scopes<F>(&self, mut visit: F) -> Result<(), StorageError>
    where
        F: FnMut(&NamedScope) -> bool + Send,
    {
        let rows = sqlx::query(SCOPE_QUERY)
            .fetch_all(&self.pool)
            .await
            .map_err(db_err)?;
        for (name, columns) in group_scope_rows(&rows)? {
            if !visit(&decode_scope(name, columns)?) {
                break;
            }
        }
        Ok(())
    }

    async fn delete_scope(&mut self, name: &ScopeName) -> Result<bool, StorageError> {
        // The foreign key cascades the delete onto the scope's areas.
        let deleted = sqlx::query("DELETE FROM scopes WHERE name = ?")
            .bind(name.as_str())
            .execute(&self.pool)
            .await
            .map_err(db_err)?;
        Ok(deleted.rows_affected() > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sekai_util::{DEFAULT_REF_NAME, RefName};
    use sqlx::ConnectOptions as _;
    use sqlx::Connection as _;

    fn branch(raw: &str) -> Ref {
        Ref::branch(RefName::parse(raw).unwrap())
    }
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

    /// World root of the main worktree the test stores register.
    const WORLD: &str = "/worlds/main";

    /// Register the main worktree at [`WORLD`] with HEAD unborn on
    /// `main`, the way an initialized store looks.
    async fn register_main(meta: &mut SqliteMeta) {
        meta.add_worktree(&Worktree {
            path: WORLD.to_owned(),
            main: true,
            head: Head::Branch(RefName::default_branch()),
        })
        .await
        .unwrap();
    }

    async fn memory_meta() -> SqliteMeta {
        let mut meta = SqliteMeta::open(&std::path::PathBuf::from(":memory:"))
            .await
            .unwrap();
        register_main(&mut meta).await;
        meta
    }

    #[tokio::test]
    async fn concurrent_openers_create_the_schema_once() {
        let (dir, _guard) = tempdir("concurrent-open");
        let path = dir.join("meta.sqlite");
        let (a, b) = tokio::join!(SqliteMeta::open(&path), SqliteMeta::open(&path));
        let a = a.unwrap();
        let _b = b.unwrap();
        let tables: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM sqlite_master WHERE name = 'refs'")
                .fetch_one(&a.pool)
                .await
                .unwrap();
        assert_eq!(tables, 1);
    }

    #[tokio::test]
    async fn a_fresh_store_seeds_no_ref() {
        let meta = memory_meta().await;
        let mut refs = Vec::new();
        meta.visit_refs(|record| {
            refs.push(record.clone());
            true
        })
        .await
        .unwrap();
        assert_eq!(refs, []);
        assert_eq!(
            meta.head(WORLD).await.unwrap(),
            Some(HeadState::Unborn {
                branch: RefName::default_branch()
            })
        );
    }

    #[tokio::test]
    async fn an_unborn_head_starts_a_new_root() {
        let mut meta = memory_meta().await;
        let main = branch(DEFAULT_REF_NAME);
        let first = meta
            .apply_commit_incremental(WORLD, 1_000, &[], &[])
            .await
            .unwrap()
            .id;

        // Deleting HEAD's branch leaves HEAD unborn, which deliberately
        // drops the parent constraint: the next commit is a fresh root.
        assert!(meta.delete_ref(&main).await.unwrap());
        let second = meta
            .apply_commit_incremental(WORLD, 3_000, &[], &[])
            .await
            .unwrap()
            .id;
        assert_ne!(first, second);
        assert_eq!(
            meta.head(WORLD)
                .await
                .unwrap()
                .and_then(|head| head.commit()),
            Some(second)
        );
        assert_eq!(meta.commit_parents(second).await.unwrap(), Vec::new());
    }

    #[tokio::test]
    async fn a_ref_cannot_name_a_missing_commit() {
        let mut meta = memory_meta().await;
        let feature = branch("feature");

        // A stale target is a typed error, not an opaque foreign-key
        // violation, and the ref does not appear.
        let missing = CommitId([0xEE; 32]);
        let err = meta.set_ref(&feature, missing).await.unwrap_err();
        assert!(matches!(err, StorageError::UnknownCommit { id } if id == missing));
        assert!(meta.lookup_ref(&feature).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn refs_visit_in_full_refname_order() {
        let mut meta = memory_meta().await;
        let first = meta.apply_commit(WORLD, 1_000, &[], &[]).await.unwrap();
        for name in ["z", "a/b", "a-b", "A"] {
            meta.set_ref(&branch(name), first).await.unwrap();
        }
        let mut seen = Vec::new();
        meta.visit_refs(|record| {
            seen.push(record.name.full_name());
            true
        })
        .await
        .unwrap();
        let mut sorted = seen.clone();
        sorted.sort();
        assert_eq!(seen, sorted);
        assert_eq!(seen.len(), 5);
        // Stored names are full refnames.
        let stored: Vec<String> = sqlx::query_scalar("SELECT name FROM refs ORDER BY name")
            .fetch_all(&meta.pool)
            .await
            .unwrap();
        assert_eq!(stored, seen);
    }

    fn worktree(path: &str, main: bool, head: Head) -> Worktree {
        Worktree {
            path: path.to_owned(),
            main,
            head,
        }
    }

    fn on_branch(raw: &str) -> Head {
        Head::Branch(RefName::parse(raw).unwrap())
    }

    async fn worktrees(meta: &SqliteMeta) -> Vec<Worktree> {
        let mut out = Vec::new();
        meta.visit_worktrees(|worktree| {
            out.push(worktree.clone());
            true
        })
        .await
        .unwrap();
        out
    }

    #[tokio::test]
    async fn worktrees_round_trip_in_path_order() {
        let mut meta = memory_meta().await;
        let first = meta.create_commit(1_000, &[]).await.unwrap();
        let added = [
            worktree("/worlds/b", false, on_branch("feature")),
            worktree("/worlds/a", false, Head::Detached(first)),
        ];
        for one in &added {
            meta.add_worktree(one).await.unwrap();
        }
        let main = worktree(WORLD, true, on_branch(DEFAULT_REF_NAME));
        assert_eq!(
            worktrees(&meta).await,
            [added[1].clone(), added[0].clone(), main]
        );
    }

    #[tokio::test]
    async fn worktrees_keep_one_main_one_path_and_one_head_per_branch() {
        let mut meta = memory_meta().await;
        let main = worktree(WORLD, true, on_branch(DEFAULT_REF_NAME));

        for refused in [
            worktree("/worlds/other", true, on_branch("other")),
            worktree("/worlds/main", false, on_branch("other")),
            worktree("/worlds/other", false, on_branch(DEFAULT_REF_NAME)),
        ] {
            assert!(
                meta.add_worktree(&refused).await.is_err(),
                "{refused:?} is refused"
            );
        }
        assert_eq!(worktrees(&meta).await, [main]);
    }

    #[tokio::test]
    async fn head_resolves_each_worktree_against_the_refs() {
        let mut meta = memory_meta().await;
        let main = RefName::default_branch();
        assert_eq!(
            meta.head(WORLD).await.unwrap(),
            Some(HeadState::Unborn {
                branch: main.clone()
            })
        );

        let first = meta
            .apply_commit_incremental(WORLD, 1_000, &[], &[])
            .await
            .unwrap()
            .id;
        let linked = worktree("/worlds/linked", false, Head::Detached(first));
        meta.add_worktree(&linked).await.unwrap();
        assert_eq!(
            meta.head(WORLD).await.unwrap(),
            Some(HeadState::Attached {
                branch: main,
                commit: first
            })
        );
        assert_eq!(
            meta.head("/worlds/linked").await.unwrap(),
            Some(HeadState::Detached { commit: first })
        );
        assert_eq!(meta.head("/elsewhere").await.unwrap(), None);
    }

    #[tokio::test]
    async fn a_corrupt_worktree_head_row_fails_loudly() {
        let (dir, _guard) = tempdir("corrupt-head");
        let path = dir.join("meta.sqlite");
        let mut meta = SqliteMeta::open(&path).await.unwrap();
        let first = meta.create_commit(1_000, &[]).await.unwrap();
        meta.add_worktree(&worktree("/worlds/main", true, Head::Detached(first)))
            .await
            .unwrap();

        // The table's CHECK normally rejects a row with both HEAD columns
        // set; disable it on a dedicated connection so no pooled
        // connection inherits the pragma.
        let mut raw = SqliteConnectOptions::new()
            .filename(&path)
            .connect()
            .await
            .unwrap();
        sqlx::query("PRAGMA ignore_check_constraints = ON")
            .execute(&mut raw)
            .await
            .unwrap();
        sqlx::query("UPDATE worktrees SET head_ref = 'refs/heads/main'")
            .execute(&mut raw)
            .await
            .unwrap();
        raw.close().await.unwrap();

        assert!(matches!(
            meta.visit_worktrees(|_| true).await,
            Err(StorageError::CorruptWorktreeHead { path }) if path == "/worlds/main"
        ));
    }

    #[tokio::test]
    async fn the_schema_refuses_short_ref_names() {
        let mut meta = memory_meta().await;
        let first = meta.apply_commit(WORLD, 1_000, &[], &[]).await.unwrap();
        let insert = |name: &'static str| {
            sqlx::query("INSERT INTO refs (name, commit_id) VALUES (?, ?)")
                .bind(name)
                .bind(commit_param(first))
        };
        insert("refs/heads/ok").execute(&meta.pool).await.unwrap();
        assert!(insert("main2").execute(&meta.pool).await.is_err());
    }

    #[tokio::test]
    async fn recording_the_same_commit_twice_is_idempotent() {
        let mut meta = memory_meta().await;
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
    async fn a_commit_must_continue_head() {
        let mut meta = memory_meta().await;
        let root = meta
            .apply_commit_incremental(WORLD, 1_000, &[], &[])
            .await
            .unwrap()
            .id;

        let err = meta
            .apply_commit_incremental(WORLD, 2_000, &[], &[])
            .await
            .unwrap_err();
        assert!(matches!(err, StorageError::NotContinuing { base } if base == root));

        // The rejected call wrote nothing.
        assert_eq!(meta.latest_commit().await.unwrap().unwrap().id, root);
        assert_eq!(
            meta.head(WORLD)
                .await
                .unwrap()
                .and_then(|head| head.commit()),
            Some(root)
        );

        let next = meta
            .apply_commit_incremental(WORLD, 3_000, &[root], &[])
            .await
            .unwrap()
            .id;
        assert_ne!(next, root);
    }

    #[tokio::test]
    async fn a_corrupt_ref_row_fails_loudly() {
        let mut meta = memory_meta().await;
        let first = meta.apply_commit(WORLD, 1_000, &[], &[]).await.unwrap();
        // Names are re-validated on decode: a row the parser refuses is
        // corruption, never a new ref name.
        sqlx::query("INSERT INTO refs (name, commit_id) VALUES ('refs/heads/bad name', ?)")
            .bind(commit_param(first))
            .execute(&meta.pool)
            .await
            .unwrap();
        let err = meta.visit_refs(|_| true).await.unwrap_err();
        assert!(
            matches!(err, StorageError::InvalidRefName { name } if name == "refs/heads/bad name")
        );
    }

    #[tokio::test]
    async fn a_commit_on_an_unknown_worktree_writes_nothing() {
        let mut meta = memory_meta().await;
        let err = meta
            .apply_commit_incremental("/elsewhere", 1_000, &[], &[])
            .await
            .unwrap_err();
        assert!(matches!(err, StorageError::UnknownWorktree { path } if path == "/elsewhere"));
        assert!(meta.latest_commit().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn a_commit_lands_on_head_and_gives_its_branch_a_commit() {
        let mut meta = memory_meta().await;
        assert_eq!(
            meta.head(WORLD).await.unwrap(),
            Some(HeadState::Unborn {
                branch: RefName::default_branch()
            })
        );
        let first = meta
            .apply_commit_incremental(WORLD, 1_000, &[], &[])
            .await
            .unwrap()
            .id;
        assert_eq!(
            meta.head(WORLD).await.unwrap(),
            Some(HeadState::Attached {
                branch: RefName::default_branch(),
                commit: first,
            })
        );
        assert!(meta.head("/elsewhere").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn worktrees_commit_onto_their_own_heads() {
        let mut meta = memory_meta().await;
        meta.add_worktree(&worktree("/worlds/linked", false, on_branch("feature")))
            .await
            .unwrap();
        let on_main = meta.apply_commit(WORLD, 1_000, &[], &[]).await.unwrap();
        let on_feature = meta
            .apply_commit("/worlds/linked", 2_000, &[], &[])
            .await
            .unwrap();
        assert_eq!(
            meta.lookup_ref(&branch(DEFAULT_REF_NAME))
                .await
                .unwrap()
                .unwrap()
                .commit,
            on_main
        );
        assert_eq!(
            meta.lookup_ref(&branch("feature"))
                .await
                .unwrap()
                .unwrap()
                .commit,
            on_feature
        );
    }

    #[tokio::test]
    async fn a_detached_head_moves_without_a_branch() {
        let mut meta = memory_meta().await;
        let first = meta.apply_commit(WORLD, 1_000, &[], &[]).await.unwrap();
        meta.add_worktree(&worktree("/worlds/linked", false, Head::Detached(first)))
            .await
            .unwrap();

        let second = meta
            .apply_commit("/worlds/linked", 2_000, &[first], &[])
            .await
            .unwrap();
        assert_eq!(
            meta.head("/worlds/linked").await.unwrap(),
            Some(HeadState::Detached { commit: second })
        );
        assert_eq!(
            meta.lookup_ref(&branch(DEFAULT_REF_NAME))
                .await
                .unwrap()
                .unwrap()
                .commit,
            first,
            "no branch moves with a detached HEAD"
        );
    }

    #[tokio::test]
    async fn a_commit_cannot_name_a_missing_commit_as_parent() {
        let mut meta = memory_meta().await;
        let missing = CommitId([9; 32]);
        assert!(matches!(
            meta.create_commit(1_000, &[missing]).await,
            Err(StorageError::UnknownCommit { id }) if id == missing
        ));
        // The rejected attempt left nothing behind.
        assert!(meta.latest_commit().await.unwrap().is_none());
        let edges: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM commit_parents")
            .fetch_one(&meta.pool)
            .await
            .unwrap();
        assert_eq!(edges, 0);

        // A rejection on a store that already holds a history leaves that
        // history alone.
        let root = meta.create_commit(2_000, &[]).await.unwrap();
        assert!(matches!(
            meta.create_commit(3_000, &[missing]).await,
            Err(StorageError::UnknownCommit { id }) if id == missing
        ));
        assert_eq!(meta.latest_commit().await.unwrap().unwrap().id, root);
        let edges: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM commit_parents")
            .fetch_one(&meta.pool)
            .await
            .unwrap();
        assert_eq!(edges, 0);
    }

    #[tokio::test]
    async fn commit_rows_store_canonical_tree_bytes() {
        use sekai_util::{ChunkHash, Dimension, RegionKind};
        let mut meta = memory_meta().await;
        let coord = ChunkCoord::new(Dimension::OVERWORLD, RegionKind::Region, 5, 5);
        let id = meta
            .apply_commit_incremental(
                WORLD,
                1_000,
                &[],
                &[sekai_util::CommitEntry::new(
                    coord,
                    Some(ChunkHash::new([1; 32])),
                )],
            )
            .await
            .unwrap()
            .id;
        let record = meta.lookup_commit(id).await.unwrap().unwrap();

        let root_bytes: Vec<u8> = sqlx::query_scalar("SELECT node FROM root_tree WHERE hash = ?")
            .bind(&record.root.as_bytes()[..])
            .fetch_one(&meta.pool)
            .await
            .unwrap();
        let root = RootNode::decode(&root_bytes, record.root).unwrap();
        let dim_hash = root.get(Dimension::OVERWORLD.raw()).unwrap();
        let dim_bytes: Vec<u8> = sqlx::query_scalar("SELECT node FROM dim_tree WHERE hash = ?")
            .bind(&dim_hash.as_bytes()[..])
            .fetch_one(&meta.pool)
            .await
            .unwrap();
        let dim = DimNode::decode(&dim_bytes, dim_hash).unwrap();
        let region_hash = dim.get(RegionKind::Region, 0, 0).unwrap();
        let region_bytes: Vec<u8> =
            sqlx::query_scalar("SELECT node FROM region_tree WHERE hash = ?")
                .bind(&region_hash.as_bytes()[..])
                .fetch_one(&meta.pool)
                .await
                .unwrap();
        let region = RegionNode::decode(&region_bytes, region_hash).unwrap();
        assert_eq!(region.get(5, 5), Some(ChunkHash::new([1; 32])));
    }

    #[tokio::test]
    async fn stored_tree_corruption_fails_loudly() {
        use sekai_storage::MetaStore as _;
        let meta = memory_meta().await;
        let hash = RootHash::new([9; 32]);
        sqlx::query("INSERT INTO root_tree (hash, node) VALUES (?, ?)")
            .bind(&hash.as_bytes()[..])
            .bind(Vec::<u8>::new())
            .execute(&meta.pool)
            .await
            .unwrap();
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
    async fn a_commit_spanning_node_insert_batches_records_every_region() {
        use sekai_storage::MetaStore as _;
        use sekai_util::{ChunkHash, RegionKind};
        let mut meta = memory_meta().await;
        // One chunk per region, so the region table alone spans three batches.
        // Region nodes are content-addressed: a repeating blob would collapse
        // same-slot regions into one node, so every blob must be distinct.
        let rows = NODE_INSERT_BATCH * 2 + 3;
        let entries: Vec<CommitEntry> = (0..rows)
            .map(|i| {
                let x = i32::try_from(i).unwrap() * 32;
                let coord = ChunkCoord::new(Dimension::OVERWORLD, RegionKind::Region, x, -x);
                let mut hash = [0u8; 32];
                hash[..2].copy_from_slice(&u16::try_from(i).unwrap().to_le_bytes());
                CommitEntry::new(coord, Some(ChunkHash::new(hash)))
            })
            .collect();
        let commit = meta
            .apply_commit(WORLD, 1_000, &[], &entries)
            .await
            .unwrap();

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
        let mut meta = memory_meta().await;
        let record = scope("spawn");
        meta.set_scope(&record).await.unwrap();
        assert!(meta.delete_scope(&record.name).await.unwrap());
        let areas: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM scope_areas")
            .fetch_one(&meta.pool)
            .await
            .unwrap();
        assert_eq!(areas, 0);
    }

    #[tokio::test]
    async fn a_corrupt_scope_fails_loudly() {
        let meta = memory_meta().await;

        // Names re-validate on decode.
        sqlx::query("INSERT INTO scopes (name, created_at_ms) VALUES ('bad name', 0)")
            .execute(&meta.pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO scope_areas (scope, dim, x0, z0, x1, z1) \
             SELECT id, 0, 0, 0, 0, 0 FROM scopes WHERE name = 'bad name'",
        )
        .execute(&meta.pool)
        .await
        .unwrap();
        let err = meta.visit_scopes(|_| true).await.unwrap_err();
        assert!(matches!(err, StorageError::InvalidScopeName { name } if name == "bad name"));
        sqlx::query("DELETE FROM scopes")
            .execute(&meta.pool)
            .await
            .unwrap();

        // A scope without areas would select nothing: `NamedScope::new`
        // refuses one, so a stored one is corruption.
        sqlx::query("INSERT INTO scopes (name, created_at_ms) VALUES ('empty', 0)")
            .execute(&meta.pool)
            .await
            .unwrap();
        let empty = ScopeName::parse("empty").unwrap();
        let err = meta.lookup_scope(&empty).await.unwrap_err();
        assert!(matches!(err, StorageError::EmptyScope { name } if name == "empty"));
    }
}
