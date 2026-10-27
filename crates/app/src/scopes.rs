//! Named scopes: store-saved world areas selected by name.
//!
//! A scope names no commit: `prune` and `gc` never read it, and deleting
//! one changes no history. It is resolved when a command runs, not when a
//! commit is recorded, so replacing a scope changes what later runs select
//! and never what earlier commits hold. The mutations run under the writer
//! lock and the reads under a shared lock, as the ref operations do.

use sekai_core::{Dimension, MetaStore as _, NamedScope, Rect, RegionKind, Scope, ScopeName};
use sekai_storage::{SekaiStore, StoreLockMode};

use crate::error::AppError;
use crate::instance::SekaiInstance;
use crate::time::now_ms;

impl<S: SekaiStore> SekaiInstance<S> {
    /// Save the areas and region kinds as the scope `name`.
    ///
    /// `kinds = None` selects every region family. A selection that would
    /// select nothing (no area, or an empty kind set) fails with
    /// [`AppError::InvalidScope`] before the store is touched. An existing
    /// name fails with [`AppError::ScopeExists`] unless `force` replaces
    /// it; a replacement keeps the original creation time and swaps every
    /// area at once. Returns the record as stored.
    ///
    /// `force` is a policy of this layer: the port's `set_scope` always
    /// replaces.
    pub async fn create_scope(
        &mut self,
        name: &ScopeName,
        kinds: Option<Vec<RegionKind>>,
        areas: Vec<(Dimension, Rect)>,
        force: bool,
    ) -> Result<NamedScope, AppError> {
        let mut record = NamedScope::new(name.clone(), kinds, areas, 0)?;
        let _lock = self.lock_store(StoreLockMode::Writer)?;
        record.created_at_ms = match self.store.meta().lookup_scope(name).await? {
            Some(_) if !force => {
                return Err(AppError::ScopeExists {
                    name: name.as_str().to_owned(),
                });
            }
            Some(existing) => existing.created_at_ms,
            None => now_ms()?,
        };
        self.store.meta_mut().set_scope(&record).await?;
        Ok(record)
    }

    /// Remove `name`, returning whether a scope was removed: `false` means
    /// no scope had that name, not that the removal failed.
    pub async fn delete_scope(&mut self, name: &ScopeName) -> Result<bool, AppError> {
        let _lock = self.lock_store(StoreLockMode::Writer)?;
        Ok(self.store.meta_mut().delete_scope(name).await?)
    }

    /// List scopes in name order.
    pub async fn list_scopes(&self) -> Result<Vec<NamedScope>, AppError> {
        let _lock = self.lock_store(StoreLockMode::Shared)?;
        sekai_core::usecase::scopes::list_scopes(self.store.meta())
            .await
            .map_err(AppError::Commit)
    }

    /// The selection the scope `name` stands for, for a `--scope` run.
    ///
    /// An unknown name fails with [`AppError::UnknownScope`]: only the
    /// store knows its scope names, so the caller cannot check earlier.
    ///
    /// Takes a shared store lock that is released on return. The scope is
    /// resolved once per run; a concurrent replacement affects only later
    /// runs.
    pub async fn resolve_scope(&self, name: &ScopeName) -> Result<Scope, AppError> {
        let _lock = self.lock_store(StoreLockMode::Shared)?;
        self.store
            .meta()
            .lookup_scope(name)
            .await?
            .map(|record| record.to_scope())
            .ok_or_else(|| AppError::UnknownScope {
                name: name.as_str().to_owned(),
            })
    }
}
