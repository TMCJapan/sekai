//! Store configuration (`<root>/config.toml`): git-style settings kept
//! beside the history, read and written through an opened store handle.
//!
//! The file is plain TOML over typed serde structs. There is one scope -
//! the store itself - so no global/system layering exists. Keys hold a
//! single value each; an empty string reads as unset and is never written.
//! Unknown sections written by a newer binary are tolerated on read but
//! dropped on the next write (pre-release policy: recreate, don't migrate).

use std::io::Write as _;
use std::path::Path;

use sekai_storage::{StorageError, io_error};
use serde::{Deserialize, Serialize};

use crate::LocalStore;

/// Config file name at the store root.
const FILE_NAME: &str = "config.toml";

/// Temporary sibling the atomic write swaps in.
const TMP_FILE_NAME: &str = "config.toml.tmp";

/// The store configuration, one typed field per section.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    /// Commit author identity.
    #[serde(default)]
    pub user: User,
}

/// The `[user]` section.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct User {
    /// Author name; empty means unset.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
}

/// A known config key, spelled `section.name` like git's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigKey {
    /// `user.name`: the commit author name.
    UserName,
}

impl ConfigKey {
    /// Every known key, in listing order.
    pub const ALL: [Self; 1] = [Self::UserName];

    /// The canonical `section.name` spelling.
    pub const fn name(self) -> &'static str {
        match self {
            Self::UserName => "user.name",
        }
    }
}

impl std::str::FromStr for ConfigKey {
    type Err = UnknownConfigKey;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // Git variable names are case-insensitive; values are not.
        Self::ALL
            .into_iter()
            .find(|key| key.name().eq_ignore_ascii_case(s))
            .ok_or_else(|| UnknownConfigKey(s.to_owned()))
    }
}

impl std::fmt::Display for ConfigKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// A config key this binary does not know.
#[derive(Debug)]
pub struct UnknownConfigKey(pub String);

impl std::fmt::Display for UnknownConfigKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "unknown config key '{}' (known keys: ", self.0)?;
        for (i, key) in ConfigKey::ALL.into_iter().enumerate() {
            if i > 0 {
                f.write_str(", ")?;
            }
            f.write_str(key.name())?;
        }
        f.write_str(")")
    }
}

impl std::error::Error for UnknownConfigKey {}

impl Config {
    /// The key's value, or `None` when unset.
    pub fn get(&self, key: ConfigKey) -> Option<&str> {
        let value = match key {
            ConfigKey::UserName => self.user.name.as_str(),
        };
        (!value.is_empty()).then_some(value)
    }

    /// Set the key's value. An empty value unsets it.
    pub fn set(&mut self, key: ConfigKey, value: String) {
        match key {
            ConfigKey::UserName => self.user.name = value,
        }
    }

    /// Remove the key.
    pub fn unset(&mut self, key: ConfigKey) {
        self.set(key, String::new());
    }

    /// Every set key with its value, in [`ConfigKey::ALL`] order.
    pub fn entries(&self) -> impl Iterator<Item = (ConfigKey, &str)> {
        ConfigKey::ALL
            .into_iter()
            .filter_map(|key| self.get(key).map(|value| (key, value)))
    }
}

impl LocalStore {
    /// Read this store's config. A missing file reads as the default
    /// config.
    pub fn read_config(&self) -> Result<Config, StorageError> {
        load(&self.root)
    }

    /// Write this store's config.
    ///
    /// The write goes to a temporary sibling and swaps in via `rename`,
    /// so a crash never leaves a torn config file.
    pub fn write_config(&self, config: &Config) -> Result<(), StorageError> {
        save(&self.root, config)
    }
}

fn load(root: &Path) -> Result<Config, StorageError> {
    let path = root.join(FILE_NAME);
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            toml::from_str(&text).map_err(|source| StorageError::backend("config.toml", source))
        }
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
        Err(source) => Err(io_error(path.display().to_string(), source)),
    }
}

