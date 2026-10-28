//! SQLite commit metadata: the history graph and per-chunk history.
//!
//! Recording a commit uses one transaction; CAS durability is established
//! before the metadata commit by the caller. The pool has one connection.

use std::collections::{BTreeMap, BTreeSet, btree_map::Entry};
use std::path::Path;
use std::time::{Duration, Instant};

use sekai_core::{
    ApplyOutcome, BlobHash, ChunkCoord, ChunkHistoryEntry, Commit, CommitEntry, CommitId,
    CommitRef, DEFAULT_REF_NAME, DiffHash, Dimension, FoldOutcome, MetaStore, NamedScope,
    NamedScopeError, Rect, RefName, RegionKind, ScopeName, commit_hash,
};
use sqlx::Row as _;
use sqlx::sqlite::{
    SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions, SqliteSynchronous,
};

use sekai_storage::{
    BackendKind, FileCas, SekaiStoreOpener, StorageError, Store, StoreLockMode, io_error,
    parse_backend_url,
};

/// Managed schema version (`PRAGMA user_version`).
pub const SCHEMA_VERSION: i64 = 14;

const SCHEMA: &str = include_str!("../schema/sqlite.sql");

/// Map a backend failure into the shared storage error.
fn db_err(source: sqlx::Error) -> StorageError {
    StorageError::backend("sqlite", source)
}

/// SQLite-backed commit metadata: the history graph and per-chunk rows.
#[derive(Debug, Clone)]
pub struct SqliteMeta {
    pool: SqlitePool,
}

/// Opened SQLite store: metadata plus file CAS under one root.
#[derive(Debug, Clone)]
pub struct SqliteStore {
    inner: Store<SqliteMeta, FileCas>,
}

impl SqliteStore {
    /// Compose a store from an already opened metadata adapter and file CAS.
    pub const fn new(meta: SqliteMeta, cas: FileCas) -> Self {
        Self {
            inner: Store::new(meta, cas),
        }
    }

    /// Metadata adapter.
    pub const fn meta(&self) -> &SqliteMeta {
        self.inner.meta()
    }

    /// Mutable metadata adapter.
    pub const fn meta_mut(&mut self) -> &mut SqliteMeta {
        self.inner.meta_mut()
    }

    /// Blob adapter.
    pub const fn cas(&self) -> &FileCas {
        self.inner.cas()
    }

    /// Mutable blob adapter.
    pub const fn cas_mut(&mut self) -> &mut FileCas {
        self.inner.cas_mut()
    }

    /// Borrow both adapters for operations such as GC.
    pub const fn cas_and_meta(&mut self) -> (&mut FileCas, &SqliteMeta) {
        self.inner.cas_and_meta()
    }
}

impl sekai_storage::SekaiStore for SqliteStore {
    type Meta = SqliteMeta;
    type Blobs = FileCas;
    type LockGuard = sekai_storage::StoreLock;

    fn meta(&self) -> &Self::Meta {
        Self::meta(self)
    }

    fn meta_mut(&mut self) -> &mut Self::Meta {
        Self::meta_mut(self)
    }

    fn cas(&self) -> &Self::Blobs {
        Self::cas(self)
    }

    fn cas_mut(&mut self) -> &mut Self::Blobs {
        Self::cas_mut(self)
    }

    fn cas_and_meta(&mut self) -> (&mut Self::Blobs, &Self::Meta) {
        Self::cas_and_meta(self)
    }

    fn lock(&self, mode: StoreLockMode) -> Result<Self::LockGuard, StorageError> {
        self.inner.lock(mode)
    }
}

/// Initialize the store rooted at `root`, creating it when missing.
///
/// The layout is `<root>/meta.sqlite` plus `<root>/blobs/`; initializing an
/// existing store again is idempotent. This is the only path that
/// materializes a store, reserved for first-time store creation. It takes no
/// store lock:
/// schema creation serializes inside SQLite (see [`SqliteMeta::open`]), so
/// concurrent first initializations all succeed.
pub async fn init_sqlite(root: impl AsRef<Path>) -> Result<SqliteStore, StorageError> {
    std::fs::create_dir_all(root.as_ref()).map_err(|source| io_error(root.as_ref(), source))?;
    connect_sqlite(root).await
}

/// Open the existing store rooted at `root`.
///
/// The layout is `<root>/meta.sqlite` plus `<root>/blobs/`. Fails with
/// [`StorageError::StoreMissing`] when the store was never initialized,
/// without creating anything.
pub async fn open_sqlite(root: impl AsRef<Path>) -> Result<SqliteStore, StorageError> {
    if !root.as_ref().join("meta.sqlite").is_file() {
        return Err(StorageError::StoreMissing {
            path: root.as_ref().to_path_buf(),
        });
    }
    connect_sqlite(root).await
}

/// Attach CAS and metadata to an existing store root; the root directory must
/// already hold `meta.sqlite`.
async fn connect_sqlite(root: impl AsRef<Path>) -> Result<SqliteStore, StorageError> {
    let cas = FileCas::open(root.as_ref())?;
    let meta = SqliteMeta::open(&root.as_ref().join("meta.sqlite")).await?;
    Ok(SqliteStore::new(meta, cas))
}

impl SekaiStoreOpener for SqliteStore {
    async fn open(url: &str) -> Result<Self, StorageError> {
        let (kind, rest) = parse_backend_url(url)?;
        if kind != BackendKind::Sqlite {
            return Err(StorageError::UnsupportedBackend {
                url: url.to_owned(),
            });
        }
        open_sqlite(rest).await
    }

