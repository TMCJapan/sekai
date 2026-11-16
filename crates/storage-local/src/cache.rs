//! Sled-backed region-image cache (the `cache/` keyspace).
//!
//! Entries map a region node hash (the `region_tree` primary key) to the
//! undecomposed `.mca` image that produced it plus that image's content
//! hash. The image lives in the default tree; the hash lives in a side tree
//! (`mca_hashes`) so the file-level change check reads 32 bytes, never the
//! image. A reserved tree holds the format version.
//!
//! The cache is derived state, so its crash order is the opposite of the
//! blob store's: entries are written after the metadata commit names their
//! region hash, and a torn write leaves at most a missing entry. `sync` is
//! advisory for the same reason.
//!
//! sled admits a single opener per keyspace: `open` takes an exclusive
//! session lock, and opening the same path again fails while the first
//! handle lives. Clones of an opened handle share that session, so workers
//! may populate the cache concurrently.
//!
//! sled is a synchronous embedded database, so every operation runs on the
//! runtime's blocking pool, never inline on an async worker.

use std::path::{Path, PathBuf};

use sekai_core::RegionHash;
use sekai_storage::{CacheStore, StorageError, io_error};
use sled::Transactional as _;

/// Version of the cache keyspace format.
///
/// Bump it for any change to the layout below; pre-release policy
/// recreates, it never migrates.
const SCHEMA_VERSION: i64 = 1;

/// Reserved tree holding the format version, kept apart from the default
/// tree so that tree's keyspace stays exactly the 32-byte region hashes.
const VERSION_TREE: &[u8] = b"__sekai_version";
const VERSION_KEY: &[u8] = b"schema";

/// Side tree mapping region hashes to their image's content hash, so a
/// hash lookup never reads image bytes.
const HASH_TREE: &[u8] = b"mca_hashes";

/// Map a backend failure into the shared storage error.
fn db_err(source: sled::Error) -> StorageError {
    StorageError::backend("sled", source)
}

/// Map a blocking-task failure (cancellation or panic) into the shared error.
fn join_err(source: tokio::task::JoinError) -> StorageError {
    StorageError::backend("sled", source)
}

/// Map a transaction failure into the shared error; with `sled::Error` as
/// the abort type both variants carry the same backend failure.
fn tx_err(source: sled::transaction::TransactionError<sled::Error>) -> StorageError {
    match source {
        sled::transaction::TransactionError::Storage(err)
        | sled::transaction::TransactionError::Abort(err) => db_err(err),
    }
}

/// Run one blocking sled operation on the runtime's blocking pool.
async fn blocking<T, F>(f: F) -> Result<T, StorageError>
where
    F: FnOnce() -> Result<T, StorageError> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(f).await.map_err(join_err)?
}

/// Open the keyspace and enforce the format version on one blocking task:
/// recovery can replay the whole log, and the exclusive session lock sled
/// takes makes the gate race-free without a write-transaction dance.
fn open_gated(path: impl AsRef<Path>) -> Result<(sled::Db, sled::Tree), StorageError> {
    let db = sled::Config::new().path(path).open().map_err(db_err)?;
    let versions = db.open_tree(VERSION_TREE).map_err(db_err)?;
    match versions.get(VERSION_KEY).map_err(db_err)? {
        None => {
            versions
                .insert(VERSION_KEY, &SCHEMA_VERSION.to_le_bytes()[..])
                .map_err(db_err)?;
            // Stamp durably before any entry can be written under the gate.
            db.flush().map_err(db_err)?;
        }
        Some(bytes) => {
            let found = version_of(&bytes)?;
            if found != SCHEMA_VERSION {
                return Err(StorageError::UnsupportedSchema {
                    found,
                    supported: SCHEMA_VERSION,
                });
            }
        }
    }
    let hashes = db.open_tree(HASH_TREE).map_err(db_err)?;
    Ok((db, hashes))
}

/// Decode the stored version marker; anything but eight bytes is corruption,
/// never a version to reinterpret.
fn version_of(bytes: &[u8]) -> Result<i64, StorageError> {
    let octets: [u8; 8] = bytes.try_into().map_err(|_| {
        db_err(sled::Error::Unsupported(format!(
            "malformed cache schema version: {} bytes, expected 8",
            bytes.len()
        )))
    })?;
    Ok(i64::from_le_bytes(octets))
}

/// Region-image cache backed by the sled keyspace at `<store>/cache`.
#[derive(Debug, Clone)]
pub struct SledCache {
    path: PathBuf,
    db: sled::Db,
    hashes: sled::Tree,
}

impl SledCache {
    /// Open (creating and version-gating) the keyspace at `path`.
    ///
    /// The parent directory must already exist; this type never creates it
    /// (directory layout is the store's concern). While another handle holds
    /// the keyspace, this fails instead of waiting: sled admits one opener.
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
        let (db, hashes) = tokio::task::spawn_blocking(move || open_gated(&opened))
            .await
            .map_err(join_err)??;
        Ok(Self { path, db, hashes })
    }

    /// Keyspace directory this cache owns.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl CacheStore for SledCache {
    type Error = StorageError;

    /// Store the (image, hash) pair atomically; `true` when newly inserted.
    ///
    /// The cross-tree transaction keeps the pair inseparable: a reader that
    /// sees the hash sees the image it describes. The existence check makes
    /// a concurrent duplicate an idempotent `false` - the first image wins,
    /// and any image that produced the region hash is a valid representative.
    async fn put(
        &mut self,
        region: RegionHash,
        mca_hash: [u8; 32],
        image: &[u8],
    ) -> Result<bool, Self::Error> {
        let key = *region.as_bytes();
        let image = image.to_vec();
        let db = self.db.clone();
        let hashes = self.hashes.clone();
        blocking(move || {
            (&*db, &hashes)
                .transaction(|(images, hashes)| {
                    if hashes.get(&key[..])?.is_some() {
                        return Ok(false);
                    }
                    images.insert(&key[..], &image[..])?;
                    hashes.insert(&key[..], &mca_hash[..])?;
                    Ok(true)
                })
                .map_err(tx_err)
        })
        .await
    }

    async fn mca_hash(&self, region: RegionHash) -> Result<Option<[u8; 32]>, Self::Error> {
        let key = *region.as_bytes();
        let hashes = self.hashes.clone();
        blocking(move || {
            let Some(bytes) = hashes.get(key).map_err(db_err)? else {
                return Ok(None);
            };
            let len = bytes.len();
            let digest: [u8; 32] = bytes
                .as_ref()
                .try_into()
                .map_err(|_| StorageError::InvalidHashLength { len })?;
            Ok(Some(digest))
        })
        .await
    }

    async fn fetch_image(
        &self,
        region: RegionHash,
        out: &mut Vec<u8>,
    ) -> Result<bool, Self::Error> {
        let key = *region.as_bytes();
        let db = self.db.clone();
        let image = blocking(move || db.get(key).map_err(db_err)).await?;
        out.clear();
        let Some(image) = image else {
            return Ok(false);
        };
        out.extend_from_slice(&image);
        Ok(true)
    }

    /// Advisory flush: the cache is derived, so this only keeps the log
    /// small, never guards a crash order.
    async fn sync(&mut self) -> Result<(), Self::Error> {
        let db = self.db.clone();
        blocking(move || db.flush().map_err(db_err).map(drop)).await
    }
}
