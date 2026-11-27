//! The parallel-execution boundary: per-item work fanned out across a
//! runner's threads, or whatever a backend substitutes for them.
//!
//! Orchestration runs over [`ThreadRunner`] instead of a runtime's task
//! API, so the flows never name an executor: a host embedding composes its
//! own runner (the `sekai` binary's OS-thread pool), and a `no_std`/wasm
//! embedding uses [`Sequential`], the inline reference implementation.
//!
//! [`ThreadRunner::run`] applies a synchronous function per item on the
//! runner's threads; async store calls stay on the caller's executor,
//! interleaved with completions through [`JobSet::join_next`]: a worker
//! thread has no runtime to drive a store future, and the caller's runtime
//! must not be blocked by file work. [`ThreadRunner::run_async`] is the
//! async-per-item variant: its futures are driven on the caller's executor,
//! and any blocking file work inside them goes back through
//! [`ThreadRunner::call`].

use core::future::Future;
use core::marker::PhantomData;
use core::pin::Pin;
use core::task::{Context, Poll};

use alloc::boxed::Box;
use alloc::collections::VecDeque;
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

    /// Fan `items` out as async per-item work, at most [`Self::parallelism`]
    /// futures in flight, driven on the caller's executor.
    ///
    /// The async counterpart of [`Self::run`]: each item's future is polled
    /// by [`JobSet::join_next`] on the joining thread, so it may await store
    /// calls directly. Blocking file work inside it must still go through
    /// [`Self::call`], keeping it off the caller's executor and its
    /// world-lock share on a worker thread. Nothing runs in the background,
    /// so [`JobSet::try_join_next`] never has a completion ready; cancelling
    /// stops queuing new items while the futures already in flight are
    /// polled to completion, and dropping the set abandons them.
    fn run_async<I, F, Fut, T, R>(&self, items: I, f: F) -> impl JobSet<R>
    where
        I: IntoIterator<Item = T>,
        F: Fn(T) -> Fut,
        Fut: Future<Output = R>,
    {
        AsyncJobs {
            items: items.into_iter(),
            f,
            active: VecDeque::new(),
            capacity: self.parallelism(),
            stopped: false,
            _output: PhantomData,
        }
    }
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

/// The completions of one [`ThreadRunner::run_async`]: up to `capacity`
/// per-item futures polled in turn on the joining thread.
///
/// A future only advances while [`JobSet::join_next`] polls it, so the bytes
/// each item holds stay bounded by the fan-out. Cancelling stops pulling new
/// items; the futures already in flight are polled to completion, which is
/// what lets a caller wait for every blocking write they spawned to release
/// what it holds before returning.
pub struct AsyncJobs<I, F, Fut, R> {
    items: I,
    f: F,
    active: VecDeque<Pin<Box<Fut>>>,
    capacity: usize,
    stopped: bool,
    _output: PhantomData<R>,
}

impl<I, F, Fut, R> JobSet<R> for AsyncJobs<I, F, Fut, R>
where
    I: Iterator,
    F: Fn(I::Item) -> Fut,
    Fut: Future<Output = R>,
{
    fn join_next(&mut self) -> impl Future<Output = Option<Result<R, WorkerPanic>>> {
        AsyncNext { jobs: self }
    }

    fn try_join_next(&mut self) -> Option<Result<R, WorkerPanic>> {
        // Futures advance only during a join, so nothing is ever ready
        // without one.
        None
    }

    fn cancel(&mut self) {
        self.stopped = true;
    }
}

/// One async join: refill the free slots, then poll the in-flight futures in
/// turn until one completes.
struct AsyncNext<'a, I, F, Fut, R> {
    jobs: &'a mut AsyncJobs<I, F, Fut, R>,
}

