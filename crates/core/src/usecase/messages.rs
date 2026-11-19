//! Reflog message formats shared by the writers and the readers
//! (ADR-0035 *Reflogs*). The messages are recorded data, not
//! presentation: the store keeps them and other reads parse them back.

use alloc::format;
use alloc::string::String;

/// The prefix Git logs for a commit.
pub(crate) const COMMIT: &str = "commit: ";
/// The prefix Git logs for the first commit on an unborn branch.
pub(crate) const COMMIT_INITIAL: &str = "commit (initial): ";
/// The prefix Git logs for a HEAD that moved.
pub(crate) const CHECKOUT_MOVING: &str = "checkout: moving from ";
/// The separator between the old and the new label in a checkout message.
pub(crate) const CHECKOUT_TO: &str = " to ";

/// Build the `checkout: moving from <from> to <to>` message.
pub(crate) fn checkout_message(from: &str, to: &str) -> String {
    format!("{CHECKOUT_MOVING}{from}{CHECKOUT_TO}{to}")
}
