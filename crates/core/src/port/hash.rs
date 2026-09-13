//! Hash-trait ports over the two hashing layers.
//!
//! Rationale: `core` defines the `BlobHasher`/`DiffHasher` shapes without
//! naming an algorithm; adapters fill both (today one Blake3-backed type).
//! The layers differ in *what* is fed (raw bytes vs canonical NBT), never
//! in the digest function.

use crate::domain::hash::{BlobHash, DiffHash};

/// Streaming Blake3 over exact raw chunk bytes (persistent CAS key).
///
/// Implemented outside `core` (e.g. with the `blake3` crate) to keep
/// `core` dependency-free. Feed the raw payload exactly as read from the
/// `.mca` sector, including its compression framing.
pub trait BlobHasher {
    /// Fresh hashing state.
    fn new() -> Self;
    /// Feed raw payload bytes; call any number of times.
    fn update(&mut self, data: &[u8]);
    /// Finalize into the persistent CAS key.
    fn finalize(self) -> BlobHash;
}

/// Streaming Blake3 over normalized NBT (volatile diff view).
///
/// A separate type from [`BlobHasher`] so future versions may swap the
/// diff function (e.g. to a faster non-cryptographic hash) without
/// touching restore-critical CAS keys.
pub trait DiffHasher {
    /// Fresh hashing state.
    fn new() -> Self;
    /// Feed normalized bytes.
    fn update(&mut self, data: &[u8]);
    /// Finalize into the volatile view hash.
    fn finalize(self) -> DiffHash;
}
