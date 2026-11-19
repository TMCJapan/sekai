//! JSON payload envelopes and terminal error formatting.

use serde::Serialize;

use crate::style::Styler;

/// How a run prints: human text, or one JSON envelope on stdout, so
/// stderr stays silent in JSON mode (ADR-0002).
pub struct Printer {
    /// Colors for human output on stdout.
    pub style: Styler,
    /// Whether the run prints one JSON envelope (`--json`).
    pub json: bool,
}

impl Printer {
    pub const fn new(style: Styler, json: bool) -> Self {
        Self { style, json }
    }

    /// Print the success envelope for `command` with `result`.
    pub fn print_ok<T: Serialize + ?Sized>(&self, command: &str, result: &T) -> anyhow::Result<()> {
        println!("{}", self.envelope_ok(command, result)?);
        Ok(())
    }

    /// The success envelope for `command` with `result`.
    pub fn envelope_ok<T: Serialize + ?Sized>(
        &self,
        command: &str,
        result: &T,
    ) -> anyhow::Result<String> {
        Ok(serde_json::to_string(&Envelope {
            command,
            status: "ok",
            result: Some(result),
            error: None,
        })?)
    }

    /// The error envelope for `command`: the full error chain as a string.
    pub fn envelope_err(&self, command: &str, err: &anyhow::Error) -> anyhow::Result<String> {
        Ok(serde_json::to_string(&Envelope::<()> {
            command,
            status: "error",
            result: None,
            error: Some(format!("{err:?}")),
        })?)
    }
}

/// The one JSON document a run prints: `result` on success, `error` on
/// failure.
#[derive(Serialize)]
struct Envelope<'a, T: ?Sized> {
    command: &'a str,
    status: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<&'a T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

/// Render a runtime failure for stderr: red bold `error:` prefix plus the
/// anyhow context chain. Mirrors clap's own parse-error look.
pub fn render_error(err: &anyhow::Error, style: Styler) -> String {
    let chain = format!("{err:?}");
    let mut lines = chain.lines();
    let mut out = lines.next().map_or_else(
        || style.red_bold("error"),
        |first| format!("{}: {first}", style.red_bold("error")),
    );
    for line in lines {
        out.push('\n');
        out.push_str(line);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_json_envelopes() {
        let printer = Printer::new(Styler::disabled(), true);
        assert_eq!(
            printer
                .envelope_ok("log", &serde_json::json!([1, 2]))
                .unwrap(),
            r#"{"command":"log","status":"ok","result":[1,2]}"#
        );
        let err = anyhow::anyhow!("root cause").context("commit of /w failed");
        assert_eq!(
            printer.envelope_err("commit", &err).unwrap(),
            r#"{"command":"commit","status":"error","error":"commit of /w failed\n\nCaused by:\n    root cause"}"#
        );
    }

    #[test]
    fn renders_error_with_chain() {
        let err = anyhow::anyhow!("torn tail at sector 1034").context("commit of /w failed");
        assert_eq!(
            render_error(&err, Styler::enabled()),
            "\x1b[1;31merror\x1b[0m: commit of /w failed\n\nCaused by:\n    torn tail at sector 1034"
        );
        assert_eq!(
            render_error(&err, Styler::disabled()),
            "error: commit of /w failed\n\nCaused by:\n    torn tail at sector 1034"
        );
    }
}
