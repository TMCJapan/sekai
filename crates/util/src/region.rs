//! Region identities and incremental-ingest fingerprints.

use super::{BlobHash, ChunkCoord, CommitId, DiffHash, Dimension, RegionKind};

/// Identity of one region file within its namespace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RegionKey {
    /// Dimension namespace.
    pub dim: Dimension,
    /// Region family.
    pub kind: RegionKind,
    /// Region X from the file name.
    pub rx: i32,
    /// Region Z from the file name.
    pub rz: i32,
}

impl RegionKey {
    /// Construct a region identity.
    pub const fn new(dim: Dimension, kind: RegionKind, rx: i32, rz: i32) -> Self {
        Self { dim, kind, rx, rz }
    }

    /// Region identity owning a chunk coordinate.
    pub const fn of(coord: ChunkCoord) -> Self {
        Self::new(coord.dim, coord.kind, coord.region_x(), coord.region_z())
    }
}

/// Fingerprint observed on disk during commit planning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegionFingerprint {
    /// Which file this fingerprint describes.
    pub key: RegionKey,
    /// Last modification time as Unix millis (`None` when unavailable).
    pub mtime_ms: Option<u64>,
    /// File size in bytes.
    pub size: u64,
    /// Blake3 of the whole file.
    pub content_hash: [u8; 32],
}

/// Persisted fingerprint of the last ingested file state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegionStateEntry {
    /// The fingerprint as observed when `commit_id` confirmed it.
    pub fingerprint: RegionFingerprint,
    /// Commit that last confirmed this fingerprint.
    pub commit_id: CommitId,
}

impl RegionFingerprint {
    /// Whether the file can skip ingestion against stored state.
    ///
    /// Every signal must agree - a fingerprint with a new signal added
    /// cannot be silently ignored here, which is the point of comparing the
    /// whole value. A missing `mtime` on either side forces ingest instead:
    /// trusting a clock the platform cannot provide would risk stale
    /// commits, so the fail-safe direction is to redo the work.
    pub fn matches_state(&self, state: &RegionStateEntry) -> bool {
        self.mtime_ms.is_some() && self == &state.fingerprint
    }
}

/// Chunk state staged for a commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CommitEntry {
    /// Which chunk this fact describes.
    pub coord: ChunkCoord,
    /// Exact CAS key, or `None` for a tombstone.
    pub blob: Option<BlobHash>,
    /// Cached volatile hash, or `None` when not computed.
    pub diff: Option<DiffHash>,
}

impl CommitEntry {
    /// Construct a chunk fact.
    pub const fn new(coord: ChunkCoord, blob: Option<BlobHash>, diff: Option<DiffHash>) -> Self {
        Self { coord, blob, diff }
    }
}

/// Result of recording a commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApplyOutcome {
    /// Newly recorded commit ID.
    pub id: CommitId,
    /// History rows carried over from the previous commit.
    pub carried_chunks: usize,
}

/// Result of retiring one commit into its successor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FoldOutcome {
    /// Rows re-stamped onto the successor commit.
    pub folded: usize,
    /// Rows superseded before the successor and removed.
    pub dropped: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn key() -> RegionKey {
        RegionKey::new(Dimension::OVERWORLD, RegionKind::Region, 0, 0)
    }

    const fn fingerprint() -> RegionFingerprint {
        RegionFingerprint {
            key: key(),
            mtime_ms: Some(1_700_000_000_000),
            size: 8192,
            content_hash: [7; 32],
        }
    }

    const fn stored() -> RegionStateEntry {
        RegionStateEntry {
            fingerprint: fingerprint(),
            commit_id: CommitId([1; 32]),
        }
    }

    #[test]
    fn fingerprint_match_requires_all_signals() {
        let base = stored();
        assert!(fingerprint().matches_state(&base));

        let mut changed = fingerprint();
        changed.mtime_ms = Some(1_700_000_000_001);
        assert!(!changed.matches_state(&base));

        let mut changed = fingerprint();
        changed.size += 1;
        assert!(!changed.matches_state(&base));

        let mut changed = fingerprint();
        changed.content_hash[0] ^= 0xFF;
        assert!(!changed.matches_state(&base));

        // Unknown clock on either side forces ingest (fail-safe direction).
        let mut changed = fingerprint();
        changed.mtime_ms = None;
        assert!(!changed.matches_state(&base));
        let mut stored = base;
        stored.fingerprint.mtime_ms = None;
        assert!(!fingerprint().matches_state(&stored));
    }

    #[test]
    fn region_key_of_chunk() {
        let coord = ChunkCoord::new(Dimension::NETHER, RegionKind::Poi, -33, 70);
        assert_eq!(
            RegionKey::of(coord),
            RegionKey::new(Dimension::NETHER, RegionKind::Poi, -2, 2)
        );
    }
}
