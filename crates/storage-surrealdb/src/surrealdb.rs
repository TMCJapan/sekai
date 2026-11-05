//! Store handle, schema lifecycle, and backend selection.

use std::fmt;
use std::path::Path;
use std::sync::Arc;
use std::time::SystemTime;

use sekai_storage::{SekaiStore, SekaiStoreOpener, StorageError};
use surrealdb::engine::local::{Db, SurrealKv};
use surrealdb::method::Transaction;
use surrealdb::opt::Config;
use surrealdb::types::NotFoundError;
use surrealdb::{Connection, Surreal};
use tokio::sync::Mutex;

use crate::SCHEMA_VERSION;
use crate::meta::finish_tx;
use crate::row::{MetaRow, RefRow, db_err, ref_record_name};

/// Default `SurrealKV` directory used when the CLI is invoked without a store.
pub const DEFAULT_PATH: &str = ".sekai";

const NAMESPACE: &str = "sekai";
const DATABASE: &str = "sekai";

/// Schema for a fresh store. Every statement is idempotent so an interrupted
/// initialization can be completed by the next `init`.
const SCHEMA: &str = r"
DEFINE TABLE IF NOT EXISTS meta TYPE NORMAL SCHEMALESS;
DEFINE TABLE IF NOT EXISTS blob TYPE NORMAL SCHEMALESS;
DEFINE TABLE IF NOT EXISTS commit TYPE NORMAL SCHEMALESS;
DEFINE TABLE IF NOT EXISTS chunk_history TYPE NORMAL SCHEMALESS;
DEFINE TABLE IF NOT EXISTS ref TYPE NORMAL SCHEMALESS;
DEFINE TABLE IF NOT EXISTS scope TYPE NORMAL SCHEMALESS;
DEFINE INDEX IF NOT EXISTS blob_hash ON blob FIELDS hash UNIQUE;
DEFINE INDEX IF NOT EXISTS commit_seq ON commit FIELDS seq;
DEFINE INDEX IF NOT EXISTS history_key ON chunk_history FIELDS commit, dim, kind, cx, cz UNIQUE;
DEFINE INDEX IF NOT EXISTS history_coord ON chunk_history FIELDS dim, kind, cx, cz, seq;
DEFINE INDEX IF NOT EXISTS ref_name ON ref FIELDS name UNIQUE;
DEFINE INDEX IF NOT EXISTS ref_active ON ref FIELDS active;
DEFINE INDEX IF NOT EXISTS scope_name ON scope FIELDS name UNIQUE;
DEFINE SEQUENCE IF NOT EXISTS commit_seq START 1;
";

/// `SurrealDB`-backed store: metadata and blobs share one embedded database.
///
/// The handle is cheap to clone: every clone reaches the same `SurrealKV`
/// instance. `BlobStore` and `MetaStore` are both implemented here because
/// `SurrealDB` serves both roles, and `SekaiStore` exposes this handle as both
/// adapters.
///
/// `SurrealKV` takes an exclusive lock on the store directory, so one process
/// keeps one open store per path. Clones of this handle share that instance;
/// opening the same path again while it is live fails.
pub struct SurrealdbStore<C: Connection = Db> {
    pub(crate) db: Surreal<C>,
    /// Serializes writes from handles in this process. `SurrealDB` executes
    /// writes optimistically, so two concurrent transactions can conflict
    /// even when they touch different rows; SQLite serialized them for free.
    /// `SurrealKV` already excludes other processes, so one lock per store is
    /// enough.
    pub(crate) write_lock: Arc<Mutex<()>>,
    /// Lazily materialized adapter handles for `SekaiStore::blob_and_meta`.
    ///
    /// The trait needs a mutable blob reference and a shared metadata
    /// reference at the same time. They are the same handle type here, so
    /// returning two views of `self` would alias; two independent handles to
    /// the same database keep the split safe.
    blob: Option<Box<Self>>,
    meta: Option<Box<Self>>,
}

impl<C: Connection> Clone for SurrealdbStore<C> {
    fn clone(&self) -> Self {
        self.handle()
    }
}

impl<C: Connection> fmt::Debug for SurrealdbStore<C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SurrealdbStore")
            .field("db", &self.db)
            .finish_non_exhaustive()
    }
}

impl<C: Connection> SurrealdbStore<C> {
    /// Wrap an existing connection. The caller must select the Sekai
    /// namespace and database (`sekai`/`sekai`) before using it as a store.
    pub fn from_db(db: Surreal<C>) -> Self {
        Self {
            db,
            write_lock: Arc::new(Mutex::new(())),
            blob: None,
            meta: None,
        }
    }

