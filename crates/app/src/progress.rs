//! Progress reporting seam between the operations and their callers.
//!
//! An operation asks its [`BuildProgress`] for a [`Progress`] once it knows
//! how many items the run holds, then advances that progress as items
//! finish. What the items are is the caller's business: it chose the
//! operation.

/// Hands an operation the [`Progress`] for its run, sized by the run's total.
///
/// Pass `()` when no progress is wanted. The progress may borrow its
/// builder: the operation holds it only while it runs.
pub trait BuildProgress: Sync {
    /// Progress for one run over `total` items.
    fn build_progress(&self, total: usize) -> impl Progress;
}

/// Receives one run's finished items.
pub trait Progress: Send {
    /// Record one more finished item.
    fn step(&mut self);
}

impl BuildProgress for () {
    fn build_progress(&self, _: usize) -> impl Progress {}
}

impl Progress for () {
    fn step(&mut self) {}
}
