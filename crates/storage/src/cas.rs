//! Content-addressed blob files.
//!
//! The CAS is append-only: blobs are content-addressed and immutable, so a
//! `put` of an existing hash is a no-op and no interleaving of writes needs
//! exclusion. Readers never observe a torn blob (temp file + rename), and a
//! blob is durable before any metadata can reference it.
//!
//! Writes are atomic; file data is fsynced in `put`, while shard-directory
//! durability is batched by `BlobStore::sync` before metadata can reference new blobs.
//! That barrier also covers the `blobs/` entry of a newly created shard: a
//! directory's own name lives in its parent, so fsyncing only the shard
//! would leave every blob in a fresh shard unreferenced after a crash.

use std::collections::HashSet;
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use sekai_core::BlobHash;

use crate::api::{StorageError, io_error};

/// Monotonic temp-file disambiguator within this process.
static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Run a blocking filesystem closure on the runtime's blocking pool.
///
/// `FileCas` futures hold every path and payload they need, so the work
/// crosses the thread boundary without borrowing the caller.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, StorageError> + Send + 'static,
) -> Result<T, StorageError> {
    tokio::task::spawn_blocking(work).await.map_err(|source| {
        StorageError::backend("file cas", std::io::Error::other(source.to_string()))
    })?
}

/// Persist a directory's own entries. A no-op on Windows, where `std` has
/// no equivalent to fsyncing a directory handle.
#[cfg(unix)]
fn sync_dir(dir: impl AsRef<Path>) -> Result<(), StorageError> {
    let handle = std::fs::File::open(&dir).map_err(|source| io_error(&dir, source))?;
    handle.sync_all().map_err(|source| io_error(&dir, source))
}

