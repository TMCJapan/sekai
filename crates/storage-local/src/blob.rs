//! Sled-backed content-addressed blob store (the `blobs/` keyspace).
//!
//! Blobs are the raw sector payloads ingest captured, keyed by their hash and
//! never rewritten. They live in the default tree of a sled keyspace beside
//! `meta.sqlite`, so the store stays a single directory, moved and backed up
//! as a unit.
//!
//! Durability follows the architecture's crash order. A `put` appends to
//! sled's log without fsyncing on its own; `sync` is the batched barrier
//! (`sled::Db::flush`), which callers run before any metadata commit names
//! the new blobs.
//!
//! sled admits a single opener per keyspace: `open` takes an exclusive
//! session lock, and opening the same path again fails while the first
//! handle lives. Clones of an opened handle share that session, so ingest
//! workers still share the CAS without exclusion.
//!
//! sled is a synchronous embedded database, so every operation runs on the
//! runtime's blocking pool, never inline on an async worker.

use std::path::{Path, PathBuf};

use sekai_storage::{BlobStore, StorageError, io_error};
use sekai_util::ChunkHash;

/// Map a backend failure into the shared storage error.
fn db_err(source: sled::Error) -> StorageError {
    StorageError::backend("sled", source)
}

/// Map a blocking-task failure (cancellation or panic) into the shared error.
fn join_err(source: tokio::task::JoinError) -> StorageError {
    StorageError::backend("sled", source)
}

/// Build the missing-blob error once, for both trait sides.
fn missing(hash: &ChunkHash) -> StorageError {
    StorageError::BlobMissing {
        hex: hash.hex_string(),
    }
}

/// Run one blocking sled operation on the runtime's blocking pool.
async fn blocking<T, F>(db: sled::Db, f: F) -> Result<T, StorageError>
where
    F: FnOnce(sled::Db) -> Result<T, StorageError> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(move || f(db))
        .await
        .map_err(join_err)?
}

/// Content-addressed blob store backed by the sled keyspace at `<store>/blobs`.
#[derive(Debug, Clone)]
pub struct SledBlob {
    path: PathBuf,
    db: sled::Db,
}

impl SledBlob {
    /// Open the keyspace at `path`, creating it when missing.
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
        let db = tokio::task::spawn_blocking(move || {
            sled::Config::new().path(opened).open().map_err(db_err)
        })
        .await
        .map_err(join_err)??;
        Ok(Self { path, db })
    }

    /// Keyspace directory this store owns.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl BlobStore for SledBlob {
    type Error = StorageError;

    async fn contains(&self, hash: &ChunkHash) -> Result<bool, Self::Error> {
        let key = *hash.as_bytes();
        blocking(self.db.clone(), move |db| {
            db.contains_key(key).map_err(db_err)
        })
        .await
    }

    async fn fetch_into(&self, hash: &ChunkHash, out: &mut Vec<u8>) -> Result<(), Self::Error> {
        let key = *hash.as_bytes();
        let payload = blocking(self.db.clone(), move |db| db.get(key).map_err(db_err)).await?;
        out.clear();
        let payload = payload.ok_or_else(|| missing(hash))?;
        out.extend_from_slice(&payload);
        Ok(())
    }

    /// Store `payload` under `hash`; `true` when newly inserted.
    ///
    /// The compare-and-swap from absent makes a concurrent duplicate a
    /// deduplicated report (ADR-0004): the first value wins and its payload
    /// is identical by construction (content-addressed), so a racer never
    /// errors and never rewrites bytes under an existing hash.
    async fn put(&mut self, hash: &ChunkHash, payload: &[u8]) -> Result<bool, Self::Error> {
        let key = *hash.as_bytes();
        let payload = payload.to_vec();
        blocking(self.db.clone(), move |db| {
            let swapped = db
                .compare_and_swap(key, None::<&[u8]>, Some(payload))
                .map_err(db_err)?;
            Ok(swapped.is_ok())
        })
        .await
    }

    /// Batched durability barrier: fsync sled's log.
    ///
    /// `put` only appends to the log, so this is what makes the blobs
    /// crash-durable before the metadata commit names them.
    async fn sync(&mut self) -> Result<(), Self::Error> {
        blocking(self.db.clone(), |db| db.flush().map_err(db_err).map(drop)).await
    }

    async fn visit_blobs<F>(&self, mut visit: F) -> Result<(), Self::Error>
    where
        F: FnMut(&ChunkHash) -> bool + Send,
    {
        // The visitor is not `'static`, so it cannot cross into the blocking
        // task: collect the hashes there (sled iterates in stable key order)
        // and visit them on this side.
        let hashes = blocking(self.db.clone(), |db| {
            let mut hashes = Vec::new();
            for entry in db.iter() {
                let (key, _) = entry.map_err(db_err)?;
                let len = key.len();
                let bytes: [u8; 32] = key
                    .as_ref()
                    .try_into()
                    .map_err(|_| StorageError::InvalidHashLength { len })?;
                hashes.push(ChunkHash::new(bytes));
            }
            Ok(hashes)
        })
        .await?;
        for hash in &hashes {
            if !visit(hash) {
                break;
            }
        }
        Ok(())
    }
}
