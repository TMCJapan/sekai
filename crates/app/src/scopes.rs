//! Named scopes: store-saved world areas selected by name.
//!
//! A scope names no commit: history reads never touch it, and deleting
//! one changes no history. It is resolved when a command runs, not when a
//! commit is recorded, so replacing a scope changes what later runs select
//! and never what earlier commits hold.

use sekai_core::{Dimension, NamedScope, Rect, Scope, ScopeName};
use sekai_storage::{MetaStore as _, SekaiStore};
use sekai_util::ThreadRunner;

use crate::error::{AppError, Lookup, NameKind};
use crate::instance::SekaiInstance;
use crate::time::now_ms;

/// The error for a scope name the store does not hold.
fn not_found(name: &ScopeName) -> AppError {
    AppError::NotFound {
        kind: Lookup::Scope,
        name: name.as_str().to_owned(),
    }
}

impl<S: SekaiStore, T: ThreadRunner> SekaiInstance<S, T> {
    /// Save the areas as the scope `name`, covering every region family.
    ///
    /// A selection without an area fails with [`AppError::InvalidScope`]
    /// before the store is touched. An existing
    /// name fails with [`AppError::AlreadyExists`] unless `force` replaces
    /// it; a replacement keeps the original creation time and swaps every
    /// area at once. Returns the record as stored.
    ///
    /// `force` is a policy of this layer: the trait's `set_scope` always
    /// replaces.
    pub async fn create_scope(
        &mut self,
        name: &ScopeName,
        areas: Vec<(Dimension, Rect)>,
        force: bool,
    ) -> Result<NamedScope, AppError> {
        // Validated before the store is touched; the placeholder time is
        // set below, once the lookup tells creation from replacement.
        let mut record = NamedScope::new(name.clone(), areas, 0)?;
        record.created_at_ms = match self.store.meta().lookup_scope(name).await? {
            Some(_) if !force => {
                return Err(AppError::AlreadyExists {
                    kind: NameKind::Scope,
                    name: name.as_str().to_owned(),
                });
            }
            Some(existing) => existing.created_at_ms,
            None => now_ms()?,
        };
        self.store.meta_mut().set_scope(&record).await?;
        Ok(record)
    }

    /// Remove `name`; fails with [`AppError::NotFound`] when no scope has
    /// that name.
    pub async fn delete_scope(&mut self, name: &ScopeName) -> Result<(), AppError> {
        if self.store.meta_mut().delete_scope(name).await? {
            Ok(())
        } else {
            Err(not_found(name))
        }
    }

    /// List scopes in name order.
    pub async fn list_scopes(&self) -> Result<Vec<NamedScope>, AppError> {
        Ok(sekai_storage::scopes::list_scopes(self.store.meta()).await?)
    }

    /// The selection the scope `name` stands for, for a `--scope` run.
    ///
    /// An unknown name fails with [`AppError::NotFound`]: only the
    /// store knows its scope names, so the caller cannot check earlier.
    ///
    /// The scope is resolved once per run; a concurrent replacement affects
    /// only later runs.
    pub async fn resolve_scope(&self, name: &ScopeName) -> Result<Scope, AppError> {
        self.store
            .meta()
            .lookup_scope(name)
            .await?
            .map(|record| record.to_scope())
            .ok_or_else(|| not_found(name))
    }
}
