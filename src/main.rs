mod cli;
mod commands;
mod domain;
mod error;
mod git;
mod materialize;
mod object_copy;
mod operation_state;
mod output;
mod symlinks;
mod worktree;

use std::process;

use clap::Parser;

fn main() -> process::ExitCode {
    #[cfg(windows)]
    if let Some(result) = materialize::run_cache_helper_if_requested() {
        match result {
            Ok(code) => process::exit(code),
            Err(error) => {
                eprintln!("error: {error}");
                return error.code.into();
            }
        }
    }
    let cli = cli::Cli::parse();

    let json = match &cli.command {
        cli::Command::List { json, .. }
        | cli::Command::Add { json, .. }
        | cli::Command::Go { json, .. }
        | cli::Command::Remove { json, .. }
        | cli::Command::Merge { json, .. }
        | cli::Command::Materialize { json, .. }
        | cli::Command::Prune { json, .. }
        | cli::Command::Setup { json, .. }
        | cli::Command::Doctor { json, .. } => *json,
        // Exec owns native child streams/status, including resolution failures.
        _ => false,
    };
    match commands::run(cli) {
        Ok(commands::RunOutcome::Success) => process::ExitCode::SUCCESS,
        Ok(commands::RunOutcome::Exit(code)) => process::exit(code),
        Err(e) => {
            if json && !output::json_response_written() {
                let response = serde_json::json!({
                    "ok": false, "message": e.message, "exit_code": e.code as u8,
                });
                output::print_json(&response).unwrap_or_else(|error| eprintln!("error: {error}"));
            }
            eprintln!("error: {e}");
            e.code.into()
        }
    }
}
