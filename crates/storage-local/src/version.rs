//! Store format version (`<root>/version`), checked before any database
//! opens.

use std::io::Write as _;
use std::path::Path;

use sekai_storage::{StorageError, io_error};
use serde::{Deserialize, Serialize};

/// Version file name at the store root.
pub(crate) const FILE_NAME: &str = "version";

/// Version of the store format this binary writes and accepts.
pub const STORE_VERSION: u32 = 1;

/// The TOML shape of `<root>/version`: `version = N`.
#[derive(Serialize, Deserialize)]
struct VersionFile {
    version: u32,
}

/// Read `<root>/version` and refuse any format but [`STORE_VERSION`].
pub(crate) fn check(root: impl AsRef<Path>) -> Result<(), StorageError> {
    let path = root.as_ref().join(FILE_NAME);
    let text = std::fs::read_to_string(&path)
        .map_err(|source| io_error(path.display().to_string(), source))?;
    let file: VersionFile =
        toml::from_str(&text).map_err(|source| StorageError::backend("toml", source))?;
    if file.version != STORE_VERSION {
        return Err(StorageError::UnsupportedSchema {
            found: i64::from(file.version),
            supported: i64::from(STORE_VERSION),
        });
    }
    Ok(())
}

/// Write the version file on first init, then check it as open does;
/// re-initialization keeps the existing file.
///
/// `create_new` keeps concurrent inits from overwriting each other: the loser
/// of the create race checks the winner's file instead. The write is synced
/// so a crash cannot leave a torn version file that no later open could
/// parse.
pub(crate) fn init(root: impl AsRef<Path>) -> Result<(), StorageError> {
    let path = root.as_ref().join(FILE_NAME);
    let text = toml::to_string(&VersionFile {
        version: STORE_VERSION,
    })
    .map_err(|source| StorageError::backend("toml", source))?;
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
    {
        Ok(mut file) => {
            file.write_all(text.as_bytes())
                .and_then(|()| file.sync_all())
                .map_err(|source| io_error(path.display().to_string(), source))?;
        }
        Err(source) if source.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(source) => return Err(io_error(path.display().to_string(), source)),
    }
    check(root)
}
