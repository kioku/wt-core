mod cli;
mod commands;
mod domain;
mod error;
mod git;
mod materialize;
mod operation_state;
mod output;
mod symlinks;
mod worktree;

use std::process;

use clap::Parser;

fn main() -> process::ExitCode {
    #[cfg(windows)]
    operation_state::run_windows_lifecycle_launcher();

    let cli = cli::Cli::parse();

    match commands::run(cli) {
        Ok(commands::RunOutcome::Success) => process::ExitCode::SUCCESS,
        Ok(commands::RunOutcome::Exit(code)) => process::exit(code),
        Err(e) => {
            eprintln!("error: {e}");
            e.code.into()
        }
    }
}
