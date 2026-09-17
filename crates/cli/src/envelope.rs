//! JSON payload envelopes and terminal error formatting.

use crate::style::Styler;
use serde::Serialize;

/// JSON success envelope: exactly one JSON document on stdout.
#[derive(Serialize)]
pub struct OkEnvelope<'a, T: ?Sized> {
    command: &'a str,
    status: &'a str,
    result: &'a T,
}

pub fn envelope_ok<T: Serialize + ?Sized>(command: &str, result: &T) -> anyhow::Result<String> {
    Ok(serde_json::to_string(&OkEnvelope {
        command,
        status: "ok",
        result,
    })?)
}

/// JSON error object: stdout stays exactly one JSON document.
/// Callers must still check the exit code; `error` is only present here.
#[derive(Serialize)]
pub struct ErrEnvelope<'a> {
    command: &'a str,
    status: &'a str,
    error: String,
}

pub fn envelope_err(command: &str, err: &anyhow::Error) -> anyhow::Result<String> {
    Ok(serde_json::to_string(&ErrEnvelope {
        command,
        status: "error",
        error: format!("{err:?}"),
    })?)
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
        assert_eq!(
            envelope_ok("list", &serde_json::json!([1, 2])).unwrap(),
            r#"{"command":"list","status":"ok","result":[1,2]}"#
        );
        let err = anyhow::anyhow!("root cause").context("backup of /w failed");
        assert_eq!(
            envelope_err("backup", &err).unwrap(),
            r#"{"command":"backup","status":"error","error":"backup of /w failed\n\nCaused by:\n    root cause"}"#
        );
    }

    #[test]
    fn renders_error_with_chain() {
        let err = anyhow::anyhow!("torn tail at sector 1034").context("backup of /w failed");
        assert_eq!(
            render_error(&err, Styler::enabled()),
            "\x1b[1;31merror\x1b[0m: backup of /w failed\n\nCaused by:\n    torn tail at sector 1034"
        );
        assert_eq!(
            render_error(&err, Styler::disabled()),
            "error: backup of /w failed\n\nCaused by:\n    torn tail at sector 1034"
        );
    }
}
