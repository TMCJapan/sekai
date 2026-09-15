//! Structural chunk diff operations over blob storage.

use alloc::vec::Vec;
use core::fmt;

use sekai_anvil::AnvilError;
use sekai_nbt::{DEFAULT_IGNORED, NbtDiffEntry, NbtError};

use crate::port::blob::BlobStore;
use sekai_util::BlobHash;

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

/// Compute AST diff between two stored blob hashes.
pub async fn diff_blobs<B: BlobStore>(
    blobs: &B,
    old_hash: &BlobHash,
    new_hash: &BlobHash,
    ignore: &[&str],
) -> Result<Vec<NbtDiffEntry>, DiffError<B::Error>> {
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

    let old_val = sekai_nbt::parse(&old_nbt).map_err(DiffError::Nbt)?;
    let new_val = sekai_nbt::parse(&new_nbt).map_err(DiffError::Nbt)?;

    Ok(sekai_nbt::diff(&old_val, &new_val, ignore))
}

/// Compute AST diff between two stored blob hashes using default ignored tags.
pub async fn diff_blobs_v1<B: BlobStore>(
    blobs: &B,
    old_hash: &BlobHash,
    new_hash: &BlobHash,
) -> Result<Vec<NbtDiffEntry>, DiffError<B::Error>> {
    diff_blobs(blobs, old_hash, new_hash, DEFAULT_IGNORED).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use sekai_nbt::NbtChange;

    use crate::hash_blob;
    use crate::support::{MemCas, block_on};

    // Uncompressed (Type 3) sector payload framing + NBT bytes
    fn framed_nbt_bytes(status: &str) -> Vec<u8> {
        let mut out = alloc::vec![3]; // Type 3: Uncompressed
        out.extend_from_slice(&[10, 0, 0, 8, 0, 6]);
        out.extend_from_slice(b"Status");
        let status_len = u16::try_from(status.len()).unwrap();
        out.extend_from_slice(&status_len.to_be_bytes());
        out.extend_from_slice(status.as_bytes());
        out.push(0);
        out
    }

    #[test]
    fn diffs_stored_blobs() {
        let mut cas = MemCas::default();
        let raw_a = framed_nbt_bytes("minecraft:full");
        let raw_b = framed_nbt_bytes("minecraft:empty");

        let hash_a = hash_blob(&raw_a);
        let hash_b = hash_blob(&raw_b);

        block_on(cas.put(&hash_a, &raw_a)).unwrap();
        block_on(cas.put(&hash_b, &raw_b)).unwrap();

        let diffs = block_on(diff_blobs_v1(&cas, &hash_a, &hash_b)).unwrap();

        assert_eq!(
            diffs,
            alloc::vec![NbtDiffEntry {
                path: "Status".into(),
                change: NbtChange::Modified {
                    old: sekai_nbt::Value::String("minecraft:full".into()),
                    new: sekai_nbt::Value::String("minecraft:empty".into()),
                }
            }]
        );
    }

    #[test]
    fn missing_blob_is_a_backend_error() {
        let cas = MemCas::default();
        let missing = hash_blob(b"never-stored");
        let err = block_on(diff_blobs_v1(&cas, &missing, &missing)).unwrap_err();
        assert!(matches!(err, DiffError::Blob(_)));
    }

    #[test]
    fn undecodable_payload_is_a_codec_error() {
        let mut cas = MemCas::default();
        // Unknown compression type byte: anvil rejects before NBT sees it.
        let bad_codec = alloc::vec![9u8, 0, 0];
        let hash_bad = hash_blob(&bad_codec);
        block_on(cas.put(&hash_bad, &bad_codec)).unwrap();
        let good = framed_nbt_bytes("minecraft:full");
        let hash_good = hash_blob(&good);
        block_on(cas.put(&hash_good, &good)).unwrap();

        let err = block_on(diff_blobs_v1(&cas, &hash_bad, &hash_good)).unwrap_err();
        assert!(matches!(err, DiffError::Anvil(_)));
    }

    #[test]
    fn corrupt_nbt_is_a_decode_error() {
        let mut cas = MemCas::default();
        // Type 3 framing with truncated NBT body.
        let corrupt = alloc::vec![3u8, 10, 0];
        let hash_corrupt = hash_blob(&corrupt);
        block_on(cas.put(&hash_corrupt, &corrupt)).unwrap();
        let good = framed_nbt_bytes("minecraft:full");
        let hash_good = hash_blob(&good);
        block_on(cas.put(&hash_good, &good)).unwrap();

        let err = block_on(diff_blobs_v1(&cas, &hash_corrupt, &hash_good)).unwrap_err();
        assert!(matches!(err, DiffError::Nbt(_)));
    }

    #[test]
    fn diff_error_messages() {
        use alloc::string::ToString;
        let blob: DiffError<crate::support::MemError> = DiffError::Blob(crate::support::MemError);
        assert_eq!(blob.to_string(), "blob store failed");
    }
}
