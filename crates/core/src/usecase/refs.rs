//! Refs and worktree names (ADR-0035).
//!
//! The store enforces the ref rules; these use cases list refs and pick
//! names for new worktrees.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::vec::Vec;

use sekai_storage::MetaStore;
use sekai_util::{CommitRef, RefName, Worktree, WorktreeName};

/// List all refs in full refname order (byte-wise).
pub async fn list_refs<M: MetaStore>(meta: &M) -> Result<Vec<CommitRef>, M::Error> {
    let mut out = Vec::new();
    meta.visit_refs(|r| {
        out.push(r.clone());
        true
    })
    .await?;
    Ok(out)
}

/// The first worktree `matches` accepts, in name order.
pub async fn find_worktree<M: MetaStore>(
    meta: &M,
    matches: impl Fn(&Worktree) -> bool + Send + Sync,
) -> Result<Option<Worktree>, M::Error> {
    let mut found = None;
    meta.visit_worktrees(|worktree| {
        if matches(worktree) {
            found = Some(worktree.clone());
        }
        found.is_none()
    })
    .await?;
    Ok(found)
}

/// The worktree other than `allowed` that has `branch` checked out, in
/// name order, born or unborn; `None` when none does.
///
/// The store refuses the move in-transaction; this ordering exists so a
/// command refuses before doing any work, as Git does.
pub async fn checked_out_holder<M: MetaStore>(
    meta: &M,
    branch: &RefName,
    allowed: Option<&WorktreeName>,
) -> Result<Option<Worktree>, M::Error> {
    find_worktree(meta, |worktree| {
        worktree.head.branch() == Some(branch) && Some(&worktree.name) != allowed
    })
    .await
}

/// Every branch checked out by some worktree, branch name to worktree
/// name.
pub async fn branch_holders<M: MetaStore>(
    meta: &M,
) -> Result<BTreeMap<RefName, WorktreeName>, M::Error> {
    let mut holders = BTreeMap::new();
    meta.visit_worktrees(|worktree| {
        if let Some(branch) = worktree.head.branch() {
            holders.insert(branch.clone(), worktree.name.clone());
        }
        true
    })
    .await?;
    Ok(holders)
}

/// A worktree name no worktree has taken: `base` itself, or `base` with
/// the first free numeric suffix, as Git names worktree IDs (ADR-0035
/// *Identity and discovery*). `base` comes from
/// [`WorktreeName::sanitize`].
///
/// Should every suffix be taken, `base` comes back, and registering it
/// fails as a taken name.
pub async fn unique_worktree_name<M: MetaStore>(
    meta: &M,
    base: &WorktreeName,
) -> Result<WorktreeName, M::Error> {
    let mut taken = BTreeSet::new();
    meta.visit_worktrees(|worktree| {
        taken.insert(worktree.name.clone());
        true
    })
    .await?;
    Ok(core::iter::once(base.clone())
        .chain((1..=u32::MAX).map(|n| base.with_suffix(n)))
        .find(|name| !taken.contains(name))
        .unwrap_or_else(|| base.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::support::{MemMeta, block_on, ref_name, worktree_name};

    #[test]
    fn checked_out_holder_is_the_first_other_holder() {
        let mut meta = MemMeta::default();
        meta.with_worktree(&worktree_name("a"), &ref_name("main"));
        meta.with_worktree(&worktree_name("b"), &ref_name("main"));
        meta.with_worktree(&worktree_name("c"), &ref_name("other"));

        let holder = |allowed| {
            block_on(checked_out_holder(&meta, &ref_name("main"), allowed))
                .map(|found| found.map(|worktree| worktree.name))
        };
        let a = worktree_name("a");
        let c = worktree_name("c");
        assert_eq!(holder(None), Ok(Some(worktree_name("a"))));
        assert_eq!(holder(Some(&a)), Ok(Some(worktree_name("b"))));
        assert_eq!(holder(Some(&c)), Ok(Some(worktree_name("a"))));
        assert_eq!(
            block_on(checked_out_holder(&meta, &ref_name("missing"), None)),
            Ok(None)
        );
    }

    #[test]
    fn branch_holders_maps_each_branch_to_its_worktree() {
        let mut meta = MemMeta::default();
        meta.with_worktree(&worktree_name("a"), &ref_name("main"));
        meta.with_worktree(&worktree_name("b"), &ref_name("other"));

        let holders = block_on(branch_holders(&meta)).unwrap();
        assert_eq!(holders.get(&ref_name("main")), Some(&worktree_name("a")));
        assert_eq!(holders.get(&ref_name("other")), Some(&worktree_name("b")));
        assert_eq!(holders.get(&ref_name("missing")), None);
    }

    #[test]
    fn a_clash_takes_the_first_free_suffix() {
        let mut meta = MemMeta::default();
        let base = worktree_name("foo");
        assert_eq!(
            block_on(unique_worktree_name(&meta, &base)),
            Ok(worktree_name("foo"))
        );

        for (worktree, branch) in [("foo", "a"), ("foo1", "b"), ("foo3", "c")] {
            meta.with_worktree(&worktree_name(worktree), &ref_name(branch));
        }
        assert_eq!(
            block_on(unique_worktree_name(&meta, &base)),
            Ok(worktree_name("foo2"))
        );
    }
}
