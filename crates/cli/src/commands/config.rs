//! Config subcommand execution, DTOs, and output rendering.
//!
//! Values print verbatim (no styling): a read must be safe to capture in
//! a shell variable. Set and unset are silent on success, like git.

use anyhow::{Context as _, bail};
use sekai_app::SekaiStoreOpener as _;
use sekai_storage_local::{LocalStore, config::Config};
use serde::Serialize;

use crate::cli::ConfigArgs;
use crate::envelope::envelope_ok;

pub async fn run(store: &str, args: &ConfigArgs) -> anyhow::Result<()> {
    let opened = LocalStore::open(store)
        .await
        .with_context(|| format!("open of {store} failed"))?;

    if args.list {
        let config = read(&opened, store)?;
        let entries: Vec<_> = config.entries().collect();
        if args.json {
            let payload: Vec<_> = entries
                .iter()
                .map(|&(key, value)| EntryJson {
                    key: key.name(),
                    value,
                })
                .collect();
            println!("{}", envelope_ok("config", &payload)?);
            return Ok(());
        }
        for (key, value) in entries {
            if args.name_only {
                println!("{key}");
            } else {
                println!("{key}={value}");
            }
        }
        return Ok(());
    }

    // clap requires a key for every action but `--list`.
    let key = args.key.context("a config key is required")?;

    if let Some(value) = &args.value {
        let mut config = read(&opened, store)?;
        config.set(key, value.clone());
        write(&opened, store, &config)?;
        if args.json {
            println!(
                "{}",
                envelope_ok(
                    "config",
                    &EntryJson {
                        key: key.name(),
                        value,
                    }
                )?
            );
        }
        return Ok(());
    }

    if args.unset {
        let mut config = read(&opened, store)?;
        if config.get(key).is_none() {
            bail!("config key '{key}' is not set");
        }
        config.unset(key);
        write(&opened, store, &config)?;
        if args.json {
            println!("{}", envelope_ok("config", &KeyJson { key: key.name() })?);
        }
        return Ok(());
    }

    // A bare key or `--get`: read, falling back to `--default`.
    let config = read(&opened, store)?;
    let Some(value) = config.get(key).or(args.default.as_deref()) else {
        bail!("config key '{key}' is not set");
    };
    if args.json {
        println!(
            "{}",
            envelope_ok(
                "config",
                &EntryJson {
                    key: key.name(),
                    value,
                }
            )?
        );
    } else {
        println!("{value}");
    }
    Ok(())
}

fn read(opened: &LocalStore, store: &str) -> anyhow::Result<Config> {
    opened
        .read_config()
        .with_context(|| format!("config read for {store} failed"))
}

fn write(opened: &LocalStore, store: &str, config: &Config) -> anyhow::Result<()> {
    opened
        .write_config(config)
        .with_context(|| format!("config write for {store} failed"))
}

/// One set key with its value: the get/set result and a `--list` entry.
#[derive(Serialize)]
struct EntryJson<'a> {
    key: &'a str,
    value: &'a str,
}

/// The unset result: the key that was removed.
#[derive(Serialize)]
struct KeyJson<'a> {
    key: &'a str,
}

#[cfg(test)]
mod tests {
    use super::*;
    use sekai_storage_local::config::ConfigKey;

    const KEY: ConfigKey = ConfigKey::UserName;

    #[test]
    fn renders_entry_payload() {
        let payload = EntryJson {
            key: KEY.name(),
            value: "foo",
        };
        assert_eq!(
            serde_json::to_string(&payload).unwrap(),
            r#"{"key":"user.name","value":"foo"}"#
        );
    }

    #[test]
    fn renders_key_payload() {
        let payload = KeyJson { key: KEY.name() };
        assert_eq!(
            serde_json::to_string(&payload).unwrap(),
            r#"{"key":"user.name"}"#
        );
    }

    #[test]
    fn renders_list_payload() {
        let payload = vec![EntryJson {
            key: KEY.name(),
            value: "foo",
        }];
        assert_eq!(
            serde_json::to_string(&payload).unwrap(),
            r#"[{"key":"user.name","value":"foo"}]"#
        );
    }
}
