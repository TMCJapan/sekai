//! Pure domain policy: the content-hash vocabulary and commit staging.
//!
//! `core` depends only on `util`. The storage traits and the generic
//! algorithms over them live in `sekai-storage`; the orchestration that
//! runs them against a world lives in `sekai-app`.

#![no_std]

extern crate alloc;

/// Pure commit-staging policy (no store access).
pub mod staging;

pub use staging::{Assembled, Ingested, Plan, Previous, assemble, stage_present};
