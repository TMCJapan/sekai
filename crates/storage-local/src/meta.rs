//! SQLite commit metadata: the history graph and snapshot trees.
//!
//! Recording a commit uses one transaction; blob durability is established
//! before the metadata commit by the caller. The pool has one connection.

use std::collections::{BTreeMap, BTreeSet, btree_map::Entry};
use std::path::Path;
use std::time::{Duration, Instant};

use sekai_core::{
    ApplyOutcome, ChunkCoord, Commit, CommitEntry, CommitId, CommitRef, DimHash, Dimension, Head,
    HeadState, NamedScope, NamedScopeError, Rect, Ref, RefKind, RefName, ReflogEntry, ReflogNote,
    RegionHash, RootHash, ScopeName, Worktree, WorktreeName, commit_hash,
};
use sekai_storage::{DimNode, RegionNode, RootNode, StorageError, TreeNodes};
use sqlx::Row as _;
use sqlx::SqliteConnection;
use sqlx::sqlite::{
    SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions, SqliteSynchronous,
};

/// Managed schema version (`PRAGMA user_version`).
pub const SCHEMA_VERSION: i64 = 18;

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

async fn read_user_version<'c>(
    executor: impl sqlx::SqliteExecutor<'c>,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar("PRAGMA user_version")
        .fetch_one(executor)
        .await
}

