//! CAS blob files and SQLite MVCC metadata.
//!
//! Rationale: immutable blobs live as plain files (`blobs/ab/cdef...`)
//! while history lives in SQLite. The two stores are deliberately separate
//! types sharing one error enum: crash-consistency ordering (blobs flushed
//! before metadata references them) is `engine`'s job, and keeping the
//! types split makes that ordering visible at the call site instead of
//! hiding it inside one opaque "database".

mod cas;
mod error;
mod meta;

pub use cas::FileCas;
pub use error::StorageError;
pub use meta::{SnapshotEntry, SqliteMeta};
