//! `SurrealDB` storage backend (metadata + blobs, via `SurrealKV`) for Sekai.

mod blob;
mod meta;
mod row;
mod surrealdb;

pub use meta::SCHEMA_VERSION;
pub use surrealdb::{DEFAULT_PATH, SurrealdbStore, init_surrealdb, open_surrealdb};
