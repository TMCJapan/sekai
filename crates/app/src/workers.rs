//! Worker-count resolution shared by the per-file pipelines.

use std::num::NonZeroUsize;

/// Resolve a requested worker count: `0` means one per CPU (or 1 when the
/// CPU count is unknown), anything else is taken as is.
pub(crate) fn worker_slots(concurrency: usize) -> usize {
    match concurrency {
        0 => std::thread::available_parallelism().map_or(1, NonZeroUsize::get),
        n => n,
    }
}

#[cfg(test)]
mod tests {
    use super::worker_slots;

    #[test]
    fn zero_resolves_to_at_least_one_worker() {
        assert!(worker_slots(0) >= 1);
    }

    #[test]
    fn explicit_count_is_kept() {
        assert_eq!(worker_slots(1), 1);
        assert_eq!(worker_slots(7), 7);
    }
}
