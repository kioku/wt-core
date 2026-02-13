use std::path::PathBuf;

use crate::cli::{Cli, Command};
use crate::domain::{self, BranchName};
use crate::error::{AppError, Result};
use crate::git;
use crate::output::{
    print_json, JsonDoctorResponse, JsonListResponse, JsonResponse, NavigationFormat, RemoveFormat,
    StatusFormat,
};
use crate::worktree;

pub fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Command::List { repo, json } => cmd_list(repo, status_fmt(json)),
        Command::Add {
            branch,
            base,
            repo,
            json,
            print_cd_path,
        } => cmd_add(
            &BranchName::new(&branch),
            base.as_deref(),
            repo,
            nav_fmt(json, print_cd_path),
        ),
        Command::Go {
            branch,
            repo,
            json,
            print_cd_path,
        } => cmd_go(
            &BranchName::new(&branch),
            repo,
            nav_fmt(json, print_cd_path),
        ),
        Command::Remove {
            branch,
            force,
            repo,
            json,
            print_paths,
        } => cmd_remove(
            branch.as_deref().map(BranchName::new),
            force,
            repo,
            remove_fmt(json, print_paths),
        ),
        Command::Doctor { repo, json } => cmd_doctor(repo, status_fmt(json)),
    }
}

fn nav_fmt(json: bool, cd_path: bool) -> NavigationFormat {
    if cd_path {
        NavigationFormat::CdPath
    } else if json {
        NavigationFormat::Json
    } else {
        NavigationFormat::Human
    }
}

fn status_fmt(json: bool) -> StatusFormat {
    if json {
        StatusFormat::Json
    } else {
        StatusFormat::Human
    }
}

fn remove_fmt(json: bool, print_paths: bool) -> RemoveFormat {
    if print_paths {
        RemoveFormat::PrintPaths
    } else if json {
        RemoveFormat::Json
    } else {
        RemoveFormat::Human
    }
}

fn resolve_repo(repo: Option<PathBuf>) -> Result<domain::RepoRoot> {
    let start = match repo {
        Some(p) => p,
        None => std::env::current_dir()
            .map_err(|e| AppError::not_a_repo(format!("cannot determine cwd: {e}")))?,
    };
    git::repo_root(&start)
}

// ── Commands ────────────────────────────────────────────────────────

fn cmd_list(repo: Option<PathBuf>, fmt: StatusFormat) -> Result<()> {
    let repo = resolve_repo(repo)?;
    let worktrees = git::list_worktrees(&repo)?;

    match fmt {
        StatusFormat::Json => {
            print_json(&JsonListResponse::from_worktrees(&worktrees))?;
        }
        StatusFormat::Human => {
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
    branch: &BranchName,
    base: Option<&str>,
    repo: Option<PathBuf>,
    fmt: NavigationFormat,
) -> Result<()> {
    let repo = resolve_repo(repo)?;
    let result = worktree::add(&repo, branch, base)?;

    let path_str = result.worktree_path.display().to_string();
    let root_str = result.repo_root.display().to_string();
    let branch_name = &result.branch;

    match fmt {
        NavigationFormat::CdPath => {
            println!("{path_str}");
        }
        NavigationFormat::Json => {
            let resp =
                JsonResponse::success(format!("created worktree for branch '{branch_name}'"))
                    .with_repo_root(&root_str)
                    .with_worktree_path(&path_str)
                    .with_cd_path(&path_str)
                    .with_branch(branch_name.as_str());
            print_json(&resp)?;
        }
        NavigationFormat::Human => {
            println!("Created worktree for branch '{branch_name}' at {path_str}");
        }
    }
    Ok(())
}

fn cmd_go(branch: &BranchName, repo: Option<PathBuf>, fmt: NavigationFormat) -> Result<()> {
    let repo = resolve_repo(repo)?;
    let result = worktree::go(&repo, branch)?;

    let path_str = result.worktree_path.display().to_string();
    let root_str = result.repo_root.display().to_string();
    let branch_name = &result.branch;

    match fmt {
        NavigationFormat::CdPath => {
            println!("{path_str}");
        }
        NavigationFormat::Json => {
            let resp =
                JsonResponse::success(format!("resolved worktree for branch '{branch_name}'"))
                    .with_repo_root(&root_str)
                    .with_worktree_path(&path_str)
                    .with_cd_path(&path_str)
                    .with_branch(branch_name.as_str());
            print_json(&resp)?;
        }
        NavigationFormat::Human => {
            println!("Worktree for branch '{branch_name}' is at {path_str}");
        }
    }
    Ok(())
}

fn cmd_remove(
    branch: Option<BranchName>,
    force: bool,
    repo: Option<PathBuf>,
    fmt: RemoveFormat,
) -> Result<()> {
    let repo = resolve_repo(repo)?;
    let result = worktree::remove(&repo, branch.as_ref(), force)?;

    let removed_str = result.removed_path.display().to_string();
    let root_str = result.repo_root.display().to_string();
    let branch_name = &result.branch;

    match fmt {
        RemoveFormat::PrintPaths => {
            println!("{removed_str}");
            println!("{root_str}");
            println!("{branch_name}");
        }
        RemoveFormat::Json => {
            let resp =
                JsonResponse::success(format!("removed worktree for branch '{branch_name}'"))
                    .with_repo_root(&root_str)
                    .with_removed_path(&removed_str)
                    .with_branch(branch_name.as_str());
            print_json(&resp)?;
        }
        RemoveFormat::Human => {
            println!("Removed worktree and branch '{branch_name}' ({removed_str})");
        }
    }
    if let Some(w) = &result.warning {
        eprintln!("warning: {w}");
    }
    Ok(())
}

fn cmd_doctor(repo: Option<PathBuf>, fmt: StatusFormat) -> Result<()> {
    let repo = resolve_repo(repo)?;
    let diags = worktree::doctor(&repo)?;

    match fmt {
        StatusFormat::Json => {
            print_json(&JsonDoctorResponse::from_diagnostics(&diags))?;
        }
        StatusFormat::Human => {
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
