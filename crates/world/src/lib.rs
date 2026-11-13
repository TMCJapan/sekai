//! World access boundary: the [`WorldTree`] trait and the [`RegionFile`]
//! handle it hands out.
//!
//! A tree names the region files it holds, derives the ones that are
//! missing, and hands out [`RegionFile`] handles that read, size,
//! fingerprint, write, and remove them - nothing else. The operations
//! never see a path, so they do not depend on the tree behind them.
//! [`WorldTree::lock`] excludes concurrent sekai operations over one
//! world: [`WorldLockMode`] says how the operation uses it, and the
//! [`WorldLock`] guard the tree hands out keeps the world excluded until
//! it drops.
//!
//! The host filesystem implementation lives in `sekai-world-local`.

#![no_std]

extern crate alloc;

mod lock;
mod region;
mod tree;

pub use lock::{UnguardedReason, WorldLock, WorldLockMode, WorldLockState};
pub use region::{Region, RegionFile};
pub use tree::WorldTree;
