//! Thin `sekai` binary over `sekai-app`.
//!
//! Argument parsing and output formatting only. Quiesce the server before
//! snapshotting (e.g. `save-off`, `save-all`, then `save-on` afterwards);
//! orchestration belongs to the caller, never to this tool.

use clap::Parser as _;
use sekai_cli::cli::Cli;

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    sekai_cli::run_app(&cli).await
}
