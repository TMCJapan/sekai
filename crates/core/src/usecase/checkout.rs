//! Checkout planning for exact commit restoration.
//!
//! The plan contains present payloads grouped by region. Adapters rebuild
//! files from CAS blobs; missing blobs are treated as corruption. Before a
//! checkout touches the world, the policy here decides which regions a
//! branch switch leaves alone and whether the files it would touch hold
//! uncommitted chunks (ADR-0036 *checkout*).
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::vec::Vec;
use core::fmt;

use sekai_storage::MetaStore;
use sekai_util::{ChunkCoord, ChunkHash, CommitEntry, CommitId, RegionKey};

use super::commit::commit_region_hashes;

/// Outcome of one checkout run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CheckoutReport {
    /// Region files rewritten.
    pub files_written: usize,
    /// Region files deleted (absent from the commit's snapshot).
    pub files_deleted: usize,
    /// Chunks restored from CAS blobs.
    pub chunks_restored: usize,
}

/// Resolved restore set for one commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckoutPlan {
    /// Commit creation time (unix millis, informational only).
    pub created_at_ms: u64,
    /// Present rows grouped by region. A region absent here has no chunks at
    /// the commit: checkout deletes its file unless a keep-policy applies.
    pub groups: BTreeMap<RegionKey, Vec<(ChunkCoord, ChunkHash)>>,
}

/// Failures while resolving a checkout plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckoutError<M> {
    /// Metadata backend failure.
    Meta(M),
    /// No commit with this ID exists.
    UnknownCommit {
        /// Requested commit ID.
        id: CommitId,
    },
}

impl<M: fmt::Debug> fmt::Display for CheckoutError<M> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Meta(_) => write!(f, "commit metadata failed"),
            Self::UnknownCommit { id } => write!(f, "unknown commit: {id}"),
        }
    }
}

/// Resolve present commit rows grouped by region.
pub async fn plan_checkout<M: MetaStore>(
    meta: &M,
    commit: CommitId,
) -> Result<CheckoutPlan, CheckoutError<M::Error>> {
    let created_at_ms = meta
        .lookup_commit(commit)
        .await
        .map_err(CheckoutError::Meta)?
        .map(|c| c.created_at_ms)
        .ok_or(CheckoutError::UnknownCommit { id: commit })?;
    let mut groups: BTreeMap<RegionKey, Vec<(ChunkCoord, ChunkHash)>> = BTreeMap::new();
    meta.visit_commit_chunks(commit, |entry| {
        if let Some(blob) = entry.blob {
            groups
                .entry(RegionKey::of(entry.coord))
                .or_default()
                .push((entry.coord, blob));
        }
        true
    })
    .await
    .map_err(CheckoutError::Meta)?;
    Ok(CheckoutPlan {
        created_at_ms,
        groups,
    })
}

/// Regions that `head` and `target` name with the same region hash among
/// `keys`.
///
/// A branch switch leaves them alone, so their uncommitted changes carry
/// over, as Git's checkout keeps a file both commits agree on.
pub async fn unchanged_regions<M: MetaStore>(
    meta: &M,
    head: CommitId,
    target: CommitId,
    keys: &[RegionKey],
) -> Result<BTreeSet<RegionKey>, M::Error> {
    let before = commit_region_hashes(meta, head, keys).await?;
    let after = commit_region_hashes(meta, target, keys).await?;
    Ok(after
        .into_iter()
        .filter(|(key, hash)| before.get(key) == Some(hash))
        .map(|(key, _)| key)
        .collect())
}

/// What checkout does to one region file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileChange<'a> {
    /// Rebuilt from the commit's present rows.
    Rebuild(&'a [(ChunkCoord, ChunkHash)]),
    /// Deleted.
    Delete,
}

