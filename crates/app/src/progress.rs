//! Progress reporting seam between the operations and their callers.
//!
//! An operation tells its [`Progress`] how many items the run holds once it
//! knows, then reports each item as it finishes. What the items are is the
//! caller's business: it chose the operation.

/// Receives one run's size and its finished items.
///
/// `()` is the progress to pass when none is wanted. Methods take `&self`
/// because the operation only borrows its progress; an implementation that
/// keeps state needs interior mutability.
pub trait Progress: Sync {
    /// The run holds `total` items. Called once, before any [`step`](Self::step).
    fn start(&self, total: usize);

    /// Record one more finished item.
    fn step(&self);
}

impl Progress for () {
    fn start(&self, _: usize) {}

    fn step(&self) {}
}
