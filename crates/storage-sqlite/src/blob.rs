//! SQLite-backed content-addressed blob store (`blob.sqlite`).
//!
//! Blobs are the raw sector payloads ingest captured, keyed by their hash and
//! never rewritten. One `WITHOUT ROWID` table indexed by the hash itself holds
//! them, so the store needs no sharded directory tree: it is a single database
//! file beside `meta.sqlite`, moved and backed up as a unit.
//!
//! Durability follows the architecture's crash order. A `put` returns only
//! once its row is committed, and `synchronous = FULL` makes that commit fsync
//! the WAL, so blobs are on disk before any metadata names them. `sync`
//! afterwards is the batched barrier: it checkpoints the WAL back into the
//! main file and truncates it, so the WAL stays bounded between commits.
//! `remove` runs in autocommit mode like every other statement, so a removal
//! lands on its own; GC and commit both hold the store's writer lock, which
//! already keeps them out of each other's way.
//!
//! Async operations share one sqlx pool. Blocking operations run on the
//! `spawn_blocking` workers of ingest, checkout, and clone, so they cannot
//! borrow the async runtime: they own a second connection and drive it with a
//! plain thread-parking executor. SQLite answers without a reactor, so this
//! never needs (or stalls) the caller's runtime.

use std::path::{Path, PathBuf};
use std::pin::pin;
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::{Context, Poll, Wake, Waker};
use std::time::Duration;

use sekai_core::{BlobHash, BlobStore};
use sekai_storage::{BlockingBlobStore, StorageError, StoreLock, StoreLockMode, lock_store};
use sqlx::sqlite::{
    SqliteConnectOptions, SqliteConnection, SqliteJournalMode, SqlitePool, SqlitePoolOptions,
    SqliteSynchronous,
};
use sqlx::{AssertSqlSafe, ConnectOptions as _};

/// Version of the blob database (`PRAGMA user_version`).
///
/// Blob rows are opaque payloads with no derived state to reinterpret, so this
/// gate only guards the storage format itself. Bump it for any change to the
/// table below; pre-release policy recreates, it never migrates.
const SCHEMA_VERSION: i64 = 1;

const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS blobs (
        hash BLOB NOT NULL PRIMARY KEY CHECK (length(hash) = 32),
        payload BLOB NOT NULL
    ) WITHOUT ROWID;";

/// How long a connect keeps retrying `SQLITE_BUSY`, matching sqlx's default
/// `busy_timeout`. Switching a fresh database into WAL needs an exclusive
/// lock the busy handler cannot wait on, so a connect racing another opener's
/// switch fails at once and is retried here.
const CONNECT_BUSY_TIMEOUT: Duration = Duration::from_secs(5);

const INSERT_SQL: &str = "INSERT OR IGNORE INTO blobs (hash, payload) VALUES (?1, ?2)";
const CONTAINS_SQL: &str = "SELECT 1 FROM blobs WHERE hash = ?1";
const FETCH_SQL: &str = "SELECT payload FROM blobs WHERE hash = ?1";
const REMOVE_SQL: &str = "DELETE FROM blobs WHERE hash = ?1";
const HASHES_SQL: &str = "SELECT hash FROM blobs ORDER BY hash";
const CHECKPOINT_SQL: &str = "PRAGMA wal_checkpoint(TRUNCATE)";

/// Map a backend failure into the shared storage error.
fn db_err(source: sqlx::Error) -> StorageError {
    StorageError::backend("sqlite", source)
}

/// Build the missing-blob error once, for both trait sides.
fn missing(hash: &BlobHash) -> StorageError {
    StorageError::BlobMissing {
        hex: hash.hex_string(),
    }
}

/// Decode a 32-byte hash column.
fn hash_col(bytes: Vec<u8>) -> Result<BlobHash, StorageError> {
    bytes
        .try_into()
        .map(BlobHash)
        .map_err(|bytes: Vec<u8>| StorageError::InvalidHashLength { len: bytes.len() })
}

