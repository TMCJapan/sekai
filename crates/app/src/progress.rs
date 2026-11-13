//! Progress reporting seam between the operations and their callers.
//!
//! What the counted items are is the caller's business: it chose the
//! operation.

/// Receives one run's size and its finished items.
///
/// Pass `()` when no progress is wanted. Methods take `&self` because the
/// operation only borrows its progress, so a stateful implementation needs
/// interior mutability.
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