/// Windows stub: keeps the fallible signature so callers stay identical
/// across platforms, even though nothing here can fail.
#[cfg(not(unix))]
#[allow(clippy::unnecessary_wraps, reason = "matches the unix signature")]
fn sync_dir(_dir: impl AsRef<Path>) -> Result<(), StorageError> {
    Ok(())
}

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
    pub fn open(root: impl AsRef<Path>) -> Result<Self, StorageError> {
        let blobs = root.as_ref().join("blobs");
        let existed = blobs.is_dir();
        std::fs::create_dir_all(&blobs).map_err(|source| io_error(&blobs, source))?;
        // A freshly created `blobs/` is itself an entry in the store root:
        // persist it now, while nothing else depends on it yet.
        if !existed {
            sync_dir(root.as_ref())?;
        }
        Ok(Self {
            root: root.as_ref().to_path_buf(),
            ensured_shards: HashSet::new(),
            pending_dir_sync: HashSet::new(),
        })
    }

    /// Storage root.
    pub fn root(&self) -> impl AsRef<Path> {
        &self.root
    }

    /// Claim the store for a run that writes blobs (commit).
    ///
    /// The returned guard removes the marker on drop - including on an
    /// early return or a panic - and [`Self::ensure_idle`] makes `gc`
    /// refuse while it is held. See [`crate::runs`] for why the two must
    /// not overlap.
    pub fn begin_run(&self) -> Result<crate::runs::RunGuard, StorageError> {
        crate::runs::begin_run(&self.root)
    }

    /// Fail when another run currently holds the store.
    pub fn ensure_idle(&self) -> Result<(), StorageError> {
        crate::runs::ensure_idle(&self.root)
    }

    /// File path for `hash` (`blobs/<first byte hex>/<remaining hex>`).
    fn path_of(&self, hash: &BlobHash) -> PathBuf {
        let hex = hash.hex_string();
        let (dir, file) = hex.split_at(2);
        self.root.join("blobs").join(dir).join(file)
    }

    /// Durably store `payload` under `hash`; `true` when newly inserted.
    ///
    /// The payload goes to a same-directory temp file, fsynced, then
    /// renamed over the destination (concurrent readers never see a torn
    /// blob). The rename itself is persisted by the next `BlobStore::sync` call, which
    /// must precede any metadata commit referencing the blob.
    ///
    /// Concurrent puts of the same blob are safe: every writer fsyncs a
    /// complete file, so the stored bytes are identical whichever rename
    /// lands (content-addressed). Racers that observe the winner report
    /// deduplicated instead of erroring; on Windows, where renaming over
    /// an existing file fails, that loser path is what saves the commit.
    /// (`new_blobs` accounting may overcount racing duplicates on
    /// platforms where every rename succeeds; cosmetic only.)
    async fn store(&mut self, hash: BlobHash, payload: Vec<u8>) -> Result<bool, StorageError> {
        let dest = self.path_of(&hash);
        if dest.is_file() {
            return Ok(false);
        }
        // Marked only once it exists: a failed or cancelled create leaves the
        // shard unmarked, so the next `put` tries again instead of failing in
        // `write_blob` with a misleading absence.
        let shard = hash.as_bytes()[0];
        if self.ensured_shards.insert(shard) {
            let dir = dest.parent().map(Path::to_path_buf).unwrap_or_default();
            let created = blocking(move || {
                std::fs::create_dir_all(&dir).map_err(|source| io_error(&dir, source))
            })
            .await;
            if let Err(err) = created {
                self.ensured_shards.remove(&shard);
                return Err(err);
            }
        }
        let is_new = blocking(move || write_blob(&dest, &payload)).await?;
        if is_new {
            self.pending_dir_sync.insert(hash.as_bytes()[0]);
        }
        Ok(is_new)
    }

    /// Persist pending shard-directory renames before metadata commit.
    async fn flush(&mut self) -> Result<(), StorageError> {
        if self.pending_dir_sync.is_empty() {
            return Ok(());
        }
        let blobs = self.root.join("blobs");
        let pending: Vec<u8> = self.pending_dir_sync.iter().copied().collect();
        blocking(move || {
            for shard in &pending {
                sync_dir(blobs.join(format!("{shard:02x}")))?;
            }
            // Blobs in a shard created by this handle only become durable
            // once the shard's own entry in `blobs/` is persisted too.
            sync_dir(&blobs)
        })
        .await?;
        self.pending_dir_sync.clear();
        Ok(())
    }

    /// Visit every stored blob hash. Return `false` to stop early.
    ///
    /// Only exact `<2 hex>/<62 hex>` names are visited; temp leftovers and
    /// foreign files are skipped, so GC never removes them.
    async fn visit_blobs<F>(&self, mut visit: F) -> Result<(), StorageError>
    where
        F: FnMut(&BlobHash) -> bool + Send,
    {
        let blobs = self.root.join("blobs");
        let hashes = blocking(move || scan_blobs(&blobs)).await?;
        for hash in hashes {
            if !visit(&hash) {
                break;
            }
        }
        Ok(())
    }
}