impl<I, F, Fut, R> Future for AsyncNext<'_, I, F, Fut, R>
where
    I: Iterator,
    F: Fn(I::Item) -> Fut,
    Fut: Future<Output = R>,
{
    type Output = Option<Result<R, WorkerPanic>>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let jobs = &mut *self.get_mut().jobs;
        while !jobs.stopped && jobs.active.len() < jobs.capacity {
            match jobs.items.next() {
                Some(item) => jobs.active.push_back(Box::pin((jobs.f)(item))),
                None => break,
            }
        }
        // Rotate through the in-flight futures once; the first completion
        // wins, the rest keep their place for the next join.
        for _ in 0..jobs.active.len() {
            let Some(mut fut) = jobs.active.pop_front() else {
                break;
            };
            match fut.as_mut().poll(cx) {
                Poll::Ready(out) => return Poll::Ready(Some(Ok(out))),
                Poll::Pending => jobs.active.push_back(fut),
            }
        }
        if jobs.active.is_empty() {
            Poll::Ready(None)
        } else {
            Poll::Pending
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{JobSet, Sequential, ThreadRunner, WorkerPanic};
    use alloc::vec;
    use alloc::vec::Vec;
    use core::future::Future;
    use core::pin::{Pin, pin};
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

    /// A runner with a fixed fan-out, to exercise `run_async`'s capacity.
    struct Fixed(usize);

    impl ThreadRunner for Fixed {
        fn new(n: usize) -> Self {
            Self(n.max(1))
        }

        fn parallelism(&self) -> usize {
            self.0
        }

        fn run<I, F, T, R>(&self, items: I, f: F) -> impl JobSet<R>
        where
            I: IntoIterator<Item = T>,
            I::IntoIter: Send + 'static,
            T: Send + 'static,
            R: Send + 'static,
            F: Fn(T) -> R + Send + Sync + 'static,
        {
            Sequential.run(items, f)
        }

        fn call<F, R>(&self, f: F) -> impl Future<Output = Result<R, WorkerPanic>>
        where
            F: FnOnce() -> R + Send + 'static,
            R: Send + 'static,
        {
            Sequential.call(f)
        }
    }

    /// A future that yields `Pending` for `remaining` polls, then completes.
    struct Steps {
        remaining: usize,
        value: usize,
    }

    impl Future for Steps {
        type Output = usize;

        fn poll(mut self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<usize> {
            if self.remaining == 0 {
                Poll::Ready(self.value)
            } else {
                self.remaining -= 1;
                Poll::Pending
            }
        }
    }

    fn drain<R>(jobs: &mut impl JobSet<R>) -> Vec<R> {
        let mut out = Vec::new();
        while let Poll::Ready(Some(item)) = poll_once(jobs.join_next()) {
            out.push(item.unwrap());
        }
        out
    }

    #[test]
    fn run_async_runs_every_item() {
        let runner = Fixed::new(2);
        let mut jobs = runner.run_async(0..4, |x| async move { x * 2 });
        assert_eq!(drain(&mut jobs), vec![0, 2, 4, 6]);
    }

    #[test]
    fn run_async_bounds_the_futures_in_flight() {
        use alloc::rc::Rc;
        use core::cell::Cell;

        let started = Rc::new(Cell::new(0));
        let count = Rc::clone(&started);
        let runner = Fixed::new(3);
        let mut jobs = runner.run_async(0..10, move |x| {
            count.set(count.get() + 1);
            async move { x }
        });
        // Futures are created lazily during a join, capped at the fan-out.
        assert_eq!(started.get(), 0);
        assert!(matches!(
            poll_once(jobs.join_next()),
            Poll::Ready(Some(Ok(0)))
        ));
        assert_eq!(started.get(), 3);
        assert_eq!(drain(&mut jobs).len(), 9);
    }

    #[test]
    fn run_async_cancel_drains_the_in_flight_futures() {
        let runner = Fixed::new(2);
        let mut jobs = runner.run_async(vec![10, 11, 12, 13], |x| Steps {
            remaining: 1,
            value: x,
        });
        // Start the first two futures; both are still pending.
        assert!(matches!(poll_once(jobs.join_next()), Poll::Pending));
        jobs.cancel();
        // The queued items are dropped, but the two in flight still finish.
        assert_eq!(drain(&mut jobs), vec![10, 11]);
    }
}
