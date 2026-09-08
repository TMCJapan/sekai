#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

//! CAS blob files and SQLite MVCC metadata.
//!
//! Rationale: immutable blobs live as plain files (`blobs/ab/cdef...`)
//! while history lives in SQLite. The two stores are deliberately separate
//! types sharing one error enum: crash-consistency ordering (blobs flushed
//! before metadata references them) is the backup caller's job, and keeping
//! the types split makes that ordering visible at the call site instead of
//! hiding it inside one opaque "database".

mod cas;
mod error;
mod hash;
mod meta;
mod region;
mod store;

pub use cas::FileCas;
pub use error::StorageError;
pub use hash::Blake3Hasher;
pub use meta::{SCHEMA_VERSION, SqliteMeta};
pub use store::Store;