/// Waker that unparks the thread driving the future.
struct ThreadWaker(std::thread::Thread);

impl Wake for ThreadWaker {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.0.unpark();
    }
}

/// Drive a future that never times out to completion on the current thread.
///
/// SQLite queries resolve through sqlx's per-connection worker thread and
/// never need the tokio reactor, so parking this thread until that worker
/// wakes it is enough. `Handle::block_on` is not an option here: the blocking
/// trait is reached from `spawn_blocking` workers, which may run under a
/// current-thread runtime whose drivers this thread must not touch.
fn block_on<F: Future>(future: F) -> F::Output {
    let waker = Waker::from(Arc::new(ThreadWaker(std::thread::current())));
    let mut context = Context::from_waker(&waker);
    let mut future = pin!(future);
    loop {
        match future.as_mut().poll(&mut context) {
            Poll::Ready(output) => return output,
            Poll::Pending => std::thread::park(),
        }
    }
}

/// Connect the blob pool at `path`.
///
/// One connection: ingest shares a handle and SQLite serializes writers
/// anyway, so a queue in front of a single connection costs nothing.
async fn connect_pool(options: &SqliteConnectOptions) -> Result<SqlitePool, StorageError> {
    const SQLITE_BUSY: i32 = 5;
    let is_busy = |err: &dyn sqlx::error::DatabaseError| {
        err.code()
            .and_then(|code| code.parse::<i32>().ok())
            .is_some_and(|code| code & 0xff == SQLITE_BUSY)
    };
    let deadline = tokio::time::Instant::now() + CONNECT_BUSY_TIMEOUT;
    loop {
        match SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options.clone())
            .await
        {
            Err(sqlx::Error::Database(err))
                if is_busy(&*err) && tokio::time::Instant::now() < deadline =>
            {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            Err(source) => return Err(db_err(source)),
            Ok(pool) => return Ok(pool),
        }
    }
}

/// Read `PRAGMA user_version`.
async fn read_user_version<'c>(
    executor: impl sqlx::SqliteExecutor<'c>,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar("PRAGMA user_version")
        .fetch_one(executor)
        .await
}

/// Content-addressed blob store backed by `<store>/blob.sqlite`.
#[derive(Debug, Clone)]
pub struct SqliteBlob {
    path: PathBuf,
    pool: SqlitePool,
    /// Second connection for the blocking trait, serialized by the mutex.
    blocking: Arc<Mutex<SqliteConnection>>,
    /// Root of the store this database lives in: the blocking lock guard is
    /// taken there, like every other store operation.
    root: PathBuf,
}

