//! Worker-failure attribution shared by the per-file pipelines.

use sekai_storage::StorageError;
use sekai_util::WorkerPanic;

use crate::error::AppError;

/// Attribute a panicked worker to the pipeline that spawned it.
pub(crate) fn worker_error(context: &'static str) -> impl Fn(WorkerPanic) -> AppError {
    move |panic| AppError::Storage(StorageError::backend(context, panic))
}
