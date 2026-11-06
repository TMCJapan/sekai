//! Structural chunk diff operations over blob storage.

use alloc::vec::Vec;
use core::fmt;
use sekai_anvil::{AnvilError, Chunk, ChunkHashError};

use crate::port::blob::BlobStore;
use sekai_util::{ChunkHash, RegionKind};

/// One chunk's normalized view on each side of a diff.
///
/// `None` means that side holds no chunk (absent or tombstoned). The two
/// sides are compared structurally: absent on both is no change, present on
/// exactly one side is a change against `None`, and no sentinel digest is
/// ever fabricated for the missing side.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct ChunkPayloadDiff {
    /// The old side's digest, `None` when it holds no chunk.
    pub old: Option<ChunkHash>,
    /// The new side's digest, `None` when it holds no chunk.
    pub new: Option<ChunkHash>,
}

impl ChunkPayloadDiff {
    /// Compares the two sides of a chunk by the digests already computed for
    /// them, without decoding either payload.
    pub const fn from_sides(old: Option<ChunkHash>, new: Option<ChunkHash>) -> Self {
        Self { old, new }
    }

    /// Whether both sides normalize identically: this chunk has no changes.
    pub fn is_empty(&self) -> bool {
        self.old == self.new
    }
}

/// Failures during chunk diff computation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffError<B> {
    /// Blob backend failure.
    Blob(B),
    /// Sector decompression failure.
    Anvil(AnvilError),
    /// Chunk hashing failure.
    ChunkHash(ChunkHashError),
}

impl<B: fmt::Debug> fmt::Display for DiffError<B> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Blob(_) => write!(f, "blob store failed"),
            Self::Anvil(e) => write!(f, "sector decompression failed: {e}"),
            Self::ChunkHash(e) => write!(f, "chunk hashing failed: {e}"),
        }
    }
}

/// Compute a chunk diff between two stored blob hashes.
pub async fn diff_blobs<B: BlobStore>(
    blobs: &B,
    old_hash: &ChunkHash,
    new_hash: &ChunkHash,
) -> Result<ChunkPayloadDiff, DiffError<B::Error>> {
    let mut old_payload = Vec::new();
    let mut new_payload = Vec::new();
    blobs
        .fetch_into(old_hash, &mut old_payload)
        .await
        .map_err(DiffError::Blob)?;
    blobs
        .fetch_into(new_hash, &mut new_payload)
        .await
        .map_err(DiffError::Blob)?;

    let old = chunk_digest(&old_payload)?;
    let new = chunk_digest(&new_payload)?;
    Ok(ChunkPayloadDiff::from_sides(Some(old), Some(new)))
}