    async fn init(url: &str) -> Result<Self, StorageError> {
        let (kind, rest) = parse_backend_url(url)?;
        if kind != BackendKind::Sqlite {
            return Err(StorageError::UnsupportedBackend {
                url: url.to_owned(),
            });
        }
        init_sqlite(rest).await
    }
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
                sqlx::query(SCHEMA)
                    .execute(&mut *tx)
                    .await
                    .map_err(db_err)?;
                // The store starts with one active ref, unborn: new
                // commits hang from it until the first one lands.
                sqlx::query(
                    "INSERT INTO refs (name, commit_id, active, created_at_ms)
                     VALUES (?, NULL, 1, ?)",
                )
                .bind(DEFAULT_REF_NAME)
                .bind(now_ms()?)
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

    /// Record a whole commit atomically; returns its new ID.
    ///
    /// Blobs must already be flushed to CAS before calling.
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

/// Read a region-kind column, rejecting codes the enum cannot represent.
fn kind_col(
    row: &sqlx::sqlite::SqliteRow,
    column: &'static str,
) -> Result<RegionKind, StorageError> {
    let value = i32_col(row, column)?;
    RegionKind::new(value).map_err(|_| StorageError::UnknownRegionKind { kind: value })
}

/// Read a 32-byte hash column.
fn hash32(bytes: Vec<u8>) -> Result<[u8; 32], StorageError> {
    bytes
        .try_into()
        .map_err(|bytes: Vec<u8>| StorageError::InvalidHashLength { len: bytes.len() })
}

/// Read an optional `BlobHash` from a 32-byte hash column.
fn hash_col(
    row: &sqlx::sqlite::SqliteRow,
    column: &'static str,
) -> Result<Option<BlobHash>, StorageError> {
    let bytes: Option<Vec<u8>> = row.try_get(column).map_err(db_err)?;
    bytes.map(|bytes| hash32(bytes).map(BlobHash)).transpose()
}

/// Read a millisecond timestamp column.
fn millis_col(row: &sqlx::sqlite::SqliteRow, column: &'static str) -> Result<u64, StorageError> {
    let value: i64 = row.try_get(column).map_err(db_err)?;
    i64_to_u64(value, column)
}

/// Decode one `chunk_history` row.
fn decode_history(row: &sqlx::sqlite::SqliteRow) -> Result<ChunkHistoryEntry, StorageError> {
    let coord = ChunkCoord::new(
        Dimension::new(i32_col(row, "dim")?),
        kind_col(row, "kind")?,
        i32_col(row, "cx")?,
        i32_col(row, "cz")?,
    );
    let blob = hash_col(row, "blob")?;
    let diff_bytes: Option<Vec<u8>> = row.try_get("diff").map_err(db_err)?;
    let diff = diff_bytes
        .map(|bytes| hash32(bytes).map(DiffHash))
        .transpose()?;
    let commit_raw: Vec<u8> = row.try_get("commit_id").map_err(db_err)?;
    let commit = commit_id_from_blob(commit_raw)?;
    Ok(ChunkHistoryEntry::new(coord, commit, blob, diff))
}

/// Decode one `refs` row. Stored names always re-validate: anything
/// else is corruption, never silent reinterpretation.
fn decode_ref(row: &sqlx::sqlite::SqliteRow) -> Result<CommitRef, StorageError> {
    let name_raw: String = row.try_get("name").map_err(db_err)?;
    let name =
        RefName::parse(&name_raw).map_err(|_| StorageError::InvalidRefName { name: name_raw })?;
    let commit_raw: Option<Vec<u8>> = row.try_get("commit_id").map_err(db_err)?;
    let commit = commit_raw.map(commit_id_from_blob).transpose()?;
    let active_raw: i64 = row.try_get("active").map_err(db_err)?;
    let active = match active_raw {
        0 => false,
        1 => true,
        value => {
            return Err(StorageError::InvalidHistoryValue {
                column: "active",
                value,
            });
        }
    };
    Ok(CommitRef::new(
        name,
        commit,
        active,
        millis_col(row, "created_at_ms")?,
    ))
}

/// `scopes.kind` mask with every region family set; the column stores
/// that selection as NULL, never as this value.
const ALL_KINDS_MASK: i64 = kind_mask(&RegionKind::ALL);

// The `scopes.kind` CHECK in `schema/sqlite.sql` hardcodes the proper
// subsets as `1..=6`; `the_kind_check_admits_proper_subsets_only` pins it
// to this mask.
const _: () = assert!(
    ALL_KINDS_MASK == 7,
    "a new region kind must widen the `scopes.kind` CHECK in a schema migration"
);

/// `scopes.kind` mask: bit `1 << RegionKind::raw()` per selected family.
///
/// A loop rather than an iterator fold so [`ALL_KINDS_MASK`] derives from
/// `RegionKind::ALL` at compile time.
const fn kind_mask(kinds: &[RegionKind]) -> i64 {
    let mut mask = 0;
    let mut i = 0;
    while i < kinds.len() {
        mask |= 1 << kinds[i].raw();
        i += 1;
    }
    mask
}

/// Decode a non-NULL `scopes.kind` mask. Only proper, non-empty subsets
/// are stored; anything else is corruption.
fn kinds_from_mask(mask: i64) -> Result<Vec<RegionKind>, StorageError> {
    if !(1..ALL_KINDS_MASK).contains(&mask) {
        return Err(StorageError::InvalidHistoryValue {
            column: "scopes.kind",
            value: mask,
        });
    }
    Ok(RegionKind::ALL
        .into_iter()
        .filter(|kind| mask & 1 << kind.raw() != 0)
        .collect())
}

/// One scope's columns, gathered from its joined area rows.
struct ScopeColumns {
    kind: Option<i64>,
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
                kind: row.try_get("kind").map_err(db_err)?,
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
    let kinds = columns.kind.map(kinds_from_mask).transpose()?;
    NamedScope::new(parsed, kinds, columns.areas, columns.created_at_ms).map_err(|err| match err {
        NamedScopeError::NoAreas => StorageError::EmptyScope { name },
        // `kinds_from_mask` never yields an empty set.
        NamedScopeError::NoKinds => StorageError::InvalidHistoryValue {
            column: "scopes.kind",
            value: 0,
        },
    })
}

/// Every scope with its areas, one row per area. A single statement reads
/// one snapshot, so a concurrent replacement is seen whole or not at all.
const SCOPE_QUERY: &str = "SELECT scopes.name, scopes.kind, scopes.created_at_ms,
            scope_areas.dim, scope_areas.x0, scope_areas.z0, scope_areas.x1, scope_areas.z1
     FROM scopes LEFT JOIN scope_areas ON scope_areas.scope = scopes.name";

/// `CommitId` as a BLOB bind parameter.
fn commit_param(id: CommitId) -> Vec<u8> {
    id.0.to_vec()
}

/// Millis as an `INTEGER` parameter. Genuinely fallible: callers compute the
/// clock, so an absurd value is their bug, surfaced loudly.
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

/// Shared `chunk_history` insert used by single-row and batch commits.
const INSERT_CHUNK_SQL: &str =
    "INSERT INTO chunk_history (commit_id, seq, dim, kind, cx, cz, blob, diff)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)";

/// Ancestors of a commit: the commit itself plus every commit
/// reachable through parent links.
///
/// The recursive term walks the history graph upwards, so the set
/// answers "what does this commit inherit?" without relying on the
/// local record order. `UNION` (not `UNION ALL`) keeps the walk finite even
/// if the graph ever holds a cycle. The seed parameter is the commit
/// the walk starts at; `head`/`tail` split it around the seed for the
/// queries that build their SQL piece by piece.
macro_rules! ancestor_cte {
    (head) => {
        "WITH RECURSIVE ancestors(id) AS (
    SELECT "
    };
    (tail) => {
        "
    UNION
    SELECT sp.parent_id FROM commit_parents sp
    JOIN ancestors ON sp.child_id = ancestors.id
)"
    };
    ($seed:literal) => {
        concat!(ancestor_cte!(head), $seed, ancestor_cte!(tail))
    };
}

