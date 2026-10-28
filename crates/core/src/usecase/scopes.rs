//! Named scopes: store-saved world areas selected by name.
//!
//! A scope names no commit, so history operations (`prune`, `gc`) never
//! read it. Reads here are pure lookups; the mutations live in the
//! storage port and the app layer.

use alloc::vec::Vec;

use crate::port::meta::MetaStore;
use sekai_util::NamedScope;

/// List all scopes in name order (byte-wise, the order `ScopeName`
/// compares by).
///
/// Only the backend can fail here, so its error passes through as is:
/// wrapping it in `CommitError` would report a scope listing as a commit
/// failure.
pub async fn list_scopes<M: MetaStore>(meta: &M) -> Result<Vec<NamedScope>, M::Error> {
    let mut out = Vec::new();
    meta.visit_scopes(|scope| {
        out.push(scope.clone());
        true
    })
    .await?;
    Ok(out)
}
