//! Progress reporting seam between the operations and their callers.
//!
//! An operation asks its [`ProgressBuilder`] for a reporter once it knows
//! how many items the run holds, then reports each item as it finishes.
//! What the items are is the caller's business: it chose the operation.

/// Hands an operation the reporter for its run, sized by the run's total.
///
/// `()` is the builder to pass when no progress is wanted. A reporter may
/// borrow the builder: the operation holds it only while it runs.
pub trait ProgressBuilder: Sync {
    /// Reporter for one run over `total` items.
    fn start(&self, total: usize) -> impl Reporter;
}

/// Receives one run's finished items.
pub trait Reporter: Send {
    /// Record one more finished item.
    fn step(&mut self);
}

impl ProgressBuilder for () {
    fn start(&self, _: usize) -> impl Reporter {}
}

impl Reporter for () {
    fn step(&mut self) {}
}
