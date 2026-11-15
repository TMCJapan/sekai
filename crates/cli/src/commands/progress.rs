//! Opt-in stderr progress bars (`--progress`), shared by report commands.

use std::time::Duration;

use indicatif::{ProgressBar, ProgressStyle};
use sekai_app::{BuildProgress, Progress};

const TEMPLATE: &str =
    "{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {pos}/{len} {msg} ({eta})";
const STEADY_TICK: Duration = Duration::from_millis(100);

/// Builds one progress bar per run for `--progress`, drawn on stderr only
/// when it is a TTY, so piped output stays clean. The TTY check is
/// indicatif's: its default stderr target hides itself off a terminal or
/// under `TERM=dumb`.
///
/// Every run counts region files. Nothing is drawn until the operation
/// knows its total, so the scan before it shows no bar.
pub struct Bars {
    enabled: bool,
}

impl Bars {
    /// Bars counting files, hidden unless `enabled`.
    pub const fn new(enabled: bool) -> Self {
        Self { enabled }
    }
}

impl BuildProgress for Bars {
    fn build_progress(&self, total: usize) -> impl Progress {
        // A disabled run gets indicatif's hidden bar rather than none, so
        // every update stays unconditional and simply draws nothing.
        if !self.enabled {
            return BarProgress(ProgressBar::hidden());
        }
        let bar = ProgressBar::new(total as u64)
            .with_style(style())
            .with_message("files");
        // Without a ticker the spinner, elapsed time and ETA redraw only on
        // `step`, so they freeze while a slow item is in flight.
        bar.enable_steady_tick(STEADY_TICK);
        BarProgress(bar)
    }
}

/// One run's bar. Dropping it clears the bar (indicatif's default
/// `ProgressFinish::AndClear`), so the run's end, success or error, leaves
/// no bar behind.
struct BarProgress(ProgressBar);

impl Progress for BarProgress {
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
