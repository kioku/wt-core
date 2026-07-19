use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::domain::{BranchName, RepoRoot, Worktree};
use crate::error::{AppError, Result};
use crate::git;
use crate::symlinks;

/// Find the worktree that most specifically contains `cwd`.
///
/// Worktree directories are nested under the main repo path, so both the
/// main worktree and linked worktree paths can be prefixes of `cwd`. We must
/// choose the longest matching prefix to select the current linked worktree.
fn worktree_for_cwd<'a>(worktrees: &'a [Worktree], cwd: &Path) -> Option<&'a Worktree> {
    worktrees
        .iter()
        .filter(|wt| cwd.starts_with(&wt.path))
        .max_by_key(|wt| wt.path.as_os_str().len())
}

/// Infer the target branch from cwd by finding the worktree whose path is
/// the most specific (longest) prefix of the current directory.
///
/// Shared by `remove` and `merge` for their cwd-inference fallback.
fn resolve_branch_from_cwd(worktrees: &[Worktree]) -> Result<BranchName> {
    let cwd = std::env::current_dir()
        .map_err(|e| AppError::usage(format!("cannot determine cwd: {e}")))?;
    match worktree_for_cwd(worktrees, &cwd) {
        Some(wt) => Ok(BranchName::new(wt.branch.clone().ok_or_else(|| {
            AppError::usage("current worktree has no branch".to_string())
        })?)),
        None => Err(AppError::usage(
            "no branch specified and cwd is not inside a worktree".to_string(),
        )),
    }
}

/// Result of a successful `add` operation.
pub struct AddResult {
    pub worktree_path: PathBuf,
    pub branch: BranchName,
    pub repo_root: PathBuf,
    /// Whether the branch was created to track an existing remote branch.
    pub tracking: bool,
    /// Symlink outcomes, if a `.wt/symlinks` config was present.
    pub symlinks: Option<symlinks::SymlinkReport>,
    /// Safe per-worktree setup recommendation for pnpm workspaces.
    pub setup_recommendation: Option<String>,
}

/// Result of a successful `go` operation.
pub struct GoResult {
    pub worktree_path: PathBuf,
    pub branch: BranchName,
    pub repo_root: PathBuf,
}

/// Result of a resolved branch-vs-mainline `diff` operation.
pub struct DiffResult {
    pub branch: BranchName,
    pub base: String,
    pub command: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirtyDiffMode {
    Dirty,
    Staged,
    Unstaged,
}

/// Result of a resolved dirty-worktree `diff` operation.
pub struct DirtyDiffResult {
    pub label: String,
    pub command: Vec<String>,
}

/// Result of a successful `remove` operation.
pub struct RemoveResult {
    pub removed_path: PathBuf,
    pub branch: BranchName,
    pub repo_root: PathBuf,
    /// Whether the local branch was deleted by this operation.
    pub branch_deleted: bool,
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

/// Resolve and optionally run a branch-vs-mainline difftool command.
pub fn diff(
    repo: &RepoRoot,
    branch: &BranchName,
    against: Option<&str>,
    tool: Option<&str>,
    dry_run: bool,
) -> Result<DiffResult> {
    let worktrees = git::list_worktrees(repo)?;
    let has_worktree = worktrees
        .iter()
        .any(|wt| !wt.is_main && wt.branch.as_deref() == Some(branch.as_str()));

    if !has_worktree {
        return Err(AppError::usage(format!(
            "branch '{}' has no associated worktree",
            branch.as_str()
        )));
    }

    let base = match against {
        Some(rev) => {
            if !git::rev_exists(repo, rev) {
                return Err(AppError::usage(format!(
                    "base revision '{rev}' does not exist"
                )));
            }
            rev.to_string()
        }
        None => git::resolve_mainline(repo)?,
    };
    let range = format!("{}...{}", base, branch.as_str());
    let command = difftool_command(repo, tool, &range);

    if !dry_run {
        git::ensure_difftool_available(repo.as_ref(), tool)?;
        git::difftool(repo, tool, &range)?;
    }

    Ok(DiffResult {
        branch: branch.clone(),
        base,
        command,
    })
}

/// Resolve and optionally run a dirty-worktree difftool command.
pub fn diff_dirty(
    worktree: &Worktree,
    mode: DirtyDiffMode,
    tool: Option<&str>,
    dry_run: bool,
) -> Result<DirtyDiffResult> {
    let command = dirty_difftool_command(&worktree.path, mode, tool);

    if !dry_run {
        git::ensure_difftool_available(&worktree.path, tool)?;
        git::difftool_dirty(&worktree.path, mode, tool)?;
    }

    Ok(DirtyDiffResult {
        label: worktree
            .branch
            .clone()
            .unwrap_or_else(|| format!("detached at {}", worktree.commit)),
        command,
    })
}

fn difftool_command(repo: &RepoRoot, tool: Option<&str>, range: &str) -> Vec<String> {
    let mut command = base_difftool_command(repo.as_ref(), tool);
    command.push(range.to_string());
    command
}

fn dirty_difftool_command(
    worktree_path: &std::path::Path,
    mode: DirtyDiffMode,
    tool: Option<&str>,
) -> Vec<String> {
    let mut command = base_difftool_command(worktree_path, tool);

    match mode {
        DirtyDiffMode::Dirty => command.push("HEAD".to_string()),
        DirtyDiffMode::Staged => command.push("--staged".to_string()),
        DirtyDiffMode::Unstaged => {}
    }

    command
}

fn base_difftool_command(path: &std::path::Path, tool: Option<&str>) -> Vec<String> {
    let mut command = vec![
        "git".to_string(),
        "-C".to_string(),
        path.display().to_string(),
        "difftool".to_string(),
    ];

    if let Some(tool) = tool {
        command.push("--tool".to_string());
        command.push(tool.to_string());
    }

    command.push("--dir-diff".to_string());
    command
}

/// Create a new worktree for the given branch.
///
/// When `base` is `None` and the branch does not exist locally but does
/// exist on `origin`, the worktree is created tracking the remote branch
/// (`origin/<branch>`) and the upstream is set automatically.
///
/// When `base` is provided, a new branch is always created from that
/// revision (remote tracking is skipped).
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

