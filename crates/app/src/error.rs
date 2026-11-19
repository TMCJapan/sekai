//! Composition failures with their source attached.

use std::fmt::Display;
use std::path::PathBuf;

use sekai_core::{ChunkCoord, Dimension, RegionKey, WorktreeName};

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

/// What a command tried to do with a branch another worktree has checked
/// out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BranchUse {
    /// `checkout` and `worktree add`.
    CheckOut,
    /// `branch -f`, and `branch -M` over it.
    ForceUpdate,
    /// `branch -d` and `branch -D`.
    Delete,
}

/// What a name names, for errors about taken or malformed names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameKind {
    /// A branch.
    Branch,
    /// A tag.
    Tag,
    /// A named scope.
    Scope,
}

impl From<sekai_core::RefKind> for NameKind {
    fn from(kind: sekai_core::RefKind) -> Self {
        match kind {
            sekai_core::RefKind::Branch => Self::Branch,
            sekai_core::RefKind::Tag => Self::Tag,
        }
    }
}

/// What a command looked up and did not find.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lookup {
    /// A branch to delete or check out.
    Branch,
    /// A branch to rename, which Git words differently.
    RenamedBranch,
    /// A tag.
    Tag,
    /// A named scope.
    Scope,
}

impl From<sekai_core::ResolveError<sekai_storage::StorageError>> for AppError {
    fn from(error: sekai_core::ResolveError<sekai_storage::StorageError>) -> Self {
        Self::CommitRef(error)
    }
}

impl From<sekai_core::CommitError<sekai_storage::StorageError>> for AppError {
    fn from(error: sekai_core::CommitError<sekai_storage::StorageError>) -> Self {
        Self::Commit(error)
    }
}

impl From<sekai_core::RefKind> for Lookup {
    fn from(kind: sekai_core::RefKind) -> Self {
        match kind {
            sekai_core::RefKind::Branch => Self::Branch,
            sekai_core::RefKind::Tag => Self::Tag,
        }
    }
}

/// Why `worktree remove` refuses its argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unremovable {
    /// No worktree matches it.
    NoMatch,
    /// It matches the main worktree.
    Main,
}

