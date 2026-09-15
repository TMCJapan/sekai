//! Minimal terminal styling for human output.

/// When to emit ANSI escapes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
pub enum ColorChoice {
    /// Emit escapes only on a TTY without `NO_COLOR` set.
    #[default]
    Auto,
    /// Always emit escapes, even piped.
    Always,
    /// Never emit escapes.
    Never,
}

/// Decoupled styling switch.
///
/// Construct once per process (`Styler::new` reads the environment and
/// probes stdout); pass by reference into render functions. Tests use
/// [`Styler::enabled`] / [`Styler::disabled`] for deterministic output.
#[derive(Debug, Clone, Copy)]
pub struct Styler {
    enabled: bool,
}

impl Styler {
    /// Resolve styling from the CLI flag, `NO_COLOR`, and stdout TTY state.
    pub fn new(choice: ColorChoice) -> Self {
        Self::from_tty(choice, std::io::IsTerminal::is_terminal(&std::io::stdout()))
    }

    /// Resolve styling against stderr TTY state instead of stdout.
    ///
    /// Error output goes to stderr, which may be a TTY while stdout is
    /// piped (or vice versa); gate each stream independently.
    pub fn new_stderr(choice: ColorChoice) -> Self {
        Self::from_tty(choice, std::io::IsTerminal::is_terminal(&std::io::stderr()))
    }

    fn from_tty(choice: ColorChoice, is_tty: bool) -> Self {
        Self {
            enabled: resolve_color(choice, std::env::var_os("NO_COLOR").is_some(), is_tty),
        }
    }

    /// Styling unconditionally on (tests, `--color=always` plumbing).
    #[cfg(test)]
    pub const fn enabled() -> Self {
        Self { enabled: true }
    }

    /// Styling unconditionally off (tests, `--porcelain` plumbing).
    #[cfg(test)]
    pub const fn disabled() -> Self {
        Self { enabled: false }
    }

    /// Whether escapes are emitted.
    #[cfg(test)]
    pub const fn is_enabled(self) -> bool {
        self.enabled
    }

    /// Bold (snapshot IDs, headings).
    pub fn bold(self, text: &str) -> String {
        self.paint(text, "1")
    }

    /// Red (deletions, failures).
    pub fn red(self, text: &str) -> String {
        self.paint(text, "31")
    }

    /// Bold red (the `error:` prefix).
    pub fn red_bold(self, text: &str) -> String {
        self.paint(text, "1;31")
    }

    /// Green (additions, completed work).
    pub fn green(self, text: &str) -> String {
        self.paint(text, "32")
    }

    /// Yellow (modifications, pending work).
    pub fn yellow(self, text: &str) -> String {
        self.paint(text, "33")
    }

    /// Faint (secondary timing detail).
    pub fn dim(self, text: &str) -> String {
        self.paint(text, "2")
    }

    fn paint(self, text: &str, code: &str) -> String {
        if !self.enabled || text.is_empty() {
            return text.to_owned();
        }
        format!("\x1b[{code}m{text}\x1b[0m")
    }
}

/// Pure TTY/`NO_COLOR`/flag resolution. Kept free of I/O so tests never
/// touch the process environment.
pub const fn resolve_color(choice: ColorChoice, no_color: bool, is_tty: bool) -> bool {
    match choice {
        ColorChoice::Always => true,
        ColorChoice::Never => false,
        ColorChoice::Auto => is_tty && !no_color,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_color_choice() {
        assert!(resolve_color(ColorChoice::Always, true, false));
        assert!(!resolve_color(ColorChoice::Never, false, true));
        assert!(resolve_color(ColorChoice::Auto, false, true));
        assert!(!resolve_color(ColorChoice::Auto, true, true));
        assert!(!resolve_color(ColorChoice::Auto, false, false));
    }

    #[test]
    fn paints_exact_escapes_when_enabled() {
        let style = Styler::enabled();
        assert!(style.is_enabled());
        assert_eq!(style.red("+"), "\x1b[31m+\x1b[0m");
        assert_eq!(style.red_bold("error:"), "\x1b[1;31merror:\x1b[0m");
        assert_eq!(style.green("+"), "\x1b[32m+\x1b[0m");
        assert_eq!(style.yellow("~"), "\x1b[33m~\x1b[0m");
        assert_eq!(style.bold("3"), "\x1b[1m3\x1b[0m");
        assert_eq!(style.dim("x"), "\x1b[2mx\x1b[0m");
    }

    #[test]
    fn passes_text_through_when_disabled_or_empty() {
        let style = Styler::disabled();
        assert!(!style.is_enabled());
        assert_eq!(style.red("+"), "+");
        assert_eq!(style.bold("3"), "3");
        assert_eq!(Styler::enabled().red(""), "");
    }
}
