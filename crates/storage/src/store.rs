//! Opened backup store: CAS plus metadata under one root.
//!
//! Rationale: the on-disk store layout (`<root>/blobs`, `<root>/meta.sqlite`)
//! is fixed here so the composition root never assembles paths itself.
//! Directory creation is explicit (never hidden inside reads) and happens
//! once at open.

use std::path::Path;

use crate::error::StorageError;
use crate::{FileCas, SqliteMeta};

/// Opened store handle handed to the backup and rollback entry points.
///
/// Fields stay private behind this seam: callers reach blobs and metadata
/// only through the accessors below (and through `core` ports at the call
/// site), so the on-disk layout and the concrete adapter types can evolve
/// without touching orchestration. Tests substitute `:memory:` SQLite and
/// scratch-dir CAS through the same [`Store::open`].
#[derive(Debug)]
pub struct Store {
    /// Content-addressed blob files.
    cas: FileCas,
    /// SQLite snapshot and history metadata.
    meta: SqliteMeta,
}

impl Store {
    /// Open (creating if needed) the store rooted at `root`.
    pub fn open(root: &Path) -> Result<Self, StorageError> {
        let io = |path: std::path::PathBuf| {
            move |source: std::io::Error| StorageError::Io { path, source }
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

    /// Borrow CAS mutably and metadata immutably together.
    ///
    /// Needed by calls that touch both at once (e.g. GC apply, which
    /// re-verifies candidates against fresh metadata while unlinking
    /// blobs). The two handles borrow disjoint fields, so they coexist.
    pub const fn cas_and_meta(&mut self) -> (&mut FileCas, &SqliteMeta) {
        (&mut self.cas, &self.meta)
    }
}
