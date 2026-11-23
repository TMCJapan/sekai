//! The parallel-execution boundary: per-item work fanned out across a
//! runner's threads, or whatever a backend substitutes for them.
//!
//! Orchestration runs over [`ThreadRunner`] instead of a runtime's task
//! API, so the flows never name an executor: a host embedding composes its
//! own runner (the `sekai` binary's OS-thread pool), and a `no_std`/wasm
//! embedding uses [`Sequential`], the inline reference implementation.
//!
//! The per-item function is synchronous by contract. Async store calls
//! stay on the caller's executor, interleaved with completions through
//! [`JobSet::join_next`]: a worker thread has no runtime to drive a store
//! future, and the caller's runtime must not be blocked by file work.

use core::future::Future;
use core::marker::PhantomData;
use core::pin::Pin;
use core::task::{Context, Poll};

use alloc::string::String;

/// A worker that panicked while running its item.
///
/// The runner captures the panic instead of tearing down the run, so the
/// caller can attribute the failure to the pipeline that spawned the
/// worker and fail that operation alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerPanic {
    message: String,
}

impl WorkerPanic {
    /// Capture a panic with its message.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    /// The panic message, as far as the payload carried one.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl core::fmt::Display for WorkerPanic {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "worker panicked: {}", self.message)
    }
}

impl core::error::Error for WorkerPanic {}

/// Runs per-item work in parallel, at most [`Self::parallelism`] items at
/// once.
///
/// A runner is composed once (the CLI builds it from `--jobs`) and reused
/// by every operation of an instance. Implementations must bound the work
/// in flight: a finished item frees its slot for the next, so a slow item
/// never holds back the items queued behind it, and memory stays bounded
/// by the fan-out.
pub trait ThreadRunner: Sized {
    /// A runner executing at most `parallelism` items at once. `0` means
    /// one per CPU (or 1 when the CPU count is unknown).
    fn new(parallelism: usize) -> Self;

    /// How many items run at once; never zero.
    fn parallelism(&self) -> usize;

    /// Fan `items` out across the runner, applying `f` to each.
    ///
    /// The returned [`JobSet`] yields completions in completion order;
    /// dropping it stops the items still queued, while the items already
    /// running finish on their own (they may hold locks or bytes the caller
    /// must not see freed early). A worker panic surfaces as [`WorkerPanic`]
    /// in place of that item's output.
    fn run<I, F, T, R>(&self, items: I, f: F) -> impl JobSet<R>
    where
        I: IntoIterator<Item = T>,
        I::IntoIter: Send + 'static,
        T: Send + 'static,
        R: Send + 'static,
        F: Fn(T) -> R + Send + Sync + 'static;

    /// Run one blocking task on the runner, away from the caller's
    /// executor. The single-item form of [`Self::run`] for work that has
    /// no fan-out, such as world discovery.
    fn call<F, R>(&self, f: F) -> impl Future<Output = Result<R, WorkerPanic>>
    where
        F: FnOnce() -> R + Send + 'static,
        R: Send + 'static;
}

/// The completions of one [`ThreadRunner::run`].
///
/// Joining is async so the caller's executor stays free while workers
/// run; the caller interleaves its own async work (store writes, progress
/// reporting) between joins.
pub trait JobSet<R> {
    /// The next completion, or `None` once every item has finished and
    /// been joined. A panicked worker yields `Err` for its item.
    fn join_next(&mut self) -> impl Future<Output = Option<Result<R, WorkerPanic>>>;

    /// The next completion if one is already ready, without waiting.
    /// `None` means nothing is ready yet (or the set is exhausted); drain
    /// it to surface completions while feeding more work.
    fn try_join_next(&mut self) -> Option<Result<R, WorkerPanic>>;

    /// Stop the items still queued; the items already running finish and
    /// can still be joined. Joining to exhaustion after a cancel waits for
    /// every worker to release what it holds.
    fn cancel(&mut self);
}

/// A [`ThreadRunner`] that runs every item inline on the joining thread.
///
/// The `no_std` baseline: no threads, no executor, work happens during
/// [`JobSet::join_next`]. Nothing runs in the background, so
/// [`JobSet::try_join_next`] never has a completion ready - important for
/// feed loops that drain before handing over more work: an inline runner
/// must not pull from a still-empty channel there. Useful on wasm and in
/// tests; a parallel embedding implements [`ThreadRunner`] over its own
/// threading instead.
#[derive(Debug, Clone, Copy, Default)]
pub struct Sequential;

