//! Concrete streaming hasher for both hash layers.
//!
//! Rationale: `core` defines the `BlobHasher`/`DiffHasher` shapes without
//! naming an algorithm; this single `blake3`-backed type fills both. One
//! struct for both layers is deliberate - the layers differ in *what* is
//! fed (raw bytes vs canonical NBT), never in the digest function. It lives
//! in `storage` because the blob layer addresses CAS keys, which `storage`
//! owns.

use sekai_core::{BlobHash, DiffHash};

/// Blake3 streaming hasher implementing both core hash traits.
#[derive(Debug, Clone)]
pub struct Blake3Hasher {
    /// Inner streaming state.
    inner: blake3::Hasher,
}

impl sekai_core::BlobHasher for Blake3Hasher {
    fn new() -> Self {
        Self {
            inner: blake3::Hasher::new(),
        }
    }

    fn update(&mut self, data: &[u8]) {
        self.inner.update(data);
    }

    fn finalize(self) -> BlobHash {
        BlobHash(*self.inner.finalize().as_bytes())
    }
}

impl sekai_core::DiffHasher for Blake3Hasher {
    fn new() -> Self {
        Self {
            inner: blake3::Hasher::new(),
        }
    }

    fn update(&mut self, data: &[u8]) {
        self.inner.update(data);
    }

    fn finalize(self) -> DiffHash {
        DiffHash(*self.inner.finalize().as_bytes())
    }
}
