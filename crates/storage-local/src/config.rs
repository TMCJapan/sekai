//! Store configuration (`<root>/conf.toml`), read and version-gated before
//! any database opens.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use sekai_storage::{StorageError, io_error};
use serde::{Deserialize, Serialize};

/// Config file name at the store root.
pub const FILE_NAME: &str = "conf.toml";

/// Version of the store format this binary writes and accepts.
pub const CONFIG_VERSION: u32 = 1;

/// Store configuration persisted at `<root>/conf.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Config format version.
    pub version: u32,
    /// Store root of the main worktree, shared by every linked worktree.
    ///
    /// When set, the databases (`meta.sqlite`, `blobs/`, `cache/`) resolve
    /// from this directory instead of the root holding this config, so all
    /// worktrees open the one shared store. A relative path joins onto that
    /// root (never the process working directory); an absolute one stands
    /// alone. Absent keeps the databases beside this config.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub main_worktree: Option<PathBuf>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            version: CONFIG_VERSION,
            main_worktree: None,
        }
    }
}

impl Config {
    /// Read `<root>/conf.toml` and enforce the format version.
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

    /// The directory the databases resolve from: [`Self::main_worktree`]
    /// joined onto `root` when set, else `root` itself. One hop only - the
    /// target's own config is never followed, so pointer chains and cycles
    /// cannot redirect an open.
    pub fn data_root(&self, root: impl AsRef<Path>) -> PathBuf {
        let root = root.as_ref();
        self.main_worktree
            .as_deref()
            .map_or_else(|| root.to_path_buf(), |main| root.join(main))
    }
}

/// Materialize the config on first init, then read it back through the same
/// gate as open; re-initialization keeps the existing file.
///
/// `create_new` keeps concurrent inits from overwriting each other: the loser
/// of the create race reads the winner's file instead. The write is synced so
/// a crash cannot leave a torn config that no later open could parse.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_serializes_without_the_pointer() {
        let text = toml::to_string(&Config::default()).unwrap();
        assert_eq!(text, format!("version = {CONFIG_VERSION}\n"));
    }

    #[test]
    fn a_conf_without_the_pointer_reads_as_none() {
        let config: Config = toml::from_str("version = 1\n").unwrap();
        assert_eq!(config.version, CONFIG_VERSION);
        assert_eq!(config.main_worktree, None);
    }

    #[test]
    fn the_pointer_round_trips() {
        let config = Config {
            main_worktree: Some(PathBuf::from("../main/.sekai")),
            ..Config::default()
        };
        let text = toml::to_string(&config).unwrap();
        let read: Config = toml::from_str(&text).unwrap();
        assert_eq!(read.main_worktree, config.main_worktree);
    }

    #[test]
    fn data_root_defaults_to_the_config_root() {
        let root = Path::new("/wt/feature/.sekai");
        assert_eq!(Config::default().data_root(root), root);
    }

    #[test]
    fn data_root_joins_a_relative_pointer_onto_the_config_root() {
        let root = Path::new("/wt/feature/.sekai");
        let config = Config {
            main_worktree: Some(PathBuf::from("../main/.sekai")),
            ..Config::default()
        };
        assert_eq!(
            config.data_root(root),
            Path::new("/wt/feature/.sekai/../main/.sekai")
        );
    }

    #[test]
    fn an_absolute_pointer_stands_alone() {
        let root = Path::new("/wt/feature/.sekai");
        let config = Config {
            main_worktree: Some(PathBuf::from("/srv/main/.sekai")),
            ..Config::default()
        };
        assert_eq!(config.data_root(root), Path::new("/srv/main/.sekai"));
    }
}
