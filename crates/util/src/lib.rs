//! Shared domain vocabulary: coordinates, namespaces, hashes, scopes, and
//! the record types every other crate passes around.
//!
//! Pure data plus the invariants that belong to it (coordinate arithmetic,
//! hex encoding, chunk digests); no I/O and no policy. Execution seams
//! (thread runners and clocks) live in `sekai-platform`, where a host
//! embedding composes its own (the `sekai` binary's `OsThread` pool and
//! OS clock).

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
pub mod revision;
pub mod scope;
pub mod tree;
pub mod worktree;

pub use commit::{Commit, CommitId};
pub use dimension::Dimension;
pub use hash::{BytesHasher, ChunkHash, hash_bytes};
pub use named_scope::{NamedScope, ScopeName};
pub use refs::{CommitRef, DEFAULT_REF_NAME, Ref, RefKind, RefName};
pub use region::{ApplyOutcome, CommitEntry, RegionFingerprint, RegionKey};
pub use region_kind::{RegionKind, UnknownRegionKind};
pub use revision::{Revision, RevisionBase};
pub use scope::{Area, Rect, Scope};
pub use tree::{DimHash, RegionHash, RootHash};
