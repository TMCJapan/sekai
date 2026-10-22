//! Wall-clock helpers.

use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::AppError;

/// The current time in Unix milliseconds, saturating a clock far enough
/// in the future to overflow `u64`.
pub(crate) fn now_ms() -> Result<u64, AppError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(AppError::Clock)
        .map(|elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
}
