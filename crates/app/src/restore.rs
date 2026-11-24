//! Git-style restore: rebuild a selected portion of the live world from a
//! source commit, without recording history or moving refs.
//!
//! Rationale: `restore` is
//! [`checkout`](crate::WorldHandleMut::checkout) narrowed to
//! `git restore` semantics. Sekai has no index, so the default source
//! is the active ref's target (`HEAD`) rather than git's index, and the
//! caller's [`Scope`] plays the pathspec. Region files are still rebuilt
//! wholesale from CAS blobs: a restored file's content is replaced, not
//! merged, so checkout's keep-chunk policies have no git analog here and
//! stay off. Only the overlay choice (git's `--overlay`/`--no-overlay`)
//! and the rebuild mechanics remain configurable.

use sekai_core::{Revision, Scope};
use sekai_storage::SekaiStore;
use sekai_util::{Clock, ThreadRunner};
use sekai_world::WorldTree;

use crate::checkout::{
    CheckoutOptions, CheckoutReport, CheckoutTimings, MissingBlobPolicy, MissingFilePolicy,
};
use crate::error::AppError;
use crate::instance::SekaiInstance;
use crate::progress::BuildProgress;

/// Restore policy.
///
/// `Default` follows git restore's no-overlay mode: selected region files
/// the source does not know are deleted, chunks unknown to the source
/// vanish with the wholesale rebuild, missing blobs abort, and missing
/// files resolve sibling-first. The rebuild fan-out follows the
/// instance's runner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RestoreOptions {
    /// Overlay mode: keep region files unknown to the source instead of
    /// deleting them (git restore `--overlay`).
    pub overlay: bool,
    /// How to handle a source blob missing from CAS.
    pub on_missing_blob: MissingBlobPolicy,
    /// Where to rebuild a source-known region whose file is missing.
    pub on_missing_file: MissingFilePolicy,
}

impl<S: SekaiStore, T: ThreadRunner, B: Clock> SekaiInstance<S, T, B> {
    /// Restore the selected portion of `world` from a source commit,
    /// returning the checkout report and per-phase timings of the run.
    ///
    /// `source` defaults to the active ref's target when `None` - git
    /// restore restores from the index, and sekai has no index, so HEAD
    /// is the default source. An unborn active ref fails with
    /// [`AppError::NoCommits`]. Nothing is recorded and no ref moves.
    ///
    /// Only `scope` is rebuilt: region files outside it are never
    /// written, deleted, or otherwise touched. Inside it, files the
    /// source does not know are deleted unless [`RestoreOptions::overlay`]
    /// keeps them, and every rebuilt file holds exactly the source's
    /// chunks. The run plans before it touches any file and holds the
    /// world lock like checkout; see
    /// [`WorldHandleMut::checkout`](crate::WorldHandleMut::checkout) for
    /// the failure and cancellation guarantees, which carry over whole.
    pub async fn restore<W: WorldTree>(
        &mut self,
        world: &mut W,
        source: Option<&Revision>,
        scope: Scope,
        options: RestoreOptions,
        progress_builder: &impl BuildProgress,
    ) -> Result<(CheckoutReport, CheckoutTimings), AppError>
    where
        W::Error: Send + Sync + 'static,
    {
        let commit = match source {
            Some(revision) => self.resolve_revision(revision).await?,
            None => self.current_commit_id().await?.ok_or(AppError::NoCommits)?,
        };
        let checkout = CheckoutOptions {
            keep_post_commit_files: options.overlay,
            keep_post_commit_chunks: false,
            keep_tombstoned_chunks: false,
            on_missing_blob: options.on_missing_blob,
            on_missing_file: options.on_missing_file,
        };
        self.world_mut(world)
            .checkout(commit, checkout, scope, progress_builder)
            .await
    }
}
