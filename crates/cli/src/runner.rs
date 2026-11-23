//! The CLI's host [`ThreadRunner`]: a bounded pool of OS threads.
//!
//! The orchestration library (`sekai-app`) names no executor and ships no
//! host runner; the binary composes this one into the instance from
//! `--jobs`. Workers pull items from the shared iterator, so a finished
//! item frees its slot at once - the caller never re-spawns. Completions
//! land in a bounded queue (one slot per worker): a worker whose result is
//! not joined yet waits instead of staging more work, which keeps the bytes
//! in flight bounded by the fan-out. Joining is a waker-driven future, so
//! the caller's executor stays free while the pool runs.

use std::any::Any;
use std::collections::VecDeque;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::task::{Context, Poll, Waker};
use std::thread;

use sekai_util::{JobSet, ThreadRunner, WorkerPanic};

/// Recover a poisoned lock instead of failing: the queue and iterator
/// stay structurally valid even when a worker panicked holding them.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The panic message, as far as the payload carried one.
fn panic_message(payload: &(dyn Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|message| (*message).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown panic".to_owned())
}

/// A [`ThreadRunner`] over OS threads, one worker per parallelism slot.
#[derive(Debug, Clone, Copy)]
pub struct OsThread {
    parallelism: usize,
}

impl ThreadRunner for OsThread {
    fn new(parallelism: usize) -> Self {
        Self {
            parallelism: match parallelism {
                0 => thread::available_parallelism().map_or(1, usize::from),
                n => n,
            },
        }
    }

    fn parallelism(&self) -> usize {
        self.parallelism
    }

    // A concrete return type keeps the jobs nameable for callers.
    #[allow(refining_impl_trait)]
    fn run<I, F, T, R>(&self, items: I, f: F) -> OsJobs<R>
    where
        I: IntoIterator<Item = T>,
        I::IntoIter: Send + 'static,
        T: Send + 'static,
        R: Send + 'static,
        F: Fn(T) -> R + Send + Sync + 'static,
    {
        let shared = Arc::new(Shared {
            queue: Mutex::new(Queue {
                ready: VecDeque::new(),
                waker: None,
                active: self.parallelism,
                stop: false,
            }),
            space: Condvar::new(),
            capacity: self.parallelism,
        });
        let items = Arc::new(Mutex::new(items.into_iter()));
        let f = Arc::new(f);
        for _ in 0..self.parallelism {
            let worker = {
                let shared = Arc::clone(&shared);
                let items = Arc::clone(&items);
                let f = Arc::clone(&f);
                move || worker(items, f, shared)
            };
            if let Err(source) = thread::Builder::new()
                .name("sekai-worker".to_owned())
                .spawn(worker)
            {
                // A slot that never spawned must not keep the set alive,
                // and the failure must surface instead of silently
                // dropping the items it would have run.
                let mut queue = lock(&shared.queue);
                queue.active -= 1;
                queue.ready.push_back(Err(WorkerPanic::new(format!(
                    "failed to spawn worker thread: {source}"
                ))));
                if let Some(waker) = queue.waker.take() {
                    waker.wake();
                }
                drop(queue);
                break;
            }
        }
        OsJobs { shared }
    }

    // A concrete return type keeps the jobs nameable for callers.
    #[allow(refining_impl_trait)]
    fn call<F, R>(&self, f: F) -> OsCall<R>
    where
        F: FnOnce() -> R + Send + 'static,
        R: Send + 'static,
    {
        let state = Arc::new(CallState {
            slot: Mutex::new(CallSlot {
                result: None,
                waker: None,
            }),
        });
        let worker = {
            let state = Arc::clone(&state);
            move || {
                let result = catch_unwind(AssertUnwindSafe(f))
                    .map_err(|payload| WorkerPanic::new(panic_message(&*payload)));
                let mut slot = lock(&state.slot);
                slot.result = Some(result);
                if let Some(waker) = slot.waker.take() {
                    waker.wake();
                }
            }
        };
        if let Err(source) = thread::Builder::new()
            .name("sekai-worker".to_owned())
            .spawn(worker)
        {
            lock(&state.slot).result = Some(Err(WorkerPanic::new(format!(
                "failed to spawn worker thread: {source}"
            ))));
        }
        OsCall { state }
    }
}

/// One worker: pull items until the iterator ends or the set is dropped,
/// then retire. A panic in `f` becomes that item's `Err`, never a lost
/// slot: the worker keeps going.
// The thread body needs owned `'static` handles; a borrow cannot cross
// the spawn boundary.
#[allow(clippy::needless_pass_by_value)]
fn worker<I, F, T, R>(items: Arc<Mutex<I>>, f: Arc<F>, shared: Arc<Shared<R>>)
where
    I: Iterator<Item = T>,
    F: Fn(T) -> R,
{
    loop {
        if lock(&shared.queue).stop {
            break;
        }
        let item = lock(&items).next();
        let Some(item) = item else { break };
        let result = catch_unwind(AssertUnwindSafe(|| f(item)))
            .map_err(|payload| WorkerPanic::new(panic_message(&*payload)));

        // Backpressure: wait for a free completion slot so the bytes in
        // flight stay bounded by the fan-out.
        let mut queue = lock(&shared.queue);
        while !queue.stop && queue.ready.len() >= shared.capacity {
            queue = shared
                .space
                .wait(queue)
                .unwrap_or_else(PoisonError::into_inner);
        }
        if queue.stop {
            break;
        }
        queue.ready.push_back(result);
        if let Some(waker) = queue.waker.take() {
            waker.wake();
        }
        drop(queue);
    }
    // Release the closure's captures (world-lock shares) and the iterator
    // before retiring: the joiner treats `active == 0` as "every resource
    // the run held is free", so nothing may outlive that signal.
    drop(items);
    drop(f);
    let mut queue = lock(&shared.queue);
    queue.active -= 1;
    if queue.active == 0
        && let Some(waker) = queue.waker.take()
    {
        waker.wake();
    }
}

/// State shared by the pool and its joiner.
struct Shared<R> {
    queue: Mutex<Queue<R>>,
    /// Signals workers waiting for a free completion slot.
    space: Condvar,
    capacity: usize,
}

struct Queue<R> {
    ready: VecDeque<Result<R, WorkerPanic>>,
    waker: Option<Waker>,
    /// Workers still alive; the set is exhausted at zero.
    active: usize,
    stop: bool,
}

impl<R> Shared<R> {
    /// Take one ready completion, freeing a slot for a waiting worker.
    fn pop(&self) -> Option<Result<R, WorkerPanic>> {
        let item = lock(&self.queue).ready.pop_front();
        if item.is_some() {
            self.space.notify_all();
        }
        item
    }
}

/// The completions of one [`OsThread::run`].
///
/// Dropping the set stops the queued items; the items already running
/// finish on their own threads, releasing whatever they hold (world-lock
/// shares, open files) when they end. Joining to exhaustion waits for that
/// release; dropping does not, so a cancelled run never blocks its caller.
pub struct OsJobs<R> {
    shared: Arc<Shared<R>>,
}

impl<R> JobSet<R> for OsJobs<R> {
    // A concrete return type keeps the jobs nameable for callers.
    #[allow(refining_impl_trait)]
    fn join_next(&mut self) -> OsJoinNext<'_, R> {
        OsJoinNext {
            shared: &self.shared,
        }
    }

    fn try_join_next(&mut self) -> Option<Result<R, WorkerPanic>> {
        self.shared.pop()
    }

    fn cancel(&mut self) {
        lock(&self.shared.queue).stop = true;
        self.shared.space.notify_all();
    }
}

