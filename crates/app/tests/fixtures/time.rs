//! A host [`Clock`] for tests: the `sekai-app` library ships
//! no clock (the `sekai` binary owns that), so tests that commit or stamp
//! scopes compose this test mirror of the binary's OS clock. The `Zero`
//! default reads no time and would fail those operations.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use sekai_util::{Clock, Timer};

/// A [`Clock`] over the OS clock, mirroring the binary's `OsTime`.
#[derive(Debug, Clone, Copy, Default)]
pub struct HostTime;

impl Clock for HostTime {
    fn timer(&self) -> impl Timer {
        HostTimer(Instant::now())
    }

    fn now_ms(&self) -> Option<u64> {
        let elapsed = SystemTime::now().duration_since(UNIX_EPOCH).ok()?;
        u64::try_from(elapsed.as_millis()).ok()
    }
}

struct HostTimer(Instant);

impl Timer for HostTimer {
    fn stop(self) -> Duration {
        self.0.elapsed()
    }
}
