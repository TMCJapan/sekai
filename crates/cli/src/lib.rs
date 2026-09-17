//! Library interface for the `sekai` CLI application.
//!
//! Argument parsing and output formatting components over `sekai-app`.

pub mod cli;
pub mod commands;
pub mod envelope;
pub mod style;

use cli::Cli;
use envelope::{envelope_err, render_error};
use style::Styler;

/// Executes the CLI application pipeline using the provided options.
pub async fn run_app(cli: &Cli) -> std::process::ExitCode {
    let command_name = cli.command.name();
    let as_json = cli.command.output_json();

    match commands::run(cli).await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(err) => {
            if as_json {
                match envelope_err(command_name, &err) {
                    Ok(doc) => println!("{doc}"),
                    Err(_) => {
                        eprintln!("{}", render_error(&err, Styler::new_stderr(cli.color)));
                    }
                }
            } else {
                eprintln!("{}", render_error(&err, Styler::new_stderr(cli.color)));
            }
            std::process::ExitCode::FAILURE
        }
    }
}