    /// A fresh handle to the same database and write lock.
    fn handle(&self) -> Self {
        Self {
            db: self.db.clone(),
            write_lock: Arc::clone(&self.write_lock),
            blob: None,
            meta: None,
        }
    }

    /// The underlying `SurrealDB` client.
    pub const fn db(&self) -> &Surreal<C> {
        &self.db
    }

    async fn schema_version(&self) -> Result<Option<i64>, StorageError> {
        let row: Result<Option<MetaRow>, surrealdb::Error> =
            self.db.select(("meta", "store")).await;
        match row {
            Ok(row) => Ok(row.map(|row| row.schema_version)),
            Err(error) if missing_meta_table(&error) => Ok(None),
            Err(error) => Err(db_err(error)),
        }
    }
}

/// A brand-new `SurrealKV` database has no `meta` table yet; that is the
/// absence of a store, not a backend failure.
fn missing_meta_table(error: &surrealdb::Error) -> bool {
    matches!(
        error.not_found_details(),
        Some(NotFoundError::Table { name, .. }) if name == "meta"
    )
}

impl SurrealdbStore<Db> {
    /// Open the existing store rooted at `path` (or named by a store URL).
    pub async fn open(path: impl AsRef<Path>) -> Result<Self, StorageError> {
        open_surrealdb(inherent_path(path)?).await
    }

    /// Open the store rooted at `path` (or named by a store URL), creating it
    /// when missing.
    pub async fn init(path: impl AsRef<Path>) -> Result<Self, StorageError> {
        init_surrealdb(inherent_path(path)?).await
    }
}

/// Inherent `open`/`init` accept both plain paths and the same URL grammar as
/// [`SekaiStoreOpener`].
fn inherent_path(path: impl AsRef<Path>) -> Result<String, StorageError> {
    let path = path.as_ref();
    let text = path
        .to_str()
        .ok_or_else(|| StorageError::UnsupportedBackend {
            url: path.display().to_string(),
        })?;
    url_path(text).map(str::to_owned)
}

impl<C: Connection + 'static> SekaiStore for SurrealdbStore<C> {
    type Meta = Self;
    type Blobs = Self;

    fn meta(&self) -> &Self::Meta {
        self
    }

    fn meta_mut(&mut self) -> &mut Self::Meta {
        self
    }

    fn blob(&self) -> &Self::Blobs {
        self
    }

    fn blob_mut(&mut self) -> &mut Self::Blobs {
        self
    }

    fn blob_and_meta(&mut self) -> (&mut Self::Blobs, &Self::Meta) {
        let db = self.db.clone();
        let write_lock = Arc::clone(&self.write_lock);
        let blob = self.blob.get_or_insert_with(|| {
            Box::new(Self {
                db: db.clone(),
                write_lock: Arc::clone(&write_lock),
                blob: None,
                meta: None,
            })
        });
        let meta = self.meta.get_or_insert_with(|| {
            Box::new(Self {
                db,
                write_lock,
                blob: None,
                meta: None,
            })
        });
        (&mut **blob, &**meta)
    }
}

impl SekaiStoreOpener for SurrealdbStore<Db> {
    async fn open(url: &str) -> Result<Self, StorageError> {
        open_surrealdb(url_path(url)?).await
    }

    async fn init(url: &str) -> Result<Self, StorageError> {
        init_surrealdb(url_path(url)?).await
    }
}

/// Strip the schemes this backend owns; bare paths select it too.
fn url_path(url: &str) -> Result<&str, StorageError> {
    let path = url
        .strip_prefix("surrealkv://")
        .or_else(|| url.strip_prefix("surrealdb://"))
        .or_else(|| (!url.contains("://")).then_some(url))
        .ok_or_else(|| StorageError::UnsupportedBackend {
            url: url.to_owned(),
        })?;
    if path.is_empty() {
        return Err(StorageError::EmptyStorePath);
    }
    Ok(path)
}

/// The filesystem component of a store path, without the `?key=value`
/// engine configuration suffix.
///
/// `SurrealKV` reads the query parameters when it opens the store, but the
/// directory checks and creation must not treat them as part of the path.
fn filesystem_path(path: &Path) -> &Path {
    match path.to_str().and_then(|text| text.split_once('?')) {
        Some((filesystem, _)) => Path::new(filesystem),
        None => path,
    }
}

