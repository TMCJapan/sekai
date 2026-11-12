//! Progress reporting seam between the operations and their callers.
//!
//! An operation asks its [`ProgressBuilder`] for a reporter once it knows
//! how many items the run holds, then reports the running count. What the
//! items are is the caller's business: it chose the operation.

/// Hands an operation the reporter for its run, sized by the run's total.
///
/// Defaults to a reporter that drops its events; `()` shows nothing. A
/// reporter may borrow the builder: the operation holds it only while it
/// runs.
pub trait ProgressBuilder: Sync {
    /// Reporter for one run over `total` items.
    fn start(&self, _total: usize) -> impl Reporter {}
}

impl ProgressBuilder for () {}

/// Receives one run's running count.
pub trait Reporter: Send {
    /// Record one finished item: `done` of the run's items have finished
    /// so far.
    fn step(&mut self, done: usize);
}

impl Reporter for () {
    fn step(&mut self, _: usize) {}
}
