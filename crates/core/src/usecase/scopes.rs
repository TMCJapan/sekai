//! Named scopes: store-saved world areas selected by name.
//!
//! A scope names no commit, so history operations (`prune`, `gc`) never
//! read it. Reads here are pure lookups; the mutations live in the
//! storage port and the app layer.

use alloc::vec::Vec;

use crate::port::meta::MetaStore;
use sekai_util::NamedScope;

use super::graph::CommitError;

/// List all scopes in name order (byte-wise, the order `ScopeName`
/// compares by).
pub async fn list_scopes<M: MetaStore>(meta: &M) -> Result<Vec<NamedScope>, CommitError<M::Error>> {
    let mut out = Vec::new();
    meta.visit_scopes(|scope| {
        out.push(scope.clone());
        true
    })
    .await
    .map_err(CommitError::Meta)?;
    Ok(out)
}
