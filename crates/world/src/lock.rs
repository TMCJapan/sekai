//! Exclusion between sekai operations over one world.
//!
//! A rollback rewrites region files that a concurrent backup, status, or
//! diff would read, and two rollbacks would interleave their swaps. The
//! lock only coordinates sekai operations: the Minecraft server never
//! takes it, and keeping the server quiet stays the caller's job.
//!
//! How a world is locked is the [`WorldTree`](crate::WorldTree)'s
//! business; [`HostWorldLock`](crate::HostWorldLock) is the file-lock form.

/// How an operation uses a world.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WorldLockMode {
    /// Reads (backup, status, diff): run alongside each other.
    Shared,
    /// Writes (rollback): nothing else may touch the world.
    Exclusive,
}

/// Guard held for as long as an operation needs a world in its mode.
///
/// Dropping the guard releases the lock. Operations share it into the
/// blocking tasks that touch files, so it must cross threads.
pub trait WorldLock: std::fmt::Debug + Send + Sync + 'static {
    /// The mode this guard holds the world in.
    fn mode(&self) -> WorldLockMode;
}

/// What a tree answered when an operation asked to lock it.
#[derive(Debug)]
pub enum WorldLockState<L> {
    /// The world is excluded against other operations until the guard
    /// drops.
    Guarded(L),
    /// The operation runs without exclusion, for the given reason.
    Unguarded(UnguardedReason),
}

impl<L> WorldLockState<L> {
    /// The guard, when the world is excluded.
    ///
    /// The error names why there is no guard, so opting out of exclusion
    /// never happens silently.
    pub fn guard(self) -> Result<L, UnguardedReason> {
        match self {
            Self::Guarded(guard) => Ok(guard),
            Self::Unguarded(reason) => Err(reason),
        }
    }

    /// Why the operation runs unguarded, when it does.
    pub const fn unguarded_reason(&self) -> Option<UnguardedReason> {
        match self {
            Self::Guarded(_) => None,
            Self::Unguarded(reason) => Some(*reason),
        }
    }

    /// Whether the world is excluded against other operations.
    pub const fn is_guarded(&self) -> bool {
        matches!(self, Self::Guarded(_))
    }
}

/// Why an operation runs without excluding against others.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnguardedReason {
    /// The tree holds nothing to protect, so there is nothing to exclude.
    NothingToProtect,
    /// The world is read-only, so no lock file can be created in it.
    ReadOnlyWorld,
}
