//! Structural chunk diff operations over blob storage.

use alloc::vec::Vec;
use core::fmt;
use sekai_nbt::{Chunk, ChunkPayloadDiff, NbtError};

use sekai_anvil::AnvilError;

use crate::port::blob::BlobStore;
use sekai_util::ChunkHash;

/// Failures during chunk diff computation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffError<B> {
    /// Blob backend failure.
    Blob(B),
    /// Sector decompression failure.
    Anvil(AnvilError),
    /// NBT decoding failure.
    Nbt(NbtError),
}

impl<B: fmt::Debug> fmt::Display for DiffError<B> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Blob(_) => write!(f, "blob store failed"),
            Self::Anvil(e) => write!(f, "sector decompression failed: {e}"),
            Self::Nbt(e) => write!(f, "NBT decoding failed: {e}"),
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

    let mut old_nbt = Vec::new();
    let mut new_nbt = Vec::new();
    sekai_anvil::decompress_into(&old_payload, &mut old_nbt).map_err(DiffError::Anvil)?;
    sekai_anvil::decompress_into(&new_payload, &mut new_nbt).map_err(DiffError::Anvil)?;

    let old_val: Chunk<'_> = sekai_nbt::parse(&old_nbt).map_err(DiffError::Nbt)?;
    let new_val: Chunk<'_> = sekai_nbt::parse(&new_nbt).map_err(DiffError::Nbt)?;

    ChunkPayloadDiff::new(&old_val, &new_val).map_err(DiffError::Nbt)
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::support::{MemCas, block_on};

    // Uncompressed (Type 3) sector payload framing + a level-chunk NBT root.
    fn framed_nbt_bytes(status: &str) -> Vec<u8> {
        use sekai_nbt::nanonbt::Cesu8;

        let chunk = sekai_nbt::nanonbt::to_bytes(&sekai_nbt::ChunkPayload {
            data_version: 1,
            x_pos: 0,
            y_pos: 0,
            z_pos: 0,
            inhabited_time: 0,
            last_update: 0,
            sections: Vec::new(),
            heightmaps: sekai_nbt::Heightmaps::default(),
            status: &Cesu8::from_str(status),
            block_entities: Vec::new(),
            block_ticks: Vec::new(),
            fluid_ticks: Vec::new(),
        })
        .unwrap();
        let mut out = alloc::vec![3]; // Type 3: Uncompressed
        out.extend_from_slice(&chunk);
        out
    }

    /// CAS key of a framed payload, through the same normalization ingest uses.
    fn chunk_key_of(framed: &[u8]) -> ChunkHash {
        let mut nbt = Vec::new();
        sekai_anvil::decompress_into(framed, &mut nbt).unwrap();
        sekai_nbt::chunk_key(sekai_util::RegionKind::Region, &nbt).unwrap()
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
        assert!(matches!(err, DiffError::Nbt(_)));
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
            matches!(diff, ChunkPayloadDiff::Full { old, new } if old != new),
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
        assert_eq!(diff, ChunkPayloadDiff::Nothing);
    }

    #[test]
    fn diff_error_messages() {
        use alloc::string::ToString;
        let blob: DiffError<crate::support::MemError> =
            DiffError::Blob(crate::support::MemError::MissingBlob);
        assert_eq!(blob.to_string(), "blob store failed");
    }
}