impl ThreadRunner for Sequential {
    fn new(_: usize) -> Self {
        Self
    }

    fn parallelism(&self) -> usize {
        1
    }

    // A concrete return type keeps the jobs nameable for callers.
    #[allow(refining_impl_trait)]
    fn run<I, F, T, R>(&self, items: I, f: F) -> SequentialJobs<I::IntoIter, F, R>
    where
        I: IntoIterator<Item = T>,
        I::IntoIter: Send + 'static,
        T: Send + 'static,
        R: Send + 'static,
        F: Fn(T) -> R + Send + Sync + 'static,
    {
        SequentialJobs {
            items: items.into_iter(),
            f,
            stopped: false,
            _output: PhantomData,
        }
    }

    fn call<F, R>(&self, f: F) -> impl Future<Output = Result<R, WorkerPanic>>
    where
        F: FnOnce() -> R + Send + 'static,
        R: Send + 'static,
    {
        core::future::ready(Ok(f()))
    }
}

/// The inline completions of a [`Sequential`] run.
pub struct SequentialJobs<I, F, R> {
    items: I,
    f: F,
    stopped: bool,
    _output: PhantomData<R>,
}

impl<I, F, R> JobSet<R> for SequentialJobs<I, F, R>
where
    I: Iterator,
    F: Fn(I::Item) -> R,
{
    fn join_next(&mut self) -> impl Future<Output = Option<Result<R, WorkerPanic>>> {
        SequentialNext { jobs: self }
    }

    fn try_join_next(&mut self) -> Option<Result<R, WorkerPanic>> {
        // Nothing runs in the background, so nothing is ever ready without
        // a join; pulling here would block feed loops on their own channel.
        None
    }

    fn cancel(&mut self) {
        self.stopped = true;
    }
}

/// One inline completion; the work runs during the poll.
struct SequentialNext<'a, I, F, R> {
    jobs: &'a mut SequentialJobs<I, F, R>,
}

impl<I, F, R> Future for SequentialNext<'_, I, F, R>
where
    I: Iterator,
    F: Fn(I::Item) -> R,
{
    type Output = Option<Result<R, WorkerPanic>>;

    fn poll(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Self::Output> {
        let jobs = &mut *self.get_mut().jobs;
        if jobs.stopped {
            return Poll::Ready(None);
        }
        Poll::Ready(jobs.items.next().map(|item| Ok((jobs.f)(item))))
    }
}

#[cfg(test)]
mod tests {
    use super::{JobSet, Sequential, ThreadRunner};
    use alloc::vec;
    use alloc::vec::Vec;
    use core::future::Future;
    use core::pin::pin;
    use core::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};

    /// A waker that does nothing: the inline futures complete in one poll.
    fn noop_waker() -> Waker {
        fn noop(_: *const ()) {}
        fn clone(_: *const ()) -> RawWaker {
            RawWaker::new(core::ptr::null(), &VTABLE)
        }
        static VTABLE: RawWakerVTable = RawWakerVTable::new(clone, noop, noop, noop);
        // SAFETY: the vtable's functions are all no-ops and never touch
        // the null data pointer.
        unsafe { Waker::from_raw(RawWaker::new(core::ptr::null(), &VTABLE)) }
    }

    fn poll_once<F: Future>(fut: F) -> Poll<F::Output> {
        let waker = noop_waker();
        let mut cx = Context::from_waker(&waker);
        pin!(fut).poll(&mut cx)
    }

    #[test]
    fn sequential_runs_every_item_in_order() {
        let runner = Sequential::new(4);
        let mut jobs = runner.run(vec![1, 2, 3], |x| x * 2);
        // Nothing runs in the background: a drain before joining is empty.
        assert!(jobs.try_join_next().is_none());
        let mut out = Vec::new();
        while let Poll::Ready(Some(item)) = poll_once(jobs.join_next()) {
            out.push(item.unwrap());
        }
        assert_eq!(out, vec![2, 4, 6]);
        assert!(matches!(poll_once(jobs.join_next()), Poll::Ready(None)));
    }

    #[test]
    fn sequential_call_runs_inline() {
        let runner = Sequential::new(0);
        assert!(matches!(poll_once(runner.call(|| 7)), Poll::Ready(Ok(7))));
    }
}
