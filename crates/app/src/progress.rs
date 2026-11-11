//! Progress reporting seam between the operations and their callers.
//!
//! An operation receives a [`ProgressBuilder`] and asks it for a reporter
//! once, as soon as it knows how much work the run holds; the reporter then
//! receives every event that run produces. The total arrives with the
//! reporter and each event carries running counts, so a reporter needs no
//! state of its own to show where the run stands.

/// Hands each operation the reporter for its run, sized by the run's total.
///
/// Every method defaults to a reporter that drops its events, so an
/// implementation spells out only the operations it shows; `()` shows none.
/// A reporter may borrow the builder: the operation holds it only while it
/// runs.
pub trait ProgressBuilder: Sync {
    /// Reporter for one commit, which ingests `files_total` files.
    fn commit(&self, _files_total: usize) -> impl CommitReporter {}

    /// Reporter for one status preview, which counts the `files_total`
    /// files a commit would ingest.
    fn status(&self, _files_total: usize) -> impl CommitReporter {}

    /// Reporter for one checkout, which visits `files_total` files.
    fn checkout(&self, _files_total: usize) -> impl CheckoutReporter {}

    /// Reporter for one clone, which writes `files_total` files.
    fn clone_to(&self, _files_total: usize) -> impl CloneReporter {}

    /// Reporter for one chunk diff over `chunks_total` chunks.
    fn diff(&self, _chunks_total: usize) -> impl DiffReporter {}

    /// Reporter for one garbage collection apply, which examines
    /// `blobs_total` orphan candidates.
    fn gc(&self, _blobs_total: usize) -> impl GcReporter {}

    /// Reporter for one prune apply, which deletes `commits_total` commits.
    fn prune(&self, _commits_total: usize) -> impl PruneReporter {}
}

impl ProgressBuilder for () {}

/// Receives a commit's or status preview's per-file events.
pub trait CommitReporter: Send {
    /// Record one finished file: `files_done` files and `chunks_done`
    /// chunks have been ingested so far.
    fn step(&mut self, files_done: usize, chunks_done: usize);
}

/// Receives a checkout's per-file events.
pub trait CheckoutReporter: Send {
    /// Record one finished file: `files_done` files have been rebuilt and
    /// `chunks_done` chunks restored so far.
    fn step(&mut self, files_done: usize, chunks_done: usize);
}

/// Receives a clone's per-file events.
pub trait CloneReporter: Send {
    /// Record one written file: `files_done` files have been rebuilt and
    /// `chunks_done` chunks restored so far.
    fn step(&mut self, files_done: usize, chunks_done: usize);
}

/// Receives a chunk diff's per-chunk events.
pub trait DiffReporter: Send {
    /// Record one compared chunk: `chunks_done` chunks have been diffed so
    /// far.
    fn step(&mut self, chunks_done: usize);
}

/// Receives a garbage collection's per-candidate events.
pub trait GcReporter: Send {
    /// Record one examined candidate: `blobs_done` orphan candidates have
    /// been examined so far.
    fn step(&mut self, blobs_done: usize);
}

/// Receives a prune's per-commit events.
pub trait PruneReporter: Send {
    /// Record one deleted commit: `commits_done` commits have been deleted
    /// so far.
    fn step(&mut self, commits_done: usize);
}

impl CommitReporter for () {
    fn step(&mut self, _: usize, _: usize) {}
}

impl CheckoutReporter for () {
    fn step(&mut self, _: usize, _: usize) {}
}

impl CloneReporter for () {
    fn step(&mut self, _: usize, _: usize) {}
}

impl DiffReporter for () {
    fn step(&mut self, _: usize) {}
}

impl GcReporter for () {
    fn step(&mut self, _: usize) {}
}

impl PruneReporter for () {
    fn step(&mut self, _: usize) {}
}
