//! Wall-clock reading through the instance's clock.

use sekai_util::Clock;

use crate::error::AppError;

/// The current time in Unix milliseconds.
///
/// No composed clock, a reading before the epoch, or one beyond `u64`
/// milliseconds yields `None`, surfaced as [`AppError::Clock`]. Neither is
/// a sentinel timestamp: the trait requires `created_at_ms` to be storable,
/// and substituting a value would hide the anomaly.
///
/// The error covers only the reading itself. A backend whose stored range
/// is narrower may reject the value with its own error after it reaches the
/// backend; callers see that as [`AppError::Storage`].
pub(crate) fn now_ms(clock: &impl Clock) -> Result<u64, AppError> {
    clock.now_ms().ok_or(AppError::Clock)
}
