//! Error type for blob and metadata persistence.
//!
//! Rationale: CAS and SQLite failures share one enum so callers handle a
//! single error type across the write path. Every variant names the object
//! at fault (blob hex, file path, schema version) instead of burying it in
//! a string.

use std::io;
use std::path::PathBuf;

/// Failures while storing blobs or metadata.
#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    /// File-system operation failed.
    #[error("file I/O failed for {path}: {source}", path = path.display())]
    Io {
        /// File (or directory, for fsync) involved.
        path: PathBuf,
        /// Underlying OS error.
        #[source]
        source: io::Error,
    },

    /// Requested blob is not on disk.
    ///
    /// Callers treat this as database corruption: absence is represented
    /// by tombstone history rows, never by missing files.
    #[error("blob missing from CAS: {hex}")]
    BlobMissing {
        /// Lowercase hex of the absent blob.
        hex: String,
    },

    /// SQLite operation failed.
    #[error("sqlite failed: {0}")]
    Sqlite(#[from] rusqlite::Error),

    /// Stored hash bytes are not 32 bytes long.
    #[error("stored hash has invalid length: {len} bytes, expected 32")]
    InvalidHashLength {
        /// Observed byte length.
        len: usize,
    },

    /// Stored history integer does not fit its domain type.
    #[error("stored history column {column} has out-of-range value: {value}")]
    InvalidHistoryValue {
        /// Column holding the bad value (`dim`, `kind`, `cx`, `cz`).
        column: &'static str,
        /// Observed integer value.
        value: i64,
    },

    /// Database schema version is newer than this binary understands.
    #[error("unsupported schema version: {found}, this binary supports {supported}")]
    UnsupportedSchema {
        /// Version found in the database.
        found: i64,
        /// Version this binary manages.
        supported: i64,
    },
}
