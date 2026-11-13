//! Opt-in stderr progress bars (`--progress`), shared by report commands.

use indicatif::{ProgressBar, ProgressStyle};
use sekai_app::{ProgressBuilder, Reporter};

/// What a bar counts, shown after its counters.
#[derive(Debug, Clone, Copy)]
pub enum Unit {
    Files,
    Chunks,
}

impl Unit {
    const fn label(self) -> &'static str {
        match self {
            Self::Files => "files",
            Self::Chunks => "chunks",
        }
    }
}

/// Progress bar for `--progress`, drawn on stderr. Hidden unless the flag
/// is set and stderr is a TTY, so piped output stays clean; `--json`
/// refuses the flag at parse time instead.
///
/// A disabled bar is indicatif's hidden bar rather than an absent one, so
/// every update stays unconditional and simply draws nothing.
///
/// Dropping it clears the bar as well (indicatif finishes an unfinished
/// bar with `ProgressFinish::AndClear` by default), so a command that
/// returns early on an error needs no `finish` call.
pub struct BarProgress(ProgressBar);

impl BarProgress {
    /// Create a bar counting `unit`, or a hidden one unless `enabled` is
    /// set. Its stderr draw target already hides itself when stderr is not a
    /// TTY.
    pub fn new(enabled: bool, unit: Unit) -> Self {
        if !enabled {
            return Self(ProgressBar::hidden());
        }
        let bar = ProgressBar::new(0);
        bar.set_style(
            ProgressStyle::with_template("{bar:40} {pos}/{len} {msg}")
                .unwrap_or_else(|_| ProgressStyle::default_bar()),
        );
        bar.set_message(unit.label());
        Self(bar)
    }

    /// Clear the bar; the human report line that follows carries the summary.
    pub fn finish(&self) {
        self.0.finish_and_clear();
    }
}

impl ProgressBuilder for BarProgress {
    fn start(&self, total: usize) -> impl Reporter {
        self.0.set_length(total as u64);
        BarReporter(&self.0)
    }
}

struct BarReporter<'a>(&'a ProgressBar);

impl Reporter for BarReporter<'_> {
    fn step(&mut self) {
        self.0.inc(1);
    }
}
