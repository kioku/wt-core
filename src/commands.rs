use std::io::IsTerminal;
use std::path::PathBuf;

use crate::cli::{Cli, Command, Shell};
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
            interactive,
            repo,
            json,
            print_cd_path,
        } => cmd_go(
            branch.as_deref(),
            interactive,
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
        Command::Init { shell } => cmd_init(shell),
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

fn cmd_go(
    branch: Option<&str>,
    interactive: bool,
    repo: Option<PathBuf>,
    fmt: NavigationFormat,
) -> Result<()> {
    let repo = resolve_repo(repo)?;

    let resolved_branch = match branch {
        Some(b) => BranchName::new(b),
        None => resolve_interactive_branch(&repo, interactive, fmt)?,
    };

    let result = worktree::go(&repo, &resolved_branch)?;

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

/// Resolve a branch via interactive picker or error if not possible.
fn resolve_interactive_branch(
    repo: &domain::RepoRoot,
    interactive: bool,
    fmt: NavigationFormat,
) -> Result<BranchName> {
    // Machine-output modes require an explicit branch argument.
    if fmt != NavigationFormat::Human {
        return Err(AppError::usage(
            "branch argument is required with --json or --print-cd-path".to_string(),
        ));
    }

    let worktrees = git::list_worktrees(repo)?;
    let candidates: Vec<_> = worktrees.iter().filter(|wt| !wt.is_main).collect();

    if candidates.is_empty() {
        return Err(AppError::usage(
            "no worktrees to select (create one with `wt add`)".to_string(),
        ));
    }

    // Auto-select when there is exactly one candidate (unless -i forces the picker).
    if !interactive && candidates.len() == 1 {
        let branch = candidates[0]
            .branch
            .as_deref()
            .ok_or_else(|| AppError::usage("worktree has no branch (detached HEAD)".to_string()))?;
        return Ok(BranchName::new(branch));
    }

    // The interactive picker always requires a TTY.
    if !std::io::stdin().is_terminal() {
        return Err(AppError::usage(
            "no branch specified; interactive mode requires a terminal".to_string(),
        ));
    }

    pick_worktree(&worktrees)
}

/// Present an interactive fuzzy picker and return the selected branch.
#[cfg(feature = "interactive")]
fn pick_worktree(worktrees: &[domain::Worktree]) -> Result<BranchName> {
    use dialoguer::theme::ColorfulTheme;
    use dialoguer::FuzzySelect;

    let items: Vec<String> = worktrees
        .iter()
        .map(|wt| {
            let branch = wt.branch.as_deref().unwrap_or("(detached)");
            let tag = if wt.is_main { " [main]" } else { "" };
            format!("{branch:<30} {:<50} {}{tag}", wt.path.display(), wt.commit)
        })
        .collect();

    let selection = FuzzySelect::with_theme(&ColorfulTheme::default())
        .with_prompt("Select worktree")
        .items(&items)
        .default(1) // skip main worktree (always index 0)
        .interact_opt()
        .map_err(|e| AppError::usage(format!("picker failed: {e}")))?;

    match selection {
        Some(idx) => {
            let branch = worktrees[idx].branch.as_deref().ok_or_else(|| {
                AppError::usage("selected worktree has no branch (detached HEAD)".to_string())
            })?;
            Ok(BranchName::new(branch))
        }
        // Esc / Ctrl-C: dialoguer has already restored the terminal state
        // before returning None, so destructors are not a concern here.
        // Exit 130 (128 + SIGINT) is the Unix convention for user cancellation.
        None => std::process::exit(130),
    }
}

#[cfg(not(feature = "interactive"))]
fn pick_worktree(_worktrees: &[domain::Worktree]) -> Result<BranchName> {
    Err(AppError::usage(
        "interactive mode not available (compiled without 'interactive' feature)".to_string(),
    ))
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

fn cmd_init(shell: Shell) -> Result<()> {
    let script = match shell {
        Shell::Bash => include_str!("../bindings/bash/wt.bash"),
        Shell::Zsh => include_str!("../bindings/zsh/wt.zsh"),
        Shell::Fish => include_str!("../bindings/fish/wt.fish"),
        Shell::Nu => include_str!("../bindings/nu/wt.nu"),
    };
    print!("{script}");
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
