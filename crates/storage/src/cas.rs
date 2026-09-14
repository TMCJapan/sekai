//! Content-addressed blob files.
//!
//! Writes are atomic; file data is fsynced in `put_blob`, while shard-directory
//! durability is batched by `sync` before metadata can reference new blobs.

use std::collections::HashSet;
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use core::future::Future;

use sekai_core::BlobHash;

use crate::api::{StorageError, io_error};

/// Monotonic temp-file disambiguator within this process.
static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// File-backed CAS rooted at `<root>/` (blobs live under `<root>/blobs/`).
#[derive(Debug, Clone)]
pub struct FileCas {
    /// Storage root (contains `blobs/`).
    root: PathBuf,
    /// Shards already created by this handle.
    ensured_shards: HashSet<u8>,
    /// Shards whose renames still need a directory fsync.
    pending_dir_sync: HashSet<u8>,
}

impl FileCas {
    /// Open (creating if needed) the CAS rooted at `root`.
    pub fn open(root: &Path) -> Result<Self, StorageError> {
        let blobs = root.join("blobs");
        std::fs::create_dir_all(&blobs).map_err(|source| io_error(&blobs, source))?;
        Ok(Self {
            root: root.to_path_buf(),
            ensured_shards: HashSet::new(),
            pending_dir_sync: HashSet::new(),
        })
    }

    /// Storage root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// File path for `hash` (`blobs/<first byte hex>/<remaining hex>`).
    fn path_of(&self, hash: &BlobHash) -> PathBuf {
        let hex = hash.hex_string();
        let (dir, file) = hex.split_at(2);
        self.root.join("blobs").join(dir).join(file)
    }

    /// Ensure the parent shard directory exists.
    fn ensure_parent(&mut self, dest: &Path, shard_id: u8) -> Result<(), StorageError> {
        if self.ensured_shards.contains(&shard_id) {
            return Ok(());
        }
        let shard = dest.parent().map(Path::to_path_buf).unwrap_or_default();
        std::fs::create_dir_all(&shard).map_err(|source| io_error(&shard, source))?;
        self.ensured_shards.insert(shard_id);
        Ok(())
    }

    /// Durably store `payload` under `hash`; `true` when newly inserted.
    ///
    /// The payload goes to a same-directory temp file, fsynced, then
    /// renamed over the destination (concurrent readers never see a torn
    /// blob). The rename itself is persisted by the next `sync` call, which
    /// must precede any metadata commit referencing the blob.
    fn put_blob(&mut self, hash: &BlobHash, payload: &[u8]) -> Result<bool, StorageError> {
        let dest = self.path_of(hash);
        if dest.exists() {
            return Ok(false);
        }
        self.ensure_parent(&dest, hash.as_bytes()[0])?;
        let io = |path: PathBuf| move |source: std::io::Error| io_error(&path, source);
        let tmp = dest.with_extension(format!(
            "tmp-{}-{}",
            std::process::id(),
            TMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let write = || -> Result<(), StorageError> {
            let mut f = std::fs::File::create(&tmp).map_err(io(tmp.clone()))?;
            f.write_all(payload).map_err(io(tmp.clone()))?;
            f.sync_all().map_err(io(tmp.clone()))?;
            drop(f);
            std::fs::rename(&tmp, &dest).map_err(io(dest.clone()))?;
            Ok(())
        };
        let result = write();
        if result.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        result?;
        self.pending_dir_sync.insert(hash.as_bytes()[0]);
        Ok(true)
    }

    /// Persist pending shard-directory renames before metadata commit.
    pub fn sync_dirs(&mut self) -> Result<(), StorageError> {
        // Directory fsync is only available here on Unix; Windows has no
        // equivalent in `std`.
        #[cfg(unix)]
        {
            let pending: Vec<u8> = self.pending_dir_sync.iter().copied().collect();
            for shard in pending {
                let hex = format!("{shard:02x}");
                let dir = self.root.join("blobs").join(hex);
                let f = std::fs::File::open(&dir).map_err(|source| io_error(&dir, source))?;
                f.sync_all().map_err(|source| io_error(&dir, source))?;
                self.pending_dir_sync.remove(&shard);
            }
        }
        #[cfg(not(unix))]
        self.pending_dir_sync.clear();
        Ok(())
    }

    /// Load the blob into `out`, clearing it first.
    fn fetch_blob(&self, hash: &BlobHash, out: &mut Vec<u8>) -> Result<(), StorageError> {
        out.clear();
        let path = self.path_of(hash);
        let mut f = match std::fs::File::open(&path) {
            Ok(file) => file,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                return Err(StorageError::BlobMissing {
                    hex: hash.hex_string(),
                });
            }
            Err(source) => return Err(io_error(&path, source)),
        };
        f.read_to_end(out)
            .map_err(|source| io_error(&path, source))?;
        Ok(())
    }

    /// Remove `hash`; `false` when already absent.
    fn remove_blob(&self, hash: &BlobHash) -> Result<bool, StorageError> {
        let path = self.path_of(hash);
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(true),
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(source) => Err(io_error(&path, source)),
        }
    }

