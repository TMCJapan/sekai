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
///
/// Fields stay private behind this seam: callers reach blobs and metadata
/// only through the accessors below (and through `core` traits at the call
/// site), so the on-disk layout and the concrete adapter types can evolve
/// without touching orchestration. Constructing the concretes here is the
/// adapter edge: tests substitute `:memory:` SQLite and scratch-dir CAS
/// through the same [`Store::open`].
#[derive(Debug)]
pub struct Store {
    /// Content-addressed blob files.
    cas: FileCas,
    /// SQLite snapshot and history metadata.
    meta: SqliteMeta,
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

    /// Blob adapter behind the seam.
    #[must_use]
    pub const fn cas(&self) -> &FileCas {
        &self.cas
    }

    /// Mutable blob adapter (CAS writes are `&mut` by contract).
    #[must_use]
    pub const fn cas_mut(&mut self) -> &mut FileCas {
        &mut self.cas
    }

    /// Metadata adapter behind the seam.
    #[must_use]
    pub const fn meta(&self) -> &SqliteMeta {
        &self.meta
    }

    /// Mutable metadata adapter (snapshot writes are `&mut` by contract).
    #[must_use]
    pub const fn meta_mut(&mut self) -> &mut SqliteMeta {
        &mut self.meta
    }
}

/// List all snapshots in ID order (for CLI `list` and pre-flight checks).
pub fn list_snapshots(store: &Store) -> Result<Vec<Snapshot>, EngineError> {
    let mut out = Vec::new();
    store
        .meta()
        .visit_snapshots(|s| {
            out.push(*s);
            true
        })
        .map_err(EngineError::from)?;
    Ok(out)
}
