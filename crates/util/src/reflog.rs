//! Reflogs: the history of where a branch or a worktree's HEAD pointed
//! (ADR-0035).

use alloc::string::String;

use crate::commit::CommitId;

/// The message and time a ref update records in its reflog.
///
/// Callers build the message in Git's forms (`commit: `, `checkout:
/// moving from <a> to <b>`, ...); the store records it verbatim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReflogNote<'a> {
    /// Reflog message.
    pub message: &'a str,
    /// Time of the update as Unix millis.
    pub at_ms: u64,
}

impl<'a> ReflogNote<'a> {
    /// Construct a note.
    pub const fn new(message: &'a str, at_ms: u64) -> Self {
        Self { message, at_ms }
    }
}

/// One recorded reflog entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReflogEntry {
    /// Commit before the update, or `None` when the update created the
    /// ref or gave an unborn HEAD its first commit.
    pub old: Option<CommitId>,
    /// Commit after the update.
    pub new: CommitId,
    /// Reflog message.
    pub message: String,
    /// Time of the update as Unix millis.
    pub at_ms: u64,
}
