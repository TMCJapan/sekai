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
pub trait WorldLock: std::fmt::Debug + Send + Sync + 'static {}
