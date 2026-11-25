//! The CLI's host [`Clock`]: OS monotonic timers and the Unix wall
//! clock.
//!
//! The orchestration library (`sekai-app`) names no clock; the binary
//! composes this one into the instance so `--timing` reports real phases
//! and commits stamp real creation times.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use sekai_util::{Clock, Timer};

/// A [`Clock`] over the OS clock: monotonic [`Instant`]s for elapsed
/// phases, [`SystemTime`] for Unix-millisecond stamps.
#[derive(Debug, Clone, Copy, Default)]
pub struct OsTime;

impl Clock for OsTime {
    // A concrete return type keeps the timer nameable for callers.
    #[allow(refining_impl_trait)]
    fn timer(&self) -> OsTimer {
        OsTimer(Instant::now())
    }

    fn now_ms(&self) -> Option<u64> {
        let elapsed = SystemTime::now().duration_since(UNIX_EPOCH).ok()?;
        u64::try_from(elapsed.as_millis()).ok()
    }
}

/// One elapsed-time measurement over a monotonic [`Instant`].
pub struct OsTimer(Instant);

impl Timer for OsTimer {
    fn stop(self) -> Duration {
        self.0.elapsed()
    }
}

#[cfg(test)]
mod tests {
    use super::OsTime;
    use sekai_util::{Clock, Timer};

    #[test]
    fn a_timer_measures_elapsed_time() {
        let timer = OsTime.timer();
        std::thread::sleep(std::time::Duration::from_millis(5));
        assert!(timer.stop() >= std::time::Duration::from_millis(5));
    }

    #[test]
    fn the_wall_clock_reads_a_plausible_unix_time() {
        // After 2020-01-01 and before 2100-01-01.
        let now = OsTime.now_ms().unwrap();
        assert!((1_577_836_800_000..4_102_444_800_000).contains(&now));
    }
}