/// Effective row of one coordinate at a commit: the nearest row
/// introduced at the commit or at one of its ancestors.
const LOOKUP_CHUNK_SQL: &str = concat!(
    ancestor_cte!("?"),
    "
SELECT commit_id, dim, kind, cx, cz, blob, diff FROM chunk_history
WHERE dim = ? AND kind = ? AND cx = ? AND cz = ?
  AND commit_id IN (SELECT id FROM ancestors)
ORDER BY seq DESC LIMIT 1"
);

/// Effective rows of every coordinate at a commit.
const VISIT_CHUNKS_SQL: &str = concat!(
    ancestor_cte!("?"),
    "
SELECT dim, kind, cx, cz, blob, diff, commit_id FROM (
   SELECT dim, kind, cx, cz, blob, diff, commit_id,
     ROW_NUMBER() OVER (
       PARTITION BY dim, kind, cx, cz ORDER BY seq DESC
     ) AS rn
   FROM chunk_history WHERE commit_id IN (SELECT id FROM ancestors)
) WHERE rn = 1 ORDER BY dim, kind, cx, cz"
);

/// Rows of a retired commit that the history of its successor already
/// supersedes. `?1` is the retired commit, `?2` the successor, `?3` the
/// retired commit's local sequence.
///
/// Soundness depends on the retired commit preceding every superseding
/// row in local *record* order: a commit must be recorded before any of
/// its successors' rows are. Prune always retires an older commit into a
/// newer (larger-sequence) descendant, so the invariant holds for a
/// single store; a store whose sequence order diverges from graph order
/// could drop surviving rows here.
const RETIRE_SUPERSEDED_SQL: &str = concat!(
    ancestor_cte!("?2"),
    "
DELETE FROM chunk_history WHERE commit_id = ?1
 AND (dim, kind, cx, cz) IN (
     SELECT dim, kind, cx, cz FROM chunk_history
     WHERE seq > ?3 AND commit_id IN (SELECT id FROM ancestors)
 )"
);

/// Whether one commit descends from another: the ancestor set of the
/// successor (`?1`) contains the retired commit (`?2`).
const DESCENDS_SQL: &str = concat!(
    ancestor_cte!("?1"),
    "SELECT COUNT(*) FROM ancestors WHERE id = ?2"
);

