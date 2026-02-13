mod cli;
mod domain;
mod error;
mod git;
mod output;
mod worktree;

use std::path::PathBuf;
use std::process;

use clap::Parser;

use crate::cli::{Cli, Command};
use crate::error::AppError;
use crate::output::{JsonListResponse, JsonResponse, OutputFormat};

fn main() -> process::ExitCode {
    let cli = Cli::parse();

    match run(cli) {
        Ok(()) => process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            e.code.into()
        }
    }
}

fn run(cli: Cli) -> error::Result<()> {
    match cli.command {
        Command::List { repo, json } => cmd_list(repo, fmt_flag(json, false)),
        Command::Add {
            branch,
            base,
            repo,
            json,
            print_cd_path,
        } => cmd_add(
            &branch,
            base.as_deref(),
            repo,
            fmt_flag(json, print_cd_path),
        ),
        Command::Go {
            branch,
            repo,
            json,
            print_cd_path,
        } => cmd_go(&branch, repo, fmt_flag(json, print_cd_path)),
        Command::Remove {
            branch,
            force,
            repo,
            json,
            print_paths,
        } => cmd_remove(
            branch.as_deref(),
            force,
            repo,
            fmt_remove(json, print_paths),
        ),
        Command::Doctor { repo, json } => cmd_doctor(repo, fmt_flag(json, false)),
    }
}

fn fmt_flag(json: bool, cd_path: bool) -> OutputFormat {
    if cd_path {
        OutputFormat::CdPath
    } else if json {
        OutputFormat::Json
    } else {
        OutputFormat::Human
    }
}

fn fmt_remove(json: bool, print_paths: bool) -> OutputFormat {
    if print_paths {
        OutputFormat::RemovePaths
    } else if json {
        OutputFormat::Json
    } else {
        OutputFormat::Human
    }
}

fn resolve_repo(repo: Option<PathBuf>) -> error::Result<domain::RepoRoot> {
    let start = match repo {
        Some(p) => p,
        None => std::env::current_dir()
            .map_err(|e| AppError::not_a_repo(format!("cannot determine cwd: {e}")))?,
    };
    git::repo_root(&start)
}

// ── Commands ────────────────────────────────────────────────────────

fn cmd_list(repo: Option<PathBuf>, fmt: OutputFormat) -> error::Result<()> {
    let repo = resolve_repo(repo)?;
    let worktrees = git::list_worktrees(&repo)?;

    match fmt {
        OutputFormat::Json | OutputFormat::CdPath => {
            let resp = JsonListResponse::from_worktrees(&worktrees);
            println!(
                "{}",
                serde_json::to_string_pretty(&resp)
                    .map_err(|e| AppError::git(format!("json error: {e}")))?
            );
        }
        OutputFormat::Human | OutputFormat::RemovePaths => {
            if worktrees.is_empty() {
                println!("No worktrees found.");
                return Ok(());
            }
            for wt in &worktrees {
                let branch_str = wt.branch.as_deref().unwrap_or("(detached)");
                let main_tag = if wt.is_main { " [main]" } else { "" };
                println!(
                    "{:<50} {:<20} {}{}",
                    wt.path.display(),
                    branch_str,
                    wt.commit,
                    main_tag
                );
            }
        }
    }
    Ok(())
}

fn cmd_add(
    branch: &str,
    base: Option<&str>,
    repo: Option<PathBuf>,
    fmt: OutputFormat,
) -> error::Result<()> {
    let repo = resolve_repo(repo)?;
    let result = worktree::add(&repo, branch, base)?;

    let path_str = result.worktree_path.display().to_string();
    let root_str = result.repo_root.display().to_string();
    let branch_name = &result.branch;

    match fmt {
        OutputFormat::CdPath => {
            println!("{path_str}");
        }
        OutputFormat::Json => {
            let resp =
                JsonResponse::success(format!("created worktree for branch '{branch_name}'"))
                    .with_repo_root(&root_str)
                    .with_worktree_path(&path_str)
                    .with_cd_path(&path_str)
                    .with_branch(branch_name);
            println!(
                "{}",
                serde_json::to_string_pretty(&resp)
                    .map_err(|e| AppError::git(format!("json error: {e}")))?
            );
        }
        OutputFormat::Human | OutputFormat::RemovePaths => {
            println!("Created worktree for branch '{branch_name}' at {path_str}");
        }
    }
    Ok(())
}

