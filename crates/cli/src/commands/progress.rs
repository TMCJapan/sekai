//! Opt-in stderr progress bars (`--progress`), shared by report commands.

use indicatif::{ProgressBar, ProgressStyle};
use sekai_app::{
    CheckoutReporter, CloneReporter, CommitReporter, DiffReporter, GcReporter, ProgressBuilder,
    PruneReporter,
};

/// Progress bar for `--progress`, drawn on stderr. Hidden unless the flag
/// is set and stderr is a TTY, so piped output stays clean; `--json`
/// refuses the flag at parse time instead.
pub struct BarProgress(Option<ProgressBar>);

impl BarProgress {
    pub fn new(enabled: bool) -> Self {
        if !enabled || !std::io::IsTerminal::is_terminal(&std::io::stderr()) {
            return Self(None);
        }
        let bar = ProgressBar::new(0);
        bar.set_style(
            ProgressStyle::with_template("{bar:40} {pos}/{len} {msg}")
                .unwrap_or_else(|_| ProgressStyle::default_bar()),
        );
        Self(Some(bar))
    }

    /// Clear the bar; the human report line that follows carries the summary.
    pub fn finish(&self) {
        if let Some(bar) = &self.0 {
            bar.finish_and_clear();
        }
    }

    /// Size the bar for a run of `total` items and hand out its reporter.
    fn reporter(&self, total: usize) -> BarReporter<'_> {
        if let Some(bar) = &self.0
            && bar.length() != Some(total as u64)
        {
            bar.set_length(total as u64);
        }
        BarReporter(self.0.as_ref())
    }
}

impl ProgressBuilder for BarProgress {
    fn commit(&self, files_total: usize) -> impl CommitReporter {
        self.reporter(files_total)
    }

    fn status(&self, files_total: usize) -> impl CommitReporter {
        self.reporter(files_total)
    }

    fn checkout(&self, files_total: usize) -> impl CheckoutReporter {
        self.reporter(files_total)
    }

    fn clone_to(&self, files_total: usize) -> impl CloneReporter {
        self.reporter(files_total)
    }

    fn diff(&self, chunks_total: usize) -> impl DiffReporter {
        self.reporter(chunks_total)
    }

    fn gc(&self, blobs_total: usize) -> impl GcReporter {
        self.reporter(blobs_total)
    }

    fn prune(&self, commits_total: usize) -> impl PruneReporter {
        self.reporter(commits_total)
    }
}

struct BarReporter<'a>(Option<&'a ProgressBar>);

impl BarReporter<'_> {
    /// Move the bar to `done`. `detail` trails the counters, e.g. `"files
    /// 12 chunks"`.
    fn advance(&self, done: usize, detail: impl Into<String>) {
        if let Some(bar) = self.0 {
            bar.set_position(done as u64);
            bar.set_message(detail.into());
        }
    }

    fn advance_files(&self, files_done: usize, chunks_done: usize) {
        self.advance(files_done, format!("files {chunks_done} chunks"));
    }
}

impl CommitReporter for BarReporter<'_> {
    fn step(&mut self, files_done: usize, chunks_done: usize) {
        self.advance_files(files_done, chunks_done);
    }
}

impl CheckoutReporter for BarReporter<'_> {
    fn step(&mut self, files_done: usize, chunks_done: usize) {
        self.advance_files(files_done, chunks_done);
    }
}

impl CloneReporter for BarReporter<'_> {
    fn step(&mut self, files_done: usize, chunks_done: usize) {
        self.advance_files(files_done, chunks_done);
    }
}

impl DiffReporter for BarReporter<'_> {
    fn step(&mut self, chunks_done: usize) {
        self.advance(chunks_done, "chunks");
    }
}

impl GcReporter for BarReporter<'_> {
    fn step(&mut self, blobs_done: usize) {
        self.advance(blobs_done, "blobs");
    }
}

impl PruneReporter for BarReporter<'_> {
    fn step(&mut self, commits_done: usize) {
        self.advance(commits_done, "commits");
    }
}