/// Normalized digest of a stored `region/` payload.
fn chunk_digest<B>(payload: &[u8]) -> Result<ChunkHash, DiffError<B>> {
    Chunk::new(RegionKind::Region, 0, 0, payload)
        .normalized_hash()
        .map_err(|source| match source {
            ChunkHashError::Decompress(source) => DiffError::Anvil(source),
            source => DiffError::ChunkHash(source),
        })
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use super::*;

    use crate::support::{MemCas, block_on};

    fn name(out: &mut Vec<u8>, key: &str) {
        out.extend_from_slice(&u16::try_from(key.len()).unwrap().to_be_bytes());
        out.extend_from_slice(key.as_bytes());
    }

    fn int(out: &mut Vec<u8>, key: &str, value: i32) {
        out.push(3);
        name(out, key);
        out.extend_from_slice(&value.to_be_bytes());
    }

    fn long(out: &mut Vec<u8>, key: &str, value: i64) {
        out.push(4);
        name(out, key);
        out.extend_from_slice(&value.to_be_bytes());
    }

    fn string(out: &mut Vec<u8>, key: &str, value: &str) {
        out.push(8);
        name(out, key);
        out.extend_from_slice(&u16::try_from(value.len()).unwrap().to_be_bytes());
        out.extend_from_slice(value.as_bytes());
    }

    fn empty_list(out: &mut Vec<u8>, key: &str) {
        out.push(9);
        name(out, key);
        out.push(0);
        out.extend_from_slice(&0i32.to_be_bytes());
    }

    fn empty_compound(out: &mut Vec<u8>, key: &str) {
        out.push(10);
        name(out, key);
        out.push(0);
    }

    /// A minimal 1.18+ level chunk whose only content is `status`.
    fn chunk_nbt(status: &str) -> Vec<u8> {
        let mut out = vec![10, 0, 0];
        int(&mut out, "DataVersion", 1);
        int(&mut out, "xPos", 0);
        int(&mut out, "yPos", 0);
        int(&mut out, "zPos", 0);
        long(&mut out, "InhabitedTime", 0);
        long(&mut out, "LastUpdate", 0);
        empty_list(&mut out, "sections");
        empty_compound(&mut out, "Heightmaps");
        string(&mut out, "Status", status);
        empty_list(&mut out, "block_entities");
        empty_list(&mut out, "block_ticks");
        empty_list(&mut out, "fluid_ticks");
        out.push(0);
        out
    }

    // Uncompressed (Type 3) sector payload framing + a level-chunk NBT root.
    fn framed_nbt_bytes(status: &str) -> Vec<u8> {
        let mut out = alloc::vec![3]; // Type 3: Uncompressed
        out.extend_from_slice(&chunk_nbt(status));
        out
    }

    /// CAS key of a framed payload, through the same normalization ingest uses.
    fn chunk_key_of(framed: &[u8]) -> ChunkHash {
        Chunk::new(RegionKind::Region, 0, 0, framed)
            .normalized_hash()
            .unwrap()
    }

    #[test]
    fn absent_on_both_sides_is_empty() {
        assert!(ChunkPayloadDiff::from_sides(None, None).is_empty());
    }

    #[test]
    fn identical_digests_are_empty() {
        let hash = |byte| ChunkHash::new([byte; 32]);
        let diff = ChunkPayloadDiff::from_sides(Some(hash(1)), Some(hash(1)));
        assert!(diff.is_empty());
    }

    #[test]
    fn differing_digests_are_a_change() {
        let hash = |byte| ChunkHash::new([byte; 32]);
        let diff = ChunkPayloadDiff::from_sides(Some(hash(1)), Some(hash(2)));
        assert!(!diff.is_empty());
        assert_eq!((diff.old, diff.new), (Some(hash(1)), Some(hash(2))));
    }

    #[test]
    fn presence_mismatch_keeps_the_missing_side_none() {
        let hash = |byte| ChunkHash::new([byte; 32]);
        let removed = ChunkPayloadDiff::from_sides(Some(hash(1)), None);
        assert!(!removed.is_empty());
        assert_eq!((removed.old, removed.new), (Some(hash(1)), None));

        let added = ChunkPayloadDiff::from_sides(None, Some(hash(2)));
        assert!(!added.is_empty());
        assert_eq!((added.old, added.new), (None, Some(hash(2))));
    }

    #[test]
    fn missing_blob_is_a_backend_error() {
        let cas = MemCas::default();
        let missing = ChunkHash::new(sekai_util::hash_bytes(b"never-stored"));
        let err = block_on(diff_blobs(&cas, &missing, &missing)).unwrap_err();
        assert!(matches!(err, DiffError::Blob(_)));
    }

    #[test]
    fn undecodable_payload_is_a_codec_error() {
        let mut cas = MemCas::default();
        // Unknown compression type byte: anvil rejects before NBT sees it.
        let bad_codec = alloc::vec![9u8, 0, 0];
        let hash_bad = ChunkHash::new(sekai_util::hash_bytes(&bad_codec));
        block_on(cas.put(&hash_bad, &bad_codec)).unwrap();
        let good = framed_nbt_bytes("minecraft:full");
        let hash_good = chunk_key_of(&good);
        block_on(cas.put(&hash_good, &good)).unwrap();

        let err = block_on(diff_blobs(&cas, &hash_bad, &hash_good)).unwrap_err();
        assert!(matches!(err, DiffError::Anvil(_)));
    }

    #[test]
    fn corrupt_nbt_is_a_decode_error() {
        let mut cas = MemCas::default();
        // Type 3 framing with truncated NBT body.
        let corrupt = alloc::vec![3u8, 10, 0];
        let hash_corrupt = ChunkHash::new(sekai_util::hash_bytes(&corrupt));
        block_on(cas.put(&hash_corrupt, &corrupt)).unwrap();
        let good = framed_nbt_bytes("minecraft:full");
        let hash_good = chunk_key_of(&good);
        block_on(cas.put(&hash_good, &good)).unwrap();

        let err = block_on(diff_blobs(&cas, &hash_corrupt, &hash_good)).unwrap_err();
        assert!(matches!(err, DiffError::ChunkHash(_)));
    }

    #[test]
    fn diff_blobs_reports_full_for_different_payloads() {
        let mut cas = MemCas::default();
        let old = framed_nbt_bytes("minecraft:full");
        let new = framed_nbt_bytes("minecraft:empty");
        let hash_old = chunk_key_of(&old);
        let hash_new = chunk_key_of(&new);
        block_on(cas.put(&hash_old, &old)).unwrap();
        block_on(cas.put(&hash_new, &new)).unwrap();

        let diff = block_on(diff_blobs(&cas, &hash_old, &hash_new)).unwrap();
        assert!(
            matches!((diff.old, diff.new), (Some(old), Some(new)) if old != new),
            "different payloads must report a full diff with distinct hashes: {diff:?}"
        );
    }

    #[test]
    fn diff_blobs_reports_nothing_for_identical_payloads() {
        let mut cas = MemCas::default();
        let payload = framed_nbt_bytes("minecraft:full");
        let hash = chunk_key_of(&payload);
        block_on(cas.put(&hash, &payload)).unwrap();

        let diff = block_on(diff_blobs(&cas, &hash, &hash)).unwrap();
        assert!(diff.is_empty());
    }

    #[test]
    fn diff_error_messages() {
        use alloc::string::ToString;
        let blob: DiffError<crate::support::MemError> =
            DiffError::Blob(crate::support::MemError::MissingBlob);
        assert_eq!(blob.to_string(), "blob store failed");
    }
}