fn save(root: &Path, config: &Config) -> Result<(), StorageError> {
    let text =
        toml::to_string(config).map_err(|source| StorageError::backend("config.toml", source))?;
    let path = root.join(FILE_NAME);
    let tmp = root.join(TMP_FILE_NAME);
    let write = || -> std::io::Result<()> {
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        std::fs::rename(&tmp, &path)
    };
    write().map_err(|source| io_error(path.display().to_string(), source))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sekai_storage::SekaiStoreOpener as _;
    use std::path::PathBuf;

    /// An initialized, opened store in a temp dir, with its root path.
    async fn open_store(name: &str) -> (LocalStore, PathBuf, tempfile::TempDir) {
        let guard = tempfile::Builder::new()
            .prefix(&format!("sekai-config-{name}-"))
            .tempdir()
            .unwrap();
        let root = guard.path().to_path_buf();
        let store = LocalStore::init(&root).await.unwrap();
        (store, root, guard)
    }

    #[test]
    fn keys_parse_case_insensitively() {
        assert_eq!(
            "user.name".parse::<ConfigKey>().unwrap(),
            ConfigKey::UserName
        );
        assert_eq!(
            "USER.NAME".parse::<ConfigKey>().unwrap(),
            ConfigKey::UserName
        );
        for bad in ["user", "user.email", "name", ""] {
            let err = bad.parse::<ConfigKey>().unwrap_err();
            assert_eq!(
                err.to_string(),
                format!("unknown config key '{bad}' (known keys: user.name)")
            );
        }
    }

    #[test]
    fn get_set_unset_and_entries() {
        let mut config = Config::default();
        assert_eq!(config.get(ConfigKey::UserName), None);
        assert_eq!(config.entries().count(), 0);

        config.set(ConfigKey::UserName, "foo".to_owned());
        assert_eq!(config.get(ConfigKey::UserName), Some("foo"));
        assert_eq!(
            config.entries().collect::<Vec<_>>(),
            [(ConfigKey::UserName, "foo")]
        );

        config.unset(ConfigKey::UserName);
        assert_eq!(config.get(ConfigKey::UserName), None);
        // An empty value reads as unset, never as a stored empty string.
        config.set(ConfigKey::UserName, String::new());
        assert_eq!(config.get(ConfigKey::UserName), None);
    }

    #[test]
    fn toml_roundtrip_skips_unset_keys() {
        let mut config = Config::default();
        config.set(ConfigKey::UserName, "foo".to_owned());
        let text = toml::to_string(&config).unwrap();
        assert_eq!(text, "[user]\nname = \"foo\"\n");
        assert_eq!(toml::from_str::<Config>(&text).unwrap(), config);

        let empty = toml::to_string(&Config::default()).unwrap();
        assert_eq!(toml::from_str::<Config>(&empty).unwrap(), Config::default());
    }

    #[test]
    fn reads_tolerate_unknown_sections_and_missing_files() {
        let config: Config = toml::from_str("[user]\nname = \"foo\"\n[future]\nkey = 1\n").unwrap();
        assert_eq!(config.get(ConfigKey::UserName), Some("foo"));
        assert_eq!(toml::from_str::<Config>("").unwrap(), Config::default());
    }

    #[tokio::test]
    async fn file_roundtrip_through_the_store_root() {
        let (store, root, _guard) = open_store("roundtrip").await;
        assert_eq!(store.read_config().unwrap(), Config::default());

        let mut config = Config::default();
        config.set(ConfigKey::UserName, "foo".to_owned());
        store.write_config(&config).unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join(FILE_NAME)).unwrap(),
            "[user]\nname = \"foo\"\n"
        );
        assert_eq!(store.read_config().unwrap(), config);
        assert!(!root.join(TMP_FILE_NAME).exists());

        config.unset(ConfigKey::UserName);
        store.write_config(&config).unwrap();
        assert_eq!(store.read_config().unwrap(), Config::default());
    }

    #[tokio::test]
    async fn a_store_link_names_the_real_root() {
        let (store, root, _guard) = open_store("link-target").await;
        // The link opens its own session, so the init handle must go first.
        drop(store);
        let link_dir = tempfile::tempdir().unwrap();
        let link = link_dir.path().join("store.link");
        std::fs::write(&link, root.to_str().unwrap()).unwrap();
        let linked = LocalStore::open(&link).await.unwrap();

        let mut config = Config::default();
        config.set(ConfigKey::UserName, "foo".to_owned());
        linked.write_config(&config).unwrap();
        assert!(root.join(FILE_NAME).is_file());
        assert_eq!(linked.read_config().unwrap(), config);
    }

    #[tokio::test]
    async fn a_toml_syntax_error_names_the_file() {
        let (store, root, _guard) = open_store("syntax").await;
        std::fs::write(root.join(FILE_NAME), "[user\n").unwrap();
        let err = store.read_config().unwrap_err();
        assert!(err.to_string().starts_with("config.toml failed:"), "{err}");
    }
}
