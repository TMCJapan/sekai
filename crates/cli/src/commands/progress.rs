//! Opt-in stderr progress bars (`--progress`), shared by report commands.

use indicatif::{ProgressBar, ProgressStyle};
use sekai_app::{
    CheckoutProgress, CheckoutReporter, CloneProgress, CloneReporter, CommitProgress,
    CommitReporter, DiffProgress, DiffReporter, GcProgress, GcReporter, ProgressBuilder,
    PruneProgress, PruneReporter,
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

    const fn reporter(&self) -> BarReporter<'_> {
        BarReporter(self.0.as_ref())
    }
}

impl ProgressBuilder for BarProgress {
    fn commit(&self) -> impl CommitReporter {
        self.reporter()
    }

    fn status(&self) -> impl CommitReporter {
        self.reporter()
    }

    fn checkout(&self) -> impl CheckoutReporter {
        self.reporter()
    }

    fn clone_to(&self) -> impl CloneReporter {
        self.reporter()
    }

    fn diff(&self) -> impl DiffReporter {
        self.reporter()
    }

    fn gc(&self) -> impl GcReporter {
        self.reporter()
    }

    fn prune(&self) -> impl PruneReporter {
        self.reporter()
    }
}

struct BarReporter<'a>(Option<&'a ProgressBar>);

impl BarReporter<'_> {
    /// Advance the bar, adopting the total on first sight (totals arrive
    /// with the first event). `detail` trails the counters, e.g. `"files 12
    /// chunks"`.
    fn advance(&self, done: usize, total: usize, detail: impl Into<String>) {
        if let Some(bar) = self.0 {
            if bar.length() != Some(total as u64) {
                bar.set_length(total as u64);
            }
            bar.set_position(done as u64);
            bar.set_message(detail.into());
        }
    }

    fn advance_files(&self, files_done: usize, files_total: usize, chunks_done: usize) {
        self.advance(
            files_done,
            files_total,
            format!("files {chunks_done} chunks"),
        );
    }
}

impl CommitReporter for BarReporter<'_> {
    fn step(&mut self, progress: CommitProgress) {
        self.advance_files(
            progress.files_done,
            progress.files_total,
            progress.chunks_done,
        );
    }
}

impl CheckoutReporter for BarReporter<'_> {
    fn step(&mut self, progress: CheckoutProgress) {
        self.advance_files(
            progress.files_done,
            progress.files_total,
            progress.chunks_done,
        );
    }
}

impl CloneReporter for BarReporter<'_> {
    fn step(&mut self, progress: CloneProgress) {
        self.advance_files(
            progress.files_done,
            progress.files_total,
            progress.chunks_done,
        );
    }
}

impl DiffReporter for BarReporter<'_> {
    fn step(&mut self, progress: DiffProgress) {
        self.advance(progress.chunks_done, progress.chunks_total, "chunks");
    }
}

impl GcReporter for BarReporter<'_> {
    fn step(&mut self, progress: GcProgress) {
        self.advance(progress.blobs_done, progress.blobs_total, "blobs");
    }
}

impl PruneReporter for BarReporter<'_> {
    fn step(&mut self, progress: PruneProgress) {
        self.advance(progress.commits_done, progress.commits_total, "commits");
    }
}
