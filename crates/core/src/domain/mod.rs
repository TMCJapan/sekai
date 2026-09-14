//! Domain types independent of concrete backends.

pub mod coords;
pub mod error;
pub mod gc;
pub mod hash;
pub mod history;
pub mod region;
pub mod snapshot;

pub use coords::{ChunkCoord, Dimension, RegionKind, resolve_custom_dimension};
pub use error::CoreError;
pub use gc::GcPlan;
pub use hash::{BlobHash, DiffHash, hash_blob};
pub use history::ChunkHistoryEntry;
pub use region::{ApplyOutcome, RegionFingerprint, RegionKey, RegionStateEntry, SnapshotEntry};
pub use snapshot::{Snapshot, SnapshotId};