/// Failures while committing, checking out, or listing commits.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    /// World filesystem operation failed.
    #[cfg(feature = "backend-local")]
    #[error(transparent)]
    World(#[from] sekai_world_local::WorldError),
    /// Storage backend operation failed.
    #[error(transparent)]
    Storage(#[from] sekai_storage::StorageError),
    /// Region image failed to parse or build.
    #[error(transparent)]
    Anvil(#[from] sekai_anvil::AnvilError),
    /// Checkout plan could not be resolved.
    #[error("{0}")]
    Checkout(sekai_core::CheckoutError<sekai_storage::StorageError>),
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
    Commit(sekai_core::CommitError<sekai_storage::StorageError>),
    /// Commit reference (`<64-char hash>` or ref name) could not be resolved.
    #[error("{0}")]
    CommitRef(sekai_core::ResolveError<sekai_storage::StorageError>),
    /// A branch, tag, or scope of that name already exists.
    #[error("{}", match .kind {
        NameKind::Branch => format!("a branch named '{name}' already exists"),
        NameKind::Tag => format!("tag '{name}' already exists"),
        NameKind::Scope => format!("a scope named '{name}' already exists"),
    })]
    AlreadyExists {
        /// What the name names.
        kind: NameKind,
        /// The name.
        name: String,
    },
    /// No branch, tag, or scope has that name. Git words the miss per
    /// command (git 2.53).
    #[error("{}", match .kind {
        Lookup::Branch => format!("branch '{name}' not found"),
        Lookup::RenamedBranch => format!("no branch named '{name}'"),
        Lookup::Tag => format!("tag '{name}' not found."),
        Lookup::Scope => format!("no scope named '{name}'"),
    })]
    NotFound {
        /// What the command looked for.
        kind: Lookup,
        /// The name.
        name: String,
    },
    /// A name Git refuses for a new branch or tag, or a `worktree add`
    /// basename that is not a valid branch name.
    #[error("{}", match .kind {
        NameKind::Branch => format!("'{name}' is not a valid branch name"),
        NameKind::Tag => format!("'{name}' is not a valid tag name."),
        NameKind::Scope => format!("'{name}' is not a valid scope name"),
    })]
    InvalidName {
        /// What the name would name.
        kind: NameKind,
        /// The name.
        name: String,
    },
    /// `worktree remove` matched no worktree, or the main one.
    #[error("{}", match .reason {
        Unremovable::NoMatch => format!("'{worktree}' is not a working tree"),
        Unremovable::Main => format!("'{worktree}' is a main working tree"),
    })]
    UnremovableWorktree {
        /// Why the worktree cannot be removed.
        reason: Unremovable,
        /// The argument as given.
        worktree: String,
    },
    /// Another worktree has the branch checked out, so it cannot be
    /// checked out, moved by force, or deleted. Git words the refusal per
    /// command (git 2.53).
    #[error("{}", match .action {
        BranchUse::CheckOut => format!("'{branch}' is already used by worktree at '{worktree}'"),
        BranchUse::ForceUpdate => format!("cannot force update the branch '{branch}' used by worktree at '{worktree}'"),
        BranchUse::Delete => format!("cannot delete branch '{branch}' used by worktree at '{worktree}'"),
    })]
    BranchCheckedOut {
        /// What the command tried to do.
        action: BranchUse,
        /// Short name of the branch.
        branch: String,
        /// World root of the worktree that has it checked out, or its
        /// name when the instance does not know where its store lies.
        worktree: String,
    },
    /// `checkout` found HEAD unborn, with no commit to act on.
    #[error("You are on a branch yet to be born")]
    UnbornBranch,
    /// A command that reads or writes the world, or copies it as
    /// `worktree add` does, ran outside every worktree.
    #[error("this operation must be run in a work tree")]
    NoWorkTree,
    /// `init` found no world layout at the path.
    #[error("not a Minecraft world: {}", path.display())]
    NotAWorld {
        /// The directory `init` was given.
        path: PathBuf,
    },
    /// Discovery found no `.sekai` from the start up.
    #[error(
        "not in a sekai world (or any parent up to the filesystem boundary): {}",
        start.display()
    )]
    NotInWorld {
        /// Where discovery started.
        start: PathBuf,
    },
    /// `init` found the world's `.sekai` naming another store.
    #[error("{} already belongs to another store through {}", world.display(), sekai.display())]
    WorldInOtherStore {
        /// The world `init` was given.
        world: PathBuf,
        /// The `.sekai` that names the other store.
        sekai: PathBuf,
    },
    /// `init` found an existing store that lacks the world.
    #[error("store {} already exists for another world", store.display())]
    StoreHasOtherWorld {
        /// The existing store.
        store: PathBuf,
    },
    /// A link file at a world root could not be read as one.
    #[error("bad link file {}: {reason}", path.display())]
    BadLinkFile {
        /// The link file.
        path: PathBuf,
        /// What is wrong with it.
        reason: &'static str,
    },
    /// A store found at a world root has no main worktree.
    #[error("store {} has no main worktree", store.display())]
    NoMainWorktree {
        /// The store.
        store: PathBuf,
    },
    /// A path the store or a link file records is not UTF-8.
    #[error("path is not UTF-8: {}", path.display())]
    NonUtf8Path {
        /// The path.
        path: PathBuf,
    },
    /// `worktree add` found its target neither missing nor empty.
    #[error("'{}' already exists", path.display())]
    TargetExists {
        /// The target as given.
        path: PathBuf,
    },
    /// `worktree add` found a region directory of the copy that a symlink
    /// leads out of it, where rebuilding would write another world's files.
    #[error("'{}' leads outside the new worktree through a symlink", path.display())]
    RegionDirOutsideTarget {
        /// The region directory in the copy.
        path: PathBuf,
    },
    /// `worktree add` found a worktree already registered at its path,
    /// whose world is gone: the target is empty or missing.
    #[error("'{}' is a missing but already registered worktree; use 'worktree remove' to clear it", path.display())]
    PathRegistered {
        /// The target as given.
        path: PathBuf,
        /// The worktree registered there.
        worktree: WorktreeName,
    },
    /// `worktree add` found a dimension in the commit that the source
    /// world does not resolve, so its region files have nowhere to go.
    #[error("the commit has dimension {dim}, which the source world does not resolve")]
    UnresolvedDimension {
        /// The dimension.
        dim: Dimension,
    },
    /// Checkout would overwrite or drop uncommitted chunks in these
    /// regions; `force` discards them.
    #[error("your local changes to {} region files would be overwritten by checkout", regions.len())]
    WouldOverwrite {
        /// Regions holding the uncommitted chunks, in key order.
        regions: Vec<RegionKey>,
    },
    /// `branch -d` refused a branch HEAD does not reach.
    #[error("the branch '{branch}' is not fully merged")]
    NotFullyMerged {
        /// Short name of the branch.
        branch: String,
    },
    /// `branch -m <new>` found HEAD detached, with no branch to rename.
    #[error("cannot rename the current branch because HEAD is not on one")]
    RenameDetachedHead,
    /// A built-in ref name failed validation.
    #[error(transparent)]
    RefName(#[from] sekai_core::RefNameError),
    /// A built-in worktree name failed validation.
    #[error(transparent)]
    WorktreeName(#[from] sekai_core::WorktreeNameError),
    /// Scope would select nothing.
    #[error(transparent)]
    InvalidScope(#[from] sekai_core::NamedScopeError),
    /// No commit is available for the operation: the store is empty or
    /// HEAD is unborn.
    #[error("no commit selected: the store is empty or HEAD is unborn")]
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
    /// Background worker task failed.
    #[error("worker task failed: {0}")]
    Join(#[from] tokio::task::JoinError),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn named(kind: NameKind) -> [String; 2] {
        let name = String::from("x");
        [
            AppError::AlreadyExists {
                kind,
                name: name.clone(),
            }
            .to_string(),
            AppError::InvalidName { kind, name }.to_string(),
        ]
    }

    /// Git's wording for each kind of name, as git 2.53 prints it; scopes
    /// are sekai's own and follow the branch wording.
    #[test]
    fn names_are_worded_per_kind() {
        assert_eq!(
            named(NameKind::Branch),
            [
                "a branch named 'x' already exists",
                "'x' is not a valid branch name"
            ]
        );
        assert_eq!(
            named(NameKind::Tag),
            ["tag 'x' already exists", "'x' is not a valid tag name."]
        );
        assert_eq!(
            named(NameKind::Scope),
            [
                "a scope named 'x' already exists",
                "'x' is not a valid scope name"
            ]
        );
        let missing = |kind| {
            AppError::NotFound {
                kind,
                name: String::from("x"),
            }
            .to_string()
        };
        assert_eq!(missing(Lookup::Branch), "branch 'x' not found");
        assert_eq!(missing(Lookup::RenamedBranch), "no branch named 'x'");
        assert_eq!(missing(Lookup::Tag), "tag 'x' not found.");
        assert_eq!(missing(Lookup::Scope), "no scope named 'x'");
    }

    #[test]
    fn worktree_refusals_are_worded_per_case() {
        let checked_out = |action| {
            AppError::BranchCheckedOut {
                action,
                branch: String::from("dev"),
                worktree: String::from("/w"),
            }
            .to_string()
        };
        assert_eq!(
            checked_out(BranchUse::CheckOut),
            "'dev' is already used by worktree at '/w'"
        );
        assert_eq!(
            checked_out(BranchUse::ForceUpdate),
            "cannot force update the branch 'dev' used by worktree at '/w'"
        );
        assert_eq!(
            checked_out(BranchUse::Delete),
            "cannot delete branch 'dev' used by worktree at '/w'"
        );
        let unremovable = |reason| {
            AppError::UnremovableWorktree {
                reason,
                worktree: String::from("w"),
            }
            .to_string()
        };
        assert_eq!(
            unremovable(Unremovable::NoMatch),
            "'w' is not a working tree"
        );
        assert_eq!(unremovable(Unremovable::Main), "'w' is a main working tree");
    }
}
