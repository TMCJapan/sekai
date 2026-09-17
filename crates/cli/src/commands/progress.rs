//! Opt-in stderr progress bars (`--progress`), shared by report commands.

use indicatif::{ProgressBar, ProgressStyle};

/// Progress bar for `--progress`, drawn on stderr. `None` unless the flag
/// is set and stderr is a TTY, so piped output stays clean; `--json`
/// refuses the flag at parse time instead.
pub fn progress_bar(enabled: bool) -> Option<ProgressBar> {
    if !enabled || !std::io::IsTerminal::is_terminal(&std::io::stderr()) {
        return None;
    }
    let bar = ProgressBar::new(0);
    bar.set_style(
        ProgressStyle::with_template("{bar:40} {pos}/{len} {msg}")
            .unwrap_or_else(|_| ProgressStyle::default_bar()),
    );
    Some(bar)
}

/// Advance `bar`, adopting the total on first sight (totals arrive with
/// the first event). `detail` trails the counters, e.g. `"files 12
/// chunks"`.
pub fn report_progress(
    bar: Option<&ProgressBar>,
    done: usize,
    total: usize,
    detail: impl Into<String>,
) {
    if let Some(bar) = bar {
        if bar.length() != Some(total as u64) {
            bar.set_length(total as u64);
        }
        bar.set_position(done as u64);
        bar.set_message(detail.into());
    }
}

/// Clear the bar; the human report line that follows carries the summary.
pub fn finish_progress(bar: Option<&ProgressBar>) {
    if let Some(bar) = bar {
        bar.finish_and_clear();
    }
}
