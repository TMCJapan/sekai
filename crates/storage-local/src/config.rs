//! Store format version (`<root>/version`), read and version-gated before
//! any database opens.

use std::io::Write as _;
use std::path::Path;

use sekai_storage::{StorageError, io_error};
use serde::{Deserialize, Serialize};

/// Version file name at the store root.
pub const FILE_NAME: &str = "version";

/// Version of the store format this binary writes and accepts.
pub const CONFIG_VERSION: u32 = 1;

/// Store format version persisted at `<root>/version`.
///
/// Provisional shape: only the format version is tracked so far.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Config format version.
    pub version: u32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            version: CONFIG_VERSION,
        }
    }
}

impl Config {
    /// Read `<root>/version` and enforce the format version.
    pub fn read(root: impl AsRef<Path>) -> Result<Self, StorageError> {
        let path = root.as_ref().join(FILE_NAME);
        let text = std::fs::read_to_string(&path)
            .map_err(|source| io_error(path.display().to_string(), source))?;
        let config: Self =
            toml::from_str(&text).map_err(|source| StorageError::backend("toml", source))?;
        if config.version != CONFIG_VERSION {
            return Err(StorageError::UnsupportedSchema {
                found: i64::from(config.version),
                supported: i64::from(CONFIG_VERSION),
            });
        }
        Ok(config)
    }
}

/// Materialize the version file on first init, then read it back through the
/// same gate as open; re-initialization keeps the existing file.
///
/// `create_new` keeps concurrent inits from overwriting each other: the loser
/// of the create race reads the winner's file instead. The write is synced so
/// a crash cannot leave a torn version file that no later open could parse.
pub fn init(root: impl AsRef<Path>) -> Result<Config, StorageError> {
    let path = root.as_ref().join(FILE_NAME);
    let text = toml::to_string(&Config::default())
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
    Config::read(root)
}
