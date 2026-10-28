//! Shared domain vocabulary: coordinates, namespaces, hashes, scopes, and
//! the record types every other crate passes around.
//!
//! Pure data plus the invariants that belong to it (coordinate arithmetic,
//! hex encoding); no I/O and no policy.

#![no_std]

extern crate alloc;

pub mod chunk_coord;
pub mod commit;
pub mod dimension;
pub mod error;
pub mod gc;
pub mod hash;
pub mod history;
mod name;
pub mod named_scope;
pub mod refs;
pub mod region;
pub mod region_kind;
pub mod scope;

pub use chunk_coord::ChunkCoord;
pub use commit::{Commit, CommitId};
pub use dimension::Dimension;
pub use error::{HexError, NamedScopeError, ParseCodeError, RefNameError, ScopeNameError};
pub use gc::GcPlan;
pub use hash::{BlobHash, DiffHash};
pub use history::ChunkHistoryEntry;
pub use named_scope::{NamedScope, ScopeName};
pub use refs::{CommitRef, DEFAULT_REF_NAME, RefName};
pub use region::{ApplyOutcome, CommitEntry, FoldOutcome, RegionFingerprint, RegionKey};
pub use region_kind::{RegionKind, UnknownRegionKind};
pub use scope::{Area, Rect, Scope};
