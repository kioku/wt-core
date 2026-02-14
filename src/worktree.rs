use std::path::PathBuf;

use crate::domain::{BranchName, RepoRoot};
use crate::error::{AppError, Result};
use crate::git;

/// Result of a successful `add` operation.
pub struct AddResult {
    pub worktree_path: PathBuf,
    pub branch: BranchName,
    pub repo_root: PathBuf,
}

/// Result of a successful `go` operation.
pub struct GoResult {
    pub worktree_path: PathBuf,
    pub branch: BranchName,
    pub repo_root: PathBuf,
}

/// Result of a successful `remove` operation.
pub struct RemoveResult {
    pub removed_path: PathBuf,
    pub branch: BranchName,
    pub repo_root: PathBuf,
    /// Non-fatal warning (e.g. branch deletion failed after worktree removal).
    pub warning: Option<String>,
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
pub fn add(repo: &RepoRoot, branch: &BranchName, base: Option<&str>) -> Result<AddResult> {
    // Refuse if branch already exists locally.
    if git::branch_exists(repo, branch) {
        return Err(AppError::conflict(format!(
            "branch '{}' already exists",
            branch
        )));
    }

    // Validate base revision if specified.
    if let Some(rev) = base.filter(|rev| !git::rev_exists(repo, rev)) {
        return Err(AppError::git(format!("revision '{rev}' not found")));
    }

    let wt_dir = repo.worktrees_dir().join(branch.to_dir_name());

    if wt_dir.exists() {
        return Err(AppError::conflict(format!(
            "worktree directory already exists: {}",
            wt_dir.display()
        )));
    }

    git::add_worktree(repo, &wt_dir, branch, base)?;

    Ok(AddResult {
        worktree_path: wt_dir,
        branch: branch.clone(),
        repo_root: repo.to_path_buf(),
    })
}

/// Resolve and return the path of an existing worktree for the given branch.
pub fn go(repo: &RepoRoot, branch: &BranchName) -> Result<GoResult> {
    let worktrees = git::list_worktrees(repo)?;

    let found = worktrees
        .iter()
        .find(|wt| wt.branch.as_deref() == Some(branch.as_str()));

    match found {
        Some(wt) => Ok(GoResult {
            worktree_path: wt.path.clone(),
            branch: branch.clone(),
            repo_root: repo.to_path_buf(),
        }),
        None => Err(AppError::usage(format!(
            "no worktree found for branch '{branch}'"
        ))),
    }
}

/// Remove a worktree and delete its local branch.
pub fn remove(repo: &RepoRoot, branch: Option<&BranchName>, force: bool) -> Result<RemoveResult> {
    let worktrees = git::list_worktrees(repo)?;

    // Resolve which branch to remove.
    let target_branch = match branch {
        Some(b) => b.clone(),
        None => {
            // Infer from cwd: find worktree whose path matches cwd.
            let cwd = std::env::current_dir()
                .map_err(|e| AppError::usage(format!("cannot determine cwd: {e}")))?;
            let found = worktrees.iter().find(|wt| cwd.starts_with(&wt.path));
            match found {
                Some(wt) => BranchName::new(wt.branch.clone().ok_or_else(|| {
                    AppError::usage("current worktree has no branch".to_string())
                })?),
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
        .find(|wt| wt.branch.as_deref() == Some(target_branch.as_str()))
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
    // Branch deletion: best-effort — bubble warning instead of blocking.
    let warning = git::delete_branch(repo, &target_branch, force)
        .err()
        .map(|e| format!("worktree removed but branch deletion failed: {e}"));

    Ok(RemoveResult {
        removed_path,
        branch: target_branch,
        repo_root: repo.to_path_buf(),
        warning,
    })
}

/// How a branch was detected as integrated into mainline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IntegrationMethod {
    /// `git merge-base --is-ancestor` succeeded (merge or fast-forward).
    Merged,
    /// `git cherry` showed all patches are in mainline (rebase merge).
    Rebase,
}

/// Integration status for a single worktree branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IntegrationStatus {
    /// Branch is fully integrated into mainline.
    Integrated(IntegrationMethod),
    /// Branch has commits not yet in mainline.
    NotIntegrated,
    /// Worktree has no branch (detached HEAD).
    NoBranch,
}

/// A worktree entry annotated with its integration status for prune.
#[derive(Debug)]
pub struct WorktreePruneEntry {
    pub branch: Option<String>,
    pub path: std::path::PathBuf,
    pub status: IntegrationStatus,
}

/// Result of a prune dry-run.
#[derive(Debug)]
pub struct PruneDryRun {
    pub mainline: String,
    pub entries: Vec<WorktreePruneEntry>,
}

/// An entry that was pruned (removed).
#[derive(Debug)]
pub struct PrunedEntry {
    pub branch: String,
    pub path: std::path::PathBuf,
}

/// An entry that was skipped during pruning.
#[derive(Debug)]
pub struct SkippedEntry {
    pub branch: Option<String>,
    pub path: std::path::PathBuf,
    pub reason: String,
}

/// Result of a prune execution.
#[derive(Debug)]
pub struct PruneExecuteResult {
    pub mainline: String,
    pub pruned: Vec<PrunedEntry>,
    pub skipped: Vec<SkippedEntry>,
    pub warnings: Vec<String>,
}

/// Classify the integration status of a branch against the mainline.
fn classify_integration(repo: &RepoRoot, branch: &str, mainline: &str) -> IntegrationStatus {
    // 1. Ancestry check (merge / fast-forward)
    if git::is_ancestor(repo, branch, mainline) {
        return IntegrationStatus::Integrated(IntegrationMethod::Merged);
    }

    // 2. Patch-id check (rebase merge)
    if git::cherry(repo, mainline, branch) {
        return IntegrationStatus::Integrated(IntegrationMethod::Rebase);
    }

    IntegrationStatus::NotIntegrated
}

/// Dry-run: scan worktrees and report integration status without removing anything.
pub fn prune_dry_run(repo: &RepoRoot, mainline_override: Option<&str>) -> Result<PruneDryRun> {
    let mainline = match mainline_override {
        Some(m) => {
            if !git::rev_exists(repo, m) {
                return Err(AppError::usage(format!(
                    "mainline branch '{m}' does not exist"
                )));
            }
            m.to_string()
        }
        None => git::resolve_mainline(repo)?,
    };

    let worktrees = git::list_worktrees(repo)?;
    let mut entries = Vec::new();

    for wt in &worktrees {
        if wt.is_main {
            continue;
        }

        let status = match &wt.branch {
            Some(branch) => classify_integration(repo, branch, &mainline),
            None => IntegrationStatus::NoBranch,
        };

        entries.push(WorktreePruneEntry {
            branch: wt.branch.clone(),
            path: wt.path.clone(),
            status,
        });
    }

    Ok(PruneDryRun { mainline, entries })
}

/// Accumulator for prune execution results.
struct PruneAccumulator {
    pruned: Vec<PrunedEntry>,
    skipped: Vec<SkippedEntry>,
    warnings: Vec<String>,
}

/// Try to remove an integrated worktree and its branch.
///
/// When the branch was integrated via rebase (patch-id match), Git's own
/// ancestry check (`git branch -d`) would refuse deletion because the
/// original commits are not ancestors of mainline.  We auto-escalate to
/// `-D` in that case since the cherry check already confirmed integration.
fn prune_integrated_entry(
    repo: &RepoRoot,
    entry: WorktreePruneEntry,
    force: bool,
    acc: &mut PruneAccumulator,
) {
    let branch_name = entry.branch.clone().expect("integrated implies branch");

    let force_branch = force
        || matches!(
            entry.status,
            IntegrationStatus::Integrated(IntegrationMethod::Rebase)
        );

    if let Err(e) = git::remove_worktree(repo, &entry.path, force) {
        acc.warnings.push(format!(
            "failed to remove worktree for '{branch_name}': {e}"
        ));
        acc.skipped.push(SkippedEntry {
            branch: Some(branch_name),
            path: entry.path,
            reason: "removal_failed".to_string(),
        });
        return;
    }

    let bn = BranchName::new(&branch_name);
    if let Err(e) = git::delete_branch(repo, &bn, force_branch) {
        acc.warnings.push(format!(
            "worktree removed but branch deletion failed for '{branch_name}': {e}"
        ));
    }
    acc.pruned.push(PrunedEntry {
        branch: branch_name,
        path: entry.path,
    });
}

/// Execute prune: remove integrated worktrees and their branches.
pub fn prune_execute(
    repo: &RepoRoot,
    mainline_override: Option<&str>,
    force: bool,
) -> Result<PruneExecuteResult> {
    let dry_run = prune_dry_run(repo, mainline_override)?;
    let mainline = dry_run.mainline;

    let mut acc = PruneAccumulator {
        pruned: Vec::new(),
        skipped: Vec::new(),
        warnings: Vec::new(),
    };

    for entry in dry_run.entries {
        match entry.status {
            IntegrationStatus::Integrated(_) => {
                prune_integrated_entry(repo, entry, force, &mut acc);
            }
            IntegrationStatus::NotIntegrated => {
                acc.skipped.push(SkippedEntry {
                    branch: entry.branch,
                    path: entry.path,
                    reason: "not_integrated".to_string(),
                });
            }
            IntegrationStatus::NoBranch => {
                acc.skipped.push(SkippedEntry {
                    branch: None,
                    path: entry.path,
                    reason: "no_branch".to_string(),
                });
            }
        }
    }

    Ok(PruneExecuteResult {
        mainline,
        pruned: acc.pruned,
        skipped: acc.skipped,
        warnings: acc.warnings,
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
