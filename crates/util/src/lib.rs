//! Shared domain vocabulary: coordinates, namespaces, hashes, scopes, and
//! the record types every other crate passes around.
//!
//! Pure data plus the invariants that belong to it (coordinate arithmetic,
//! hex encoding, chunk digests); no I/O and no policy.

#![no_std]

extern crate alloc;

pub mod chunk_coord;
pub mod commit;
pub mod dimension;
pub mod error;
pub mod hash;
mod name;
pub mod named_scope;
pub mod refs;
pub mod region;
pub mod region_kind;
pub mod scope;
pub mod tree;

pub use chunk_coord::ChunkCoord;
pub use commit::{Commit, CommitId};
pub use dimension::Dimension;
pub use error::{HexError, NamedScopeError, ParseCodeError, RefNameError, ScopeNameError};
pub use hash::{BytesHasher, ChunkHash, hash_bytes};
pub use named_scope::{NamedScope, ScopeName};
pub use refs::{CommitRef, DEFAULT_REF_NAME, RefName};
pub use region::{ApplyOutcome, CommitEntry, RegionFingerprint, RegionKey};
pub use region_kind::{RegionKind, UnknownRegionKind};
pub use scope::{Area, Rect, Scope};
pub use tree::{DimHash, RegionHash, RootHash};