    // Determine whether to track a remote branch:
    // - Only when no explicit --base is provided
    // - Only when origin/<branch> exists
    let tracking = base.is_none() && git::remote_branch_exists(repo, branch);

    let effective_base = if tracking {
        Some(format!("origin/{}", branch.as_str()))
    } else {
        None
    };

    git::add_worktree(repo, &wt_dir, branch, effective_base.as_deref().or(base))?;

    // Set upstream so `git pull`/`git push` work without arguments.
    if tracking {
        git::set_upstream(repo, branch)?;
    }

    let symlink_report = symlinks::apply_symlinks(repo, &wt_dir);
    let setup_recommendation = symlinks::is_pnpm_workspace(repo)
        .then(|| symlinks::pnpm_install_recommendation().to_string());

    Ok(AddResult {
        worktree_path: wt_dir,
        branch: branch.clone(),
        repo_root: repo.to_path_buf(),
        tracking,
        symlinks: symlink_report,
        setup_recommendation,
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
    remove_with_keep_branch(repo, branch, force, false)
}

/// Remove a worktree, optionally preserving its local branch.
pub fn remove_with_keep_branch(
    repo: &RepoRoot,
    branch: Option<&BranchName>,
    force: bool,
    keep_branch: bool,
) -> Result<RemoveResult> {
    let worktrees = git::list_worktrees(repo)?;

    // Resolve which branch to remove.
    let target_branch = match branch {
        Some(b) => b.clone(),
        None => resolve_branch_from_cwd(&worktrees)?,
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

    // Snapshot the previous marker before updating it. A repeated keep
    // request may be retrying a branch that was already preserved, and a
    // failed removal must not discard that valid cleanup eligibility.
    let previous_marker = if keep_branch {
        git::preserved_branch_oid(repo, &target_branch)?
    } else {
        None
    };

    // Record preservation before removal so a successful worktree removal
    // cannot lose the branch's later prune eligibility.
    if keep_branch {
        git::mark_preserved_branch(repo, &target_branch)?;
    }

    // Remove worktree first, then optionally delete the branch. A failed
    // worktree removal prevents branch cleanup and preserves the old safety
    // ordering. Restore a prior marker only when it still matches the branch;
    // otherwise clear the newly-created marker instead of retaining invalid
    // lifecycle state.
    if let Err(error) = git::remove_worktree(repo, &removed_path, force) {
        match (
            keep_branch,
            previous_marker
                .filter(|oid| git::branch_oid(repo, &target_branch).as_deref() == Some(oid)),
        ) {
            (true, Some(oid)) => {
                let _ = git::restore_preserved_branch(repo, &target_branch, &oid);
            }
            (true, None) => {
                let _ = git::clear_preserved_branch(repo, &target_branch);
            }
            (false, _) => {}
        }
        return Err(error);
    }
    let (branch_deleted, warning) = if keep_branch {
        (false, None)
    } else {
        // Branch deletion is best-effort: the worktree is already gone, so
        // report a warning while accurately retaining the branch state.
        match git::delete_branch(repo, &target_branch, force) {
            Ok(()) => {
                let warning = git::clear_preserved_branch(repo, &target_branch)
                    .err()
                    .map(|e| {
                        format!(
                            "branch '{target_branch}' deleted but lifecycle marker cleanup failed: {e}"
                        )
                    });
                (true, warning)
            }
            Err(e) => (
                false,
                Some(format!("worktree removed but branch deletion failed: {e}")),
            ),
        }
    };

    Ok(RemoveResult {
        removed_path,
        branch: target_branch,
        repo_root: repo.to_path_buf(),
        branch_deleted,
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
    /// `None` means the branch was preserved after its worktree was removed.
    pub path: Option<std::path::PathBuf>,
    pub status: IntegrationStatus,
    /// Marker OID captured during planning for a preserved branch. Execution
    /// rechecks it so a moved/recreated branch cannot be deleted by an old
    /// preservation request.
    pub preserved_oid: Option<String>,
    /// `git branch -d` can only verify the currently checked-out branch. For
    /// another branch, a commit, or a remote ref, integration was checked
    /// against an explicit target, so deletion must use `-D` after that
    /// check rather than accidentally retaining a valid candidate.
    pub force_branch_delete: bool,
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
    /// `None` means this entry only deleted a preserved branch.
    pub path: Option<std::path::PathBuf>,
    pub worktree_removed: bool,
    pub branch_deleted: bool,
}

/// An entry that was skipped during pruning.
#[derive(Debug)]
pub struct SkippedEntry {
    pub branch: Option<String>,
    pub path: Option<std::path::PathBuf>,
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

/// The revision used for integration checks plus the target identity that
/// prune must protect. Branch refs have a stable local name; commit-only and
/// remote refs additionally protect matching branch tips because their target
/// worktree cannot always be identified by name.
#[derive(Debug, Clone)]
struct IntegrationTarget {
    revision: String,
    protected_branches: HashSet<String>,
    protected_tip: Option<String>,
    force_branch_delete: bool,
}

impl IntegrationTarget {
    fn protects_branch(&self, repo: &RepoRoot, branch: &str) -> bool {
        self.protected_branches.contains(branch)
            || self.protected_tip.as_deref().is_some_and(|tip| {
                git::branch_oid(repo, &BranchName::new(branch)).as_deref() == Some(tip)
            })
    }
}

fn local_branch_revision(repo: &RepoRoot, revision: &str) -> Option<String> {
    if let Some(branch) = revision.strip_prefix("refs/heads/") {
        let branch_name = BranchName::new(branch);
        return git::branch_exists(repo, &branch_name).then_some(branch.to_string());
    }

    if revision == "HEAD" {
        return git::current_branch(repo);
    }

    if revision.starts_with("refs/") {
        return None;
    }

    let branch_name = BranchName::new(revision);
    git::branch_exists(repo, &branch_name).then_some(revision.to_string())
}

fn resolve_integration_target(
    repo: &RepoRoot,
    mainline_override: Option<&str>,
) -> Result<IntegrationTarget> {
    let requested = match mainline_override {
        Some(revision) => revision.to_string(),
        None => git::resolve_mainline(repo)?,
    };

    if let Some(branch) = local_branch_revision(repo, &requested) {
        let force_branch_delete = git::current_branch(repo).as_deref() != Some(branch.as_str());
        return Ok(IntegrationTarget {
            revision: branch.clone(),
            protected_branches: [branch].into_iter().collect(),
            protected_tip: None,
            force_branch_delete,
        });
    }

    if let Some(remote) = git::remote_branch_revision(repo, &requested) {
        let protected_branches = git::local_branch_for_remote(repo, &remote)
            .into_iter()
            .collect();
        return Ok(IntegrationTarget {
            revision: remote,
            protected_branches,
            protected_tip: Some(git::resolve_commit(repo, &requested)?),
            force_branch_delete: true,
        });
    }

    let commit = git::resolve_commit(repo, &requested)
        .map_err(|_| AppError::usage(format!("mainline branch '{requested}' does not exist")))?;

    Ok(IntegrationTarget {
        revision: commit.clone(),
        protected_branches: HashSet::new(),
        protected_tip: Some(commit),
        force_branch_delete: true,
    })
}

/// Dry-run: scan worktrees and preserved local branches without removing anything.
pub fn prune_dry_run(repo: &RepoRoot, mainline_override: Option<&str>) -> Result<PruneDryRun> {
    let target = resolve_integration_target(repo, mainline_override)?;
    let mainline = target.revision.clone();

    let worktrees = git::list_worktrees(repo)?;
    let worktree_branches: HashSet<String> = worktrees
        .iter()
        .filter_map(|wt| wt.branch.clone())
        .collect();
    let mut stale_marker_branches = HashSet::new();
    let preserved_branches = git::list_preserved_branches(repo)?;
    let mut valid_preserved_branches = Vec::new();

    // Validate every marker before adding worktree entries. This also blocks a
    // moved preserved branch that was recreated/reattached as a worktree from
    // falling through the ordinary worktree cleanup path.
    for preserved in preserved_branches {
        let branch = BranchName::new(&preserved.name);
        if git::branch_oid(repo, &branch).as_deref() != Some(preserved.oid.as_str()) {
            git::clear_preserved_branch(repo, &branch)?;
            stale_marker_branches.insert(preserved.name);
        } else {
            valid_preserved_branches.push(preserved);
        }
    }

    let mut entries = Vec::new();

    for wt in &worktrees {
        if wt.is_main
            || wt
                .branch
                .as_deref()
                .is_some_and(|branch| target.protects_branch(repo, branch))
            || wt
                .branch
                .as_deref()
                .is_some_and(|branch| stale_marker_branches.contains(branch))
        {
            continue;
        }

        let status = match &wt.branch {
            Some(branch) => classify_integration(repo, branch, &mainline),
            None => IntegrationStatus::NoBranch,
        };

        entries.push(WorktreePruneEntry {
            branch: wt.branch.clone(),
            path: Some(wt.path.clone()),
            status,
            preserved_oid: None,
            force_branch_delete: target.force_branch_delete,
        });
    }

    // A worktree removed with --keep-branch no longer appears in `git
    // worktree list`. Its private lifecycle marker lets a later prune delete
    // the preserved branch once its commits reach the selected target,
    // without treating every unrelated local branch as pruneable. The marker
    // OID is checked before planning; a moved, deleted, or recreated branch
    // invalidates the old preservation request and its marker is cleared.
    for preserved in valid_preserved_branches {
        if worktree_branches.contains(&preserved.name)
            || target.protects_branch(repo, &preserved.name)
        {
            continue;
        }

        entries.push(WorktreePruneEntry {
            status: classify_integration(repo, &preserved.name, &mainline),
            branch: Some(preserved.name),
            path: None,
            preserved_oid: Some(preserved.oid),
            force_branch_delete: target.force_branch_delete,
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

/// Try to remove an integrated worktree and delete its branch.
///
/// When the branch was integrated via rebase (patch-id match), Git's own
/// ancestry check (`git branch -d`) would refuse deletion because the
/// original commits are not ancestors of mainline. We auto-escalate to `-D`
/// in that case since the cherry check already confirmed integration.
fn prune_integrated_entry(
    repo: &RepoRoot,
    entry: WorktreePruneEntry,
    force: bool,
    acc: &mut PruneAccumulator,
) {
    let Some(branch_name) = entry.branch.clone() else {
        acc.skipped.push(SkippedEntry {
            branch: None,
            path: entry.path,
            reason: "no_branch".to_string(),
        });
        return;
    };

    // Recheck the marker immediately before deleting. `prune_execute` plans
    // first, so a branch moved between planning and execution must not be
    // authorized by an old preservation marker.
    match entry.preserved_oid.as_deref() {
        Some(expected_oid)
            if git::branch_oid(repo, &BranchName::new(&branch_name)).as_deref()
                != Some(expected_oid) =>
        {
            git::clear_preserved_branch(repo, &BranchName::new(&branch_name))
                .err()
                .into_iter()
                .for_each(|e| {
                    acc.warnings.push(format!(
                        "stale lifecycle marker for '{branch_name}' could not be cleared: {e}"
                    ));
                });
            acc.skipped.push(SkippedEntry {
                branch: Some(branch_name),
                path: entry.path,
                reason: "stale_marker".to_string(),
            });
            return;
        }
        _ => {}
    }

    let force_branch = force
        || entry.force_branch_delete
        || matches!(
            &entry.status,
            IntegrationStatus::Integrated(IntegrationMethod::Rebase)
        );

    let worktree_removed = match entry.path.as_ref() {
        Some(path) => match git::remove_worktree(repo, path, force) {
            Ok(()) => true,
            Err(e) => {
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
        },
        None => false,
    };

    let bn = BranchName::new(&branch_name);
    let branch_deleted = match git::delete_branch(repo, &bn, force_branch) {
        Ok(()) => {
            if let Err(e) = git::clear_preserved_branch(repo, &bn) {
                acc.warnings.push(format!(
                    "branch '{branch_name}' deleted but lifecycle marker cleanup failed: {e}"
                ));
            }
            true
        }
        Err(e) => {
            let subject = if worktree_removed {
                "worktree removed"
            } else {
                "no worktree removed"
            };
            acc.warnings.push(format!(
                "{subject} but branch deletion failed for '{branch_name}': {e}"
            ));
            false
        }
    };

    acc.pruned.push(PrunedEntry {
        branch: branch_name,
        path: entry.path,
        worktree_removed,
        branch_deleted,
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

/// Result of a successful `merge` operation.
pub struct MergeResult {
    pub branch: BranchName,
    pub mainline: String,
    pub repo_root: PathBuf,
    pub cleaned_up: bool,
    /// Path of the removed worktree (only set when `cleaned_up` is true).
    pub removed_path: Option<PathBuf>,
    pub pushed: bool,
    /// Non-fatal warnings (e.g. cleanup or push failure after merge).
    pub warnings: Vec<String>,
}

/// Merge a worktree's branch into the mainline.
///
/// 1. Resolve the target branch (argument, cwd inference, or picker)
/// 2. Refuse if it is the main worktree
/// 3. Resolve the target branch (`--into` or detected mainline)
/// 4. Run `git merge --no-ff <branch>` from the main worktree
/// 5. On conflict: abort the merge and return an error
/// 6. On success: optionally remove the worktree+branch, optionally push
pub fn merge(
    repo: &RepoRoot,
    branch: Option<&BranchName>,
    into: Option<&str>,
    push: bool,
    no_cleanup: bool,
) -> Result<MergeResult> {
    let worktrees = git::list_worktrees(repo)?;

    // Resolve which branch to merge (same cwd-inference as `remove`).
    let target_branch = match branch {
        Some(b) => b.clone(),
        None => resolve_branch_from_cwd(&worktrees)?,
    };

    // Find the worktree entry.
    let wt = worktrees
        .iter()
        .find(|wt| wt.branch.as_deref() == Some(target_branch.as_str()))
        .ok_or_else(|| {
            AppError::usage(format!("no worktree found for branch '{target_branch}'"))
        })?;

    // Never merge the main worktree into itself.
    if wt.is_main {
        return Err(AppError::invariant(
            "refusing to merge the main worktree".to_string(),
        ));
    }

    // Resolve target and verify the main worktree is checked out to it.
    let mainline = into
        .map(str::to_string)
        .map(Ok)
        .unwrap_or_else(|| git::resolve_mainline(repo))?;
    if target_branch.as_str() == mainline {
        return Err(AppError::invariant(
            "refusing to merge a branch into itself".to_string(),
        ));
    }

    let main_wt_branch = worktrees
        .iter()
        .find(|w| w.is_main)
        .and_then(|w| w.branch.as_deref());
    let checkout_hint = match into {
        Some(_) => "checkout target branch first",
        None => "checkout mainline first",
    };
    if main_wt_branch != Some(&mainline) {
        return Err(AppError::invariant(format!(
            "main worktree is on '{}', expected '{mainline}' — {checkout_hint}",
            main_wt_branch.unwrap_or("(detached)")
        )));
    }

    // Attempt the merge from the main worktree's context.
    if let Err(e) = git::merge_no_ff(repo, target_branch.as_str()) {
        // Abort to restore the main worktree to a clean state.
        git::merge_abort(repo);
        return Err(AppError::conflict(format!(
            "merge conflicts with '{}' — merge aborted; use `git merge` directly to handle conflicts\n{e}",
            target_branch
        )));
    }

    let mut warnings = Vec::new();

    // Cleanup: remove worktree and branch (default behaviour).
    // Downgraded to a warning because the merge has already been committed;
    // a hard error would hide the successful merge from the caller.
    let (cleaned_up, removed_path) = if no_cleanup {
        (false, None)
    } else {
        match remove(repo, Some(&target_branch), false) {
            Ok(result) => {
                if let Some(w) = result.warning {
                    warnings.push(w);
                }
                (true, Some(result.removed_path))
            }
            Err(e) => {
                warnings.push(format!("merge succeeded but cleanup failed: {e}"));
                (false, None)
            }
        }
    };

    // Push mainline to origin if requested.
    let pushed = if push {
        match git::push(repo, &mainline) {
            Ok(()) => true,
            Err(e) => {
                warnings.push(format!("merge succeeded but push failed: {e}"));
                false
            }
        }
    } else {
        false
    };

    Ok(MergeResult {
        branch: target_branch,
        mainline,
        repo_root: repo.to_path_buf(),
        cleaned_up,
        removed_path,
        pushed,
        warnings,
    })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn wt(path: &str, branch: Option<&str>, is_main: bool) -> Worktree {
        Worktree {
            path: PathBuf::from(path),
            branch: branch.map(str::to_string),
            commit: "deadbee".to_string(),
            is_main,
        }
    }

    #[test]
    fn diff_dirty_dry_run_supports_detached_worktree() {
        let worktree = wt("/repo/.worktrees/detached--abc12345", None, false);
        let result = diff_dirty(&worktree, DirtyDiffMode::Dirty, None, true)
            .expect("dry-run dirty diff should resolve");

        assert_eq!(result.label, "detached at deadbee");
        assert_eq!(
            result.command,
            [
                "git",
                "-C",
                "/repo/.worktrees/detached--abc12345",
                "difftool",
                "--dir-diff",
                "HEAD"
            ]
        );
    }

    #[test]
    fn worktree_for_cwd_prefers_longest_prefix() {
        let worktrees = vec![
            wt("/repo", Some("main"), true),
            wt(
                "/repo/.worktrees/feature-a--aaaa1111",
                Some("feature/a"),
                false,
            ),
        ];
        let cwd = PathBuf::from("/repo/.worktrees/feature-a--aaaa1111");

        let selected = worktree_for_cwd(&worktrees, &cwd).expect("expected matching worktree");
        assert_eq!(selected.branch.as_deref(), Some("feature/a"));
    }

    #[test]
    fn worktree_for_cwd_matches_nested_directories() {
        let worktrees = vec![
            wt("/repo", Some("main"), true),
            wt(
                "/repo/.worktrees/feature-b--bbbb2222",
                Some("feature/b"),
                false,
            ),
        ];
        let cwd = PathBuf::from("/repo/.worktrees/feature-b--bbbb2222/src/module");

        let selected = worktree_for_cwd(&worktrees, &cwd).expect("expected matching worktree");
        assert_eq!(selected.branch.as_deref(), Some("feature/b"));
    }

    #[test]
    fn worktree_for_cwd_returns_none_outside_repo() {
        let worktrees = vec![
            wt("/repo", Some("main"), true),
            wt(
                "/repo/.worktrees/feature-c--cccc3333",
                Some("feature/c"),
                false,
            ),
        ];
        let cwd = PathBuf::from("/tmp");

        let selected = worktree_for_cwd(&worktrees, &cwd);
        assert!(selected.is_none());
    }
}
