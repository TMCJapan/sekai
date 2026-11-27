//! Sled-backed region-image cache (the `cache/` keyspace).
//!
//! Entries map a region node hash (the `region_tree` primary key) to the
//! undecomposed `.mca` image that produced it plus that image's content
//! hash. The image lives in the default tree; the hash lives in a side tree
//! (`mca_hashes`) so the file-level change check reads 32 bytes, never the
//! image.
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

use std::path::Path;

use sekai_storage::{CacheStore, StorageError, io_error};
use sekai_util::RegionHash;
use sled::Transactional as _;

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

/// Region-image cache backed by the sled keyspace at `<store>/cache`.
#[derive(Debug, Clone)]
pub struct SledCache {
    db: sled::Db,
    hashes: sled::Tree,
}

impl SledCache {
    /// Open the keyspace at `path`, creating it when missing.
    ///
    /// The parent directory must already exist; this type never creates it
    /// (directory layout is the store's concern). While another handle holds
    /// the keyspace, this fails instead of waiting: sled admits one opener.
    pub(crate) async fn open(path: impl AsRef<Path>) -> Result<Self, StorageError> {
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
        let (db, hashes) = tokio::task::spawn_blocking(move || {
            let db = sled::Config::new().path(opened).open().map_err(db_err)?;
            let hashes = db.open_tree(HASH_TREE).map_err(db_err)?;
            Ok::<_, StorageError>((db, hashes))
        })
        .await
        .map_err(join_err)??;
        Ok(Self { db, hashes })
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

    /// Wipe both entry trees in one transaction: a reader never sees an
    /// image without its hash, and the keyspace stays openable afterwards.
    /// Keys are snapshotted before the transaction (sled's transactional
    /// trees cannot iterate), so an entry written concurrently may survive -
    /// harmless for derived state. The count is the image tree's key count,
    /// since every entry holds exactly one image and one hash.
    async fn clear(&mut self) -> Result<u64, Self::Error> {
        let db = self.db.clone();
        let hashes = self.hashes.clone();
        blocking(move || {
            let keys = db
                .iter()
                .keys()
                .collect::<Result<Vec<_>, _>>()
                .map_err(db_err)?;
            let removed = keys.len() as u64;
            (&*db, &hashes)
                .transaction(|(images, hashes)| {
                    for key in &keys {
                        images.remove(key)?;
                        hashes.remove(key)?;
                    }
                    Ok(())
                })
                .map_err(tx_err)?;
            db.flush().map_err(db_err)?;
            Ok(removed)
        })
        .await
    }
}
