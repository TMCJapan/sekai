//! SQLite commit metadata: the history graph and per-chunk history.
//!
//! Recording a commit uses one transaction; CAS durability is established
//! before the metadata commit by the caller. The pool has one connection.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::{Duration, Instant};

use sekai_core::{
    ApplyOutcome, BlobHash, ChunkCoord, ChunkHistoryEntry, Commit, CommitEntry, CommitId,
    CommitRef, CommitTag, DEFAULT_REF_NAME, DiffHash, Dimension, FoldOutcome, MetaStore, RefName,
    RegionFingerprint, RegionKey, RegionKind, RegionStateEntry, TagName,
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
pub const SCHEMA_VERSION: i64 = 10;

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
            .apply_commit_incremental(created_at_ms, parents, entries, None, &[], &[])
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
            let child = commit_id_from_i64(row.try_get("child_id").map_err(db_err)?)?;
            let parent = commit_id_from_i64(row.try_get("parent_id").map_err(db_err)?)?;
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

/// Fallible `i64` row ID -> domain `CommitId`.
fn commit_id_from_i64(id: i64) -> Result<CommitId, StorageError> {
    i64_to_u64(id, "id").map(CommitId)
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
    let commit_raw: i64 = row.try_get("commit_id").map_err(db_err)?;
    let commit = commit_id_from_i64(commit_raw)?;
    Ok(ChunkHistoryEntry::new(coord, commit, blob, diff))
}

/// Decode one `region_state` row.
fn decode_state(row: &sqlx::sqlite::SqliteRow) -> Result<RegionStateEntry, StorageError> {
    let key = RegionKey::new(
        Dimension::new(i32_col(row, "dim")?),
        kind_col(row, "kind")?,
        i32_col(row, "rx")?,
        i32_col(row, "rz")?,
    );
    let mtime_ms: Option<i64> = row.try_get("mtime_ms").map_err(db_err)?;
    let mtime_ms = mtime_ms.map(|v| i64_to_u64(v, "mtime_ms")).transpose()?;
    let size_raw: i64 = row.try_get("size").map_err(db_err)?;
    let size = i64_to_u64(size_raw, "size")?;
    let content: Vec<u8> = row.try_get("content_hash").map_err(db_err)?;
    let content_hash = hash32(content)?;
    let commit_raw: i64 = row.try_get("commit_id").map_err(db_err)?;
    let commit_id = commit_id_from_i64(commit_raw)?;
    Ok(RegionStateEntry {
        fingerprint: RegionFingerprint {
            key,
            mtime_ms,
            size,
            content_hash,
        },
        commit_id,
    })
}

/// Decode one `commit_tags` row. Stored names always re-validate:
/// anything else is corruption, never silent reinterpretation.
fn decode_tag(row: &sqlx::sqlite::SqliteRow) -> Result<CommitTag, StorageError> {
    let name_raw: String = row.try_get("name").map_err(db_err)?;
    let name = TagName::parse(&name_raw).map_err(|_| StorageError::InvalidTagName {
        name: name_raw.clone(),
    })?;
    let commit_raw: i64 = row.try_get("commit_id").map_err(db_err)?;
    let commit = commit_id_from_i64(commit_raw)?;
    Ok(CommitTag::new(
        name,
        commit,
        millis_col(row, "created_at_ms")?,
    ))
}

/// Decode one `refs` row. Stored names always re-validate: anything
/// else is corruption, never silent reinterpretation.
fn decode_ref(row: &sqlx::sqlite::SqliteRow) -> Result<CommitRef, StorageError> {
    let name_raw: String = row.try_get("name").map_err(db_err)?;
    let name =
        RefName::parse(&name_raw).map_err(|_| StorageError::InvalidRefName { name: name_raw })?;
    let commit_raw: Option<i64> = row.try_get("commit_id").map_err(db_err)?;
    let commit = commit_raw.map(commit_id_from_i64).transpose()?;
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

/// `CommitId` as an `INTEGER` bind parameter.
fn commit_param(id: CommitId) -> Result<i64, StorageError> {
    i64::try_from(id.0).map_err(|_| StorageError::InvalidCommitId(id.0))
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
    "INSERT INTO chunk_history (commit_id, dim, kind, cx, cz, blob, diff)
             VALUES (?, ?, ?, ?, ?, ?, ?)";

/// Ancestors of a commit: the commit itself plus every commit
/// reachable through parent links.
///
/// The recursive term walks the history graph upwards, so the set
/// answers "what does this commit inherit?" without relying on the
/// identifier order. `UNION` (not `UNION ALL`) keeps the walk finite even
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
ORDER BY commit_id DESC LIMIT 1"
);

/// Effective rows of every coordinate at a commit.
const VISIT_CHUNKS_SQL: &str = concat!(
    ancestor_cte!("?"),
    "
SELECT dim, kind, cx, cz, blob, diff, commit_id FROM (
   SELECT dim, kind, cx, cz, blob, diff, commit_id,
     ROW_NUMBER() OVER (
       PARTITION BY dim, kind, cx, cz ORDER BY commit_id DESC
     ) AS rn
   FROM chunk_history WHERE commit_id IN (SELECT id FROM ancestors)
) WHERE rn = 1 ORDER BY dim, kind, cx, cz"
);

/// Rows of a retired commit that the history of its successor already
/// supersedes. `?1` is the retired commit, `?2` the successor.
const RETIRE_SUPERSEDED_SQL: &str = concat!(
    ancestor_cte!("?2"),
    "
DELETE FROM chunk_history WHERE commit_id = ?1
 AND (dim, kind, cx, cz) IN (
     SELECT dim, kind, cx, cz FROM chunk_history
     WHERE commit_id > ?1 AND commit_id IN (SELECT id FROM ancestors)
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
    let id: i64 = row.try_get("id").map_err(db_err)?;
    Ok(Commit::new(
        commit_id_from_i64(id)?,
        millis_col(row, "created_at_ms")?,
        parents.to_vec(),
    ))
}

/// Insert a commit row and the edges naming its parents.
///
/// Every parent must exist and none of them may be the commit
/// itself; a parent listed twice is recorded once. The checks run in
/// the transaction, so a rejected commit leaves nothing behind.
async fn insert_commit(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    created_at_ms: u64,
    parents: &[CommitId],
) -> Result<CommitId, StorageError> {
    let id: i64 = sqlx::query("INSERT INTO commits (created_at_ms) VALUES (?)")
        .bind(millis_param(created_at_ms)?)
        .execute(&mut **tx)
        .await
        .map_err(db_err)?
        .last_insert_rowid();
    let commit = commit_id_from_i64(id)?;
    for parent in parents {
        if *parent == commit {
            return Err(StorageError::SelfParent { id: parent.0 });
        }
    }
    // One query for the whole list, so a merge does not cost a round
    // trip per parent.
    let existing: Vec<i64> = if parents.is_empty() {
        Vec::new()
    } else {
        let mut qb = sqlx::QueryBuilder::new("SELECT id FROM commits WHERE id IN (");
        let mut separated = qb.separated(", ");
        for parent in parents {
            separated.push_bind(commit_param(*parent)?);
        }
        separated.push_unseparated(")");
        qb.build_query_scalar()
            .fetch_all(&mut **tx)
            .await
            .map_err(db_err)?
    };
    for parent in parents {
        if !existing.contains(&commit_param(*parent)?) {
            return Err(StorageError::UnknownCommit { id: parent.0 });
        }
        sqlx::query("INSERT OR IGNORE INTO commit_parents (child_id, parent_id) VALUES (?, ?)")
            .bind(id)
            .bind(commit_param(*parent)?)
            .execute(&mut **tx)
            .await
            .map_err(db_err)?;
    }
    Ok(commit)
}

impl sekai_core::MetaStore for SqliteMeta {
    type Error = StorageError;

    async fn create_commit(
        &mut self,
        created_at_ms: u64,
        parents: &[CommitId],
    ) -> Result<CommitId, StorageError> {
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        let id = insert_commit(&mut tx, created_at_ms, parents).await?;
        tx.commit().await.map_err(db_err)?;
        Ok(id)
    }

    async fn commit_parents(&self, commit: CommitId) -> Result<Vec<CommitId>, StorageError> {
        let rows = sqlx::query(
            "SELECT parent_id FROM commit_parents WHERE child_id = ? ORDER BY parent_id",
        )
        .bind(commit_param(commit)?)
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?;
        rows.iter()
            .map(|row| commit_id_from_i64(row.try_get("parent_id").map_err(db_err)?))
            .collect()
    }

    async fn record_chunk(
        &mut self,
        commit: CommitId,
        coord: &ChunkCoord,
        blob: Option<&BlobHash>,
        diff: Option<&DiffHash>,
    ) -> Result<(), StorageError> {
        sqlx::query(INSERT_CHUNK_SQL)
            .bind(commit_param(commit)?)
            .bind(i64::from(coord.dim.raw()))
            .bind(i64::from(coord.kind.raw()))
            .bind(i64::from(coord.x))
            .bind(i64::from(coord.z))
            .bind(blob.map(|h| &h.0[..]))
            .bind(diff.map(|h| &h.0[..]))
            .execute(&self.pool)
            .await
            .map_err(db_err)?;
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
            .bind(commit_param(commit)?)
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
            .bind(commit_param(id)?)
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
        let row = sqlx::query("SELECT id, created_at_ms FROM commits ORDER BY id DESC LIMIT 1")
            .fetch_optional(&self.pool)
            .await
            .map_err(db_err)?;
        let Some(row) = row else {
            return Ok(None);
        };
        let id = commit_id_from_i64(row.try_get("id").map_err(db_err)?)?;
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
            .bind(commit_param(commit)?)
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
        let rows = sqlx::query("SELECT id, created_at_ms FROM commits ORDER BY id")
            .fetch_all(&self.pool)
            .await
            .map_err(db_err)?;
        let parents = self.parent_map().await?;
        let mut ids: Vec<CommitId> = Vec::with_capacity(rows.len());
        for row in &rows {
            ids.push(commit_id_from_i64(row.try_get("id").map_err(db_err)?)?);
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

    async fn load_region_states(
        &self,
        within: Option<CommitId>,
    ) -> Result<Vec<RegionStateEntry>, StorageError> {
        // Only fingerprints confirmed inside the given history may be
        // carried over: one confirmed elsewhere describes another state
        // of the file. An unborn history knows no file at all.
        let Some(within) = within else {
            return Ok(Vec::new());
        };
        let mut qb = sqlx::QueryBuilder::new(ancestor_cte!(head));
        qb.push_bind(commit_param(within)?);
        qb.push(ancestor_cte!(tail));
        qb.push(
            " SELECT dim, kind, rx, rz, mtime_ms, size, content_hash, commit_id
               FROM region_state WHERE commit_id IN (SELECT id FROM ancestors)",
        );
        let rows = qb.build().fetch_all(&self.pool).await.map_err(db_err)?;
        rows.iter().map(decode_state).collect()
    }

    async fn tag_commit(
        &mut self,
        name: &TagName,
        commit: CommitId,
        created_at_ms: u64,
    ) -> Result<(), StorageError> {
        sqlx::query("INSERT INTO commit_tags (name, commit_id, created_at_ms) VALUES (?, ?, ?)")
            .bind(name.as_str())
            .bind(commit_param(commit)?)
            .bind(millis_param(created_at_ms)?)
            .execute(&self.pool)
            .await
            .map_err(db_err)?;
        Ok(())
    }

    async fn untag(&mut self, name: &TagName) -> Result<bool, StorageError> {
        let done = sqlx::query("DELETE FROM commit_tags WHERE name = ?")
            .bind(name.as_str())
            .execute(&self.pool)
            .await
            .map_err(db_err)?;
        Ok(done.rows_affected() > 0)
    }

    async fn lookup_tag(&self, name: &TagName) -> Result<Option<CommitTag>, StorageError> {
        let row =
            sqlx::query("SELECT name, commit_id, created_at_ms FROM commit_tags WHERE name = ?")
                .bind(name.as_str())
                .fetch_optional(&self.pool)
                .await
                .map_err(db_err)?;
        row.map(|row| decode_tag(&row)).transpose()
    }

    async fn visit_tags<F>(&self, mut visit: F) -> Result<(), StorageError>
    where
        F: FnMut(&CommitTag) -> bool + Send,
    {
        let rows =
            sqlx::query("SELECT name, commit_id, created_at_ms FROM commit_tags ORDER BY name")
                .fetch_all(&self.pool)
                .await
                .map_err(db_err)?;
        for row in &rows {
            if !visit(&decode_tag(row)?) {
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
        .bind(commit_param(commit)?)
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
        // commit), and a missing target silently no-ops.
        if into <= from {
            return Err(StorageError::InvalidRetirement {
                from: from.0,
                into: into.0,
            });
        }
        for id in [from, into] {
            let found: Option<i64> = sqlx::query_scalar("SELECT 1 FROM commits WHERE id = ?")
                .bind(commit_param(id)?)
                .fetch_optional(&self.pool)
                .await
                .map_err(db_err)?;
            if found.is_none() {
                return Err(StorageError::UnknownCommit { id: id.0 });
            }
        }
        // The successor must descend from the retired commit: folding
        // into an unrelated history would graft rows the successor never
        // had.
        let descends: i64 = sqlx::query_scalar(DESCENDS_SQL)
            .bind(commit_param(into)?)
            .bind(commit_param(from)?)
            .fetch_one(&self.pool)
            .await
            .map_err(db_err)?;
        if descends == 0 {
            return Err(StorageError::UnrelatedRetirement {
                from: from.0,
                into: into.0,
            });
        }
        let from = commit_param(from)?;
        let into = commit_param(into)?;
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        // Rows already superseded along the history the successor continues
        // vanish; survivors move onto it. Tags follow the commit row by
        // cascade. Positional convention below: `?1` is always the retired
        // commit, `?2` the successor.
        let dropped = sqlx::query(RETIRE_SUPERSEDED_SQL)
            .bind(from)
            .bind(into)
            .execute(&mut *tx)
            .await
            .map_err(db_err)?
            .rows_affected();
        let folded = sqlx::query("UPDATE chunk_history SET commit_id = ?2 WHERE commit_id = ?1")
            .bind(from)
            .bind(into)
            .execute(&mut *tx)
            .await
            .map_err(db_err)?
            .rows_affected();
        sqlx::query("UPDATE region_state SET commit_id = ?2 WHERE commit_id = ?1")
            .bind(from)
            .bind(into)
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
        // A ref naming the retired commit follows the fold: the
        // successor continues the retired history, so the ref keeps
        // naming it. The update runs before the commit row goes away -
        // the foreign key would refuse the delete otherwise.
        sqlx::query("UPDATE refs SET commit_id = ?2 WHERE commit_id = ?1")
            .bind(from)
            .bind(into)
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
        .bind(from)
        .execute(&mut *tx)
        .await
        .map_err(db_err)?;
        sqlx::query("DELETE FROM commit_parents WHERE child_id = ?1 OR parent_id = ?1")
            .bind(from)
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
        sqlx::query("DELETE FROM commits WHERE id = ?")
            .bind(from)
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
        carry_from: Option<(CommitId, &[RegionKey])>,
        fingerprints: &[RegionFingerprint],
        removed: &[RegionKey],
    ) -> Result<ApplyOutcome, StorageError> {
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        // The new commit must continue the active ref's target: moving the
        // ref onto an unrelated history would fork it. Read the target
        // before inserting, so a rejected call writes nothing.
        let active_target: Option<Option<i64>> =
            sqlx::query_scalar("SELECT commit_id FROM refs WHERE active = 1")
                .fetch_optional(&mut *tx)
                .await
                .map_err(db_err)?;
        let Some(active_target) = active_target else {
            tx.rollback().await.map_err(db_err)?;
            return Err(StorageError::NoActiveRef);
        };
        if let Some(target) = active_target {
            let base = commit_id_from_i64(target)?;
            if !parents.contains(&base) {
                tx.rollback().await.map_err(db_err)?;
                return Err(StorageError::NotContinuing { base: base.raw() });
            }
        }
        let commit = insert_commit(&mut tx, created_at_ms, parents).await?;
        let id_param = commit_param(commit)?;
        for entry in entries {
            sqlx::query(INSERT_CHUNK_SQL)
                .bind(id_param)
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
        let mut carried_chunks = 0usize;
        if let Some((prev, keys)) = carry_from {
            // Delta storage: carried regions contribute no rows. Their
            // effective present chunks are counted for the report with the
            // same ancestor semantics as reads; only derived state advances
            // below.
            if !keys.is_empty() {
                // Region predicates are structural (fixed shape per region);
                // all values stay bound parameters.
                let mut qb = sqlx::QueryBuilder::new(ancestor_cte!(head));
                qb.push_bind(commit_param(prev)?);
                qb.push(ancestor_cte!(tail));
                qb.push(
                    " SELECT COUNT(*) FROM (
                       SELECT ROW_NUMBER() OVER (
                         PARTITION BY dim, kind, cx, cz ORDER BY commit_id DESC
                       ) AS rn, blob
                       FROM chunk_history WHERE commit_id IN (SELECT id FROM ancestors) AND (",
                );
                for (i, key) in keys.iter().enumerate() {
                    if i > 0 {
                        qb.push(" OR ");
                    }
                    qb.push("(dim = ");
                    qb.push_bind(i64::from(key.dim.raw()));
                    qb.push(" AND kind = ");
                    qb.push_bind(i64::from(key.kind.raw()));
                    qb.push(" AND cx BETWEEN ");
                    qb.push_bind(i64::from(key.rx) * 32);
                    qb.push(" AND ");
                    qb.push_bind(i64::from(key.rx) * 32 + 31);
                    qb.push(" AND cz BETWEEN ");
                    qb.push_bind(i64::from(key.rz) * 32);
                    qb.push(" AND ");
                    qb.push_bind(i64::from(key.rz) * 32 + 31);
                    qb.push(")");
                }
                qb.push(")) WHERE rn = 1 AND blob IS NOT NULL");
                let carried: i64 = qb
                    .build_query_scalar()
                    .fetch_one(&mut *tx)
                    .await
                    .map_err(db_err)?;
                carried_chunks = usize::try_from(carried).unwrap_or(usize::MAX);
            }
            for key in keys {
                sqlx::query(
                    "UPDATE region_state SET commit_id = ?
                     WHERE dim = ? AND kind = ? AND rx = ? AND rz = ?",
                )
                .bind(id_param)
                .bind(i64::from(key.dim.raw()))
                .bind(i64::from(key.kind.raw()))
                .bind(i64::from(key.rx))
                .bind(i64::from(key.rz))
                .execute(&mut *tx)
                .await
                .map_err(db_err)?;
            }
        }
        for fp in fingerprints {
            let mtime_ms: Option<i64> = fp
                .mtime_ms
                .map(|ms| i64::try_from(ms).map_err(|_| StorageError::InvalidTimestamp(ms)))
                .transpose()?;
            let size = i64::try_from(fp.size).map_err(|_| StorageError::InvalidSize(fp.size))?;
            sqlx::query(
                "INSERT INTO region_state (dim, kind, rx, rz, mtime_ms, size, content_hash, commit_id)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?)
                 ON CONFLICT(dim, kind, rx, rz) DO UPDATE SET
                   mtime_ms = excluded.mtime_ms, size = excluded.size,
                   content_hash = excluded.content_hash, commit_id = excluded.commit_id",
            )
            .bind(i64::from(fp.key.dim.raw()))
            .bind(i64::from(fp.key.kind.raw()))
            .bind(i64::from(fp.key.rx))
            .bind(i64::from(fp.key.rz))
            .bind(mtime_ms)
            .bind(size)
            .bind(&fp.content_hash[..])
            .bind(id_param)
            .execute(&mut *tx)
            .await.map_err(db_err)?;
        }
        for key in removed {
            sqlx::query(
                "DELETE FROM region_state WHERE dim = ? AND kind = ? AND rx = ? AND rz = ?",
            )
            .bind(i64::from(key.dim.raw()))
            .bind(i64::from(key.kind.raw()))
            .bind(i64::from(key.rx))
            .bind(i64::from(key.rz))
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
        }
        // The active ref moves onto the new commit, in the same
        // transaction: a ref lagging behind its history would fork the
        // next commit.
        let moved = sqlx::query("UPDATE refs SET commit_id = ? WHERE active = 1")
            .bind(id_param)
            .execute(&mut *tx)
            .await
            .map_err(db_err)?
            .rows_affected();
        if moved == 0 {
            tx.rollback().await.map_err(db_err)?;
            return Err(StorageError::NoActiveRef);
        }
        tx.commit().await.map_err(db_err)?;
        Ok(ApplyOutcome {
            id: commit,
            carried_chunks,
        })
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
        let stored_commit = commit.map(commit_param).transpose()?;
        let created_at_ms = millis_param(created_at_ms)?;
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        if let Some(id) = commit {
            // Pointing at a commit that does not exist is corruption, not
            // a move: report it as such instead of the foreign-key
            // violation the upsert would raise.
            let found: Option<i64> = sqlx::query_scalar("SELECT 1 FROM commits WHERE id = ?")
                .bind(stored_commit)
                .fetch_optional(&mut *tx)
                .await
                .map_err(db_err)?;
            if found.is_none() {
                tx.rollback().await.map_err(db_err)?;
                return Err(StorageError::UnknownCommit { id: id.raw() });
            }
        }
        sqlx::query(
            "INSERT INTO refs (name, commit_id, active, created_at_ms) VALUES (?, ?, 0, ?)
             ON CONFLICT(name) DO UPDATE SET commit_id = excluded.commit_id",
        )
        .bind(name.as_str())
        .bind(stored_commit)
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
        let rows =
            sqlx::query("SELECT name, commit_id, active, created_at_ms FROM refs ORDER BY name")
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
            let target: Option<i64> = row.try_get("target").map_err(db_err)?;
            if target.is_none() {
                return Err(StorageError::UnknownCommit { id: commit.raw() });
            }
        }
        Ok(Some(record))
    }

    async fn activate_ref(&mut self, name: &RefName) -> Result<(), StorageError> {
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        let exists: Option<i64> = sqlx::query_scalar("SELECT 1 FROM refs WHERE name = ?")
            .bind(name.as_str())
            .fetch_optional(&mut *tx)
            .await
            .map_err(db_err)?;
        if exists.is_none() {
            tx.rollback().await.map_err(db_err)?;
            return Err(StorageError::UnknownRef {
                name: name.as_str().to_owned(),
            });
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use sekai_core::{
        RegionKind, Scope,
        usecase::commit::{
            Ingested, Observation, assemble, plan_commit, record_commit, stage_present,
        },
    };
    use sqlx::ConnectOptions as _;
    use sqlx::Connection as _;
    use std::collections::BTreeSet;
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
    async fn a_ref_cannot_name_a_missing_commit() {
        let mut meta = memory_meta().await;
        let feature = RefName::parse("feature").unwrap();

        // A stale target is a typed error, not an opaque foreign-key
        // violation, and the ref does not appear.
        let err = meta
            .set_ref(&feature, Some(CommitId(99)), 1_000)
            .await
            .unwrap_err();
        assert!(matches!(err, StorageError::UnknownCommit { id: 99 }));
        assert!(meta.lookup_ref(&feature).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn moving_a_born_active_ref_back_to_unborn_keeps_its_state() {
        let mut meta = memory_meta().await;
        let main = RefName::parse(DEFAULT_REF_NAME).unwrap();
        let first = meta
            .apply_commit_incremental(1_000, &[], &[], None, &[], &[])
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
    async fn a_commit_must_continue_the_active_ref() {
        let mut meta = memory_meta().await;
        let root = meta
            .apply_commit_incremental(1_000, &[], &[], None, &[], &[])
            .await
            .unwrap()
            .id;

        let err = meta
            .apply_commit_incremental(2_000, &[], &[], None, &[], &[])
            .await
            .unwrap_err();
        assert!(matches!(err, StorageError::NotContinuing { base } if base == root.raw()));

        // The rejected call wrote nothing and consumed no identifier.
        assert_eq!(meta.latest_commit().await.unwrap().unwrap().id, root);
        assert_eq!(meta.active_ref().await.unwrap().unwrap().commit, Some(root));

        let next = meta
            .apply_commit_incremental(3_000, &[root], &[], None, &[], &[])
            .await
            .unwrap()
            .id;
        assert_eq!(next, CommitId(2));
    }

    #[tokio::test]
    async fn retiring_the_active_ref_target_moves_the_active_flag() {
        let mut meta = memory_meta().await;
        let main = RefName::parse(DEFAULT_REF_NAME).unwrap();
        let root = meta
            .apply_commit_incremental(1_000, &[], &[], None, &[], &[])
            .await
            .unwrap()
            .id;
        let second = meta
            .apply_commit_incremental(2_000, &[root], &[], None, &[], &[])
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
        let meta = memory_meta().await;
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

        // The table's CHECK normally rejects this; disable it on one
        // connection to prove the decoder itself refuses a value that a
        // corrupt or older table could hold.
        let mut conn = meta.pool.acquire().await.unwrap();
        sqlx::query("PRAGMA ignore_check_constraints = ON")
            .execute(&mut *conn)
            .await
            .unwrap();
        sqlx::query("UPDATE refs SET active = 2 WHERE name = ?")
            .bind(DEFAULT_REF_NAME)
            .execute(&mut *conn)
            .await
            .unwrap();
        sqlx::query("PRAGMA ignore_check_constraints = OFF")
            .execute(&mut *conn)
            .await
            .unwrap();
        drop(conn);

        let main = RefName::parse(DEFAULT_REF_NAME).unwrap();
        assert!(matches!(
            meta.lookup_ref(&main).await,
            Err(StorageError::InvalidHistoryValue {
                column: "active",
                value: 2
            })
        ));
    }

    #[tokio::test]
    async fn a_commit_without_an_active_ref_writes_nothing() {
        let mut meta = memory_meta().await;
        // A first commit gives the store a history.
        meta.apply_commit_incremental(1_000, &[], &[], None, &[], &[])
            .await
            .unwrap();
        // A store whose active flag was lost (the app never clears it).
        sqlx::query("UPDATE refs SET active = 0")
            .execute(&meta.pool)
            .await
            .unwrap();

        let err = meta
            .apply_commit_incremental(2_000, &[], &[], None, &[], &[])
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
        assert_eq!(commits[0].id, CommitId(1));
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
            .apply_commit_incremental(1_000, &[], &[], None, &[], &[])
            .await
            .unwrap()
            .id;
        let second = meta
            .apply_commit_incremental(2_000, &[root], &[], None, &[], &[])
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
    async fn fingerprints_are_read_within_one_history_only() {
        let mut meta = memory_meta().await;
        let fingerprint = RegionFingerprint {
            key: RegionKey::new(Dimension::OVERWORLD, RegionKind::Region, 0, 0),
            mtime_ms: Some(1),
            size: 8192,
            content_hash: [7; 32],
        };
        let root = meta
            .apply_commit_incremental(1_000, &[], &[], None, &[fingerprint], &[])
            .await
            .unwrap()
            .id;
        // The fingerprint was confirmed at the root: the root's history
        // may carry it.
        assert_eq!(meta.load_region_states(Some(root)).await.unwrap().len(), 1);

        let branch = meta
            .apply_commit_incremental(2_000, &[root], &[], None, &[fingerprint], &[])
            .await
            .unwrap()
            .id;
        // The branch confirmed it again, so only the branch history (and
        // the histories continuing it) may carry it.
        assert!(
            meta.load_region_states(Some(root))
                .await
                .unwrap()
                .is_empty(),
            "a fingerprint confirmed outside the history is not carried"
        );
        assert_eq!(
            meta.load_region_states(Some(branch)).await.unwrap().len(),
            1
        );
        // An unborn history knows no file at all.
        assert_eq!(
            meta.load_region_states(None).await.unwrap(),
            [] as [sekai_core::RegionStateEntry; 0]
        );
    }

    #[tokio::test]
    async fn a_commit_cannot_name_itself_or_a_missing_commit_as_parent() {
        let mut meta = memory_meta().await;
        // The first commit takes id 1, so naming it is a self-reference.
        assert!(matches!(
            meta.create_commit(1_000, &[CommitId(1)]).await,
            Err(StorageError::SelfParent { id: 1 })
        ));
        assert!(matches!(
            meta.create_commit(1_000, &[CommitId(9)]).await,
            Err(StorageError::UnknownCommit { id: 9 })
        ));
        // The rejected attempts left nothing behind.
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
            meta.create_commit(3_000, &[CommitId(2)]).await,
            Err(StorageError::SelfParent { id: 2 })
        ));
        assert!(matches!(
            meta.create_commit(3_000, &[CommitId(9)]).await,
            Err(StorageError::UnknownCommit { id: 9 })
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

    /// The vocabulary rename bumped the schema to 9, so a store stamped
    /// with the previous version must fail the gate - not fail later on a
    /// missing table. The gate reads the version before any table is
    /// touched, so the schema behind the stamp does not matter.
    #[tokio::test]
    async fn a_store_from_the_previous_schema_version_is_refused() {
        let dir = tempdir("version-8");
        let path = dir.join("meta.sqlite");
        let mut seed = SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(true)
            .connect()
            .await
            .unwrap();
        sqlx::query(SCHEMA).execute(&mut seed).await.unwrap();
        sqlx::query("PRAGMA user_version=8")
            .execute(&mut seed)
            .await
            .unwrap();
        seed.close().await.unwrap();
        assert!(matches!(
            SqliteMeta::open(&path).await,
            Err(StorageError::UnsupportedSchema {
                found: 8,
                supported: SCHEMA_VERSION,
            })
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn wiped_derived_state_degrades_to_full_ingest() {
        let mut meta = memory_meta().await;
        let key = RegionKey::new(Dimension::OVERWORLD, RegionKind::Region, 0, 0);
        let fp = RegionFingerprint {
            key,
            mtime_ms: Some(1),
            size: 8192,
            content_hash: [7; 32],
        };
        let coord = ChunkCoord::new(Dimension::OVERWORLD, RegionKind::Region, 0, 0);
        meta.apply_commit_incremental(
            1_000,
            &[],
            &[stage_present(coord, BlobHash([1; 32]))],
            None,
            &[fp],
            &[],
        )
        .await
        .unwrap();
        sqlx::query("DELETE FROM region_state")
            .execute(&meta.pool)
            .await
            .unwrap();
        let (previous, plan) = plan_commit(
            &meta,
            &[Observation {
                key,
                fingerprint: fp,
            }],
            &Scope::World,
        )
        .await
        .unwrap();
        assert_eq!(plan.carries, [] as [sekai_core::RegionKey; 0]);
        assert_eq!(plan.ingest, vec![key]);
        let staged = assemble(
            plan,
            &previous,
            Ingested {
                entries: vec![stage_present(coord, BlobHash([1; 32]))],
                present: BTreeSet::from([coord]),
                new_blobs: 0,
                fingerprints: vec![fp],
            },
            &Scope::World,
        );
        let report = record_commit(&mut meta, &previous, &staged, 2_000)
            .await
            .unwrap();
        assert_eq!(report.carried_chunks, 0);
        assert_eq!(report.skipped_regions, 0);
    }

    #[tokio::test]
    async fn carry_writes_no_chunk_rows() {
        use sekai_core::MetaStore as _;
        let mut meta = memory_meta().await;
        let key0 = RegionKey::new(Dimension::OVERWORLD, RegionKind::Region, 0, 0);
        let key1 = RegionKey::new(Dimension::OVERWORLD, RegionKind::Region, 1, 0);
        let fp = |key| RegionFingerprint {
            key,
            mtime_ms: Some(1),
            size: 8192,
            content_hash: [7; 32],
        };
        let c0 = ChunkCoord::new(Dimension::OVERWORLD, RegionKind::Region, 0, 0);
        let c1 = ChunkCoord::new(Dimension::OVERWORLD, RegionKind::Region, 32, 0);
        // The commit belongs to the store's active ref, so the next
        // commit continues it.
        meta.apply_commit_incremental(
            1_000,
            &[],
            &[
                stage_present(c0, BlobHash([1; 32])),
                stage_present(c1, BlobHash([2; 32])),
            ],
            None,
            &[fp(key0), fp(key1)],
            &[],
        )
        .await
        .unwrap();

        // Second commit: region 0 changes, region 1 carries.
        let mut changed = fp(key0);
        changed.size += 1;
        let (previous, plan) = plan_commit(
            &meta,
            &[
                Observation {
                    key: key0,
                    fingerprint: changed,
                },
                Observation {
                    key: key1,
                    fingerprint: fp(key1),
                },
            ],
            &Scope::World,
        )
        .await
        .unwrap();
        let staged = assemble(
            plan,
            &previous,
            Ingested {
                entries: vec![stage_present(c0, BlobHash([9; 32]))],
                present: BTreeSet::from([c0]),
                new_blobs: 1,
                fingerprints: vec![changed],
            },
            &Scope::World,
        );
        let report = record_commit(&mut meta, &previous, &staged, 2_000)
            .await
            .unwrap();
        assert_eq!(report.carried_chunks, 1);

        // Physical rows at the new commit: the fresh entry only. The
        // carried chunk stays readable through fallback.
        let physical: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM chunk_history WHERE commit_id = ?")
                .bind(commit_param(report.commit).unwrap())
                .fetch_one(&meta.pool)
                .await
                .unwrap();
        assert_eq!(physical, 1);
        let kept = meta
            .lookup_chunk(report.commit, &c1)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(kept.blob, Some(BlobHash([2; 32])));
        assert_eq!(kept.commit, CommitId(1));
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

    #[tokio::test]
    async fn unknown_region_state_kind_fails_loudly() {
        use sekai_core::MetaStore as _;
        let mut meta = memory_meta().await;
        let fingerprint = RegionFingerprint {
            key: RegionKey::new(Dimension::OVERWORLD, RegionKind::Region, 0, 0),
            mtime_ms: Some(1),
            size: 8192,
            content_hash: [7; 32],
        };
        let commit = meta
            .apply_commit_incremental(1_000, &[], &[], None, &[fingerprint], &[])
            .await
            .unwrap()
            .id;
        sqlx::query("UPDATE region_state SET kind = 9")
            .execute(&meta.pool)
            .await
            .unwrap();
        // The fingerprint is read within the history that confirmed it.
        assert!(matches!(
            meta.load_region_states(Some(commit)).await,
            Err(StorageError::UnknownRegionKind { kind: 9 })
        ));
    }
}
