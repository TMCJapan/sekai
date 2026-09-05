//! Content-addressed blob files.
//!
//! Rationale: blobs are immutable and addressed solely by `BlobHash`, so
//! the layout is a fixed two-level fanout (`blobs/ab/cdef...`) keeping
//! directory sizes bounded without any index. Writes are atomic (same-dir
//! temp + `fsync` + `rename`, plus directory fsync) because the write-path
//! contract promises metadata never references an unflushed blob.

use std::fs;
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use sekai_core::BlobHash;

use crate::error::StorageError;

/// Monotonic temp-file disambiguator within this process.
static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// File-backed CAS rooted at `<root>/` (blobs live under `<root>/blobs/`).
#[derive(Debug, Clone)]
pub struct FileCas {
    /// Storage root (contains `blobs/`).
    root: PathBuf,
}

impl FileCas {
    /// Open (creating if needed) the CAS rooted at `root`.
    pub fn open(root: &Path) -> Result<Self, StorageError> {
        let blobs = root.join("blobs");
        fs::create_dir_all(&blobs).map_err(|source| StorageError::Io {
            path: blobs.clone(),
            source,
        })?;
        Ok(Self {
            root: root.to_path_buf(),
        })
    }

    /// Storage root.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// File path for `hash` (`blobs/<first byte hex>/<remaining hex>`).
    fn path_of(&self, hash: &BlobHash) -> PathBuf {
        let hex = hash.hex_string();
        let blobs = self.root.join("blobs");
        // `hex` is 64 ASCII chars by construction, so both ranges are
        // always char boundaries; the fallback is unreachable-but-total.
        match (hex.get(..2), hex.get(2..)) {
            (Some(dir), Some(file)) => blobs.join(dir).join(file),
            (None, _) | (_, None) => blobs.join(hex),
        }
    }

    /// Ensure the shard directory for `hash` exists.
    fn ensure_shard(&self, hash: &BlobHash) -> Result<PathBuf, StorageError> {
        let path = self.path_of(hash);
        let shard = path.parent().map(Path::to_path_buf).unwrap_or_default();
        fs::create_dir_all(&shard).map_err(|source| StorageError::Io {
            path: shard.clone(),
            source,
        })?;
        Ok(path)
    }

    /// Durably store `payload` under `hash`; `true` when newly inserted.
    ///
    /// The payload is written to a same-directory temp file, fsynced, and
    /// renamed over the destination (no in-place mutation, so concurrent
    /// readers never see a torn blob), then the shard directory is fsynced
    /// to persist the rename itself (Unix-only; see below).
    pub fn put(&mut self, hash: &BlobHash, payload: &[u8]) -> Result<bool, StorageError> {
        let dest = self.ensure_shard(hash)?;
        if dest.exists() {
            return Ok(false);
        }
        let io = |path: PathBuf| move |source: std::io::Error| StorageError::Io { path, source };
        let tmp = dest.with_extension(format!(
            "tmp-{}-{}",
            std::process::id(),
            TMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let write = || -> Result<(), StorageError> {
            let mut f = fs::File::create(&tmp).map_err(io(tmp.clone()))?;
            f.write_all(payload).map_err(io(tmp.clone()))?;
            f.sync_all().map_err(io(tmp.clone()))?;
            drop(f);
            fs::rename(&tmp, &dest).map_err(io(dest.clone()))?;
            // Persist the rename itself. Unix-only: opening a directory
            // with `File::open` fails on Windows (ERROR_ACCESS_DENIED),
            // and std offers no directory-fsync equivalent there.
            #[cfg(unix)]
            {
                let shard = dest.parent().map(Path::to_path_buf).unwrap_or_default();
                let dir = fs::File::open(&shard).map_err(io(shard.clone()))?;
                dir.sync_all().map_err(io(shard.clone()))?;
            }
            Ok(())
        };
        // A lost rename race (two writers, same blob) converges: the loser
        // cleans its temp file and reports the blob as present.
        if let Err(err) = write() {
            let _ = fs::remove_file(&tmp);
            if dest.exists() {
                return Ok(false);
            }
            return Err(err);
        }
        Ok(true)
    }

    /// Load the blob into `out`, clearing it first.
    pub fn fetch_into(&self, hash: &BlobHash, out: &mut Vec<u8>) -> Result<(), StorageError> {
        out.clear();
        let path = self.path_of(hash);
        let mut f = fs::File::open(&path).map_err(|source| {
            if source.kind() == std::io::ErrorKind::NotFound {
                StorageError::BlobMissing {
                    hex: hash.hex_string(),
                }
            } else {
                StorageError::Io {
                    path: path.clone(),
                    source,
                }
            }
        })?;
        f.read_to_end(out).map_err(|source| StorageError::Io {
            path: path.clone(),
            source,
        })?;
        Ok(())
    }
}

impl sekai_core::BlobStore for FileCas {
    type Error = StorageError;

    fn contains(&self, hash: &BlobHash) -> Result<bool, Self::Error> {
        Ok(self.path_of(hash).exists())
    }

    fn put(&mut self, hash: &BlobHash, payload: &[u8]) -> Result<bool, Self::Error> {
        Self::put(self, hash, payload)
    }

    fn fetch_into(&self, hash: &BlobHash, out: &mut Vec<u8>) -> Result<(), Self::Error> {
        Self::fetch_into(self, hash, out)
    }
}
