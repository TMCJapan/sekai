//! Opt-in stderr progress bars (`--progress`), shared by report commands.

use std::{fmt, time::Duration};

use indicatif::{ProgressBar, ProgressStyle};
use sekai_app::{BuildProgress, Progress};

const TEMPLATE: &str =
    "{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {pos}/{len} {msg} ({eta})";
const STEADY_TICK: Duration = Duration::from_millis(100);

/// What a bar counts, shown after its counters.
#[derive(Debug, Clone, Copy)]
pub enum Unit {
    Files,
    Chunks,
}

impl fmt::Display for Unit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Files => "files",
            Self::Chunks => "chunks",
        })
    }
}

/// Progress bar for `--progress`, drawn on stderr only when it is a TTY,
/// so piped output stays clean. The TTY check is indicatif's: its default
/// stderr target hides itself off a terminal or under `TERM=dumb`.
///
/// A disabled bar is indicatif's hidden bar rather than an absent one, so
/// every update stays unconditional and simply draws nothing.
///
/// Dropping it clears the bar (indicatif's default `ProgressFinish::AndClear`),
/// so a command that returns early on an error needs no `finish` call.
pub struct BarProgress(ProgressBar);

impl BarProgress {
    /// Create a bar counting `unit`, hidden unless `enabled`.
    pub fn new(enabled: bool, unit: Unit) -> Self {
        if !enabled {
            return Self(ProgressBar::hidden());
        }
        // No length until `build_progress`: a zero length would draw a full bar
        // while the operation is still scanning for its items.
        let bar = ProgressBar::no_length()
            .with_style(style())
            .with_message(unit.to_string());
        // Without a ticker the spinner, elapsed time and ETA redraw only on
        // `step`, so they freeze while a slow item is in flight.
        bar.enable_steady_tick(STEADY_TICK);
        Self(bar)
    }

    /// Clear the bar; the human report line that follows carries the summary.
    pub fn finish(&self) {
        self.0.finish_and_clear();
    }
}

impl BuildProgress for BarProgress {
    fn build_progress(&self, total: usize) -> impl Progress {
        self.0.set_length(total as u64);
        self
    }
}

impl Progress for &BarProgress {
    fn step(&mut self) {
        self.0.inc(1);
    }
}

/// Only a typo in [`TEMPLATE`] fails to parse, which `template_parses`
/// catches; the fallback keeps the bar drawing regardless.
fn style() -> ProgressStyle {
    ProgressStyle::with_template(TEMPLATE).map_or_else(
        |_| ProgressStyle::default_bar(),
        |style| style.progress_chars("#>-"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_parses() {
        assert!(ProgressStyle::with_template(TEMPLATE).is_ok());
    }
}