/// The regions, in the order given, where checkout would overwrite or drop
/// an uncommitted chunk (ADR-0036 *checkout*).
///
/// `uncommitted` lists the chunks that differ from HEAD's commit, as a
/// commit would record them. A chunk holding data HEAD lacks is lost when
/// its file is deleted, or rebuilt over it, or rebuilt without it unless
/// `keep_live_chunks` keeps it. A chunk gone from disk is not: rebuilding
/// it loses nothing, as Git restores a deleted file without asking.
pub fn overwritten_regions<'a>(
    changes: impl IntoIterator<Item = (RegionKey, FileChange<'a>)>,
    uncommitted: &[CommitEntry],
    keep_live_chunks: bool,
) -> Vec<RegionKey> {
    let mut local: BTreeMap<RegionKey, BTreeSet<ChunkCoord>> = BTreeMap::new();
    for entry in uncommitted.iter().filter(|entry| entry.blob.is_some()) {
        local
            .entry(RegionKey::of(entry.coord))
            .or_default()
            .insert(entry.coord);
    }
    changes
        .into_iter()
        .filter(|(key, change)| {
            local.get(key).is_some_and(|coords| match change {
                FileChange::Delete => true,
                // Kept or not, a chunk the commit restores is overwritten.
                FileChange::Rebuild(rows) => {
                    !keep_live_chunks || rows.iter().any(|(coord, _)| coords.contains(coord))
                }
            })
        })
        .map(|(key, _)| key)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::support::{MemMeta, block_on};
    use sekai_util::{CommitEntry, Dimension, RegionKind};

    const OVER: Dimension = Dimension::OVERWORLD;
    const REGION: RegionKind = RegionKind::Region;

    #[test]
    fn resolves_present_rows_grouped_by_region() {
        let mut meta = MemMeta::default();
        meta.with_main();
        let a = ChunkCoord::new(OVER, REGION, 0, 0);
        let b = ChunkCoord::new(OVER, REGION, 1, 0);
        let gone = ChunkCoord::new(OVER, REGION, 2, 0);
        let id = block_on(meta.apply_main(
            1_000,
            &[],
            &[
                CommitEntry::new(a, Some(ChunkHash::new([1; 32]))),
                CommitEntry::new(b, Some(ChunkHash::new([2; 32]))),
                CommitEntry::new(gone, None),
            ],
        ))
        .unwrap()
        .id;

        let plan = block_on(plan_checkout(&meta, id)).unwrap();
        assert_eq!(plan.created_at_ms, 1_000);
        assert_eq!(plan.groups.len(), 1);
        let rows = &plan.groups[&RegionKey::new(OVER, REGION, 0, 0)];
        assert_eq!(rows.len(), 2);
        assert!(rows.contains(&(a, ChunkHash::new([1; 32]))));
        assert!(rows.contains(&(b, ChunkHash::new([2; 32]))));
        assert!(
            block_on(meta.lookup_chunk(id, &gone)).unwrap().is_none(),
            "absence is the tombstone"
        );
    }

    #[test]
    fn unknown_commit_is_an_error() {
        let meta = MemMeta::default();
        assert_eq!(
            block_on(plan_checkout(&meta, CommitId([7; 32]))),
            Err(CheckoutError::UnknownCommit {
                id: CommitId([7; 32])
            })
        );
    }

    #[test]
    fn a_switch_leaves_regions_both_commits_agree_on() {
        let mut meta = MemMeta::default();
        meta.with_main();
        let at = |x| ChunkCoord::new(OVER, REGION, x * 32, 0);
        let hash = |n| Some(ChunkHash::new([n; 32]));
        let head = block_on(meta.apply_main(
            1_000,
            &[],
            &[
                CommitEntry::new(at(0), hash(1)),
                CommitEntry::new(at(1), hash(2)),
            ],
        ))
        .unwrap()
        .id;
        let target = block_on(meta.apply_main(
            2_000,
            &[head],
            &[
                CommitEntry::new(at(1), hash(3)),
                CommitEntry::new(at(2), hash(4)),
            ],
        ))
        .unwrap()
        .id;
        let keys: Vec<RegionKey> = (0..3).map(|x| RegionKey::of(at(x))).collect();

        assert_eq!(
            block_on(unchanged_regions(&meta, head, target, &keys)),
            Ok(BTreeSet::from([keys[0]]))
        );
    }

    #[test]
    fn only_chunks_holding_new_data_block_a_checkout() {
        let at = |x, z| ChunkCoord::new(OVER, REGION, x, z);
        let key = RegionKey::of(at(0, 0));
        let rows = [(at(0, 0), ChunkHash::new([1; 32]))];
        let check = |uncommitted: &[CommitEntry], change, keep| {
            overwritten_regions([(key, change)], uncommitted, keep)
        };
        let modified = [CommitEntry::new(at(0, 0), Some(ChunkHash::new([9; 32])))];
        let added = [CommitEntry::new(at(1, 0), Some(ChunkHash::new([9; 32])))];
        let gone = [CommitEntry::new(at(0, 0), None)];

        // A chunk the commit restores is overwritten, kept or not.
        assert_eq!(check(&modified, FileChange::Rebuild(&rows), true), [key]);
        // A chunk the commit lacks is dropped unless kept.
        assert_eq!(check(&added, FileChange::Rebuild(&rows), false), [key]);
        assert_eq!(check(&added, FileChange::Rebuild(&rows), true), []);
        assert_eq!(check(&added, FileChange::Delete, true), [key]);
        // A chunk gone from disk loses nothing.
        assert_eq!(check(&gone, FileChange::Rebuild(&rows), false), []);
        assert_eq!(check(&[], FileChange::Delete, false), []);
    }
}