/// Open the existing store rooted at `path`.
///
/// A store carries a `SurrealKV` `manifest/` directory; its absence means the
/// path was never initialized, so this fails without creating anything.
pub async fn open_surrealdb(path: impl AsRef<Path>) -> Result<SurrealdbStore, StorageError> {
    let path = path.as_ref().to_path_buf();
    let filesystem = filesystem_path(&path);
    if !filesystem.join("manifest").is_dir() {
        return Err(StorageError::StoreMissing {
            path: filesystem.to_path_buf(),
        });
    }
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    let mut last_error = None;
    while tokio::time::Instant::now() < deadline {
        match connect(&path).await {
            Ok(store) => match store.schema_version().await {
                Ok(Some(found)) if found == SCHEMA_VERSION => return Ok(store),
                Ok(Some(found)) => {
                    return Err(StorageError::UnsupportedSchema {
                        found,
                        supported: SCHEMA_VERSION,
                    });
                }
                Ok(None) => {
                    last_error = Some(StorageError::StoreMissing {
                        path: filesystem.to_path_buf(),
                    });
                }
                Err(error) => last_error = Some(error),
            },
            Err(error) => last_error = Some(error),
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    Err(last_error.unwrap_or(StorageError::StoreMissing {
        path: filesystem.to_path_buf(),
    }))
}

/// Open the store rooted at `path`, creating and initializing it when missing.
pub async fn init_surrealdb(path: impl AsRef<Path>) -> Result<SurrealdbStore, StorageError> {
    let path = path.as_ref();
    let filesystem = filesystem_path(path);
    std::fs::create_dir_all(filesystem)
        .map_err(|source| sekai_storage::io_error(filesystem, source))?;
    let store = connect(path).await?;
    store.initialize().await?;
    Ok(store)
}

async fn connect(path: impl AsRef<Path>) -> Result<SurrealdbStore, StorageError> {
    let path = path.as_ref().to_path_buf();
    // The embedded SDK deliberately reads no `SURREAL_*` environment
    // variables, but SurrealKV's migration error tells operators to set this
    // one. Bridge it so the documented opt-in works for embedded stores.
    let mut config = Config::new();
    if let Ok(value) = std::env::var("SURREAL_EXPERIMENTAL_UPGRADE_FROM_3X") {
        config = config.datastore_option("experimental_upgrade_from_3x", value);
    }
    // The engine shuts its SurrealKV datastore down asynchronously when the
    // last handle drops. A reopen in the same process can race that shutdown
    // and see the old `LOCK`; retry briefly instead of surfacing a transient
    // failure. A lock held by another process still fails after the deadline.
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    let db = loop {
        match Surreal::new::<SurrealKv>((path.clone(), config.clone())).await {
            Ok(db) => break db,
            Err(error)
                if error.message().to_lowercase().contains("lock")
                    && tokio::time::Instant::now() < deadline =>
            {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            Err(error) => return Err(db_err(error)),
        }
    };
    db.use_ns(NAMESPACE)
        .use_db(DATABASE)
        .await
        .map_err(db_err)?;
    Ok(SurrealdbStore::from_db(db))
}

impl SurrealdbStore<Db> {
    async fn initialize(&self) -> Result<(), StorageError> {
        let existing = self.schema_version().await?;
        if let Some(found) = existing
            && found != SCHEMA_VERSION
        {
            return Err(StorageError::UnsupportedSchema {
                found,
                supported: SCHEMA_VERSION,
            });
        }

        self.db
            .query(SCHEMA)
            .await
            .map_err(db_err)?
            .check()
            .map_err(db_err)?;

        if existing.is_none() {
            self.create_initial_records().await?;
        }
        Ok(())
    }

    async fn create_initial_records(&self) -> Result<(), StorageError> {
        let tx = self.db.clone().begin().await.map_err(db_err)?;
        let result = create_initial_records_tx(&tx).await;
        finish_tx(tx, result).await
    }
}

async fn create_initial_records_tx<C: Connection>(tx: &Transaction<C>) -> Result<(), StorageError> {
    let created: Option<MetaRow> = tx
        .create(("meta", "store"))
        .content(MetaRow {
            schema_version: SCHEMA_VERSION,
        })
        .await
        .map_err(db_err)?;

    if created.is_some() {
        let created_at_ms = now_ms()?;
        let _: Option<RefRow> = tx
            .create(ref_record_name(sekai_core::DEFAULT_REF_NAME))
            .content(RefRow::new_default(created_at_ms)?)
            .await
            .map_err(db_err)?;
    }

    Ok(())
}

fn now_ms() -> Result<u64, StorageError> {
    let millis = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_err(|_| StorageError::ClockBeforeEpoch)?
        .as_millis();
    u64::try_from(millis).map_err(|_| StorageError::ClockOutOfRange)
}
