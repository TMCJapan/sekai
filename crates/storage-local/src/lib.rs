//! SQLite storage backend (metadata + blobs, via sqlx) for Sekai.

mod blob;
mod meta;
mod sqlite;

pub use blob::SqliteBlob;
pub use meta::{SCHEMA_VERSION, SqliteMeta};
pub use sqlite::{SqliteStore, init_sqlite, open_sqlite};
