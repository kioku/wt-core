use std::path::PathBuf;

use crate::domain::{BranchName, RepoRoot};
use crate::error::{AppError, Result};
use crate::git;

/// Result of a successful `add` operation.
pub struct AddResult {
    pub worktree_path: PathBuf,
    pub branch: String,
    pub repo_root: PathBuf,
}

/// Result of a successful `go` operation.
pub struct GoResult {
    pub worktree_path: PathBuf,
    pub branch: String,
    pub repo_root: PathBuf,
}

/// Result of a successful `remove` operation.
pub struct RemoveResult {
    pub removed_path: PathBuf,
    pub branch: String,
    pub repo_root: PathBuf,
}

/// Diagnostic from the `doctor` command.
#[derive(Debug)]
pub struct Diagnostic {
    pub level: DiagLevel,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DiagLevel {
    Ok,
    Warn,
    Error,
}

/// Create a new worktree for the given branch.
pub fn add(repo: &RepoRoot, branch: &str, base: Option<&str>) -> Result<AddResult> {
    let branch_name = BranchName::new(branch);

    // Refuse if branch already exists locally.
    if git::branch_exists(repo, branch) {
        return Err(AppError::conflict(format!(
            "branch '{branch}' already exists"
        )));
    }

    // Validate base revision if specified.
    if let Some(rev) = base.filter(|rev| !git::rev_exists(repo, rev)) {
        return Err(AppError::git(format!("revision '{rev}' not found")));
    }

    let wt_dir = repo.worktrees_dir().join(branch_name.to_dir_name());

    if wt_dir.exists() {
        return Err(AppError::conflict(format!(
            "worktree directory already exists: {}",
            wt_dir.display()
        )));
    }

    git::add_worktree(repo, &wt_dir, branch, base)?;

    Ok(AddResult {
        worktree_path: wt_dir,
        branch: branch.to_string(),
        repo_root: repo.0.clone(),
    })
}

/// Resolve and return the path of an existing worktree for the given branch.
pub fn go(repo: &RepoRoot, branch: &str) -> Result<GoResult> {
    let worktrees = git::list_worktrees(repo)?;

    let found = worktrees
        .iter()
        .find(|wt| wt.branch.as_ref().is_some_and(|b| b == branch));

    match found {
        Some(wt) => Ok(GoResult {
            worktree_path: wt.path.clone(),
            branch: branch.to_string(),
            repo_root: repo.0.clone(),
        }),
        None => Err(AppError::usage(format!(
            "no worktree found for branch '{branch}'"
        ))),
    }
}

/// Remove a worktree and delete its local branch.
pub fn remove(repo: &RepoRoot, branch: Option<&str>, force: bool) -> Result<RemoveResult> {
    let worktrees = git::list_worktrees(repo)?;

    // Resolve which branch to remove.
    let target_branch = match branch {
        Some(b) => b.to_string(),
        None => {
            // Infer from cwd: find worktree whose path matches cwd.
            let cwd = std::env::current_dir()
                .map_err(|e| AppError::usage(format!("cannot determine cwd: {e}")))?;
            let found = worktrees.iter().find(|wt| cwd.starts_with(&wt.path));
            match found {
                Some(wt) => wt
                    .branch
                    .clone()
                    .ok_or_else(|| AppError::usage("current worktree has no branch".to_string()))?,
                None => {
                    return Err(AppError::usage(
                        "no branch specified and cwd is not inside a worktree".to_string(),
                    ))
                }
            }
        }
    };

    // Find the worktree entry.
    let wt = worktrees
        .iter()
        .find(|wt| wt.branch.as_deref() == Some(&target_branch))
        .ok_or_else(|| {
            AppError::usage(format!("no worktree found for branch '{target_branch}'"))
        })?;

    // Never remove main worktree.
    if wt.is_main {
        return Err(AppError::invariant(
            "refusing to remove the main worktree".to_string(),
        ));
    }

    let removed_path = wt.path.clone();

    // Remove worktree first, then branch.
    git::remove_worktree(repo, &removed_path, force)?;
    // Branch deletion: best-effort. If the branch was already deleted upstream, ignore error.
    let _ = git::delete_branch(repo, &target_branch, force);

    Ok(RemoveResult {
        removed_path,
        branch: target_branch,
        repo_root: repo.0.clone(),
    })
}

/// Run health diagnostics on the repository's worktree state.
pub fn doctor(repo: &RepoRoot) -> Result<Vec<Diagnostic>> {
    let mut diags = Vec::new();

    // Check .worktrees directory exists.
    let wt_dir = repo.worktrees_dir();
    if !wt_dir.exists() {
        diags.push(Diagnostic {
            level: DiagLevel::Ok,
            message: "no .worktrees directory (no worktrees created yet)".to_string(),
        });
        return Ok(diags);
    }

    // List worktrees and check for orphaned directories.
    let worktrees = git::list_worktrees(repo)?;

    let managed_paths: Vec<_> = worktrees.iter().map(|wt| &wt.path).collect();

    let orphaned = std::fs::read_dir(&wt_dir)
        .into_iter()
        .flat_map(|entries| entries.flatten())
        .map(|entry| entry.path())
        .filter(|p| p.is_dir() && !managed_paths.contains(&p));

    for orphan in orphaned {
        diags.push(Diagnostic {
            level: DiagLevel::Warn,
            message: format!(
                "orphaned directory not tracked by git: {}",
                orphan.display()
            ),
        });
    }

    // Check each worktree has a valid branch.
    for wt in &worktrees {
        if wt.is_main {
            continue;
        }
        if wt.branch.is_none() {
            diags.push(Diagnostic {
                level: DiagLevel::Warn,
                message: format!(
                    "worktree has no branch (detached HEAD): {}",
                    wt.path.display()
                ),
            });
        }
    }

    if diags.is_empty() {
        diags.push(Diagnostic {
            level: DiagLevel::Ok,
            message: "all worktrees healthy".to_string(),
        });
    }

    Ok(diags)
}
