//! Opened backup store: CAS plus metadata under one root.
//!
//! Rationale: the on-disk store layout (`<root>/blobs`, `<root>/meta.sqlite`)
//! is fixed here so the CLI never assembles paths itself. Directory
//! creation is explicit (never hidden inside reads) and happens once at
//! open.

use std::path::Path;

use sekai_core::{MetaStore as _, Snapshot};
use sekai_storage::{FileCas, SqliteMeta};

use crate::error::EngineError;

/// Opened store handle handed to [`backup`](crate::backup) and [`rollback`](crate::rollback).
#[derive(Debug)]
pub struct Store {
    /// Content-addressed blob files.
    pub cas: FileCas,
    /// SQLite snapshot and history metadata.
    pub meta: SqliteMeta,
}

impl Store {
    /// Open (creating if needed) the store rooted at `root`.
    pub fn open(root: &Path) -> Result<Self, EngineError> {
        let io = |path: std::path::PathBuf| {
            move |source: std::io::Error| EngineError::Io { path, source }
        };
        std::fs::create_dir_all(root).map_err(io(root.to_path_buf()))?;
        let cas = FileCas::open(root)?;
        let meta = SqliteMeta::open(&root.join("meta.sqlite"))?;
        Ok(Self { cas, meta })
    }
}

/// List all snapshots in ID order (for CLI `list` and pre-flight checks).
pub fn list_snapshots(store: &Store) -> Result<Vec<Snapshot>, EngineError> {
    let mut out = Vec::new();
    store
        .meta
        .visit_snapshots(|s| {
            out.push(*s);
            true
        })
        .map_err(EngineError::from)?;
    Ok(out)
}