fn cmd_go(branch: &str, repo: Option<PathBuf>, fmt: OutputFormat) -> error::Result<()> {
    let repo = resolve_repo(repo)?;
    let result = worktree::go(&repo, branch)?;

    let path_str = result.worktree_path.display().to_string();
    let root_str = result.repo_root.display().to_string();
    let branch_name = &result.branch;

    match fmt {
        OutputFormat::CdPath => {
            println!("{path_str}");
        }
        OutputFormat::Json => {
            let resp =
                JsonResponse::success(format!("resolved worktree for branch '{branch_name}'"))
                    .with_repo_root(&root_str)
                    .with_worktree_path(&path_str)
                    .with_cd_path(&path_str)
                    .with_branch(branch_name);
            println!(
                "{}",
                serde_json::to_string_pretty(&resp)
                    .map_err(|e| AppError::git(format!("json error: {e}")))?
            );
        }
        OutputFormat::Human | OutputFormat::RemovePaths => {
            println!("Worktree for branch '{branch_name}' is at {path_str}");
        }
    }
    Ok(())
}

fn cmd_remove(
    branch: Option<&str>,
    force: bool,
    repo: Option<PathBuf>,
    fmt: OutputFormat,
) -> error::Result<()> {
    let repo = resolve_repo(repo)?;
    let result = worktree::remove(&repo, branch, force)?;

    let removed_str = result.removed_path.display().to_string();
    let root_str = result.repo_root.display().to_string();
    let branch_name = &result.branch;

    match fmt {
        OutputFormat::RemovePaths => {
            println!("{removed_str}");
            println!("{root_str}");
        }
        OutputFormat::Json | OutputFormat::CdPath => {
            let resp =
                JsonResponse::success(format!("removed worktree for branch '{branch_name}'"))
                    .with_repo_root(&root_str)
                    .with_removed_path(&removed_str)
                    .with_branch(branch_name);
            println!(
                "{}",
                serde_json::to_string_pretty(&resp)
                    .map_err(|e| AppError::git(format!("json error: {e}")))?
            );
        }
        OutputFormat::Human => {
            println!("Removed worktree and branch '{branch_name}' ({removed_str})");
        }
    }
    Ok(())
}

fn cmd_doctor(repo: Option<PathBuf>, fmt: OutputFormat) -> error::Result<()> {
    let repo = resolve_repo(repo)?;
    let diags = worktree::doctor(&repo)?;

    match fmt {
        OutputFormat::Json | OutputFormat::CdPath => {
            #[derive(serde::Serialize)]
            struct JsonDoctorResponse {
                ok: bool,
                diagnostics: Vec<JsonDiag>,
            }
            #[derive(serde::Serialize)]
            struct JsonDiag {
                level: worktree::DiagLevel,
                message: String,
            }

            let has_errors = diags.iter().any(|d| d.level == worktree::DiagLevel::Error);
            let resp = JsonDoctorResponse {
                ok: !has_errors,
                diagnostics: diags
                    .iter()
                    .map(|d| JsonDiag {
                        level: d.level,
                        message: d.message.clone(),
                    })
                    .collect(),
            };
            println!(
                "{}",
                serde_json::to_string_pretty(&resp)
                    .map_err(|e| AppError::git(format!("json error: {e}")))?
            );
        }
        OutputFormat::Human | OutputFormat::RemovePaths => {
            for d in &diags {
                let icon = match d.level {
                    worktree::DiagLevel::Ok => "✓",
                    worktree::DiagLevel::Warn => "⚠",
                    worktree::DiagLevel::Error => "✗",
                };
                println!("{icon} {}", d.message);
            }
        }
    }
    Ok(())
}
