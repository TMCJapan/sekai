//! SQLite metadata backend (sqlx) for Sekai.

mod sqlite;

pub use sqlite::{SCHEMA_VERSION, SqliteMeta, SqliteStore, init_sqlite, open_sqlite};
