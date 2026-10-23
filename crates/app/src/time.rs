//! Wall-clock helpers.

use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::AppError;

/// The current time in Unix milliseconds.
///
/// A clock before the epoch returns [`AppError::Clock`]; a reading beyond
/// `u64` milliseconds returns [`AppError::ClockOutOfRange`]. Neither is a
/// sentinel timestamp: the port requires `created_at_ms` to be storable,
/// and substituting a value would hide the anomaly.
///
/// The app's `ClockOutOfRange` covers only the `u64` conversion. A
/// backend whose stored type is narrower (SQLite's signed `i64`) reports
/// its own error (`StorageError::InvalidTimestamp`) after the value
/// reaches it.
pub(crate) fn now_ms() -> Result<u64, AppError> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(AppError::Clock)?;
    u64::try_from(elapsed.as_millis()).map_err(|_| AppError::ClockOutOfRange)
}