impl<R> Drop for OsJobs<R> {
    fn drop(&mut self) {
        self.cancel();
    }
}

/// One async join; completes when a worker pushes a result or the pool
/// retires.
pub struct OsJoinNext<'a, R> {
    shared: &'a Shared<R>,
}

impl<R> core::future::Future for OsJoinNext<'_, R> {
    type Output = Option<Result<R, WorkerPanic>>;

    fn poll(self: core::pin::Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        let mut queue = lock(&this.shared.queue);
        if let Some(item) = queue.ready.pop_front() {
            this.shared.space.notify_all();
            return Poll::Ready(Some(item));
        }
        if queue.active == 0 {
            return Poll::Ready(None);
        }
        queue.waker = Some(cx.waker().clone());
        Poll::Pending
    }
}

/// State shared by a [`OsThread::call`] task and its joiner.
struct CallState<R> {
    slot: Mutex<CallSlot<R>>,
}

struct CallSlot<R> {
    result: Option<Result<R, WorkerPanic>>,
    waker: Option<Waker>,
}

/// The completion of one [`OsThread::call`].
pub struct OsCall<R> {
    state: Arc<CallState<R>>,
}

impl<R> core::future::Future for OsCall<R> {
    type Output = Result<R, WorkerPanic>;

    fn poll(self: core::pin::Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut slot = lock(&self.state.slot);
        if let Some(result) = slot.result.take() {
            return Poll::Ready(result);
        }
        slot.waker = Some(cx.waker().clone());
        Poll::Pending
    }
}

