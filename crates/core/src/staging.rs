//! Pure commit-staging policy: the decisions a commit run makes between
//! reading the world and writing the store.
//!
//! Chunk normalization happens in the adapter's ingest pass, before changes
//! are staged here; `diff` is not computed during commit. This module only
//! decides which chunk facts are fresh and which coordinates vanished.
//!
//! The policy is stated in ARCHITECTURE.md ("Snapshot Tree Storage"):
//! chunks unchanged since the base commit reuse their subtree hash, vanished
//! coordinates simply leave the tree, and a scoped commit reuses every
//! out-of-scope subtree by hash. The store-touching halves of the pipeline
//! live elsewhere:
//!
//! ```text
//! plan_commit    read the base snapshot and list in-scope regions to ingest
//!   -> adapter ingests every in-scope region (hash + CAS put per chunk)
//! assemble       drop unchanged chunks, add absent coordinates (pure, here)
//!   -> adapter flushes CAS (durability barrier before metadata)
//! record_commit  merge the tree and record the commit atomically
//!   -> adapter populates the region-image cache (derived, best-effort)
//! ```
//!
//! `plan_commit` and `record_commit` live in `sekai-app`; the tree merge
//! they drive lives in `sekai-storage`. Nothing here touches a store, so
//! the policy stays portable (`no_std` + `alloc`) and tested without I/O.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::vec::Vec;

use sekai_util::{ChunkCoord, ChunkHash, Commit, CommitEntry, RegionKey, Scope};

/// Previous commit plus the state the walk needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Previous {
    /// The commit the new one continues: the target of the active ref,
    /// when the store has one (`None` on an unborn ref).
    pub commit: Option<Commit>,
    /// Effective present entries of the base commit (empty on an unborn
    /// ref). Keyed by coordinate; the entry carries the blob hash used
    /// for chunk-level change detection. Absent coordinates are not part
    /// of the snapshot, so they never appear here.
    pub state: BTreeMap<ChunkCoord, CommitEntry>,
}

/// Staged commit decision: what to ingest or report as gone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// In-scope regions the adapter must ingest.
    pub ingest: Vec<RegionKey>,
    /// Regions with present chunks at the base but no file on disk now.
    pub removed: Vec<RegionKey>,
    /// Discovered region identities inside the scope.
    pub discovered: BTreeSet<RegionKey>,
}

/// What the adapter's ingest pass produced for the in-scope files.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Ingested {
    /// Snapshot changes for every chunk read.
    pub entries: Vec<CommitEntry>,
    /// Coordinates those entries cover.
    pub present: BTreeSet<ChunkCoord>,
    /// Blobs newly written to CAS.
    pub new_blobs: usize,
}

/// Merged changes ready to commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assembled {
    /// Fresh snapshot changes (changed chunks plus absent coordinates).
    pub entries: Vec<CommitEntry>,
    /// Coordinates present on disk.
    pub chunks: usize,
    /// Blobs newly written to CAS.
    pub new_blobs: usize,
    /// Vanished chunks removed from the snapshot.
    pub tombstones: usize,
    /// In-scope files that contributed a chunk absent from the base
    /// snapshot.
    pub new_files: usize,
    /// Stored regions gone from disk.
    pub removed: Vec<RegionKey>,
}

/// Stage an ingested chunk as a snapshot change.
pub const fn stage_present(coord: ChunkCoord, hash: ChunkHash) -> CommitEntry {
    CommitEntry::new(coord, Some(hash))
}