/// Decode one `commits` row.
fn decode_commit(
    row: &sqlx::sqlite::SqliteRow,
    parents: &[CommitId],
) -> Result<Commit, StorageError> {
    let id: Vec<u8> = row.try_get("id").map_err(db_err)?;
    Ok(Commit::new(
        commit_id_from_blob(id)?,
        millis_col(row, "created_at_ms")?,
        parents.to_vec(),
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
    let inserted = sqlx::query("INSERT OR IGNORE INTO commits (id, created_at_ms) VALUES (?, ?)")
        .bind(commit_param(id))
        .bind(millis_param(created_at_ms)?)
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

impl sekai_core::MetaStore for SqliteMeta {
    type Error = StorageError;

    async fn create_commit(
        &mut self,
        created_at_ms: u64,
        parents: &[CommitId],
    ) -> Result<CommitId, StorageError> {
        let id = commit_hash(parents, created_at_ms, &[]);
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        insert_commit(&mut tx, id, created_at_ms, parents).await?;
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

    async fn record_chunk(
        &mut self,
        commit: CommitId,
        coord: &ChunkCoord,
        blob: Option<&BlobHash>,
        diff: Option<&DiffHash>,
    ) -> Result<(), StorageError> {
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        let seq: i64 = sqlx::query_scalar("SELECT seq FROM commits WHERE id = ?")
            .bind(commit_param(commit))
            .fetch_optional(&mut *tx)
            .await
            .map_err(db_err)?
            .ok_or(StorageError::UnknownCommit { id: commit })?;
        sqlx::query(INSERT_CHUNK_SQL)
            .bind(commit_param(commit))
            .bind(seq)
            .bind(i64::from(coord.dim.raw()))
            .bind(i64::from(coord.kind.raw()))
            .bind(i64::from(coord.x))
            .bind(i64::from(coord.z))
            .bind(blob.map(|h| &h.0[..]))
            .bind(diff.map(|h| &h.0[..]))
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
        tx.commit().await.map_err(db_err)?;
        Ok(())
    }

    async fn lookup_chunk(
        &self,
        commit: CommitId,
        coord: &ChunkCoord,
    ) -> Result<Option<ChunkHistoryEntry>, StorageError> {
        // The nearest row introduced at the commit or at one of its
        // ancestors: unchanged chunks have no row at newer commits
        // (delta storage). The coord index serves the point lookup.
        let row = sqlx::query(LOOKUP_CHUNK_SQL)
            .bind(commit_param(commit))
            .bind(i64::from(coord.dim.raw()))
            .bind(i64::from(coord.kind.raw()))
            .bind(i64::from(coord.x))
            .bind(i64::from(coord.z))
            .fetch_optional(&self.pool)
            .await
            .map_err(db_err)?;
        row.map(|row| decode_history(&row)).transpose()
    }

    async fn lookup_commit(&self, id: CommitId) -> Result<Option<Commit>, StorageError> {
        let row = sqlx::query("SELECT id, created_at_ms FROM commits WHERE id = ?")
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
        let row = sqlx::query("SELECT id, created_at_ms FROM commits ORDER BY seq DESC LIMIT 1")
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

    async fn visit_commit_chunks<F>(
        &self,
        commit: CommitId,
        mut visit: F,
    ) -> Result<(), StorageError>
    where
        F: FnMut(&ChunkHistoryEntry) -> bool + Send,
    {
        // Effective rows at `commit`: the newest row introduced at it or
        // at one of its ancestors, tombstones included. Window functions
        // keep the query portable across backends. A single fetch beats
        // paged window queries (which recompute the aggregation per page),
        // and every caller materializes the full effective set anyway
        // (checkout groups, GC sets, plan universes).
        let rows = sqlx::query(VISIT_CHUNKS_SQL)
            .bind(commit_param(commit))
            .fetch_all(&self.pool)
            .await
            .map_err(db_err)?;
        for row in &rows {
            if !visit(&decode_history(row)?) {
                break;
            }
        }
        Ok(())
    }

    async fn visit_commits<F>(&self, mut visit: F) -> Result<(), StorageError>
    where
        F: FnMut(&Commit) -> bool + Send,
    {
        let rows = sqlx::query("SELECT id, created_at_ms FROM commits ORDER BY seq")
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

    async fn visit_fresh_rows<F>(&self, commit: CommitId, mut visit: F) -> Result<(), StorageError>
    where
        F: FnMut(&ChunkHistoryEntry) -> bool + Send,
    {
        let rows = sqlx::query(
            "SELECT commit_id, dim, kind, cx, cz, blob, diff FROM chunk_history
             WHERE commit_id = ?",
        )
        .bind(commit_param(commit))
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?;
        for row in &rows {
            if !visit(&decode_history(row)?) {
                break;
            }
        }
        Ok(())
    }

    async fn retire_commit(
        &mut self,
        from: CommitId,
        into: CommitId,
    ) -> Result<FoldOutcome, StorageError> {
        // Preconditions are cheap to check and expensive to get wrong:
        // folding a commit into an older or equal one rewrites history
        // (a chunk first seen later becomes visible at the earlier
        // commit), and a missing target silently no-ops. "Newer" follows
        // the local sequence, not the content hash.
        let from_seq: Option<i64> = sqlx::query_scalar("SELECT seq FROM commits WHERE id = ?")
            .bind(commit_param(from))
            .fetch_optional(&self.pool)
            .await
            .map_err(db_err)?;
        let from_seq = from_seq.ok_or(StorageError::UnknownCommit { id: from })?;
        let into_seq: Option<i64> = sqlx::query_scalar("SELECT seq FROM commits WHERE id = ?")
            .bind(commit_param(into))
            .fetch_optional(&self.pool)
            .await
            .map_err(db_err)?;
        let into_seq = into_seq.ok_or(StorageError::UnknownCommit { id: into })?;
        if into_seq <= from_seq {
            return Err(StorageError::InvalidRetirement { from, into });
        }
        // The successor must descend from the retired commit: folding
        // into an unrelated history would graft rows the successor never
        // had.
        let descends: i64 = sqlx::query_scalar(DESCENDS_SQL)
            .bind(commit_param(into))
            .bind(commit_param(from))
            .fetch_one(&self.pool)
            .await
            .map_err(db_err)?;
        if descends == 0 {
            return Err(StorageError::UnrelatedRetirement { from, into });
        }
        let from_blob = commit_param(from);
        let into_blob = commit_param(into);
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        // Rows already superseded along the history the successor continues
        // vanish; survivors move onto it. Positional convention: `?1` is the
        // retired commit, `?2` the successor; `?3` is the retired commit's
        // local sequence in RETIRE_SUPERSEDED_SQL and the successor's local
        // sequence in the chunk_history UPDATE below.
        let dropped = sqlx::query(RETIRE_SUPERSEDED_SQL)
            .bind(&from_blob[..])
            .bind(&into_blob[..])
            .bind(from_seq)
            .execute(&mut *tx)
            .await
            .map_err(db_err)?
            .rows_affected();
        let folded =
            sqlx::query("UPDATE chunk_history SET commit_id = ?2, seq = ?3 WHERE commit_id = ?1")
                .bind(&from_blob[..])
                .bind(&into_blob[..])
                .bind(into_seq)
                .execute(&mut *tx)
                .await
                .map_err(db_err)?
                .rows_affected();
        // A ref naming the retired commit follows the fold: the
        // successor continues the retired history, so the ref keeps
        // naming it. The update runs before the commit row goes away -
        // the foreign key would refuse the delete otherwise.
        sqlx::query("UPDATE refs SET commit_id = ?2 WHERE commit_id = ?1")
            .bind(&from_blob[..])
            .bind(&into_blob[..])
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
        // The retired commit leaves the graph: its children continue the
        // history of its own parents, so the graph stays connected and no
        // edge points at a deleted commit.
        sqlx::query(
            "INSERT OR IGNORE INTO commit_parents (child_id, parent_id)
             SELECT child.child_id, parent.parent_id
             FROM commit_parents AS child, commit_parents AS parent
             WHERE parent.child_id = ?1 AND child.parent_id = ?1",
        )
        .bind(&from_blob[..])
        .execute(&mut *tx)
        .await
        .map_err(db_err)?;
        sqlx::query("DELETE FROM commit_parents WHERE child_id = ?1 OR parent_id = ?1")
            .bind(&from_blob[..])
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
        sqlx::query("DELETE FROM commits WHERE id = ?")
            .bind(&from_blob[..])
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
        tx.commit().await.map_err(db_err)?;
        Ok(FoldOutcome {
            folded: usize::try_from(folded).unwrap_or(usize::MAX),
            dropped: usize::try_from(dropped).unwrap_or(usize::MAX),
        })
    }

    async fn apply_commit_incremental(
        &mut self,
        created_at_ms: u64,
        parents: &[CommitId],
        entries: &[CommitEntry],
    ) -> Result<ApplyOutcome, StorageError> {
        let commit = commit_hash(parents, created_at_ms, entries);
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        // The new commit must continue the active ref's target: moving the
        // ref onto an unrelated history would fork it. Read the target
        // before inserting, so a rejected call writes nothing.
        let active_target: Option<Option<Vec<u8>>> =
            sqlx::query_scalar("SELECT commit_id FROM refs WHERE active = 1")
                .fetch_optional(&mut *tx)
                .await
                .map_err(db_err)?;
        let Some(active_target) = active_target else {
            tx.rollback().await.map_err(db_err)?;
            return Err(StorageError::NoActiveRef);
        };
        if let Some(target) = active_target {
            let base = commit_id_from_blob(target)?;
            if !parents.contains(&base) {
                tx.rollback().await.map_err(db_err)?;
                return Err(StorageError::NotContinuing { base });
            }
        }
        let (seq, inserted) = insert_commit(&mut tx, commit, created_at_ms, parents).await?;
        let id_param = commit_param(commit);
        if inserted {
            for entry in entries {
                sqlx::query(INSERT_CHUNK_SQL)
                    .bind(&id_param[..])
                    .bind(seq)
                    .bind(i64::from(entry.coord.dim.raw()))
                    .bind(i64::from(entry.coord.kind.raw()))
                    .bind(i64::from(entry.coord.x))
                    .bind(i64::from(entry.coord.z))
                    .bind(entry.blob.as_ref().map(|h| &h.0[..]))
                    .bind(entry.diff.as_ref().map(|h| &h.0[..]))
                    .execute(&mut *tx)
                    .await
                    .map_err(db_err)?;
            }
        }
        // The active ref moves onto the new commit, in the same
        // transaction: a ref lagging behind its history would fork the
        // next commit.
        let moved = sqlx::query("UPDATE refs SET commit_id = ? WHERE active = 1")
            .bind(&id_param[..])
            .execute(&mut *tx)
            .await
            .map_err(db_err)?
            .rows_affected();
        if moved == 0 {
            tx.rollback().await.map_err(db_err)?;
            return Err(StorageError::NoActiveRef);
        }
        tx.commit().await.map_err(db_err)?;
        Ok(ApplyOutcome { id: commit })
    }

    async fn set_ref(
        &mut self,
        name: &RefName,
        commit: Option<CommitId>,
        created_at_ms: u64,
    ) -> Result<(), StorageError> {
        // A new ref starts inactive; a moved ref keeps its flag and
        // creation time. `commit = None` is the documented unborn state,
        // so moving a ref back to unborn is allowed.
        let stored_commit = commit.map(commit_param);
        let created_at_ms = millis_param(created_at_ms)?;
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        if let Some(id) = commit {
            // Pointing at a commit that does not exist is corruption, not
            // a move: report it as such instead of the foreign-key
            // violation the upsert would raise.
            let found: Option<i64> = sqlx::query_scalar("SELECT 1 FROM commits WHERE id = ?")
                .bind(stored_commit.as_deref())
                .fetch_optional(&mut *tx)
                .await
                .map_err(db_err)?;
            if found.is_none() {
                tx.rollback().await.map_err(db_err)?;
                return Err(StorageError::UnknownCommit { id });
            }
        }
        sqlx::query(
            "INSERT INTO refs (name, commit_id, active, created_at_ms) VALUES (?, ?, 0, ?)
             ON CONFLICT(name) DO UPDATE SET commit_id = excluded.commit_id",
        )
        .bind(name.as_str())
        .bind(stored_commit.as_deref())
        .bind(created_at_ms)
        .execute(&mut *tx)
        .await
        .map_err(db_err)?;
        tx.commit().await.map_err(db_err)?;
        Ok(())
    }

    async fn lookup_ref(&self, name: &RefName) -> Result<Option<CommitRef>, StorageError> {
        let row =
            sqlx::query("SELECT name, commit_id, active, created_at_ms FROM refs WHERE name = ?")
                .bind(name.as_str())
                .fetch_optional(&self.pool)
                .await
                .map_err(db_err)?;
        row.map(|row| decode_ref(&row)).transpose()
    }

    async fn visit_refs<F>(&self, mut visit: F) -> Result<(), StorageError>
    where
        F: FnMut(&CommitRef) -> bool + Send,
    {
        let rows = sqlx::query(
            "SELECT name, commit_id, active, created_at_ms
             FROM refs ORDER BY name COLLATE BINARY",
        )
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

    async fn delete_ref(&mut self, name: &RefName) -> Result<bool, StorageError> {
        // One transaction so the DELETE and the follow-up read see the
        // same snapshot: the DELETE refuses the active ref, and the read
        // only refines why nothing was removed.
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        let deleted = sqlx::query("DELETE FROM refs WHERE name = ? AND active = 0")
            .bind(name.as_str())
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
        if deleted.rows_affected() > 0 {
            tx.commit().await.map_err(db_err)?;
            return Ok(true);
        }
        // Nothing was deleted: the ref is absent or active.
        let active: Option<i64> =
            sqlx::query_scalar("SELECT 1 FROM refs WHERE name = ? AND active = 1")
                .bind(name.as_str())
                .fetch_optional(&mut *tx)
                .await
                .map_err(db_err)?;
        tx.commit().await.map_err(db_err)?;
        match active {
            Some(_) => Err(StorageError::ActiveRefProtected {
                name: name.as_str().to_owned(),
            }),
            None => Ok(false),
        }
    }

    async fn active_ref(&self) -> Result<Option<CommitRef>, StorageError> {
        // The target commit must exist: the foreign key keeps it honest,
        // and a dangling target is corruption, not an unborn ref.
        let row = sqlx::query(
            "SELECT refs.name, refs.commit_id, refs.active, refs.created_at_ms,
                    commits.id AS target
             FROM refs LEFT JOIN commits ON commits.id = refs.commit_id
             WHERE refs.active = 1",
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(db_err)?;
        let Some(row) = row else {
            return Ok(None);
        };
        let record = decode_ref(&row)?;
        if let Some(commit) = record.commit {
            let target: Option<Vec<u8>> = row.try_get("target").map_err(db_err)?;
            if target.is_none() {
                return Err(StorageError::UnknownCommit { id: commit });
            }
        }
        Ok(Some(record))
    }

    async fn activate_ref(&mut self, name: &RefName) -> Result<(), StorageError> {
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        let row = sqlx::query(
            "SELECT refs.name, refs.commit_id, refs.active, refs.created_at_ms,
                    commits.id AS target
             FROM refs LEFT JOIN commits ON commits.id = refs.commit_id
             WHERE refs.name = ?",
        )
        .bind(name.as_str())
        .fetch_optional(&mut *tx)
        .await
        .map_err(db_err)?;
        let Some(row) = row else {
            tx.rollback().await.map_err(db_err)?;
            return Err(StorageError::UnknownRef {
                name: name.as_str().to_owned(),
            });
        };
        let record = decode_ref(&row)?;
        if let Some(commit) = record.commit {
            let target: Option<Vec<u8>> = row.try_get("target").map_err(db_err)?;
            if target.is_none() {
                tx.rollback().await.map_err(db_err)?;
                return Err(StorageError::UnknownCommit { id: commit });
            }
        }
        // The partial unique index allows one active row: clear the old
        // one before marking the new one.
        sqlx::query("UPDATE refs SET active = 0 WHERE active = 1")
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
        // The row can still vanish between the check above and this
        // update; committing then would leave the store without a head.
        let activated = sqlx::query("UPDATE refs SET active = 1 WHERE name = ?")
            .bind(name.as_str())
            .execute(&mut *tx)
            .await
            .map_err(db_err)?
            .rows_affected();
        if activated == 0 {
            tx.rollback().await.map_err(db_err)?;
            return Err(StorageError::UnknownRef {
                name: name.as_str().to_owned(),
            });
        }
        tx.commit().await.map_err(db_err)?;
        Ok(())
    }

    async fn set_scope(&mut self, scope: &NamedScope) -> Result<(), StorageError> {
        let created_at_ms = millis_param(scope.created_at_ms)?;
        let name = scope.name.as_str();
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        // A replacement keeps the scope row, and with it the creation
        // time; only the kinds and the whole area set are swapped.
        sqlx::query(
            "INSERT INTO scopes (name, kind, created_at_ms) VALUES (?, ?, ?)
             ON CONFLICT(name) DO UPDATE SET kind = excluded.kind",
        )
        .bind(name)
        // NULL selects every family.
        .bind(scope.kinds().map(kind_mask))
        .bind(created_at_ms)
        .execute(&mut *tx)
        .await
        .map_err(db_err)?;
        sqlx::query("DELETE FROM scope_areas WHERE scope = ?")
            .bind(name)
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
        for (dim, rect) in scope.areas() {
            sqlx::query(
                "INSERT INTO scope_areas (scope, dim, x0, z0, x1, z1) VALUES (?, ?, ?, ?, ?, ?)",
            )
            .bind(name)
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
    use sekai_core::{RegionKind, usecase::commit::stage_present};
    use sqlx::ConnectOptions as _;
    use sqlx::Connection as _;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn tempdir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "sekai-{}-{}-{}",
            name,
            std::process::id(),
            TMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
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

    #[tokio::test]
    async fn a_fresh_store_starts_with_one_active_ref() {
        let meta = memory_meta().await;
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
        let mut meta = memory_meta().await;
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
        let dir = tempdir("dangling-target");
        let path = dir.join("meta.sqlite");
        let mut meta = SqliteMeta::open(&path).await.unwrap();
        let main = RefName::parse(DEFAULT_REF_NAME).unwrap();
        let feature = RefName::parse("feature").unwrap();
        let root = meta
            .apply_commit_incremental(1_000, &[], &[])
            .await
            .unwrap()
            .id;
        meta.set_ref(&feature, Some(root), 2_000).await.unwrap();

        // Simulate corruption that bypassed the foreign key on a dedicated
        // connection, so no pooled connection inherits the pragma.
        let mut raw = SqliteConnectOptions::new()
            .filename(&path)
            .foreign_keys(false)
            .connect()
            .await
            .unwrap();
        let missing = CommitId([0xEE; 32]);
        sqlx::query("UPDATE refs SET commit_id = ? WHERE name = 'feature'")
            .bind(missing.0.to_vec())
            .execute(&mut raw)
            .await
            .unwrap();
        raw.close().await.unwrap();

        let err = meta.activate_ref(&feature).await.unwrap_err();
        assert!(matches!(err, StorageError::UnknownCommit { id } if id == missing));
        assert_eq!(meta.active_ref().await.unwrap().unwrap().name, main);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn an_unborn_active_ref_starts_a_new_root() {
        let mut meta = memory_meta().await;
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
        let mut meta = memory_meta().await;
        let feature = RefName::parse("feature").unwrap();

        // A stale target is a typed error, not an opaque foreign-key
        // violation, and the ref does not appear.
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
        let mut meta = memory_meta().await;
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

        // Existence is the commit row, not a cached hash: a retired
        // commit can be recorded again and gets a fresh local position.
        let second = meta.create_commit(2_000, &[first]).await.unwrap();
        meta.retire_commit(first, second).await.unwrap();
        assert!(meta.lookup_commit(first).await.unwrap().is_none());
        let again = meta.create_commit(1_000, &[]).await.unwrap();
        assert_eq!(again, first);
        assert!(meta.lookup_commit(first).await.unwrap().is_some());
        assert_eq!(meta.latest_commit().await.unwrap().unwrap().id, first);
    }

    #[tokio::test]
    async fn a_commit_must_continue_the_active_ref() {
        let mut meta = memory_meta().await;
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
    async fn retiring_the_active_ref_target_moves_the_active_flag() {
        let mut meta = memory_meta().await;
        let main = RefName::parse(DEFAULT_REF_NAME).unwrap();
        let root = meta
            .apply_commit_incremental(1_000, &[], &[])
            .await
            .unwrap()
            .id;
        let second = meta
            .apply_commit_incremental(2_000, &[root], &[])
            .await
            .unwrap()
            .id;
        // Point the active ref back at the root: retiring it must move the
        // active row, not only inactive refs.
        meta.set_ref(&main, Some(root), 1_500).await.unwrap();

        meta.retire_commit(root, second).await.unwrap();
        let active = meta.active_ref().await.unwrap().unwrap();
        assert_eq!(active.name, main);
        assert_eq!(active.commit, Some(second));
        assert!(active.active);
    }

    #[tokio::test]
    async fn a_corrupt_ref_row_fails_loudly() {
        let dir = tempdir("corrupt-ref");
        let path = dir.join("meta.sqlite");
        let meta = SqliteMeta::open(&path).await.unwrap();
        // Names are re-validated on decode: a row the schema no longer
        // accepts is corruption, never a new ref name.
        sqlx::query(
            "INSERT INTO refs (name, commit_id, active, created_at_ms) \
             VALUES ('bad name', NULL, 0, 0)",
        )
        .execute(&meta.pool)
        .await
        .unwrap();

        let err = meta.visit_refs(|_| true).await.unwrap_err();
        assert!(matches!(err, StorageError::InvalidRefName { name } if name == "bad name"));

        // The table's CHECK normally rejects this; disable it on a
        // dedicated connection so no pooled connection inherits the pragma.
        let mut raw = SqliteConnectOptions::new()
            .filename(&path)
            .connect()
            .await
            .unwrap();
        sqlx::query("PRAGMA ignore_check_constraints = ON")
            .execute(&mut raw)
            .await
            .unwrap();
        sqlx::query("UPDATE refs SET active = 2 WHERE name = ?")
            .bind(DEFAULT_REF_NAME)
            .execute(&mut raw)
            .await
            .unwrap();
        sqlx::query("PRAGMA ignore_check_constraints = OFF")
            .execute(&mut raw)
            .await
            .unwrap();
        raw.close().await.unwrap();

        let main = RefName::parse(DEFAULT_REF_NAME).unwrap();
        assert!(matches!(
            meta.lookup_ref(&main).await,
            Err(StorageError::InvalidHistoryValue {
                column: "active",
                value: 2
            })
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_commit_without_an_active_ref_writes_nothing() {
        let mut meta = memory_meta().await;
        // A first commit gives the store a history.
        let first = meta
            .apply_commit_incremental(1_000, &[], &[])
            .await
            .unwrap()
            .id;
        // A store whose active flag was lost (corruption or a legacy row),
        // leaving the history intact but with no head.
        sqlx::query("UPDATE refs SET active = 0")
            .execute(&meta.pool)
            .await
            .unwrap();

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
        let mut meta = memory_meta().await;
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
    async fn a_retired_commit_hands_its_ref_to_the_successor() {
        let mut meta = memory_meta().await;
        let main = RefName::parse(DEFAULT_REF_NAME).unwrap();
        let feature = RefName::parse("feature").unwrap();
        let root = meta
            .apply_commit_incremental(1_000, &[], &[])
            .await
            .unwrap()
            .id;
        let second = meta
            .apply_commit_incremental(2_000, &[root], &[])
            .await
            .unwrap()
            .id;
        // A second ref names the root of the history.
        meta.set_ref(&feature, Some(root), 1_500).await.unwrap();

        // Retiring the root folds it into its successor, and the ref
        // that named it follows: the successor holds the same state.
        meta.retire_commit(root, second).await.unwrap();
        assert_eq!(
            meta.lookup_ref(&feature).await.unwrap().unwrap().commit,
            Some(second)
        );
        assert_eq!(
            meta.lookup_ref(&main).await.unwrap().unwrap().commit,
            Some(second)
        );

        // The successor the refs followed is a valid active target:
        // activating the ref names it, not the retired commit.
        meta.activate_ref(&feature).await.unwrap();
        assert_eq!(
            meta.active_ref().await.unwrap().unwrap().commit,
            Some(second)
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

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_first_inits_all_succeed() {
        for round in 0..20 {
            let dir = tempdir(&format!("init-race-{round}"));
            std::fs::remove_dir_all(&dir).unwrap();
            let inits: Vec<_> = (0..4)
                .map(|_| tokio::spawn(init_sqlite(dir.clone())))
                .collect();
            for init in inits {
                init.await.unwrap().unwrap();
            }
            let meta = open_sqlite(&dir).await.unwrap();
            let version: i64 = sqlx::query_scalar("PRAGMA user_version")
                .fetch_one(&meta.meta().pool)
                .await
                .unwrap();
            assert_eq!(version, SCHEMA_VERSION);
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[tokio::test]
    async fn version_gate_rejects_foreign_schema() {
        let dir = tempdir("version");
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
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The version gate rejects any store whose `user_version` is not
    /// `SCHEMA_VERSION` (14) before any table is touched. The immediately
    /// previous store version, 13, is refused with `UnsupportedSchema
    /// { found: 13, supported: SCHEMA_VERSION }`.
    #[tokio::test]
    async fn a_store_from_the_previous_schema_version_is_refused() {
        let dir = tempdir("version-13");
        let path = dir.join("meta.sqlite");
        let mut seed = SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(true)
            .connect()
            .await
            .unwrap();
        sqlx::query(SCHEMA).execute(&mut seed).await.unwrap();
        sqlx::query("PRAGMA user_version=13")
            .execute(&mut seed)
            .await
            .unwrap();
        seed.close().await.unwrap();
        assert!(matches!(
            SqliteMeta::open(&path).await,
            Err(StorageError::UnsupportedSchema {
                found: 13,
                supported: SCHEMA_VERSION,
            })
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn unknown_stored_kind_fails_loudly() {
        use sekai_core::MetaStore as _;
        let mut meta = memory_meta().await;
        let coord = ChunkCoord::new(Dimension::OVERWORLD, RegionKind::Region, 0, 0);
        let commit = meta
            .apply_commit(1_000, &[], &[stage_present(coord, BlobHash([1; 32]))])
            .await
            .unwrap();
        sqlx::query("UPDATE chunk_history SET kind = 9")
            .execute(&meta.pool)
            .await
            .unwrap();
        assert!(matches!(
            meta.visit_fresh_rows(commit, |_| true).await,
            Err(StorageError::UnknownRegionKind { kind: 9 })
        ));
    }

    fn scope(name: &str, kinds: Option<Vec<RegionKind>>) -> NamedScope {
        NamedScope::new(
            ScopeName::parse(name).unwrap(),
            kinds,
            vec![(Dimension::OVERWORLD, Rect::new(0, 0, 1, 1))],
            1_000,
        )
        .unwrap()
    }

    #[tokio::test]
    async fn scope_kinds_are_stored_as_a_mask() {
        let mut meta = memory_meta().await;
        meta.set_scope(&scope(
            "some",
            Some(vec![RegionKind::Region, RegionKind::Poi]),
        ))
        .await
        .unwrap();
        meta.set_scope(&scope("all", None)).await.unwrap();
        let stored: Vec<(String, Option<i64>)> =
            sqlx::query_as("SELECT name, kind FROM scopes ORDER BY name")
                .fetch_all(&meta.pool)
                .await
                .unwrap();
        assert_eq!(
            stored,
            vec![("all".to_owned(), None), ("some".to_owned(), Some(0b101))]
        );
    }

    #[tokio::test]
    async fn the_kind_check_admits_proper_subsets_only() {
        let meta = memory_meta().await;
        let insert = |mask: i64| {
            sqlx::query("INSERT INTO scopes (name, kind, created_at_ms) VALUES (?, ?, 0)")
                .bind(format!("mask-{mask}"))
                .bind(mask)
                .execute(&meta.pool)
        };
        for mask in 1..ALL_KINDS_MASK {
            insert(mask).await.unwrap();
        }
        for mask in [0, ALL_KINDS_MASK] {
            let err = insert(mask).await.unwrap_err();
            assert!(
                err.to_string().contains("CHECK constraint failed"),
                "mask {mask}: {err}"
            );
        }
    }

    #[tokio::test]
    async fn deleting_a_scope_drops_its_areas() {
        let mut meta = memory_meta().await;
        let record = scope("spawn", None);
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
        let dir = tempdir("corrupt-scope");
        let path = dir.join("meta.sqlite");
        let meta = SqliteMeta::open(&path).await.unwrap();

        // Names re-validate on decode.
        sqlx::query("INSERT INTO scopes (name, kind, created_at_ms) VALUES ('bad name', NULL, 0)")
            .execute(&meta.pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO scope_areas (scope, dim, x0, z0, x1, z1) \
             VALUES ('bad name', 0, 0, 0, 0, 0)",
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
        sqlx::query("INSERT INTO scopes (name, kind, created_at_ms) VALUES ('empty', NULL, 0)")
            .execute(&meta.pool)
            .await
            .unwrap();
        let empty = ScopeName::parse("empty").unwrap();
        let err = meta.lookup_scope(&empty).await.unwrap_err();
        assert!(matches!(err, StorageError::EmptyScope { name } if name == "empty"));

        // The full kind set is always spelled NULL; the CHECK refuses the
        // mask, so disable it on a dedicated connection. The pragma is per
        // connection, so `meta`'s pooled one keeps enforcing it.
        let mut raw = SqliteConnectOptions::new()
            .filename(&path)
            .connect()
            .await
            .unwrap();
        sqlx::query("PRAGMA ignore_check_constraints = ON")
            .execute(&mut raw)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO scope_areas (scope, dim, x0, z0, x1, z1) VALUES ('empty', 0, 0, 0, 0, 0)",
        )
        .execute(&mut raw)
        .await
        .unwrap();
        sqlx::query("UPDATE scopes SET kind = 7 WHERE name = 'empty'")
            .execute(&mut raw)
            .await
            .unwrap();
        sqlx::query("PRAGMA ignore_check_constraints = OFF")
            .execute(&mut raw)
            .await
            .unwrap();
        raw.close().await.unwrap();
        let err = meta.lookup_scope(&empty).await.unwrap_err();
        assert!(matches!(
            err,
            StorageError::InvalidHistoryValue {
                column: "scopes.kind",
                value: 7
            }
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
