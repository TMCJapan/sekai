//! Whole-file reads and atomic swaps.

use std::fs;
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::error::WorldError;

static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

struct TempFileGuard<'a>(&'a Path);

impl Drop for TempFileGuard<'_> {
    fn drop(&mut self) {
        let _ = fs::remove_file(self.0);
    }
}

/// Read a whole file into memory with path-carrying errors.
pub fn open_image(path: &Path) -> Result<Vec<u8>, WorldError> {
    fs::read(path).map_err(|e| WorldError::io(path, e))
}

/// Swap `image` into place at `target` atomically.
pub fn atomic_swap(target: &Path, image: &[u8]) -> Result<(), WorldError> {
    let parent = target
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));

    fs::create_dir_all(parent).map_err(|e| WorldError::io(parent, e))?;

    let file_name = target.file_name().map_or_else(
        || "region.mca".to_owned(),
        |n| n.to_string_lossy().into_owned(),
    );

    // Use `create_new` so a stale `*.tmp-*` left by a crashed previous
    // process is not truncated; retry with a fresh counter on collision.
    let (tmp, mut file) = {
        loop {
            let candidate = parent.join(format!(
                "{file_name}.tmp-{}-{}",
                std::process::id(),
                TMP_COUNTER.fetch_add(1, Ordering::Relaxed)
            ));
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&candidate)
            {
                Ok(f) => break (candidate, f),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(WorldError::io(&candidate, e)),
            }
        }
    };

    #[allow(clippy::collection_is_never_read)]
    let mut guard = Some(TempFileGuard(&tmp));

    file.write_all(image).map_err(|e| WorldError::io(&tmp, e))?;
    file.sync_all().map_err(|e| WorldError::io(&tmp, e))?;
    drop(file);

    fs::rename(&tmp, target).map_err(|e| WorldError::io(target, e))?;

    // Rename succeeded; disarm the cleanup guard.
    guard.take();

    #[cfg(unix)]
    {
        let dir = fs::File::open(parent).map_err(|e| WorldError::io(parent, e))?;
        dir.sync_all().map_err(|e| WorldError::io(parent, e))?;
    }

    Ok(())
}