/// Write `payload` to `dest` through a fsynced temp file and a rename.
fn write_blob(dest: impl AsRef<Path>, payload: &[u8]) -> Result<bool, StorageError> {
    let dest = dest.as_ref();
    let io = |path: PathBuf| move |source: std::io::Error| io_error(&path, source);
    // `create_new`: a name left behind by a crashed process (or by a
    // second process that happens to reuse this pid) must never be
    // truncated and reused - draw a fresh counter instead.
    let (tmp, mut file) = loop {
        let candidate = dest.with_extension(format!(
            "tmp-{}-{}",
            std::process::id(),
            TMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(f) => break (candidate, f),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(source) => return Err(io(candidate)(source)),
        }
    };
    let result = (|| {
        file.write_all(payload).map_err(io(tmp.clone()))?;
        file.sync_all().map_err(io(tmp.clone()))?;
        // On wasm `File` is a zero-sized handle without `Drop`; closing
        // eagerly is still the portable way to release native handles
        // before the rename.
        #[allow(clippy::drop_non_drop)]
        drop(file);
        match std::fs::rename(&tmp, dest) {
            Ok(()) => Ok(true),
            Err(_) if dest.is_file() => Ok(false),
            Err(source) => Err(io(dest.to_path_buf())(source)),
        }
    })();
    if !matches!(result, Ok(true)) {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// Hash every file stored under `blobs/`, in shard then name order.
fn scan_blobs(blobs: impl AsRef<Path>) -> Result<Vec<BlobHash>, StorageError> {
    let blobs = blobs.as_ref();
    let mut hashes = Vec::new();
    let shards = std::fs::read_dir(blobs).map_err(|source| io_error(blobs, source))?;
    for shard in shards {
        let shard = shard.map_err(|source| io_error(blobs, source))?;
        if !shard
            .file_type()
            .map_err(|source| io_error(shard.path(), source))?
            .is_dir()
        {
            continue;
        }
        let entries =
            std::fs::read_dir(shard.path()).map_err(|source| io_error(shard.path(), source))?;
        for entry in entries {
            let entry = entry.map_err(|source| io_error(shard.path(), source))?;
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
            hashes.push(hash);
        }
    }
    Ok(hashes)
}

impl sekai_core::BlobStore for FileCas {
    type Error = StorageError;

    async fn contains(&self, hash: &BlobHash) -> Result<bool, Self::Error> {
        // `is_file`, matching `put`: a directory squatting on a blob path
        // is not a stored blob.
        let path = self.path_of(hash);
        blocking(move || Ok(path.is_file())).await
    }

    async fn put(&mut self, hash: &BlobHash, payload: &[u8]) -> Result<bool, Self::Error> {
        self.store(*hash, payload.to_vec()).await
    }

    async fn sync(&mut self) -> Result<(), Self::Error> {
        self.flush().await
    }

    async fn fetch_into(&self, hash: &BlobHash, out: &mut Vec<u8>) -> Result<(), Self::Error> {
        let path = self.path_of(hash);
        let hex = hash.hex_string();
        *out = blocking(move || read_blob(&path, hex)).await?;
        Ok(())
    }

    async fn remove(&mut self, hash: &BlobHash) -> Result<bool, Self::Error> {
        let path = self.path_of(hash);
        blocking(move || remove_blob(&path)).await
    }

    async fn visit_blobs<F>(&self, visit: F) -> Result<(), Self::Error>
    where
        F: FnMut(&BlobHash) -> bool + Send,
    {
        Self::visit_blobs(self, visit).await
    }
}

/// Read one blob file, reporting absence as CAS corruption.
fn read_blob(path: impl AsRef<Path>, hex: String) -> Result<Vec<u8>, StorageError> {
    let path = path.as_ref();
    let mut out = Vec::new();
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            return Err(StorageError::BlobMissing { hex });
        }
        Err(source) => return Err(io_error(path, source)),
    };
    file.read_to_end(&mut out)
        .map_err(|source| io_error(path, source))?;
    Ok(out)
}

/// Remove one blob file; `false` when it was already absent.
fn remove_blob(path: impl AsRef<Path>) -> Result<bool, StorageError> {
    let path = path.as_ref();
    match std::fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(io_error(path, source)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sekai_core::BlobStore as _;
    use std::sync::Barrier;

    /// Drive a CAS future to completion on the test runtime.
    fn run<F: std::future::Future>(future: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(future)
    }

    fn temp_root(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "sekai-cas-{name}-{}-{}",
            std::process::id(),
            TMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn concurrent_same_blob_put_never_errors() {
        let root = temp_root("race");
        let hash = BlobHash([9; 32]);
        let payload = vec![7u8; 1024];
        for _ in 0..25 {
            let _ = std::fs::remove_dir_all(root.join("blobs"));
            let barrier = Barrier::new(8);
            let wins = std::thread::scope(|s| {
                let handles: Vec<_> = (0..8)
                    .map(|_| {
                        s.spawn(|| {
                            let mut cas = FileCas::open(&root).unwrap();
                            barrier.wait();
                            run(cas.put(&hash, payload.as_slice()))
                        })
                    })
                    .collect();
                handles
                    .into_iter()
                    .map(|handle| handle.join().unwrap().unwrap())
                    .collect::<Vec<_>>()
            });
            // Every racer succeeds (losers deduplicate); at least one wrote.
            // Note: on platforms where concurrent renames all succeed, more
            // than one racer may report newly-written (`new_blobs` may
            // overcount racing duplicates); the bytes are identical either
            // way, so this stays a cosmetic inaccuracy, never corruption.
            assert!(wins.iter().any(|win| *win));
            let mut read_back = Vec::new();
            run(FileCas::open(&root)
                .unwrap()
                .fetch_into(&hash, &mut read_back))
            .unwrap();
            assert_eq!(read_back, payload);
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_failed_shard_create_is_retried() {
        // A `put` that cannot create its shard must not mark the shard as
        // ensured: the next `put` has to try again instead of writing into a
        // directory that was never made.
        let root = temp_root("shard-retry");
        let mut cas = FileCas::open(&root).unwrap();
        // Occupy the shard path with a file, so `create_dir_all` fails.
        let hash = BlobHash([0x5a; 32]);
        std::fs::write(root.join("blobs/5a"), b"not a directory").unwrap();
        run(cas.put(&hash, b"payload")).unwrap_err();

        // Clear the obstruction: the retry must now succeed.
        std::fs::remove_file(root.join("blobs/5a")).unwrap();
        assert!(run(cas.put(&hash, b"payload")).unwrap());

        let mut read_back = Vec::new();
        run(cas.fetch_into(&hash, &mut read_back)).unwrap();
        assert_eq!(read_back, b"payload");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_stale_temp_name_is_never_reused() {
        // A temp file left by a crashed process (same pid, same counter)
        // must not be truncated and rewritten: the put draws a fresh name.
        let root = temp_root("stale-tmp");
        let hash = BlobHash([3; 32]);
        let stale = root.join("blobs/03").join(format!(
            "{}.tmp-{}-0",
            &hash.hex_string()[2..],
            std::process::id()
        ));
        std::fs::create_dir_all(stale.parent().unwrap()).unwrap();
        std::fs::write(&stale, b"leftover").unwrap();

        let mut cas = FileCas::open(&root).unwrap();
        assert!(run(cas.put(&hash, b"payload")).unwrap());
        // The leftover is untouched, and the blob landed beside it.
        assert_eq!(std::fs::read(&stale).unwrap(), b"leftover");
        let mut out = Vec::new();
        run(cas.fetch_into(&hash, &mut out)).unwrap();
        assert_eq!(out, b"payload");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn contains_ignores_a_directory_on_a_blob_path() {
        // An externally corrupted store may hold a directory where a blob
        // belongs; it must not read as a stored blob.
        let root = temp_root("dir-squat");
        let hash = BlobHash([5; 32]);
        let path = root.join("blobs/05").join(&hash.hex_string()[2..]);
        std::fs::create_dir_all(&path).unwrap();

        let cas = FileCas::open(&root).unwrap();
        assert!(!run(cas.contains(&hash)).unwrap());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn sync_is_idempotent_and_visits_only_blob_names() {
        let root = temp_root("sync-visit");
        let mut cas = FileCas::open(&root).unwrap();
        let hash = BlobHash([0x2a; 32]);
        assert!(run(cas.put(&hash, b"payload")).unwrap());
        run(cas.sync()).unwrap();
        run(cas.sync()).unwrap();
        // A temp leftover and a foreign file are never visited.
        std::fs::write(root.join("blobs/2a/not-a-blob"), b"x").unwrap();

        let mut seen = Vec::new();
        run(cas.visit_blobs(|hash| {
            seen.push(*hash);
            true
        }))
        .unwrap();
        assert_eq!(seen, vec![hash]);
        let _ = std::fs::remove_dir_all(&root);
    }
}