impl SqliteMeta {
    /// Open (creating and version-gating) the database at `path`.
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
        let mut version = read_user_version(&pool).await.map_err(db_err)?;
        if version == 0 {
            // Re-read under the write lock: a concurrent opener that won the
            // race has stamped the version by the time this one gets it, so
            // the schema is created exactly once. DDL and the stamp commit
            // together: a crash between them would leave a half-created
            // schema that no later open can repair (the gate would demand a
            // version the store never got).
            let mut tx = pool.begin_with("BEGIN IMMEDIATE").await.map_err(db_err)?;
            version = read_user_version(&mut *tx).await.map_err(db_err)?;
            if version == 0 {
                // No ref or worktree is seeded: `init` registers the
                // world (ADR-0035).
                sqlx::query(SCHEMA)
                    .execute(&mut *tx)
                    .await
                    .map_err(db_err)?;
                sqlx::query(sqlx::AssertSqlSafe(format!(
                    "PRAGMA user_version = {SCHEMA_VERSION}"
                )))
                .execute(&mut *tx)
                .await
                .map_err(db_err)?;
                version = SCHEMA_VERSION;
            }
            tx.commit().await.map_err(db_err)?;
        }
        if version != SCHEMA_VERSION {
            return Err(StorageError::UnsupportedSchema {
                found: version,
                supported: SCHEMA_VERSION,
            });
        }
        Ok(Self { pool })
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

/// Decode a stored full refname. Stored names always re-validate:
/// anything else is corruption, never silent reinterpretation.
fn ref_from_column(raw: String) -> Result<Ref, StorageError> {
    Ref::from_full(&raw).map_err(|_| StorageError::InvalidRefName { name: raw })
}

/// Decode a stored `head_ref`, which must name a branch.
fn branch_from_column(raw: String) -> Result<RefName, StorageError> {
    match Ref::from_full(&raw) {
        Ok(Ref {
            kind: RefKind::Branch,
            name,
        }) => Ok(name),
        _ => Err(StorageError::InvalidRefName { name: raw }),
    }
}

/// Decode a stored worktree name.
fn worktree_name_from_column(raw: String) -> Result<WorktreeName, StorageError> {
    WorktreeName::parse(&raw).map_err(|_| StorageError::InvalidWorktreeName { name: raw })
}

/// Decode one `refs` row.
fn decode_ref(row: &sqlx::sqlite::SqliteRow) -> Result<CommitRef, StorageError> {
    Ok(CommitRef::new(
        ref_from_column(row.try_get("name").map_err(db_err)?)?,
        commit_id_from_blob(row.try_get("commit_id").map_err(db_err)?)?,
    ))
}

/// Decode one `worktrees` row.
fn decode_worktree(row: &sqlx::sqlite::SqliteRow) -> Result<Worktree, StorageError> {
    let head_ref: Option<String> = row.try_get("head_ref").map_err(db_err)?;
    let head_commit: Option<Vec<u8>> = row.try_get("head_commit").map_err(db_err)?;
    let name: String = row.try_get("name").map_err(db_err)?;
    // The CHECK constraint keeps exactly one of the two columns set.
    let head = match (head_ref, head_commit) {
        (Some(branch), None) => Head::Branch(branch_from_column(branch)?),
        (None, Some(commit)) => Head::Detached(commit_id_from_blob(commit)?),
        _ => {
            return Err(StorageError::CorruptWorktreeHead { name });
        }
    };
    Ok(Worktree {
        name: worktree_name_from_column(name)?,
        path: row.try_get("path").map_err(db_err)?,
        head,
    })
}

/// Decode one `ref_log` or `head_log` row.
fn decode_log_entry(row: &sqlx::sqlite::SqliteRow) -> Result<ReflogEntry, StorageError> {
    let old: Option<Vec<u8>> = row.try_get("old_commit").map_err(db_err)?;
    Ok(ReflogEntry {
        old: old.map(commit_id_from_blob).transpose()?,
        new: commit_id_from_blob(row.try_get("new_commit").map_err(db_err)?)?,
        message: row.try_get("message").map_err(db_err)?,
        at_ms: millis_col(row, "at_ms")?,
    })
}

/// The commit `name` points at, or `None` when the ref does not exist.
async fn ref_target(
    conn: &mut SqliteConnection,
    name: &str,
) -> Result<Option<CommitId>, StorageError> {
    sqlx::query_scalar("SELECT commit_id FROM refs WHERE name = ?")
        .bind(name)
        .fetch_optional(conn)
        .await
        .map_err(db_err)?
        .map(commit_id_from_blob)
        .transpose()
}

/// Fail with `UnknownCommit` unless `id` is stored, instead of the
/// foreign-key violation a write would raise.
async fn require_commit(conn: &mut SqliteConnection, id: CommitId) -> Result<(), StorageError> {
    let found: Option<i64> = sqlx::query_scalar("SELECT 1 FROM commits WHERE id = ?")
        .bind(commit_param(id))
        .fetch_optional(conn)
        .await
        .map_err(db_err)?;
    found.map(drop).ok_or(StorageError::UnknownCommit { id })
}

/// The worktree that has branch `name` checked out, unborn or not.
async fn checked_out_by(
    conn: &mut SqliteConnection,
    name: &str,
) -> Result<Option<String>, StorageError> {
    sqlx::query_scalar("SELECT name FROM worktrees WHERE head_ref = ?")
        .bind(name)
        .fetch_optional(conn)
        .await
        .map_err(db_err)
}

/// Delete the ref row `name`; a branch's reflog goes with it through
/// its cascade.
async fn delete_ref_row(conn: &mut SqliteConnection, name: &str) -> Result<(), StorageError> {
    sqlx::query("DELETE FROM refs WHERE name = ?")
        .bind(name)
        .execute(conn)
        .await
        .map_err(db_err)?;
    Ok(())
}

/// Refuse `branch` when a worktree other than `allowed` has it checked
/// out.
async fn check_not_checked_out(
    conn: &mut SqliteConnection,
    branch: &Ref,
    allowed: Option<&WorktreeName>,
) -> Result<(), StorageError> {
    match checked_out_by(conn, &branch.full_name()).await? {
        Some(holder) if allowed.is_none_or(|allowed| allowed.as_str() != holder) => {
            Err(StorageError::BranchCheckedOut {
                branch: branch.clone(),
                worktree: holder,
            })
        }
        _ => Ok(()),
    }
}

/// Refuse `name` when it conflicts as a directory and a file would with
/// a ref, or with the branch an unborn HEAD names, other than `except`.
///
/// A conflicting name is either one of `name`'s `/`-separated ancestors,
/// or sorts inside `[name/, name0)`: `0` follows `/` in ASCII, so that
/// range holds exactly the names below `name/`.
// The range scan plus the ancestor disjuncts implement the same predicate
// as `util::refs::paths_conflict` over full refnames; it cannot call that
// function because SQLite does the comparison here.
async fn check_no_conflict(
    conn: &mut SqliteConnection,
    requested: &Ref,
    except: Option<&str>,
) -> Result<(), StorageError> {
    let name = &requested.full_name();
    let mut query = sqlx::QueryBuilder::new(
        "SELECT name FROM (
             SELECT name FROM refs
             UNION ALL SELECT head_ref FROM worktrees WHERE head_ref IS NOT NULL
         ) WHERE ((name >= ",
    );
    query
        .push_bind(format!("{name}/"))
        .push(" AND name < ")
        .push_bind(format!("{name}0"))
        .push(")");
    // Ancestors below the namespace only: `refs` and `refs/heads` are never
    // stored names.
    let namespace = RefKind::ALL
        .into_iter()
        .map(RefKind::prefix)
        .find(|prefix| name.starts_with(prefix))
        .map_or(0, str::len);
    for (at, _) in name.match_indices('/').filter(|&(at, _)| at > namespace) {
        query.push(" OR name = ").push_bind(&name[..at]);
    }
    query.push(")");
    if let Some(except) = except {
        query.push(" AND name != ").push_bind(except);
    }
    query.push(" LIMIT 1");
    let existing: Option<String> = query
        .build_query_scalar()
        .fetch_optional(conn)
        .await
        .map_err(db_err)?;
    match existing {
        Some(existing) => Err(StorageError::RefConflict {
            name: requested.clone(),
            existing: ref_from_column(existing)?,
        }),
        None => Ok(()),
    }
}

/// Which reflog to append to.
#[derive(Clone, Copy)]
enum LogTable {
    /// Branch reflogs, keyed by full refname.
    Ref,
    /// Worktree HEAD reflogs, keyed by worktree name.
    Head,
}

impl LogTable {
    const fn insert_sql(self) -> &'static str {
        match self {
            Self::Ref => {
                "INSERT INTO ref_log (name, old_commit, new_commit, message, at_ms)
                 VALUES (?, ?, ?, ?, ?)"
            }
            Self::Head => {
                "INSERT INTO head_log (worktree, old_commit, new_commit, message, at_ms)
                 VALUES (?, ?, ?, ?, ?)"
            }
        }
    }
}

/// Append one entry to a reflog.
async fn append_log(
    conn: &mut SqliteConnection,
    log: LogTable,
    key: &str,
    old: Option<CommitId>,
    new: CommitId,
    note: ReflogNote<'_>,
) -> Result<(), StorageError> {
    sqlx::query(log.insert_sql())
        .bind(key)
        .bind(old.map(commit_param))
        .bind(commit_param(new))
        .bind(note.message)
        .bind(millis_param(note.at_ms)?)
        .execute(conn)
        .await
        .map_err(db_err)?;
    Ok(())
}

/// Worktree `name`'s HEAD, refusing when the worktree does not exist.
async fn expect_head(conn: &mut SqliteConnection, name: &str) -> Result<HeadState, StorageError> {
    head_state(conn, name)
        .await?
        .ok_or_else(|| StorageError::UnknownWorktree {
            name: name.to_owned(),
        })
}

/// Worktree `name`'s HEAD resolved against the refs, or `None` when the
/// worktree does not exist.
async fn head_state(
    conn: &mut SqliteConnection,
    name: &str,
) -> Result<Option<HeadState>, StorageError> {
    let row = sqlx::query(
        "SELECT worktrees.name, worktrees.path, worktrees.head_ref, worktrees.head_commit,
                refs.commit_id AS branch_commit
         FROM worktrees LEFT JOIN refs ON refs.name = worktrees.head_ref
         WHERE worktrees.name = ?",
    )
    .bind(name)
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

/// The full refname of branch `name`.
fn branch_full_name(name: &RefName) -> String {
    RefKind::Branch.full_name(name)
}

/// Visit the rows `query` selects, in the order it sorts them, binding
/// `key` as the query's single parameter when it carries one.
async fn visit_rows<T, F, D>(
    pool: &SqlitePool,
    query: &'static str,
    key: Option<&str>,
    decode: D,
    mut visit: F,
) -> Result<(), StorageError>
where
    F: FnMut(&T) -> bool + Send,
    D: Fn(&sqlx::sqlite::SqliteRow) -> Result<T, StorageError>,
{
    let mut query = sqlx::query(query);
    if let Some(key) = key {
        query = query.bind(key);
    }
    let rows = query.fetch_all(pool).await.map_err(db_err)?;
    for row in &rows {
        if !visit(&decode(row)?) {
            break;
        }
    }
    Ok(())
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
        sekai_core::tree::lookup_chunk(self, commit, coord).await
    }

    async fn visit_commit_chunks<F>(&self, commit: CommitId, visit: F) -> Result<(), StorageError>
    where
        F: FnMut(&CommitEntry) -> bool + Send,
    {
        sekai_core::tree::visit_chunks(self, commit, visit).await
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
        worktree: &WorktreeName,
        created_at_ms: u64,
        parents: &[CommitId],
        entries: &[CommitEntry],
        note: ReflogNote<'_>,
    ) -> Result<ApplyOutcome, StorageError> {
        sekai_core::tree::apply_entries(self, worktree, created_at_ms, parents, entries, note).await
    }

    async fn commit_tree(
        &mut self,
        worktree: &WorktreeName,
        created_at_ms: u64,
        parents: &[CommitId],
        root: RootHash,
        nodes: &TreeNodes,
        note: ReflogNote<'_>,
    ) -> Result<ApplyOutcome, StorageError> {
        let commit = commit_hash(parents, created_at_ms, root);
        // Dropping the transaction on an early return rolls it back, so a
        // rejected call writes nothing.
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        let state = expect_head(&mut tx, worktree.as_str()).await?;
        // The new commit must continue HEAD's commit: moving HEAD onto an
        // unrelated history would fork it.
        if let Some(base) = state.commit()
            && !parents.contains(&base)
        {
            return Err(StorageError::NotContinuing { base });
        }
        if let HeadState::Unborn { branch } = &state {
            check_no_conflict(&mut tx, &Ref::branch(branch.clone()), None).await?;
        }
        insert_tree_nodes(&mut tx, nodes).await?;
        insert_commit(&mut tx, commit, created_at_ms, parents, root).await?;
        // HEAD moves onto the new commit in the same transaction: a HEAD
        // lagging behind its history would fork the next commit.
        match &state {
            HeadState::Attached {
                branch,
                commit: old,
            } => {
                let name = branch_full_name(branch);
                sqlx::query("UPDATE refs SET commit_id = ? WHERE name = ?")
                    .bind(commit_param(commit))
                    .bind(&name)
                    .execute(&mut *tx)
                    .await
                    .map_err(db_err)?;
                append_log(&mut tx, LogTable::Ref, &name, Some(*old), commit, note).await?;
            }
            HeadState::Unborn { branch } => {
                let name = branch_full_name(branch);
                sqlx::query("INSERT INTO refs (name, commit_id) VALUES (?, ?)")
                    .bind(&name)
                    .bind(commit_param(commit))
                    .execute(&mut *tx)
                    .await
                    .map_err(db_err)?;
                append_log(&mut tx, LogTable::Ref, &name, None, commit, note).await?;
            }
            HeadState::Detached { .. } => {
                sqlx::query("UPDATE worktrees SET head_commit = ? WHERE name = ?")
                    .bind(commit_param(commit))
                    .bind(worktree.as_str())
                    .execute(&mut *tx)
                    .await
                    .map_err(db_err)?;
            }
        }
        append_log(
            &mut tx,
            LogTable::Head,
            worktree.as_str(),
            state.commit(),
            commit,
            note,
        )
        .await?;
        tx.commit().await.map_err(db_err)?;
        Ok(ApplyOutcome { id: commit })
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

    async fn visit_refs<F>(&self, visit: F) -> Result<(), StorageError>
    where
        F: FnMut(&CommitRef) -> bool + Send,
    {
        visit_rows(
            &self.pool,
            "SELECT name, commit_id FROM refs ORDER BY name",
            None,
            decode_ref,
            visit,
        )
        .await
    }

    async fn create_ref(
        &mut self,
        name: &Ref,
        commit: CommitId,
        note: ReflogNote<'_>,
    ) -> Result<(), StorageError> {
        let full = name.full_name();
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        if ref_target(&mut tx, &full).await?.is_some() {
            return Err(StorageError::RefExists { name: name.clone() });
        }
        check_no_conflict(&mut tx, name, None).await?;
        require_commit(&mut tx, commit).await?;
        sqlx::query("INSERT INTO refs (name, commit_id) VALUES (?, ?)")
            .bind(&full)
            .bind(commit_param(commit))
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
        if name.kind == RefKind::Branch {
            append_log(&mut tx, LogTable::Ref, &full, None, commit, note).await?;
            // The branch an unborn HEAD names now has a commit, which
            // leaves that HEAD at it.
            if let Some(holder) = checked_out_by(&mut tx, &full).await? {
                append_log(&mut tx, LogTable::Head, &holder, None, commit, note).await?;
            }
        }
        tx.commit().await.map_err(db_err)?;
        Ok(())
    }

    async fn move_ref(
        &mut self,
        name: &Ref,
        commit: CommitId,
        note: ReflogNote<'_>,
    ) -> Result<CommitId, StorageError> {
        let full = name.full_name();
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        let old = ref_target(&mut tx, &full)
            .await?
            .ok_or_else(|| StorageError::UnknownRef { name: name.clone() })?;
        require_commit(&mut tx, commit).await?;
        if name.kind == RefKind::Branch {
            check_not_checked_out(&mut tx, name, None).await?;
        }
        sqlx::query("UPDATE refs SET commit_id = ? WHERE name = ?")
            .bind(commit_param(commit))
            .bind(&full)
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
        if name.kind == RefKind::Branch {
            append_log(&mut tx, LogTable::Ref, &full, Some(old), commit, note).await?;
        }
        tx.commit().await.map_err(db_err)?;
        Ok(old)
    }

    async fn delete_ref(&mut self, name: &Ref) -> Result<Option<CommitId>, StorageError> {
        let full = name.full_name();
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        let Some(old) = ref_target(&mut tx, &full).await? else {
            return Ok(None);
        };
        if name.kind == RefKind::Branch {
            check_not_checked_out(&mut tx, name, None).await?;
        }
        delete_ref_row(&mut tx, &full).await?;
        tx.commit().await.map_err(db_err)?;
        Ok(Some(old))
    }

    async fn rename_branch(
        &mut self,
        from: &RefName,
        to: &RefName,
        force: bool,
        note: ReflogNote<'_>,
    ) -> Result<(), StorageError> {
        let (from_ref, to_ref) = (Ref::branch(from.clone()), Ref::branch(to.clone()));
        let (from, to) = (from_ref.full_name(), to_ref.full_name());
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        let commit = ref_target(&mut tx, &from).await?;
        // The unique index on `head_ref` allows one HEAD per branch.
        let holder = checked_out_by(&mut tx, &from).await?;
        if commit.is_none() && holder.is_none() {
            return Err(StorageError::UnknownRef { name: from_ref });
        }
        let mut replaced = None;
        if from != to {
            replaced = ref_target(&mut tx, &to).await?;
            if replaced.is_some() && !force {
                return Err(StorageError::RefExists { name: to_ref });
            }
            check_not_checked_out(&mut tx, &to_ref, None).await?;
            check_no_conflict(&mut tx, &to_ref, Some(&from)).await?;
            if commit.is_some() && replaced.is_some() {
                delete_ref_row(&mut tx, &to).await?;
            }
            // `ref_log` follows the row through its `ON UPDATE CASCADE`.
            sqlx::query("UPDATE refs SET name = ? WHERE name = ?")
                .bind(&to)
                .bind(&from)
                .execute(&mut *tx)
                .await
                .map_err(db_err)?;
            sqlx::query("UPDATE worktrees SET head_ref = ? WHERE head_ref = ?")
                .bind(&to)
                .bind(&from)
                .execute(&mut *tx)
                .await
                .map_err(db_err)?;
        }
        match (commit, replaced, holder) {
            (Some(commit), _, holder) => {
                append_log(&mut tx, LogTable::Ref, &to, Some(commit), commit, note).await?;
                if let Some(holder) = holder {
                    append_log(&mut tx, LogTable::Head, &holder, Some(commit), commit, note)
                        .await?;
                }
            }
            // An unborn HEAD lands on the branch it was renamed over.
            (None, Some(landed), Some(holder)) => {
                append_log(&mut tx, LogTable::Head, &holder, None, landed, note).await?;
            }
            // Otherwise an unborn HEAD stays unborn and logs nothing.
            (None, _, _) => {}
        }
        tx.commit().await.map_err(db_err)?;
        Ok(())
    }

    async fn add_worktree(
        &mut self,
        worktree: &Worktree,
        note: ReflogNote<'_>,
    ) -> Result<(), StorageError> {
        let name = worktree.name.as_str();
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        let existing: Option<String> =
            sqlx::query_scalar("SELECT name FROM worktrees WHERE name = ?")
                .bind(name)
                .fetch_optional(&mut *tx)
                .await
                .map_err(db_err)?;
        if existing.is_some() {
            return Err(StorageError::WorktreeExists {
                name: name.to_owned(),
            });
        }
        // `IS` matches NULL too, so one query finds the main worktree or
        // the holder of a linked path.
        let path_taken: Option<String> =
            sqlx::query_scalar("SELECT name FROM worktrees WHERE path IS ?")
                .bind(worktree.path.as_deref())
                .fetch_optional(&mut *tx)
                .await
                .map_err(db_err)?;
        if path_taken.is_some() {
            return Err(worktree
                .path
                .clone()
                .map_or(StorageError::MainWorktreeExists, |path| {
                    StorageError::WorktreePathTaken { path }
                }));
        }
        let (head_ref, head_commit) = match &worktree.head {
            Head::Branch(branch) => {
                let branch = Ref::branch(branch.clone());
                check_not_checked_out(&mut tx, &branch, None).await?;
                check_no_conflict(&mut tx, &branch, None).await?;
                (Some(branch.full_name()), None)
            }
            Head::Detached(commit) => {
                require_commit(&mut tx, *commit).await?;
                (None, Some(commit_param(*commit)))
            }
        };
        sqlx::query(
            "INSERT INTO worktrees (name, path, head_ref, head_commit) VALUES (?, ?, ?, ?)",
        )
        .bind(name)
        .bind(worktree.path.as_deref())
        .bind(head_ref)
        .bind(head_commit)
        .execute(&mut *tx)
        .await
        .map_err(db_err)?;
        if let Some(commit) = head_state(&mut tx, name)
            .await?
            .and_then(|state| state.commit())
        {
            append_log(&mut tx, LogTable::Head, name, None, commit, note).await?;
        }
        tx.commit().await.map_err(db_err)?;
        Ok(())
    }

    async fn lookup_worktree(&self, name: &WorktreeName) -> Result<Option<Worktree>, StorageError> {
        sqlx::query("SELECT name, path, head_ref, head_commit FROM worktrees WHERE name = ?")
            .bind(name.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(db_err)?
            .map(|row| decode_worktree(&row))
            .transpose()
    }

    async fn visit_worktrees<F>(&self, visit: F) -> Result<(), StorageError>
    where
        F: FnMut(&Worktree) -> bool + Send,
    {
        visit_rows(
            &self.pool,
            "SELECT name, path, head_ref, head_commit FROM worktrees ORDER BY name",
            None,
            decode_worktree,
            visit,
        )
        .await
    }

    async fn remove_worktree(&mut self, name: &WorktreeName) -> Result<bool, StorageError> {
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        let path: Option<Option<String>> =
            sqlx::query_scalar("SELECT path FROM worktrees WHERE name = ?")
                .bind(name.as_str())
                .fetch_optional(&mut *tx)
                .await
                .map_err(db_err)?;
        match path {
            None => return Ok(false),
            Some(None) => {
                return Err(StorageError::MainWorktreeProtected {
                    name: name.as_str().to_owned(),
                });
            }
            Some(Some(_)) => {}
        }
        // The HEAD reflog goes with the worktree through its cascade.
        sqlx::query("DELETE FROM worktrees WHERE name = ?")
            .bind(name.as_str())
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
        tx.commit().await.map_err(db_err)?;
        Ok(true)
    }

    async fn set_head(
        &mut self,
        name: &WorktreeName,
        head: &Head,
        note: ReflogNote<'_>,
    ) -> Result<(), StorageError> {
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        let old = expect_head(&mut tx, name.as_str()).await?;
        let (head_ref, head_commit) = match head {
            Head::Branch(new_branch) => {
                let new_ref = Ref::branch(new_branch.clone());
                check_not_checked_out(&mut tx, &new_ref, Some(name)).await?;
                // Only a branch HEAD names exists only as the `head_ref`
                // this move rewrites. Such an unborn branch may be excluded
                // from the conflict scan; a born branch stays in `refs` and
                // must be seen. A re-checkout of the branch HEAD is already
                // on must also pass.
                let own = match &old {
                    HeadState::Unborn { branch } => Some(branch_full_name(branch)),
                    HeadState::Attached { branch, .. } if branch == new_branch => {
                        Some(branch_full_name(branch))
                    }
                    _ => None,
                };
                check_no_conflict(&mut tx, &new_ref, own.as_deref()).await?;
                (Some(new_ref.full_name()), None)
            }
            Head::Detached(commit) => {
                require_commit(&mut tx, *commit).await?;
                (None, Some(commit_param(*commit)))
            }
        };
        sqlx::query("UPDATE worktrees SET head_ref = ?, head_commit = ? WHERE name = ?")
            .bind(head_ref)
            .bind(head_commit)
            .bind(name.as_str())
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
        if let Some(new) = head_state(&mut tx, name.as_str())
            .await?
            .and_then(|state| state.commit())
        {
            append_log(
                &mut tx,
                LogTable::Head,
                name.as_str(),
                old.commit(),
                new,
                note,
            )
            .await?;
        }
        tx.commit().await.map_err(db_err)?;
        Ok(())
    }

    async fn head(&self, name: &WorktreeName) -> Result<Option<HeadState>, StorageError> {
        let mut conn = self.pool.acquire().await.map_err(db_err)?;
        head_state(&mut conn, name.as_str()).await
    }

    async fn visit_ref_log<F>(&self, name: &RefName, visit: F) -> Result<(), StorageError>
    where
        F: FnMut(&ReflogEntry) -> bool + Send,
    {
        visit_rows(
            &self.pool,
            "SELECT old_commit, new_commit, message, at_ms FROM ref_log
             WHERE name = ? ORDER BY seq DESC",
            Some(&branch_full_name(name)),
            decode_log_entry,
            visit,
        )
        .await
    }

    async fn visit_head_log<F>(&self, name: &WorktreeName, visit: F) -> Result<(), StorageError>
    where
        F: FnMut(&ReflogEntry) -> bool + Send,
    {
        visit_rows(
            &self.pool,
            "SELECT old_commit, new_commit, message, at_ms FROM head_log
             WHERE worktree = ? ORDER BY seq DESC",
            Some(name.as_str()),
            decode_log_entry,
            visit,
        )
        .await
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
    use sekai_storage::MetaStore as _;
    use sqlx::ConnectOptions as _;
    use sqlx::Connection as _;
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

    async fn memory_meta() -> SqliteMeta {
        SqliteMeta::open(&std::path::PathBuf::from(":memory:"))
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn fresh_store_carries_schema_version() {
        let meta = memory_meta().await;
        let version: i64 = sqlx::query_scalar("PRAGMA user_version")
            .fetch_one(&meta.pool)
            .await
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
    }

    fn worktree(name: &str) -> WorktreeName {
        WorktreeName::parse(name).unwrap()
    }

    fn branch_name(name: &str) -> RefName {
        RefName::parse(name).unwrap()
    }

    fn branch(name: &str) -> Ref {
        Ref::branch(branch_name(name))
    }

    fn tag(name: &str) -> Ref {
        Ref::tag(RefName::parse(name).unwrap())
    }

    const fn note(message: &str) -> ReflogNote<'_> {
        ReflogNote::new(message, 5_000)
    }

    /// A store whose main worktree `main` is unborn on `main`, the way
    /// `init` leaves it.
    async fn initialized_meta() -> SqliteMeta {
        let mut meta = memory_meta().await;
        let main = Worktree {
            name: worktree("main"),
            path: None,
            head: Head::Branch(branch_name("main")),
        };
        meta.add_worktree(&main, note("unused")).await.unwrap();
        meta
    }

    /// Commit an unchanged snapshot on `on`'s HEAD.
    async fn commit_on(
        meta: &mut SqliteMeta,
        on: &str,
        created_at_ms: u64,
        parents: &[CommitId],
    ) -> Result<CommitId, StorageError> {
        let message = if parents.is_empty() {
            "commit (initial): "
        } else {
            "commit: "
        };
        meta.apply_commit_incremental(
            &worktree(on),
            created_at_ms,
            parents,
            &[],
            ReflogNote::new(message, created_at_ms),
        )
        .await
        .map(|outcome| outcome.id)
    }

    async fn ref_log(meta: &SqliteMeta, name: &str) -> Vec<ReflogEntry> {
        let mut entries = Vec::new();
        meta.visit_ref_log(&branch_name(name), |entry| {
            entries.push(entry.clone());
            true
        })
        .await
        .unwrap();
        entries
    }

    async fn head_log(meta: &SqliteMeta, name: &str) -> Vec<ReflogEntry> {
        let mut entries = Vec::new();
        meta.visit_head_log(&worktree(name), |entry| {
            entries.push(entry.clone());
            true
        })
        .await
        .unwrap();
        entries
    }

    fn entry(old: Option<CommitId>, new: CommitId, message: &str, at_ms: u64) -> ReflogEntry {
        ReflogEntry {
            old,
            new,
            message: message.to_owned(),
            at_ms,
        }
    }

    #[tokio::test]
    async fn a_fresh_store_seeds_no_ref_or_worktree() {
        let meta = memory_meta().await;
        let mut refs = 0;
        meta.visit_refs(|_| {
            refs += 1;
            true
        })
        .await
        .unwrap();
        let mut worktrees = 0;
        meta.visit_worktrees(|_| {
            worktrees += 1;
            true
        })
        .await
        .unwrap();
        assert_eq!((refs, worktrees), (0, 0));
    }

    #[tokio::test]
    async fn the_first_commit_creates_heads_branch() {
        let mut meta = initialized_meta().await;
        assert_eq!(
            meta.head(&worktree("main")).await.unwrap(),
            Some(HeadState::Unborn {
                branch: branch_name("main")
            })
        );
        assert_eq!(
            head_log(&meta, "main").await,
            [],
            "an unborn HEAD logs nothing"
        );

        let first = commit_on(&mut meta, "main", 1_000, &[]).await.unwrap();
        assert_eq!(meta.commit_parents(first).await.unwrap(), Vec::new());
        assert_eq!(
            meta.head(&worktree("main")).await.unwrap(),
            Some(HeadState::Attached {
                branch: branch_name("main"),
                commit: first
            })
        );
        let second = commit_on(&mut meta, "main", 2_000, &[first]).await.unwrap();
        assert_eq!(
            meta.lookup_ref(&branch("main")).await.unwrap(),
            Some(CommitRef::new(branch("main"), second))
        );
        let expected = vec![
            entry(Some(first), second, "commit: ", 2_000),
            entry(None, first, "commit (initial): ", 1_000),
        ];
        assert_eq!(ref_log(&meta, "main").await, expected);
        assert_eq!(head_log(&meta, "main").await, expected);
    }

    #[tokio::test]
    async fn a_commit_must_continue_head() {
        let mut meta = initialized_meta().await;
        let root = commit_on(&mut meta, "main", 1_000, &[]).await.unwrap();

        let err = commit_on(&mut meta, "main", 2_000, &[]).await.unwrap_err();
        assert!(matches!(err, StorageError::NotContinuing { base } if base == root));

        // The rejected call wrote nothing.
        assert_eq!(meta.latest_commit().await.unwrap().unwrap().id, root);
        assert_eq!(
            meta.head(&worktree("main"))
                .await
                .unwrap()
                .unwrap()
                .commit(),
            Some(root)
        );
        assert_eq!(ref_log(&meta, "main").await.len(), 1);

        let next = commit_on(&mut meta, "main", 3_000, &[root]).await.unwrap();
        assert_ne!(next, root);
    }

    #[tokio::test]
    async fn a_commit_on_an_unknown_worktree_writes_nothing() {
        let mut meta = initialized_meta().await;
        let err = commit_on(&mut meta, "ghost", 1_000, &[]).await.unwrap_err();
        assert!(matches!(err, StorageError::UnknownWorktree { name } if name == "ghost"));
        assert!(meta.latest_commit().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn a_detached_head_moves_without_a_branch() {
        let mut meta = initialized_meta().await;
        let root = commit_on(&mut meta, "main", 1_000, &[]).await.unwrap();
        let linked = Worktree {
            name: worktree("linked"),
            path: Some("/worlds/linked".to_owned()),
            head: Head::Detached(root),
        };
        meta.add_worktree(&linked, note("add")).await.unwrap();
        assert_eq!(
            head_log(&meta, "linked").await,
            vec![entry(None, root, "add", 5_000)]
        );

        let next = commit_on(&mut meta, "linked", 2_000, &[root])
            .await
            .unwrap();
        assert_eq!(
            meta.head(&worktree("linked")).await.unwrap(),
            Some(HeadState::Detached { commit: next })
        );
        assert_eq!(
            meta.lookup_ref(&branch("main"))
                .await
                .unwrap()
                .unwrap()
                .commit,
            root,
            "no branch moved"
        );
        assert_eq!(head_log(&meta, "linked").await.len(), 2);
    }

    #[tokio::test]
    async fn a_ref_cannot_name_a_missing_commit() {
        let mut meta = memory_meta().await;
        // A stale target is a typed error, not an opaque foreign-key
        // violation, and the ref does not appear.
        let missing = CommitId([0xEE; 32]);
        let err = meta
            .create_ref(&branch("feature"), missing, note("branch: Created from x"))
            .await
            .unwrap_err();
        assert!(matches!(err, StorageError::UnknownCommit { id } if id == missing));
        assert!(meta.lookup_ref(&branch("feature")).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn refs_refuse_directory_and_file_conflicts() {
        let mut meta = initialized_meta().await;
        let root = meta.create_commit(1_000, &[]).await.unwrap();
        meta.create_ref(&branch("a/b"), root, note("x"))
            .await
            .unwrap();

        for (name, existing) in [
            (branch("a"), branch("a/b")),
            (branch("a/b/c"), branch("a/b")),
            // The branch an unborn HEAD names counts as taken.
            (branch("main/x"), branch("main")),
        ] {
            let err = meta.create_ref(&name, root, note("x")).await.unwrap_err();
            assert!(
                matches!(&err, StorageError::RefConflict { existing: found, .. } if *found == existing),
                "{name}: {err}"
            );
        }
        // Names that only share a prefix, or live in another namespace, do
        // not conflict.
        for name in [branch("a-b"), branch("a/bc"), branch("ab"), tag("a/b/c")] {
            meta.create_ref(&name, root, note("x")).await.unwrap();
        }
        assert!(matches!(
            meta.create_ref(&tag("a"), root, note("x")).await,
            Err(StorageError::RefConflict { existing, .. }) if existing == tag("a/b/c")
        ));
        assert!(matches!(
            meta.create_ref(&branch("a/b"), root, note("x")).await,
            Err(StorageError::RefExists { name }) if name == branch("a/b")
        ));
    }

    #[tokio::test]
    async fn a_checked_out_branch_is_never_deleted_or_moved() {
        let mut meta = initialized_meta().await;
        let first = commit_on(&mut meta, "main", 1_000, &[]).await.unwrap();
        let second = commit_on(&mut meta, "main", 2_000, &[first]).await.unwrap();

        assert!(matches!(
            meta.delete_ref(&branch("main")).await,
            Err(StorageError::BranchCheckedOut { branch: held, worktree })
                if held == branch("main") && worktree == "main"
        ));
        assert!(matches!(
            meta.move_ref(&branch("main"), first, note("branch: Reset to x"))
                .await,
            Err(StorageError::BranchCheckedOut { .. })
        ));
        assert_eq!(
            meta.lookup_ref(&branch("main"))
                .await
                .unwrap()
                .unwrap()
                .commit,
            second
        );

        // Once HEAD leaves it, the branch moves, and only its own reflog
        // records the move.
        meta.set_head(&worktree("main"), &Head::Detached(second), note("detach"))
            .await
            .unwrap();
        let old = meta
            .move_ref(&branch("main"), first, note("reset"))
            .await
            .unwrap();
        assert_eq!(old, second);
        assert_eq!(
            ref_log(&meta, "main").await[0],
            entry(Some(second), first, "reset", 5_000)
        );
        assert_eq!(
            head_log(&meta, "main").await[0],
            entry(Some(second), second, "detach", 5_000)
        );
    }

    #[tokio::test]
    async fn a_branch_is_checked_out_by_at_most_one_worktree() {
        let mut meta = initialized_meta().await;
        let linked = Worktree {
            name: worktree("linked"),
            path: Some("/worlds/linked".to_owned()),
            head: Head::Branch(branch_name("main")),
        };
        // Unborn counts too.
        assert!(matches!(
            meta.add_worktree(&linked, note("add")).await,
            Err(StorageError::BranchCheckedOut { worktree, .. }) if worktree == "main"
        ));
        let linked = Worktree {
            head: Head::Branch(branch_name("other")),
            ..linked
        };
        meta.add_worktree(&linked, note("add")).await.unwrap();
        assert!(matches!(
            meta.set_head(
                &worktree("linked"),
                &Head::Branch(branch_name("main")),
                note("x")
            )
            .await,
            Err(StorageError::BranchCheckedOut { .. })
        ));
        // Checking out the branch a worktree is already on is a no-op move.
        meta.set_head(
            &worktree("main"),
            &Head::Branch(branch_name("main")),
            note("x"),
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn checkout_logs_only_a_head_left_at_a_commit() {
        let mut meta = initialized_meta().await;
        let root = commit_on(&mut meta, "main", 1_000, &[]).await.unwrap();
        meta.create_ref(&tag("v1"), root, note("unused"))
            .await
            .unwrap();

        meta.set_head(&worktree("main"), &Head::Detached(root), note("detach"))
            .await
            .unwrap();
        meta.set_head(
            &worktree("main"),
            &Head::Branch(branch_name("orphan")),
            note("orphan"),
        )
        .await
        .unwrap();
        meta.set_head(
            &worktree("main"),
            &Head::Branch(branch_name("main")),
            note("back"),
        )
        .await
        .unwrap();
        let messages: Vec<String> = head_log(&meta, "main")
            .await
            .into_iter()
            .map(|entry| entry.message)
            .collect();
        assert_eq!(messages, ["back", "detach", "commit (initial): "]);
        assert_eq!(
            head_log(&meta, "main").await[0],
            entry(None, root, "back", 5_000),
            "leaving an unborn HEAD has no old commit"
        );
        assert!(
            meta.visit_ref_log(&branch_name("v1"), |_| panic!("tags are not logged"))
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn renaming_a_branch_carries_heads_and_reflog() {
        let mut meta = initialized_meta().await;
        let root = commit_on(&mut meta, "main", 1_000, &[]).await.unwrap();

        meta.rename_branch(
            &branch_name("main"),
            &branch_name("trunk"),
            false,
            note("renamed"),
        )
        .await
        .unwrap();
        assert!(meta.lookup_ref(&branch("main")).await.unwrap().is_none());
        assert_eq!(
            meta.head(&worktree("main")).await.unwrap(),
            Some(HeadState::Attached {
                branch: branch_name("trunk"),
                commit: root
            })
        );
        assert_eq!(ref_log(&meta, "main").await, []);
        let expected = vec![
            entry(Some(root), root, "renamed", 5_000),
            entry(None, root, "commit (initial): ", 1_000),
        ];
        assert_eq!(ref_log(&meta, "trunk").await, expected);
        assert_eq!(head_log(&meta, "main").await, expected);

        // A branch may move below its own old name.
        meta.rename_branch(
            &branch_name("trunk"),
            &branch_name("trunk/x"),
            false,
            note("deeper"),
        )
        .await
        .unwrap();
        assert_eq!(ref_log(&meta, "trunk/x").await.len(), 3);
    }

    #[tokio::test]
    async fn a_head_move_does_not_strand_a_nested_branch() {
        let mut meta = initialized_meta().await;
        let root = meta.create_commit(1_000, &[]).await.unwrap();
        meta.create_ref(&branch("other"), root, note("x"))
            .await
            .unwrap();
        meta.set_head(
            &worktree("main"),
            &Head::Branch(branch_name("other")),
            note("checkout"),
        )
        .await
        .unwrap();
        meta.create_ref(&branch("main/x"), root, note("x"))
            .await
            .unwrap();
        meta.set_head(
            &worktree("main"),
            &Head::Branch(branch_name("main/x")),
            note("checkout"),
        )
        .await
        .unwrap();
        // `main/x` is born and stays in `refs`: HEAD may not move onto the
        // unborn `main`, which would shadow it.
        let err = meta
            .set_head(
                &worktree("main"),
                &Head::Branch(branch_name("main")),
                note("checkout"),
            )
            .await
            .unwrap_err();
        assert!(
            matches!(&err, StorageError::RefConflict { name, existing }
                if *name == branch("main") && *existing == branch("main/x")),
            "{err}"
        );
        // A re-checkout of the branch HEAD holds is a no-op.
        meta.set_head(
            &worktree("main"),
            &Head::Branch(branch_name("main/x")),
            note("checkout"),
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn a_head_moves_onto_and_off_an_unborn_branch() {
        let mut meta = initialized_meta().await;
        // HEAD starts unborn on `main`: moving it onto an unborn `main/x`
        // must not trip the conflict scan over the unborn `main`.
        meta.set_head(
            &worktree("main"),
            &Head::Branch(branch_name("main/x")),
            note("checkout"),
        )
        .await
        .unwrap();
        // And back onto the unborn `main`.
        meta.set_head(
            &worktree("main"),
            &Head::Branch(branch_name("main")),
            note("checkout"),
        )
        .await
        .unwrap();
        // Re-checking out the unborn `main` is a no-op.
        meta.set_head(
            &worktree("main"),
            &Head::Branch(branch_name("main")),
            note("checkout"),
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn a_corrupt_worktree_head_row_fails_loudly() {
        let mut meta = memory_meta().await;
        let root = meta.create_commit(1_000, &[]).await.unwrap();
        // Drop the CHECK that keeps exactly one of the two HEAD columns
        // set, the way corruption would.
        for sql in [
            "PRAGMA legacy_alter_table = ON",
            "DROP INDEX idx_worktrees_main",
            "DROP INDEX idx_worktrees_head_ref",
            "ALTER TABLE worktrees RENAME TO worktrees_no_check",
            "CREATE TABLE worktrees(name TEXT NOT NULL COLLATE BINARY PRIMARY KEY, path TEXT NULL UNIQUE, head_ref TEXT NULL CHECK (head_ref GLOB 'refs/heads/?*'), head_commit BLOB NULL REFERENCES commits(id)) WITHOUT ROWID",
            "INSERT INTO worktrees SELECT * FROM worktrees_no_check",
            "DROP TABLE worktrees_no_check",
            "PRAGMA legacy_alter_table = OFF",
        ] {
            sqlx::query(sql).execute(&meta.pool).await.unwrap();
        }
        // `w` holds neither HEAD column; `w2` holds both.
        sqlx::query(
            "INSERT INTO worktrees (name, path, head_ref, head_commit) \
             VALUES ('w', NULL, NULL, NULL), ('w2', NULL, 'refs/heads/main', ?)",
        )
        .bind(commit_param(root))
        .execute(&meta.pool)
        .await
        .unwrap();
        for name in ["w", "w2"] {
            let err = meta.lookup_worktree(&worktree(name)).await.unwrap_err();
            assert!(
                matches!(&err, StorageError::CorruptWorktreeHead { name: stored } if stored == name),
                "{err}"
            );
        }
    }

    #[tokio::test]
    async fn renaming_an_unborn_branch_rewrites_only_head() {
        let mut meta = initialized_meta().await;
        meta.rename_branch(
            &branch_name("main"),
            &branch_name("trunk"),
            false,
            note("renamed"),
        )
        .await
        .unwrap();
        assert_eq!(
            meta.head(&worktree("main")).await.unwrap(),
            Some(HeadState::Unborn {
                branch: branch_name("trunk")
            })
        );
        assert_eq!(head_log(&meta, "main").await, []);
        assert!(matches!(
            meta.rename_branch(&branch_name("main"), &branch_name("x"), false, note("x")).await,
            Err(StorageError::UnknownRef { name }) if name == branch("main")
        ));
    }

    #[tokio::test]
    async fn renaming_refuses_taken_and_conflicting_names() {
        let mut meta = initialized_meta().await;
        let root = meta.create_commit(1_000, &[]).await.unwrap();
        meta.create_ref(&branch("a"), root, note("x"))
            .await
            .unwrap();
        meta.create_ref(&branch("b/c"), root, note("x"))
            .await
            .unwrap();

        assert!(matches!(
            meta.rename_branch(&branch_name("a"), &branch_name("b/c"), false, note("x"))
                .await,
            Err(StorageError::RefExists { .. })
        ));
        assert!(matches!(
            meta.rename_branch(&branch_name("a"), &branch_name("b"), false, note("x"))
                .await,
            Err(StorageError::RefConflict { .. })
        ));
        assert!(matches!(
            meta.rename_branch(&branch_name("a"), &branch_name("main"), false, note("x"))
                .await,
            Err(StorageError::BranchCheckedOut { .. })
        ));
        assert!(meta.lookup_ref(&branch("a")).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn forcing_a_rename_replaces_the_target() {
        let mut meta = initialized_meta().await;
        let root = commit_on(&mut meta, "main", 1_000, &[]).await.unwrap();
        let next = meta.create_commit(2_000, &[root]).await.unwrap();
        meta.create_ref(&branch("old"), next, note("created old"))
            .await
            .unwrap();

        // HEAD's branch replaces `old`, whose reflog goes with it.
        meta.rename_branch(
            &branch_name("main"),
            &branch_name("old"),
            true,
            note("forced"),
        )
        .await
        .unwrap();
        assert_eq!(
            meta.lookup_ref(&branch("old")).await.unwrap(),
            Some(CommitRef::new(branch("old"), root))
        );
        let expected = vec![
            entry(Some(root), root, "forced", 5_000),
            entry(None, root, "commit (initial): ", 1_000),
        ];
        assert_eq!(ref_log(&meta, "old").await, expected);
        assert_eq!(head_log(&meta, "main").await, expected);

        // A branch checked out elsewhere is never replaced.
        meta.create_ref(&branch("side"), next, note("x"))
            .await
            .unwrap();
        assert!(matches!(
            meta.rename_branch(&branch_name("side"), &branch_name("old"), true, note("x"))
                .await,
            Err(StorageError::BranchCheckedOut { .. })
        ));
    }

    #[tokio::test]
    async fn forcing_an_unborn_rename_attaches_head_to_the_target() {
        let mut meta = initialized_meta().await;
        let root = meta.create_commit(1_000, &[]).await.unwrap();
        meta.create_ref(&branch("x"), root, note("created x"))
            .await
            .unwrap();

        meta.rename_branch(
            &branch_name("main"),
            &branch_name("x"),
            true,
            note("forced"),
        )
        .await
        .unwrap();
        assert_eq!(
            meta.head(&worktree("main")).await.unwrap(),
            Some(HeadState::Attached {
                branch: branch_name("x"),
                commit: root
            })
        );
        assert_eq!(
            head_log(&meta, "main").await,
            [entry(None, root, "forced", 5_000)]
        );
        assert_eq!(
            ref_log(&meta, "x").await,
            [entry(None, root, "created x", 5_000)]
        );
    }

    #[tokio::test]
    async fn renaming_a_branch_to_itself_only_logs() {
        let mut meta = initialized_meta().await;
        let root = commit_on(&mut meta, "main", 1_000, &[]).await.unwrap();
        meta.rename_branch(
            &branch_name("main"),
            &branch_name("main"),
            false,
            note("same"),
        )
        .await
        .unwrap();
        assert_eq!(
            ref_log(&meta, "main").await[0],
            entry(Some(root), root, "same", 5_000)
        );
        assert_eq!(
            head_log(&meta, "main").await[0],
            entry(Some(root), root, "same", 5_000)
        );
    }

    #[tokio::test]
    async fn deleting_a_branch_drops_its_reflog() {
        let mut meta = initialized_meta().await;
        let root = meta.create_commit(1_000, &[]).await.unwrap();
        meta.create_ref(&branch("feature"), root, note("created"))
            .await
            .unwrap();
        assert_eq!(ref_log(&meta, "feature").await.len(), 1);

        assert_eq!(
            meta.delete_ref(&branch("feature")).await.unwrap(),
            Some(root)
        );
        assert_eq!(meta.delete_ref(&branch("feature")).await.unwrap(), None);
        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM ref_log")
            .fetch_one(&meta.pool)
            .await
            .unwrap();
        assert_eq!(rows, 0);
    }

    #[tokio::test]
    async fn worktrees_keep_one_main_and_unique_paths() {
        let mut meta = initialized_meta().await;
        let root = meta.create_commit(1_000, &[]).await.unwrap();
        let linked = |name: &str, path: Option<&str>| Worktree {
            name: worktree(name),
            path: path.map(str::to_owned),
            head: Head::Detached(root),
        };
        assert!(matches!(
            meta.add_worktree(&linked("second", None), note("x")).await,
            Err(StorageError::MainWorktreeExists)
        ));
        assert!(matches!(
            meta.add_worktree(&linked("main", Some("/w/main")), note("x"))
                .await,
            Err(StorageError::WorktreeExists { .. })
        ));
        meta.add_worktree(&linked("a", Some("/w/a")), note("x"))
            .await
            .unwrap();
        meta.add_worktree(&linked("b", Some("/w/b")), note("x"))
            .await
            .unwrap();
        assert!(matches!(
            meta.add_worktree(&linked("c", Some("/w/a")), note("x"))
                .await,
            Err(StorageError::WorktreePathTaken { .. })
        ));
        assert!(matches!(
            meta.remove_worktree(&worktree("main")).await,
            Err(StorageError::MainWorktreeProtected { .. })
        ));
        assert!(meta.remove_worktree(&worktree("a")).await.unwrap());
        assert!(!meta.remove_worktree(&worktree("a")).await.unwrap());
        let logs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM head_log WHERE worktree = 'a'")
            .fetch_one(&meta.pool)
            .await
            .unwrap();
        assert_eq!(logs, 0, "the HEAD reflog goes with its worktree");

        let mut names = Vec::new();
        meta.visit_worktrees(|worktree| {
            names.push(worktree.name.to_string());
            true
        })
        .await
        .unwrap();
        assert_eq!(names, ["b", "main"]);
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
    async fn a_corrupt_ref_row_fails_loudly() {
        let meta = memory_meta().await;
        let mut meta = meta;
        let root = meta.create_commit(1_000, &[]).await.unwrap();
        // Names are re-validated on decode: a row the validator no longer
        // accepts is corruption, never a new ref name.
        sqlx::query("INSERT INTO refs (name, commit_id) VALUES ('refs/heads/bad name', ?)")
            .bind(commit_param(root))
            .execute(&meta.pool)
            .await
            .unwrap();
        let err = meta.visit_refs(|_| true).await.unwrap_err();
        assert!(
            matches!(err, StorageError::InvalidRefName { name } if name == "refs/heads/bad name")
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
    async fn version_gate_rejects_foreign_schema() {
        let (dir, _guard) = tempdir("version");
        let path = dir.join("meta.sqlite");
        let mut seed = SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(true)
            .connect()
            .await
            .unwrap();
        sqlx::query(SCHEMA).execute(&mut seed).await.unwrap();
        sqlx::query("PRAGMA user_version=999")
            .execute(&mut seed)
            .await
            .unwrap();
        seed.close().await.unwrap();
        assert!(matches!(
            SqliteMeta::open(&path).await,
            Err(StorageError::UnsupportedSchema {
                found: 999,
                supported: SCHEMA_VERSION,
            })
        ));
    }

    /// The version gate rejects any store whose `user_version` is not
    /// `SCHEMA_VERSION` (18) before any table is touched. A store from the
    /// immediately previous version (17: one active ref, no worktrees) is
    /// refused with `UnsupportedSchema { found: 17, supported:
    /// SCHEMA_VERSION }`, never opened and failed on the first query.
    #[tokio::test]
    async fn a_store_from_the_previous_schema_version_is_refused() {
        let (dir, _guard) = tempdir("version-17");
        let path = dir.join("meta.sqlite");
        let mut seed = SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(true)
            .connect()
            .await
            .unwrap();
        // The schema 17 refs table as shipped: an active flag, no worktrees.
        sqlx::query(
            "CREATE TABLE refs(
                name TEXT NOT NULL COLLATE BINARY PRIMARY KEY,
                commit_id BLOB NULL,
                active INTEGER NOT NULL DEFAULT 0 CHECK (active IN (0, 1)),
                created_at_ms INTEGER NOT NULL
            );
            INSERT INTO refs (name, commit_id, active, created_at_ms)
                VALUES ('main', NULL, 1, 0);",
        )
        .execute(&mut seed)
        .await
        .unwrap();
        sqlx::query("PRAGMA user_version=17")
            .execute(&mut seed)
            .await
            .unwrap();
        seed.close().await.unwrap();
        assert!(matches!(
            SqliteMeta::open(&path).await,
            Err(StorageError::UnsupportedSchema {
                found: 17,
                supported: SCHEMA_VERSION,
            })
        ));
    }

    #[tokio::test]
    async fn commit_rows_store_canonical_tree_bytes() {
        use sekai_core::{ChunkHash, Dimension, RegionKind};
        let mut meta = initialized_meta().await;
        let coord = ChunkCoord::new(Dimension::OVERWORLD, RegionKind::Region, 5, 5);
        let id = meta
            .apply_commit_incremental(
                &worktree("main"),
                1_000,
                &[],
                &[sekai_core::CommitEntry::new(
                    coord,
                    Some(ChunkHash::new([1; 32])),
                )],
                note("commit (initial): "),
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
        use sekai_core::{ChunkHash, RegionKind};
        let mut meta = initialized_meta().await;
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
            .apply_commit_incremental(&worktree("main"), 1_000, &[], &entries, note("x"))
            .await
            .unwrap()
            .id;

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