    /// Visit every stored blob hash. Return `false` to stop early.
    ///
    /// Only exact `<2 hex>/<62 hex>` names are visited; temp leftovers and
    /// foreign files are skipped, so GC never removes them.
    fn scan_blobs<F>(&self, mut visit: F) -> Result<(), StorageError>
    where
        F: FnMut(&BlobHash) -> bool,
    {
        let blobs = self.root.join("blobs");
        let shards = std::fs::read_dir(&blobs).map_err(|source| io_error(&blobs, source))?;
        for shard in shards {
            let shard = shard.map_err(|source| io_error(&blobs, source))?;
            if !shard
                .file_type()
                .map_err(|source| io_error(&shard.path(), source))?
                .is_dir()
            {
                continue;
            }
            let entries = std::fs::read_dir(shard.path())
                .map_err(|source| io_error(&shard.path(), source))?;
            for entry in entries {
                let entry = entry.map_err(|source| io_error(&shard.path(), source))?;
                let path = entry.path();
                if !entry
                    .file_type()
                    .map_err(|source| io_error(&path, source))?
                    .is_file()
                {
                    continue;
                }
                let (Some(dir), Some(file)) = (
                    path.parent()
                        .and_then(|p| p.file_name())
                        .and_then(|n| n.to_str()),
                    path.file_name().and_then(|n| n.to_str()),
                ) else {
                    continue;
                };
                if dir.len() != 2 || file.len() != 62 {
                    continue;
                }
                let mut hex = [0u8; 64];
                hex[..2].copy_from_slice(dir.as_bytes());
                hex[2..].copy_from_slice(file.as_bytes());
                let Ok(hash) = BlobHash::from_hex(&hex) else {
                    continue;
                };
                if !visit(&hash) {
                    return Ok(());
                }
            }
        }
        Ok(())
    }
}

impl sekai_core::BlobStore for FileCas {
    type Error = StorageError;

    fn contains(&self, hash: &BlobHash) -> impl Future<Output = Result<bool, Self::Error>> + Send {
        core::future::ready(Ok(self.path_of(hash).exists()))
    }

    fn put(
        &mut self,
        hash: &BlobHash,
        payload: &[u8],
    ) -> impl Future<Output = Result<bool, Self::Error>> + Send {
        core::future::ready(Self::put_blob(self, hash, payload))
    }

    fn sync(&mut self) -> impl Future<Output = Result<(), Self::Error>> + Send {
        core::future::ready(Self::sync_dirs(self))
    }

    fn fetch_into(
        &self,
        hash: &BlobHash,
        out: &mut Vec<u8>,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send {
        core::future::ready(Self::fetch_blob(self, hash, out))
    }

    fn remove(
        &mut self,
        hash: &BlobHash,
    ) -> impl Future<Output = Result<bool, Self::Error>> + Send {
        core::future::ready(Self::remove_blob(self, hash))
    }

    fn visit_blobs<F>(&self, visit: F) -> impl Future<Output = Result<(), Self::Error>> + Send
    where
        F: FnMut(&BlobHash) -> bool + Send,
    {
        core::future::ready(Self::scan_blobs(self, visit))
    }
}
