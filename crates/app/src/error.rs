//! Composition failures with their source attached.

use std::fmt::Display;

use sekai_core::{ChunkCoord, RegionKey};

/// Failure for a world tree that reports one region identity twice.
pub(crate) fn duplicate_region(
    key: &RegionKey,
    first: &impl Display,
    second: &impl Display,
) -> AppError {
    AppError::Io(std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        format!("world tree enumerated region {key:?} twice ({first} and {second})"),
    ))
}

/// What a name names, for errors about taken names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameKind {
    /// A branch.
    Branch,
    /// A named scope.
    Scope,
}

/// What a command looked up and did not find.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lookup {
    /// A branch to delete or check out.
    Branch,
    /// A named scope.
    Scope,
}

/// Failures while committing, checking out, or listing commits.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    /// World tree operation failed.
    #[error(transparent)]
    World(#[from] sekai_world::WorldError),
    /// Storage backend operation failed.
    #[error(transparent)]
    Storage(#[from] sekai_storage::StorageError),
    /// Region image failed to parse or build.
    #[error(transparent)]
    Anvil(#[from] sekai_anvil::AnvilError),
    /// Region file failed to parse, with its location attached for diagnosis.
    #[error("failed to process region file {location}: {source}")]
    RegionFailed {
        /// Where the region file lives.
        location: String,
        /// Underlying parse failure.
        #[source]
        source: sekai_anvil::AnvilError,
    },
    /// Checkout plan could not be resolved.
    #[error("{0}")]
    Checkout(crate::checkout::CheckoutError<sekai_storage::StorageError>),
    /// A commit-known region file is missing on disk and the checkout
    /// policy refuses to guess where to rebuild it.
    #[error("region file r.{region_x}.{region_z}.mca of dim {dim_code}, kind {kind_code} is missing and the checkout policy refuses to derive its location", dim_code = key.dim.raw(), kind_code = key.kind.raw(), region_x = key.rx, region_z = key.rz)]
    MissingRegionFile {
        /// Identity of the missing region file.
        key: RegionKey,
    },
    /// Chunk hashing failed.
    #[error(transparent)]
    ChunkHash(#[from] sekai_anvil::ChunkHashError),
    /// Chunk NBT ended before its root compound closed, with its
    /// coordinates attached for diagnosis.
    #[error(
        "chunk {coord} in region r.{rx}.{rz}.mca is truncated: its root compound ends early (the payload is incomplete, not a format mismatch): {source}"
    )]
    TruncatedChunk {
        /// Chunk whose payload is cut short.
        coord: ChunkCoord,
        /// Region X of the owning region file.
        rx: i32,
        /// Region Z of the owning region file.
        rz: i32,
        /// Underlying parse failure that reported the short input.
        #[source]
        source: sekai_anvil::ChunkNbtError,
    },
    /// A payload that could not be reduced to a chunk hash, with its
    /// coordinates attached for diagnosis.
    #[error(
        "chunk {coord} in region r.{rx}.{rz}.mca has no chunk hash (the payload is corrupt, truncated, or of a schema this version does not read): {source}"
    )]
    UnhashableChunk {
        /// Chunk whose payload has no hash.
        coord: ChunkCoord,
        /// Region X of the owning region file.
        rx: i32,
        /// Region Z of the owning region file.
        rz: i32,
        /// Why no key could be derived.
        #[source]
        source: sekai_anvil::ChunkHashError,
    },
    /// Commit-graph operation failed.
    #[error("{0}")]
    Commit(sekai_storage::CommitError<sekai_storage::StorageError>),
    /// A revision could not be resolved to a commit.
    #[error("{0}")]
    Revision(sekai_storage::ResolveError<sekai_storage::StorageError>),
    /// A branch or scope of that name already exists.
    #[error("{}", match .kind {
        NameKind::Branch => format!("a branch named '{name}' already exists"),
        NameKind::Scope => format!("a scope named '{name}' already exists"),
    })]
    AlreadyExists {
        /// What the name names.
        kind: NameKind,
        /// The name.
        name: String,
    },
    /// No branch or scope has that name. Git words the miss per command
    /// (git 2.53).
    #[error("{}", match .kind {
        Lookup::Branch => format!("branch '{name}' not found"),
        Lookup::Scope => format!("no scope named '{name}'"),
    })]
    NotFound {
        /// What the command looked for.
        kind: Lookup,
        /// The name.
        name: String,
    },
    /// Scope would select nothing.
    #[error(transparent)]
    InvalidScope(#[from] sekai_core::NamedScopeError),
    /// No commit is available for the operation: the store is empty or
    /// the active ref is unborn.
    #[error("no commit selected: the store is empty or the active ref is unborn")]
    NoCommits,
    /// I/O operation failed.
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// System clock went backwards or overflowed.
    #[error("system clock error: {0}")]
    Clock(#[from] std::time::SystemTimeError),
    /// System clock reading does not fit a Unix-millisecond `u64`.
    #[error("system clock is out of range")]
    ClockOutOfRange,
}
