//! Domain types and invariants independent of concrete implementations.
//!
//! Rationale: everything here is pure data over chunk coordinates,
//! identifiers, and byte/hash views. No SQLite, no filesystem, no CLI, no
//! Anvil specifics: adapters in `storage`, `mca`, and `nbt` implement the
//! [`port`](crate::port) traits over these shapes, and application use cases
//! in [`usecase`](crate::usecase) orchestrate them.

pub mod coords;
pub mod error;
pub mod gc;
pub mod hash;
pub mod history;
pub mod region;
pub mod snapshot;

pub use coords::{ChunkCoord, Dimension, RegionKind};
pub use error::CoreError;
pub use gc::GcPlan;
pub use hash::{BlobHash, DiffHash};
pub use history::ChunkHistoryEntry;
pub use region::{ApplyOutcome, RegionFingerprint, RegionKey, RegionStateEntry, SnapshotEntry};
pub use snapshot::{Snapshot, SnapshotId};
