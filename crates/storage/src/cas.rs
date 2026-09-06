//! Content-addressed blob files.
//!
//! Rationale: blobs are immutable and addressed solely by `BlobHash`, so
//! the layout is a fixed two-level fanout (`blobs/ab/cdef...`) keeping
//! directory sizes bounded without any index. Writes are atomic (same-dir
//! temp + `fsync` + `rename`); shard-directory fsyncs batch in
//! [`FileCas::sync_dirs`] because the write-path contract only promises
//! metadata never references an unflushed blob, and one fsync per touched
//! shard before the metadata commit provides exactly that.

use std::collections::HashSet;
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
    /// Shard ids (`blobs/<first hash byte hex>/`) already created by this
    /// handle, so repeat backups skip redundant `create_dir_all` syscalls.
    /// At most 256 entries; a vanished directory surfaces as a loud I/O
    /// error on write, never silent misplacement.
    ensured_shards: HashSet<u8>,
    /// Shard ids with renames not yet persisted via directory fsync.
    ///
    /// `put` fsyncs file data immediately but defers the directory fsync to
    /// [`BlobStore::sync`](sekai_core::BlobStore::sync): one fsync per
    /// touched shard (at most 256) instead of one per blob. At most 256
    /// entries, drained by `sync`.
    pending_dir_sync: HashSet<u8>,
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
            ensured_shards: HashSet::new(),
            pending_dir_sync: HashSet::new(),
        })
    }

    /// Storage root.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// File path for `hash` (`blobs/<first byte hex>/<remaining hex>`).
    fn path_of(&self, hash: &BlobHash) -> PathBuf {
        // `hex_into` emits ASCII hex by construction; the empty fallback
        // only exists to stay panic-free (no `unwrap` in library code).
        let hex = hash.hex_into();
        let dir = std::str::from_utf8(&hex[..2]).unwrap_or_default();
        let file = std::str::from_utf8(&hex[2..]).unwrap_or_default();
        let blobs = self.root.join("blobs");
        blobs.join(dir).join(file)
    }

    /// Ensure the parent shard directory of `dest` exists.
    ///
    /// Shard ids are single hash bytes (256 directories max), so the cache
    /// stays tiny while turning per-chunk `create_dir_all` into a hash
    /// lookup after warmup.
    fn ensure_parent(&mut self, dest: &Path, shard_id: u8) -> Result<(), StorageError> {
        if self.ensured_shards.contains(&shard_id) {
            return Ok(());
        }
        let shard = dest.parent().map(Path::to_path_buf).unwrap_or_default();
        fs::create_dir_all(&shard).map_err(|source| StorageError::Io {
            path: shard.clone(),
            source,
        })?;
        self.ensured_shards.insert(shard_id);
        Ok(())
    }

    /// Durably store `payload` under `hash`; `true` when newly inserted.
    ///
    /// The payload is written to a same-directory temp file, fsynced, and
    /// renamed over the destination (no in-place mutation, so concurrent
    /// readers never see a torn blob). The rename itself is persisted by the
    /// next [`BlobStore::sync`](sekai_core::BlobStore::sync) call, which must
    /// precede any metadata commit referencing the blob.
    pub fn put(&mut self, hash: &BlobHash, payload: &[u8]) -> Result<bool, StorageError> {
        let dest = self.path_of(hash);
        // Deduplicated chunks return before touching the filesystem beyond
        // one `stat`: only new blobs pay for directory creation and writes.
        // A concurrent writer winning the race between this check and the
        // rename below converges on identical bytes (same hash, same
        // payload); only the `true` count may double-count, which is
        // cosmetic next to the single-writer CLI contract.
        if dest.exists() {
            return Ok(false);
        }
        self.ensure_parent(&dest, hash.0[0])?;
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
        self.pending_dir_sync.insert(hash.0[0]);
        Ok(true)
    }

    /// Persist all renames since the last call via one directory fsync per
    /// touched shard (Unix-only; see below).
    ///
    /// Callers must invoke this after a batch of `put`s and before committing
    /// any metadata referencing the new blobs: file data is fsynced by `put`
    /// itself, but the directory entries only become crash-durable here.
    /// Opening a directory with `File::open` fails on Windows
    /// (`ERROR_ACCESS_DENIED`), and std offers no directory-fsync equivalent
    /// there, so non-Unix callers use the infallible drain below instead.
    #[cfg(unix)]
    fn sync_dirs(&mut self) -> Result<(), StorageError> {
        let io = |path: PathBuf| move |source: std::io::Error| StorageError::Io { path, source };
        // Draining first keeps the set consistent even when a shard
        // fsync fails: the error aborts the caller loudly, and the next
        // backup re-marks only shards it actually rewrites.
        let pending = std::mem::take(&mut self.pending_dir_sync);
        for shard_id in &pending {
            let shard = self.root.join("blobs").join(format!("{shard_id:02x}"));
            let dir = match fs::File::open(&shard) {
                Ok(dir) => dir,
                Err(source) => {
                    // Restore unsynced shards so a retry still covers
                    // them (same as the `sync_all` path below).
                    self.pending_dir_sync.extend(pending);
                    return Err(io(shard)(source));
                }
            };
            if let Err(source) = dir.sync_all() {
                // Restore unsynced shards so a retry still covers them.
                self.pending_dir_sync.extend(pending);
                return Err(io(shard)(source));
            }
        }
        Ok(())
    }

    /// Drain pending shards without persisting (non-Unix: std offers no
    /// directory-fsync equivalent, so there is nothing fallible to do).
    #[cfg(not(unix))]
    fn sync_dirs(&mut self) {
        self.pending_dir_sync.clear();
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

    /// Unlink the blob under `hash`; `false` when already absent.
    ///
    /// The shard directory is fsynced after the unlink so a crash never
    /// resurrects the blob (Unix-only; see `put`).
    pub fn remove(&mut self, hash: &BlobHash) -> Result<bool, StorageError> {
        let path = self.path_of(hash);
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(source) => {
                return Err(StorageError::Io { path, source });
            }
        }
        // Persist the unlink itself. Unix-only: opening a directory
        // with `File::open` fails on Windows (ERROR_ACCESS_DENIED),
        // and std offers no directory-fsync equivalent there.
        #[cfg(unix)]
        {
            let io =
                |path: PathBuf| move |source: std::io::Error| StorageError::Io { path, source };
            let shard = path.parent().map(Path::to_path_buf).unwrap_or_default();
            let dir = fs::File::open(&shard).map_err(io(shard.clone()))?;
            dir.sync_all().map_err(io(shard))?;
        }
        Ok(true)
    }

    /// Visit every stored blob hash, skipping foreign file names.
    ///
    /// Only `<2-hex>/<62-hex>` names decoding as hashes are visited; temp
    /// leftovers and foreign files never surface (and are never GC targets).
    /// A missing `blobs/` directory visits nothing.
    pub fn visit_blobs<F>(&self, mut visit: F) -> Result<(), StorageError>
    where
        F: FnMut(&BlobHash) -> bool,
    {
        let blobs = self.root.join("blobs");
        let shards = match fs::read_dir(&blobs) {
            Ok(entries) => entries,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(source) => {
                return Err(StorageError::Io {
                    path: blobs,
                    source,
                });
            }
        };
        for shard in shards {
            let shard = shard.map_err(|source| StorageError::Io {
                path: blobs.clone(),
                source,
            })?;
            let shard_type = shard.file_type().map_err(|source| StorageError::Io {
                path: shard.path(),
                source,
            })?;
            if !shard_type.is_dir() {
                continue;
            }
            let dir_name = shard.file_name();
            let dir_name = dir_name.to_str().unwrap_or_default();
            if dir_name.len() != 2 {
                continue;
            }
            let files = fs::read_dir(shard.path()).map_err(|source| StorageError::Io {
                path: shard.path(),
                source,
            })?;
            for file in files {
                let file = file.map_err(|source| StorageError::Io {
                    path: shard.path(),
                    source,
                })?;
                let file_name = file.file_name();
                let file_name = file_name.to_str().unwrap_or_default();
                if file_name.len() != 62 {
                    continue;
                }
                let mut hex = [0u8; 64];
                hex[..2].copy_from_slice(dir_name.as_bytes());
                hex[2..].copy_from_slice(file_name.as_bytes());
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

    fn contains(&self, hash: &BlobHash) -> Result<bool, Self::Error> {
        Ok(self.path_of(hash).exists())
    }

    fn put(&mut self, hash: &BlobHash, payload: &[u8]) -> Result<bool, Self::Error> {
        Self::put(self, hash, payload)
    }

    fn sync(&mut self) -> Result<(), Self::Error> {
        #[cfg(unix)]
        {
            Self::sync_dirs(self)
        }
        #[cfg(not(unix))]
        {
            Self::sync_dirs(self);
            Ok(())
        }
    }

    fn fetch_into(&self, hash: &BlobHash, out: &mut Vec<u8>) -> Result<(), Self::Error> {
        Self::fetch_into(self, hash, out)
    }

    fn remove(&mut self, hash: &BlobHash) -> Result<bool, Self::Error> {
        Self::remove(self, hash)
    }

    fn visit_blobs<F>(&self, visit: F) -> Result<(), Self::Error>
    where
        F: FnMut(&BlobHash) -> bool,
    {
        Self::visit_blobs(self, visit)
    }
}