/// Merge ingested changes with absent coordinates.
///
/// Previously present coordinates inside `scope` but not present in the new
/// scan leave the snapshot; out-of-scope subtrees are reused by hash, so
/// scoped commits never drop anything outside the scope. Coordinates already
/// absent from the parent snapshot are not in `Previous::state` and therefore
/// record no further change.
///
/// Entries whose blob matches the base commit are dropped: the file was
/// reread, but its content did not change, so the tree would keep the same
/// hash without the patch.
pub fn assemble(plan: Plan, previous: &Previous, ingested: Ingested, scope: &Scope) -> Assembled {
    let Ingested {
        entries,
        present,
        new_blobs,
    } = ingested;
    let mut fresh: Vec<CommitEntry> = entries
        .into_iter()
        .filter(|entry| {
            previous
                .state
                .get(&entry.coord)
                .is_none_or(|base| base.blob != entry.blob)
        })
        .collect();
    // An empty file contributes no entries and is not part of the logical
    // world state, so it must not keep the preview permanently dirty.
    let new_files: BTreeSet<RegionKey> = fresh
        .iter()
        .filter(|entry| entry.blob.is_some() && !previous.state.contains_key(&entry.coord))
        .map(|entry| RegionKey::of(entry.coord))
        .collect();
    let new_files = new_files.len();
    // Present before, absent from this scan: the coordinate just vanished.
    let mut tombstones = 0usize;
    for coord in previous.state.keys() {
        if scope.contains(*coord) && !present.contains(coord) {
            fresh.push(CommitEntry::new(*coord, None));
            tombstones += 1;
        }
    }
    Assembled {
        chunks: present.len(),
        entries: fresh,
        new_blobs,
        tombstones,
        new_files,
        removed: plan.removed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sekai_util::{Dimension, RegionKind};

    const OVER: Dimension = Dimension::OVERWORLD;
    const NETHER: Dimension = Dimension::NETHER;
    const REGION: RegionKind = RegionKind::Region;

    const fn coord(x: i32, z: i32) -> ChunkCoord {
        ChunkCoord::new(OVER, REGION, x, z)
    }

    const fn nether_coord(x: i32, z: i32) -> ChunkCoord {
        ChunkCoord::new(NETHER, REGION, x, z)
    }

    fn previous(entries: &[CommitEntry]) -> Previous {
        Previous {
            commit: None,
            state: entries.iter().map(|entry| (entry.coord, *entry)).collect(),
        }
    }

    fn plan() -> Plan {
        Plan {
            ingest: Vec::new(),
            removed: Vec::new(),
            discovered: BTreeSet::new(),
        }
    }

    #[test]
    fn unchanged_chunks_are_dropped() {
        let base = previous(&[stage_present(coord(0, 0), ChunkHash::new([1; 32]))]);
        let staged = assemble(
            plan(),
            &base,
            Ingested {
                entries: alloc::vec![stage_present(coord(0, 0), ChunkHash::new([1; 32]))],
                present: BTreeSet::from([coord(0, 0)]),
                new_blobs: 0,
            },
            &Scope::World,
        );
        assert_eq!(staged.entries, [] as [CommitEntry; 0]);
        assert_eq!(staged.chunks, 1);
        assert_eq!(staged.new_files, 0);
        assert_eq!(staged.tombstones, 0);
    }

    #[test]
    fn vanished_in_scope_coordinates_become_tombstones() {
        let base = previous(&[
            stage_present(coord(0, 0), ChunkHash::new([1; 32])),
            stage_present(coord(1, 0), ChunkHash::new([2; 32])),
            stage_present(nether_coord(0, 0), ChunkHash::new([3; 32])),
        ]);
        let staged = assemble(
            plan(),
            &base,
            Ingested {
                entries: alloc::vec![stage_present(coord(0, 0), ChunkHash::new([9; 32]))],
                present: BTreeSet::from([coord(0, 0)]),
                new_blobs: 1,
            },
            &Scope::dimension(OVER),
        );
        assert_eq!(staged.tombstones, 1);
        assert_eq!(staged.chunks, 1);
        let tombstoned: Vec<ChunkCoord> = staged
            .entries
            .iter()
            .filter(|entry| entry.blob.is_none())
            .map(|entry| entry.coord)
            .collect();
        // The out-of-scope nether coordinate never becomes a tombstone.
        assert_eq!(tombstoned, alloc::vec![coord(1, 0)]);
    }

    #[test]
    fn new_files_counts_only_regions_absent_from_the_base() {
        let base = previous(&[stage_present(coord(0, 0), ChunkHash::new([1; 32]))]);
        let staged = assemble(
            plan(),
            &base,
            Ingested {
                entries: alloc::vec![
                    stage_present(coord(0, 0), ChunkHash::new([9; 32])),
                    stage_present(coord(32, 0), ChunkHash::new([2; 32])),
                ],
                present: BTreeSet::from([coord(0, 0), coord(32, 0)]),
                new_blobs: 2,
            },
            &Scope::World,
        );
        assert_eq!(staged.new_files, 1);
        assert_eq!(staged.entries.len(), 2);
    }

    #[test]
    fn an_empty_ingest_contributes_nothing() {
        let staged = assemble(plan(), &previous(&[]), Ingested::default(), &Scope::World);
        assert_eq!(staged.entries, [] as [CommitEntry; 0]);
        assert_eq!(staged.chunks, 0);
        assert_eq!(staged.new_files, 0);
        assert_eq!(staged.tombstones, 0);
    }

    #[test]
    fn removed_regions_pass_through() {
        let mut plan = plan();
        plan.removed = alloc::vec![RegionKey::new(OVER, REGION, 0, 0)];
        let staged = assemble(plan, &previous(&[]), Ingested::default(), &Scope::World);
        assert_eq!(
            staged.removed,
            alloc::vec![RegionKey::new(OVER, REGION, 0, 0)]
        );
    }
}
