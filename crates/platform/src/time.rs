//! The time boundary: elapsed-time measurement and wall-clock readings.
//!
//! Orchestration measures its phases and stamps commits through a
//! composed [`Clock`], so the flows stay `no_std`: a host embedding
//! composes its own (the `sekai` binary's OS clock), and a `no_std`/wasm
//! embedding uses [`Zero`], the timing-free baseline.

use core::time::Duration;

/// Starts elapsed-time measurements and reads the wall clock.
///
/// Composed once into an instance and reused by every operation. A clock
/// is cloned into runner jobs so each worker can time its own file pass, so
/// it must be cheap to clone and safe to share across threads.
pub trait Clock: Clone + Send + Sync + 'static {
    /// Begin an elapsed-time measurement; [`Timer::stop`] reads it.
    fn timer(&self) -> impl Timer;

    /// The current time in Unix milliseconds, or `None` when no clock is
    /// available, the reading is before the epoch, or it does not fit a
    /// `u64` of milliseconds.
    fn now_ms(&self) -> Option<u64>;
}

/// One elapsed-time measurement.
pub trait Timer {
    /// The time elapsed since the clock started this timer.
    fn stop(self) -> Duration;
}

/// A [`Clock`] that measures nothing and reads no time.
///
/// The `no_std` baseline: every timer reports [`Duration::ZERO`] and
/// `now_ms` is `None`, so timing output is empty and the operations that
/// stamp a time (commit, scope creation) fail until a host composes a real
/// clock. Read and checkout flows run unchanged.
#[derive(Debug, Clone, Copy, Default)]
pub struct Zero;

impl Clock for Zero {
    fn timer(&self) -> impl Timer {
        Self
    }

    fn now_ms(&self) -> Option<u64> {
        None
    }
}

impl Timer for Zero {
    fn stop(self) -> Duration {
        Duration::ZERO
    }
}

#[cfg(test)]
mod tests {
    use super::{Clock, Timer, Zero};
    use core::time::Duration;

    #[test]
    fn zero_measures_nothing_and_reads_no_clock() {
        assert_eq!(Zero.timer().stop(), Duration::ZERO);
        assert_eq!(Zero.now_ms(), None);
    }
}
