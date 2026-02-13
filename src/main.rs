mod cli;
mod commands;
mod domain;
mod error;
mod git;
mod output;
mod worktree;

use std::process;

use clap::Parser;

fn main() -> process::ExitCode {
    let cli = cli::Cli::parse();

    match commands::run(cli) {
        Ok(()) => process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            e.code.into()
        }
    }
}