#[cfg(test)]
mod tests {
    use super::OsThread;
    use sekai_util::{JobSet, ThreadRunner};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    /// A minimal park-based executor: enough to await the runner's
    /// waker-driven futures without an async runtime.
    fn block_on<F: core::future::Future>(fut: F) -> F::Output {
        use std::task::{Context, Poll, Wake};
        struct ParkWake(std::thread::Thread);
        impl Wake for ParkWake {
            fn wake(self: Arc<Self>) {
                self.0.unpark();
            }
            fn wake_by_ref(self: &Arc<Self>) {
                self.0.unpark();
            }
        }
        let waker = Arc::new(ParkWake(std::thread::current())).into();
        let mut cx = Context::from_waker(&waker);
        let mut fut = core::pin::pin!(fut);
        loop {
            match fut.as_mut().poll(&mut cx) {
                Poll::Ready(out) => return out,
                Poll::Pending => std::thread::park(),
            }
        }
    }

    #[test]
    fn zero_resolves_to_at_least_one_worker() {
        assert!(OsThread::new(0).parallelism() >= 1);
        assert_eq!(OsThread::new(3).parallelism(), 3);
    }

    #[test]
    fn every_item_runs_exactly_once() {
        let runner = OsThread::new(4);
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&seen);
        let mut jobs = runner.run(0..100, move |x| {
            sink.lock().unwrap().push(x);
            x * 2
        });
        let mut out = Vec::new();
        while let Some(item) = block_on(jobs.join_next()) {
            out.push(item.unwrap());
        }
        out.sort_unstable();
        assert_eq!(out, (0..100).map(|x| x * 2).collect::<Vec<_>>());
        assert_eq!(seen.lock().unwrap().len(), 100);
    }

    #[test]
    fn a_panic_surfaces_as_that_items_error() {
        let runner = OsThread::new(2);
        let mut jobs = runner.run(0..4, |x| {
            assert_ne!(x, 2, "boom");
            x
        });
        let mut results = Vec::new();
        while let Some(item) = block_on(jobs.join_next()) {
            results.push(item);
        }
        assert_eq!(results.len(), 4);
        let errors: Vec<_> = results.iter().filter(|r| r.is_err()).collect();
        assert_eq!(errors.len(), 1);
        assert!(errors[0].as_ref().unwrap_err().message().contains("boom"));
    }

    #[test]
    fn dropping_the_set_stops_queued_items() {
        let runner = OsThread::new(1);
        let started = Arc::new(AtomicUsize::new(0));
        let count = Arc::clone(&started);
        let jobs = runner.run(0..1000, move |_| {
            count.fetch_add(1, Ordering::SeqCst);
            std::thread::sleep(std::time::Duration::from_millis(1));
        });
        // Let the single worker start, then abandon the run.
        while started.load(Ordering::SeqCst) == 0 {
            std::thread::yield_now();
        }
        drop(jobs);
        std::thread::sleep(std::time::Duration::from_millis(50));
        let after_drop = started.load(Ordering::SeqCst);
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert_eq!(started.load(Ordering::SeqCst), after_drop);
        assert!(after_drop < 1000);
    }

    #[test]
    fn call_runs_one_blocking_task() {
        let runner = OsThread::new(2);
        let tid = std::thread::current().id();
        let ran_on = block_on(runner.call(std::thread::current)).unwrap();
        assert_ne!(ran_on.id(), tid);
        let panic = block_on(runner.call(|| panic!("call boom"))).unwrap_err();
        assert!(panic.message().contains("call boom"));
    }

    #[test]
    fn backpressure_bounds_the_results_in_flight() {
        let runner = OsThread::new(2);
        let produced = Arc::new(AtomicUsize::new(0));
        let count = Arc::clone(&produced);
        let mut jobs = runner.run(0..64, move |x| {
            count.fetch_add(1, Ordering::SeqCst);
            x
        });
        // Without joining, the workers can only stage a bounded number
        // of results (queue slots plus the items in their hands).
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!(produced.load(Ordering::SeqCst) <= 4);
        while let Some(item) = block_on(jobs.join_next()) {
            item.unwrap();
        }
        assert_eq!(produced.load(Ordering::SeqCst), 64);
    }
}
