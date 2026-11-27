//! A host OS-thread [`ThreadRunner`] for tests that need real parallelism.
//!
//! The `sekai-app` library ships no host runner (the `sekai` binary owns
//! that), so the lock suite composes this test mirror of the binary's pool
//! to exercise the cross-thread cancellation semantics: a write must keep
//! the world locked on its own thread after the checkout future is dropped.
//! Most tests run fine on the inline `Sequential` default; only the ones
//! that observe another thread need this.

use std::any::Any;
use std::collections::VecDeque;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::task::{Context, Poll, Waker};
use std::thread;

use sekai_platform::thread::{JobSet, ThreadRunner, WorkerPanic};

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

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
                .name("sekai-test-worker".to_owned())
                .spawn(worker)
            {
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
            .name("sekai-test-worker".to_owned())
            .spawn(worker)
        {
            lock(&state.slot).result = Some(Err(WorkerPanic::new(format!(
                "failed to spawn worker thread: {source}"
            ))));
        }
        OsCall { state }
    }
}

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
    // Release the closure's captures (world-lock shares) before retiring:
    // the joiner treats `active == 0` as "every resource the run held is
    // free", so nothing may outlive that signal.
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

struct Shared<R> {
    queue: Mutex<Queue<R>>,
    space: Condvar,
    capacity: usize,
}

struct Queue<R> {
    ready: VecDeque<Result<R, WorkerPanic>>,
    waker: Option<Waker>,
    active: usize,
    stop: bool,
}

impl<R> Shared<R> {
    fn pop(&self) -> Option<Result<R, WorkerPanic>> {
        let item = lock(&self.queue).ready.pop_front();
        if item.is_some() {
            self.space.notify_all();
        }
        item
    }
}

/// The completions of one [`OsThread::run`]; dropping stops queued items.
pub struct OsJobs<R> {
    shared: Arc<Shared<R>>,
}

impl<R> JobSet<R> for OsJobs<R> {
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
