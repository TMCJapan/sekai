//! Library interface for the `sekai` CLI application.
//!
//! Argument parsing and output formatting components over `sekai-app`.

pub mod cli;
pub mod commands;
pub mod envelope;
pub mod style;

use cli::Cli;
use envelope::{Printer, render_error};
use style::Styler;

/// Executes the CLI application pipeline using the provided options.
pub async fn run_app(cli: &Cli) -> std::process::ExitCode {
    let printer = Printer::new(Styler::new(cli.color), cli.command.output_json());
    let Err(err) = commands::run(cli, &printer).await else {
        return std::process::ExitCode::SUCCESS;
    };
    match printer
        .json
        .then(|| printer.envelope_err(cli.command.name(), &err))
    {
        Some(Ok(doc)) => println!("{doc}"),
        _ => eprintln!("{}", render_error(&err, Styler::new_stderr(cli.color))),
    }
    std::process::ExitCode::FAILURE
}
