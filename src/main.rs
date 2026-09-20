mod cli;
mod color;
mod merge;
mod model;
mod query;
mod store;
mod tmux;
mod tui;

use clap::Parser;
use std::process::ExitCode;

fn main() -> ExitCode {
    let cli = cli::Cli::parse();
    let colors = cli.color.stderr();
    match cli::run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{} {error:#}", colors.paint("1;31", "Error:"));
            ExitCode::FAILURE
        }
    }
}
