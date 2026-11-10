//! Progress reporting seam between the operations and their callers.
//!
//! An operation receives a [`ProgressBuilder`] and asks it for a reporter
//! once, when it starts; the reporter then receives every event that run
//! produces. Each event carries running totals, so a reporter needs no
//! state of its own to show where the run stands.

use crate::{
    CheckoutProgress, CloneProgress, CommitProgress, DiffProgress, GcProgress, PruneProgress,
};

/// Hands each operation the reporter for its run.
///
/// Every method defaults to a reporter that drops its events, so an
/// implementation spells out only the operations it shows; `()` shows none.
/// A reporter may borrow the builder: the operation holds it only while it
/// runs.
pub trait ProgressBuilder: Sync {
    /// Reporter for one commit.
    fn commit(&self) -> impl CommitReporter {}

    /// Reporter for one status preview, which counts the files a commit
    /// would ingest.
    fn status(&self) -> impl CommitReporter {}

    /// Reporter for one checkout.
    fn checkout(&self) -> impl CheckoutReporter {}

    /// Reporter for one clone.
    fn clone_to(&self) -> impl CloneReporter {}

    /// Reporter for one chunk diff.
    fn diff(&self) -> impl DiffReporter {}

    /// Reporter for one garbage collection apply.
    fn gc(&self) -> impl GcReporter {}

    /// Reporter for one prune apply.
    fn prune(&self) -> impl PruneReporter {}
}

impl ProgressBuilder for () {}

/// Receives a commit's or status preview's events (see [`CommitProgress`]).
pub trait CommitReporter: Send {
    /// Record one event.
    fn step(&mut self, progress: CommitProgress);
}

/// Receives a checkout's events (see [`CheckoutProgress`]).
pub trait CheckoutReporter: Send {
    /// Record one event.
    fn step(&mut self, progress: CheckoutProgress);
}

/// Receives a clone's events (see [`CloneProgress`]).
pub trait CloneReporter: Send {
    /// Record one event.
    fn step(&mut self, progress: CloneProgress);
}

/// Receives a chunk diff's events (see [`DiffProgress`]).
pub trait DiffReporter: Send {
    /// Record one event.
    fn step(&mut self, progress: DiffProgress);
}

/// Receives a garbage collection's events (see [`GcProgress`]).
pub trait GcReporter: Send {
    /// Record one event.
    fn step(&mut self, progress: GcProgress);
}

/// Receives a prune's events (see [`PruneProgress`]).
pub trait PruneReporter: Send {
    /// Record one event.
    fn step(&mut self, progress: PruneProgress);
}

impl CommitReporter for () {
    fn step(&mut self, _: CommitProgress) {}
}

impl CheckoutReporter for () {
    fn step(&mut self, _: CheckoutProgress) {}
}

impl CloneReporter for () {
    fn step(&mut self, _: CloneProgress) {}
}

impl DiffReporter for () {
    fn step(&mut self, _: DiffProgress) {}
}

impl GcReporter for () {
    fn step(&mut self, _: GcProgress) {}
}

impl PruneReporter for () {
    fn step(&mut self, _: PruneProgress) {}
}