impl SqliteBlob {
    /// Open (creating and version-gating) the database at `path`.
    ///
    /// The parent directory must already exist; this type never creates it
    /// (directory layout is the store's concern). Opening while another
    /// opener creates the schema waits for it, up to the busy timeout.
    pub async fn open(path: impl AsRef<Path>) -> Result<Self, StorageError> {
        let path = path.as_ref();
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Full);
        let pool = connect_pool(&options).await?;
        let mut version = read_user_version(&pool).await.map_err(db_err)?;
        if version == 0 {
            // Re-read under the write lock so a concurrent opener that won
            // the race does not replay the DDL: `IF NOT EXISTS` keeps that
            // harmless, and the stamp still commits with the DDL.
            let mut tx = pool.begin_with("BEGIN IMMEDIATE").await.map_err(db_err)?;
            version = read_user_version(&mut *tx).await.map_err(db_err)?;
            if version == 0 {
                sqlx::query(SCHEMA)
                    .execute(&mut *tx)
                    .await
                    .map_err(db_err)?;
                sqlx::query(AssertSqlSafe(format!(
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
        let blocking = options.connect().await.map_err(db_err)?;
        let root = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
        Ok(Self {
            path: path.to_path_buf(),
            pool,
            blocking: Arc::new(Mutex::new(blocking)),
            root,
        })
    }

    /// Store root this database lives in (the blocking lock guards it).
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Database file this store owns.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Lock the store root in `mode`; see [`StoreLockMode`] for the matrix.
    pub fn lock(&self, mode: StoreLockMode) -> Result<StoreLock, StorageError> {
        lock_store(&self.root, mode)
    }

    /// The blocking connection, serialized for one operation at a time.
    ///
    /// A poisoned mutex still holds a usable connection: the previous
    /// panic aborts its statement, and SQLite rolls uncommitted work back, so
    /// the store recovers instead of staying wedged for the process lifetime.
    fn blocking_conn(&self) -> MutexGuard<'_, SqliteConnection> {
        self.blocking
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Whether `hash` is stored.
    async fn contains_impl(&self, hash: &BlobHash) -> Result<bool, StorageError> {
        let found: Option<i64> = sqlx::query_scalar(CONTAINS_SQL)
            .bind(&hash.0[..])
            .fetch_optional(&self.pool)
            .await
            .map_err(db_err)?;
        Ok(found.is_some())
    }

    /// Load the blob into `out`, clearing it first.
    async fn fetch_impl(&self, hash: &BlobHash, out: &mut Vec<u8>) -> Result<(), StorageError> {
        out.clear();
        let payload: Option<Vec<u8>> = sqlx::query_scalar(FETCH_SQL)
            .bind(&hash.0[..])
            .fetch_optional(&self.pool)
            .await
            .map_err(db_err)?;
        let payload = payload.ok_or_else(|| missing(hash))?;
        out.extend_from_slice(&payload);
        Ok(())
    }

    /// Store `payload` under `hash`; `true` when newly inserted.
    ///
    /// `INSERT OR IGNORE` makes a concurrent duplicate a deduplicated report
    /// (ADR-0004): the first row wins and its payload is identical by
    /// construction (content-addressed), so a racer never errors and never
    /// rewrites bytes under an existing hash.
    async fn put_impl(&self, hash: &BlobHash, payload: &[u8]) -> Result<bool, StorageError> {
        let inserted = sqlx::query(INSERT_SQL)
            .bind(&hash.0[..])
            .bind(payload)
            .execute(&self.pool)
            .await
            .map_err(db_err)?;
        Ok(inserted.rows_affected() > 0)
    }

    /// Remove `hash`; `false` when already absent.
    async fn remove_impl(&self, hash: &BlobHash) -> Result<bool, StorageError> {
        let removed = sqlx::query(REMOVE_SQL)
            .bind(&hash.0[..])
            .execute(&self.pool)
            .await
            .map_err(db_err)?;
        Ok(removed.rows_affected() > 0)
    }

    /// Visit every stored blob hash; stop early when `visit` returns `false`.
    async fn visit_hashes<F>(&self, mut visit: F) -> Result<(), StorageError>
    where
        F: FnMut(&BlobHash) -> bool + Send,
    {
        let rows = sqlx::query(HASHES_SQL)
            .fetch_all(&self.pool)
            .await
            .map_err(db_err)?;
        for row in &rows {
            let bytes: Vec<u8> = sqlx::Row::try_get(row, "hash").map_err(db_err)?;
            if !visit(&hash_col(bytes)?) {
                break;
            }
        }
        Ok(())
    }

    /// Batched barrier: checkpoint the WAL into the main file.
    ///
    /// `put` already returns durable (`synchronous=FULL` fsyncs each commit),
    /// so this is the between-commits barrier: it folds the WAL back into the
    /// main database file and truncates it, bounding WAL growth. A reader
    /// parked on the WAL can leave the checkpoint incomplete; the WAL stays
    /// valid and the next sync retries the truncation.
    async fn checkpoint(&self) -> Result<(), StorageError> {
        sqlx::query(CHECKPOINT_SQL)
            .execute(&self.pool)
            .await
            .map_err(db_err)?;
        Ok(())
    }

    /// Blocking [`Self::contains_impl`].
    fn blocking_contains(&self, hash: &BlobHash) -> Result<bool, StorageError> {
        let found: Option<i64> = {
            let mut conn = self.blocking_conn();
            block_on(
                sqlx::query_scalar(CONTAINS_SQL)
                    .bind(&hash.0[..])
                    .fetch_optional(&mut *conn),
            )
            .map_err(db_err)?
        };
        Ok(found.is_some())
    }

    /// Blocking [`Self::fetch_impl`].
    fn blocking_fetch(&self, hash: &BlobHash, out: &mut Vec<u8>) -> Result<(), StorageError> {
        out.clear();
        let payload: Option<Vec<u8>> = {
            let mut conn = self.blocking_conn();
            block_on(
                sqlx::query_scalar(FETCH_SQL)
                    .bind(&hash.0[..])
                    .fetch_optional(&mut *conn),
            )
            .map_err(db_err)?
        };
        let payload = payload.ok_or_else(|| missing(hash))?;
        out.extend_from_slice(&payload);
        Ok(())
    }

    /// Blocking [`Self::put_impl`].
    fn blocking_put(&self, hash: &BlobHash, payload: &[u8]) -> Result<bool, StorageError> {
        let inserted = {
            let mut conn = self.blocking_conn();
            block_on(
                sqlx::query(INSERT_SQL)
                    .bind(&hash.0[..])
                    .bind(payload)
                    .execute(&mut *conn),
            )
            .map_err(db_err)?
        };
        Ok(inserted.rows_affected() > 0)
    }

    /// Blocking [`Self::checkpoint`].
    fn blocking_checkpoint(&self) -> Result<(), StorageError> {
        {
            let mut conn = self.blocking_conn();
            block_on(sqlx::query(CHECKPOINT_SQL).execute(&mut *conn)).map_err(db_err)?;
        }
        Ok(())
    }
}

impl BlobStore for SqliteBlob {
    type Error = StorageError;

    async fn contains(&self, hash: &BlobHash) -> Result<bool, Self::Error> {
        self.contains_impl(hash).await
    }

    async fn fetch_into(&self, hash: &BlobHash, out: &mut Vec<u8>) -> Result<(), Self::Error> {
        self.fetch_impl(hash, out).await
    }

    async fn put(&mut self, hash: &BlobHash, payload: &[u8]) -> Result<bool, Self::Error> {
        self.put_impl(hash, payload).await
    }

    async fn remove(&mut self, hash: &BlobHash) -> Result<bool, Self::Error> {
        self.remove_impl(hash).await
    }

    async fn sync(&mut self) -> Result<(), Self::Error> {
        self.checkpoint().await
    }

    async fn visit_blobs<F>(&self, visit: F) -> Result<(), Self::Error>
    where
        F: FnMut(&BlobHash) -> bool + Send,
    {
        self.visit_hashes(visit).await
    }
}

impl BlockingBlobStore for SqliteBlob {
    type LockGuard = StoreLock;

    fn contains_blob(&self, hash: &BlobHash) -> Result<bool, StorageError> {
        self.blocking_contains(hash)
    }

    fn fetch_blob(&self, hash: &BlobHash, out: &mut Vec<u8>) -> Result<(), StorageError> {
        self.blocking_fetch(hash, out)
    }

    fn lock(&self, mode: StoreLockMode) -> Result<Self::LockGuard, StorageError> {
        Self::lock(self, mode)
    }

    fn put_blob(&mut self, hash: &BlobHash, payload: &[u8]) -> Result<bool, StorageError> {
        self.blocking_put(hash, payload)
    }

    fn sync_dirs(&mut self) -> Result<(), StorageError> {
        self.blocking_checkpoint()
    }
}
