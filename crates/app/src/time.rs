//! Wall-clock helpers.

use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::AppError;

/// The current time in Unix milliseconds, saturating to `u64::MAX` when
/// the clock is beyond the representable range.
///
/// The SQLite backend keeps its own clock helper: its failures are
/// `StorageError`s, and it rejects out-of-range times (`ClockOutOfRange`,
/// `InvalidTimestamp`) instead of saturating. Here the value is a `u64`
/// passed down to the port, so saturation keeps the app error-free; a
/// value the backend cannot store fails there as `InvalidTimestamp`.
pub(crate) fn now_ms() -> Result<u64, AppError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(AppError::Clock)
        .map(|elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
}
