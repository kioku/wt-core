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
    remove_with_branch_context(repo, branch, force, repo.as_ref(), false)
}

/// Remove a worktree, optionally preserving its local branch.
pub fn remove_with_keep_branch(
    repo: &RepoRoot,
    branch: Option<&BranchName>,
    force: bool,
    keep_branch: bool,
) -> Result<RemoveResult> {
    remove_with_branch_context(repo, branch, force, repo.as_ref(), keep_branch)
}

/// Remove a worktree and evaluate safe branch deletion from `branch_context`.
///
/// Merges into linked destinations need the destination worktree as the
/// context for `git branch -d`; the ordinary `remove` command keeps its
/// existing main-worktree context.
fn remove_with_branch_context(
    repo: &RepoRoot,
    branch: Option<&BranchName>,
    force: bool,
    branch_context: &Path,
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
        match git::delete_branch_at(branch_context, &target_branch, force) {
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

/// Remove the exact source worktree captured by merge preflight.
///
/// Unlike the user-facing remove path, merge cleanup must not prune and then
/// rediscover a worktree by branch name. Requiring both paths and identities
/// prevents cleanup from deleting a replacement repository or branch.
fn remove_exact_merge_source(repo: &RepoRoot, preflight: &MergePreflight) -> Result<RemoveResult> {
    let (source, _) = validate_preflight_worktrees(repo, preflight, "source", "destination")?;
    let removed_path = source.path.clone();
    let target_branch = BranchName::new(&preflight.source);

    git::remove_worktree(repo, &removed_path, false)?;

    // The source was removed above, so validate the destination separately
    // immediately before deleting the merged source branch in its context.
    let destination = validate_preflight_destination(repo, preflight, "destination")?;

    let (branch_deleted, warning) = match git::delete_branch_at(
        &destination.path,
        &target_branch,
        false,
    ) {
        Ok(()) => {
            let warning = git::clear_preserved_branch(repo, &target_branch)
                .err()
                .map(|error| {
                    format!(
                        "branch '{target_branch}' deleted but lifecycle marker cleanup failed: {error}"
                    )
                });
            (true, warning)
        }
        Err(error) => (
            false,
            Some(format!(
                "worktree removed but branch deletion failed: {error}"
            )),
        ),
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

/// How the destination branch relates to its configured upstream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MergeTopology {
    NoUpstream,
    UpstreamUnavailable,
    Synchronized,
    Ahead,
    Behind,
    Diverged,
}

/// History relationship between the source branch and destination history.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceHistory {
    NotMerged,
    AlreadyMerged,
    MergedThenReverted,
}

/// A refusal identified before Git's content merge starts.
#[derive(Debug, Clone, serde::Serialize)]
pub struct MergeRefusal {
    pub kind: String,
    pub reason: String,
    pub message: String,
}

/// Read-only facts and policy decision for a merge.
#[derive(Debug, Clone, serde::Serialize)]
pub struct MergePreflight {
    pub source: String,
    /// Exact source path captured from the owning repository's worktree list.
    /// It is skipped by serde because the established JSON contract only
    /// exposes the destination path.
    #[serde(skip)]
    pub(crate) source_path: PathBuf,
    /// Stable Git registration identity captured before any merge mutation.
    #[serde(skip)]
    pub(crate) source_identity: git::WorktreeIdentity,
    pub destination: String,
    pub destination_path: PathBuf,
    /// Stable Git registration identity captured before any merge mutation.
    #[serde(skip)]
    pub(crate) destination_identity: git::WorktreeIdentity,
    pub upstream: Option<String>,
    pub ahead: Option<u32>,
    pub behind: Option<u32>,
    pub topology: MergeTopology,
    pub source_history: SourceHistory,
    pub source_was_merged: bool,
    pub source_was_reverted: bool,
    pub reverted_commit: Option<String>,
    pub allowed: bool,
    pub refusal: Option<MergeRefusal>,
}

/// Result of a successful `merge` operation.
pub struct MergeResult {
    pub branch: BranchName,
    pub mainline: String,
    /// Path of the worktree where the destination branch was checked out.
    pub destination_path: PathBuf,
    pub repo_root: PathBuf,
    pub cleaned_up: bool,
    /// Path of the removed worktree (only set when `cleaned_up` is true).
    pub removed_path: Option<PathBuf>,
    pub pushed: bool,
    /// Facts gathered before the merge mutation.
    pub preflight: MergePreflight,
    /// Non-fatal warnings (e.g. cleanup or push failure after merge).
    pub warnings: Vec<String>,
}

/// Category for a Git failure while attempting the content merge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeFailureKind {
    ContentConflict,
    GitFailure,
}

/// A merge attempt failure with enough context for human and JSON output.
#[derive(Debug)]
pub struct MergeFailure {
    pub kind: MergeFailureKind,
    pub error: AppError,
}

#[derive(Debug)]
struct MergeDestination {
    branch: String,
    path: PathBuf,
}

/// Resolve the worktree where a merge destination branch is checked out.
///
/// Explicit destinations may be in the main worktree or any linked
/// worktree. With no explicit destination, retain the legacy requirement
/// that the auto-detected mainline is checked out in the main worktree.
fn resolve_merge_destination(
    repo: &RepoRoot,
    worktrees: &[Worktree],
    into: Option<&str>,
) -> Result<MergeDestination> {
    let mainline = into
        .map(str::to_string)
        .map(Ok)
        .unwrap_or_else(|| git::resolve_mainline_readonly(repo))?;

    if into.is_none() {
        let main = worktrees
            .iter()
            .find(|wt| wt.is_main)
            .ok_or_else(|| AppError::invariant("main worktree is not available".to_string()))?;
        return match main.branch.as_deref() {
            Some(branch) if branch == mainline => Ok(MergeDestination {
                branch: mainline,
                path: main.path.clone(),
            }),
            branch => Err(AppError::invariant(format!(
                "main worktree is on '{}', expected '{mainline}' — checkout mainline first",
                branch.unwrap_or("(detached)")
            ))),
        };
    }

    let matches: Vec<&Worktree> = worktrees
        .iter()
        .filter(|wt| wt.branch.as_deref() == Some(mainline.as_str()))
        .collect();

    match matches.as_slice() {
        [] if git::rev_exists(repo, &mainline) => {
            let main_wt_branch = worktrees
                .iter()
                .find(|wt| wt.is_main)
                .and_then(|wt| wt.branch.as_deref());
            Err(AppError::invariant(format!(
                "main worktree is on '{}', expected '{mainline}' — checkout target branch first",
                main_wt_branch.unwrap_or("(detached)")
            )))
        }
        [] => Err(AppError::usage(format!(
            "destination branch '{mainline}' is not checked out in a worktree"
        ))),
        [destination] => Ok(MergeDestination {
            branch: mainline,
            path: destination.path.clone(),
        }),
        _ => {
            let paths = matches
                .iter()
                .map(|wt| wt.path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ");
            Err(AppError::invariant(format!(
                "destination branch '{mainline}' is checked out in multiple worktrees: {paths}"
            )))
        }
    }
}

fn topology(upstream: Option<&str>, ahead: Option<u32>, behind: Option<u32>) -> MergeTopology {
    match (upstream, ahead, behind) {
        (None, None, None) => MergeTopology::NoUpstream,
        (Some(_), None, None) => MergeTopology::UpstreamUnavailable,
        (Some(_), Some(0), Some(0)) => MergeTopology::Synchronized,
        (Some(_), Some(ahead), Some(0)) if ahead > 0 => MergeTopology::Ahead,
        (Some(_), Some(0), Some(behind)) if behind > 0 => MergeTopology::Behind,
        (Some(_), Some(ahead), Some(behind)) if ahead > 0 && behind > 0 => MergeTopology::Diverged,
        _ => MergeTopology::UpstreamUnavailable,
    }
}

fn topology_refusal(
    destination: &str,
    upstream: Option<&str>,
    topology: MergeTopology,
    ahead: Option<u32>,
    behind: Option<u32>,
) -> Option<MergeRefusal> {
    let upstream = upstream?;
    let (ahead, behind) = (ahead.unwrap_or(0), behind.unwrap_or(0));
    match topology {
        MergeTopology::UpstreamUnavailable => Some(MergeRefusal {
            kind: "topology".to_string(),
            reason: "destination_upstream_unavailable".to_string(),
            message: format!(
                "merge preflight refused: destination '{destination}' tracks upstream '{upstream}', but that configured ref is unavailable locally; restore the upstream before merging"
            ),
        }),
        MergeTopology::Diverged => Some(MergeRefusal {
            kind: "topology".to_string(),
            reason: "destination_diverged_from_upstream".to_string(),
            message: format!(
                "merge preflight refused: destination '{destination}' has diverged from upstream '{upstream}' (ahead {ahead}, behind {behind}); reconcile the destination with its upstream before merging"
            ),
        }),
        MergeTopology::Behind => Some(MergeRefusal {
            kind: "topology".to_string(),
            reason: "destination_behind_upstream".to_string(),
            message: format!(
                "merge preflight refused: destination '{destination}' is behind upstream '{upstream}' by {behind} commit{}; update the destination before merging",
                if behind == 1 { "" } else { "s" }
            ),
        }),
        _ => None,
    }
}

/// Validate a worktree record before using it for a merge and capture its
/// stable Git registration identity.
///
/// Normal merges prune stale Git metadata before this check. Keeping the
/// explicit identity validation here also handles locked or otherwise
/// replaced records without falling through to a low-level Git mutation.
fn validate_merge_worktree(
    repo: &RepoRoot,
    wt: &Worktree,
    role: &str,
) -> Result<git::WorktreeIdentity> {
    git::capture_worktree_identity(repo, wt).map_err(|error| {
        let branch = wt.branch.as_deref().unwrap_or("(detached)");
        AppError::conflict(format!(
            "stale {role} worktree metadata for branch '{branch}' at {}: {error}",
            wt.path.display()
        ))
    })
}

fn identity_changed(
    role: &str,
    branch: &str,
    path: &Path,
    expected: &git::WorktreeIdentity,
    actual: &git::WorktreeIdentity,
) -> AppError {
    AppError::conflict(format!(
        "worktree identity changed for {role} branch '{branch}' at {}: preflight {} Git admin directory '{}' is now {} Git admin directory '{}'; refusing to use the replacement",
        path.display(),
        expected.kind(),
        expected.admin_dir().display(),
        actual.kind(),
        actual.admin_dir().display()
    ))
}

/// Re-read both sides of a merge and require the paths, branch metadata, and
/// stable Git registration identities to still be the exact records used by
/// preflight. This is intentionally read-only so cleanup never selects a
/// replacement by branch name alone.
fn validate_preflight_worktrees(
    repo: &RepoRoot,
    preflight: &MergePreflight,
    source_role: &str,
    destination_role: &str,
) -> Result<(Worktree, Worktree)> {
    let worktrees = git::list_worktrees_readonly(repo)?;
    let source = worktrees
        .iter()
        .find(|wt| {
            wt.path == preflight.source_path
                && wt.branch.as_deref() == Some(preflight.source.as_str())
        })
        .ok_or_else(|| {
            AppError::conflict(format!(
                "stale {source_role} worktree metadata for branch '{}' at {}: path or branch no longer matches the preflight record",
                preflight.source,
                preflight.source_path.display()
            ))
        })?;
    let destination = worktrees
        .iter()
        .find(|wt| {
            wt.path == preflight.destination_path
                && wt.branch.as_deref() == Some(preflight.destination.as_str())
        })
        .ok_or_else(|| {
            AppError::conflict(format!(
                "stale {destination_role} worktree metadata for branch '{}' at {}: path or branch no longer matches the preflight record",
                preflight.destination,
                preflight.destination_path.display()
            ))
        })?;

    let source_identity = validate_merge_worktree(repo, source, source_role)?;
    if source_identity != preflight.source_identity {
        return Err(identity_changed(
            source_role,
            &preflight.source,
            &preflight.source_path,
            &preflight.source_identity,
            &source_identity,
        ));
    }
    let destination_identity = validate_merge_worktree(repo, destination, destination_role)?;
    if destination_identity != preflight.destination_identity {
        return Err(identity_changed(
            destination_role,
            &preflight.destination,
            &preflight.destination_path,
            &preflight.destination_identity,
            &destination_identity,
        ));
    }
    Ok((source.clone(), destination.clone()))
}

/// Re-read and validate the exact destination record after source cleanup.
fn validate_preflight_destination(
    repo: &RepoRoot,
    preflight: &MergePreflight,
    role: &str,
) -> Result<Worktree> {
    let worktrees = git::list_worktrees_readonly(repo)?;
    let destination = worktrees
        .iter()
        .find(|wt| {
            wt.path == preflight.destination_path
                && wt.branch.as_deref() == Some(preflight.destination.as_str())
        })
        .ok_or_else(|| {
            AppError::conflict(format!(
                "stale {role} worktree metadata for branch '{}' at {}: path or branch no longer matches the preflight record",
                preflight.destination,
                preflight.destination_path.display()
            ))
        })?;
    let identity = validate_merge_worktree(repo, destination, role)?;
    if identity != preflight.destination_identity {
        return Err(identity_changed(
            role,
            &preflight.destination,
            &preflight.destination_path,
            &preflight.destination_identity,
            &identity,
        ));
    }
    Ok(destination.clone())
}

/// Inspect the merge topology without changing repository state.
///
/// `readonly` is true for `--inspect`. Normal merges deliberately prune
/// stale worktree metadata before selecting either side of the merge.
pub fn merge_preflight(
    repo: &RepoRoot,
    branch: Option<&BranchName>,
    into: Option<&str>,
    readonly: bool,
) -> Result<MergePreflight> {
    let worktrees = if readonly {
        git::list_worktrees_readonly(repo)?
    } else {
        git::list_worktrees(repo)?
    };

    let target_branch = match branch {
        Some(branch) => branch.clone(),
        None => resolve_branch_from_cwd(&worktrees)?,
    };
    let source_wt = worktrees
        .iter()
        .find(|wt| wt.branch.as_deref() == Some(target_branch.as_str()))
        .ok_or_else(|| {
            AppError::usage(format!("no worktree found for branch '{target_branch}'"))
        })?;

    if source_wt.is_main {
        return Err(AppError::invariant(
            "refusing to merge the main worktree".to_string(),
        ));
    }
    let source_identity = validate_merge_worktree(repo, source_wt, "source")?;

    let destination = resolve_merge_destination(repo, &worktrees, into)?;
    let destination_wt = worktrees
        .iter()
        .find(|wt| wt.path == destination.path)
        .ok_or_else(|| {
            AppError::invariant(format!(
                "destination worktree '{}' disappeared during merge preflight",
                destination.path.display()
            ))
        })?;
    let destination_identity = validate_merge_worktree(repo, destination_wt, "destination")?;
    if target_branch.as_str() == destination.branch {
        return Err(AppError::invariant(
            "refusing to merge a branch into itself".to_string(),
        ));
    }

    let upstream = git::branch_upstream(&destination.path, &destination.branch)?;
    let (ahead, behind) = match upstream.as_deref() {
        Some(upstream) => {
            match git::upstream_counts(&destination.path, upstream, &destination.branch)? {
                Some((ahead, behind)) => (Some(ahead), Some(behind)),
                None => (None, None),
            }
        }
        None => (None, None),
    };
    let topology = topology(upstream.as_deref(), ahead, behind);
    let source_was_merged = git::is_ancestor(repo, target_branch.as_str(), &destination.branch);
    let source_patch_equivalent =
        git::patch_equivalent(repo, &destination.branch, target_branch.as_str());
    let reverted_commit = git::reverted_source_commit(
        &destination.path,
        target_branch.as_str(),
        &destination.branch,
    )?;
    let source_was_reverted = reverted_commit.is_some();
    let source_history = if source_was_reverted {
        SourceHistory::MergedThenReverted
    } else if source_was_merged || source_patch_equivalent {
        // Keep the existing `already_merged` output for both ancestry and a
        // proven equivalent tree/patch. Do not report a known squash or rebase
        // integration as `not_merged` merely because commit IDs differ.
        SourceHistory::AlreadyMerged
    } else {
        SourceHistory::NotMerged
    };

    let refusal = match git::operation_state(&destination.path)? {
        Some(state) => Some(MergeRefusal {
            kind: "state".to_string(),
            reason: "destination_operation_in_progress".to_string(),
            message: format!(
                "destination worktree '{}' has an in-progress {state}; finish or abort it before merging",
                destination.path.display()
            ),
        }),
        None => topology_refusal(
            &destination.branch,
            upstream.as_deref(),
            topology,
            ahead,
            behind,
        ),
    };

    Ok(MergePreflight {
        source: target_branch.to_string(),
        source_path: source_wt.path.clone(),
        source_identity,
        destination: destination.branch,
        destination_path: destination.path,
        destination_identity,
        upstream,
        ahead,
        behind,
        topology,
        source_history,
        source_was_merged,
        source_was_reverted,
        reverted_commit,
        allowed: refusal.is_none(),
        refusal,
    })
}

/// Abort the merge marker created by this invocation, if any.
fn abort_created_merge(path: &Path) {
    if git::merge_in_progress(path) {
        git::merge_abort(path);
    }
}

fn classify_merge_failure(
    path: &Path,
    target_branch: &BranchName,
    error: AppError,
) -> MergeFailure {
    let kind = match git::has_unmerged_entries(path) {
        true => MergeFailureKind::ContentConflict,
        false => MergeFailureKind::GitFailure,
    };
    let error = match kind {
        MergeFailureKind::ContentConflict => AppError::conflict(format!(
            "content merge conflicts with '{}' — merge aborted; use `git merge` directly to handle conflicts\n{error}",
            target_branch
        )),
        MergeFailureKind::GitFailure => AppError {
            code: error.code,
            message: format!("merge of '{}' failed and was aborted\n{error}", target_branch),
        },
    };
    MergeFailure { kind, error }
}

fn partial_success_warning(action: &str, replacement_action: &str, error: &AppError) -> String {
    if error.message.contains("worktree identity changed") {
        format!(
            "partial success: merge succeeded; {action} skipped because {error}. The merge commit was preserved; the replacement was not {replacement_action}. Inspect the original worktree and recover manually before retrying."
        )
    } else {
        format!("merge succeeded but {action} failed: {error}")
    }
}

/// Run a merge using an already collected preflight.
pub fn merge_with_preflight(
    repo: &RepoRoot,
    preflight: MergePreflight,
    push: bool,
    no_cleanup: bool,
) -> std::result::Result<MergeResult, MergeFailure> {
    if let Some(refusal) = &preflight.refusal {
        return Err(MergeFailure {
            kind: MergeFailureKind::GitFailure,
            error: AppError::conflict(refusal.message.clone()),
        });
    }

    // Revalidate immediately before the first mutation. Preflight paths are
    // not trusted across the gap between inspection and merge execution.
    if let Err(error) = validate_preflight_worktrees(repo, &preflight, "source", "destination") {
        return Err(MergeFailure {
            kind: MergeFailureKind::GitFailure,
            error,
        });
    }

    let destination_path = preflight.destination_path.clone();
    let target_branch = BranchName::new(&preflight.source);
    let destination_branch = preflight.destination.clone();

    // Attempt the merge from the selected destination worktree's context.
    if let Err(error) = git::merge_no_ff(&destination_path, target_branch.as_str()) {
        // The preflight established that any merge state now belongs to this
        // invocation. Abort it after classifying the failure so a hook or
        // other Git failure is not mislabeled as a content conflict.
        let failure = classify_merge_failure(&destination_path, &target_branch, error);
        abort_created_merge(&destination_path);
        return Err(failure);
    }

    let mut warnings = Vec::new();

    // Cleanup: remove the source worktree and branch (default behaviour).
    // Downgraded to a warning because the merge has already been committed;
    // a hard error would hide the successful merge from the caller.
    let (cleaned_up, removed_path) = if no_cleanup {
        (false, None)
    } else {
        match remove_exact_merge_source(repo, &preflight) {
            Ok(result) => {
                if let Some(warning) = result.warning {
                    warnings.push(warning);
                }
                (true, Some(result.removed_path))
            }
            Err(error) => {
                warnings.push(partial_success_warning("cleanup", "removed", &error));
                (false, None)
            }
        }
    };

    // Push the selected destination branch to origin if requested. The
    // destination may have changed while cleanup ran, so verify it again
    // before allowing a remote mutation.
    let pushed = if push {
        match validate_preflight_destination(repo, &preflight, "destination") {
            Ok(_) => match git::push(&destination_path, &destination_branch) {
                Ok(()) => true,
                Err(error) => {
                    warnings.push(format!("merge succeeded but push failed: {error}"));
                    false
                }
            },
            Err(error) => {
                warnings.push(partial_success_warning(
                    "destination push",
                    "pushed",
                    &error,
                ));
                false
            }
        }
    } else {
        false
    };

    Ok(MergeResult {
        branch: target_branch,
        mainline: destination_branch,
        destination_path,
        repo_root: repo.to_path_buf(),
        cleaned_up,
        removed_path,
        pushed,
        preflight,
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
    fn resolve_merge_destination_accepts_linked_worktree() {
        let repo = RepoRoot(PathBuf::from("/repo"));
        let worktrees = vec![
            wt("/repo", Some("main"), true),
            wt(
                "/repo/.worktrees/release--12345678",
                Some("release/1.0"),
                false,
            ),
        ];

        let destination = resolve_merge_destination(&repo, &worktrees, Some("release/1.0"))
            .expect("linked destination should resolve");

        assert_eq!(destination.branch, "release/1.0");
        assert_eq!(
            destination.path,
            PathBuf::from("/repo/.worktrees/release--12345678")
        );
    }

    #[test]
    fn resolve_merge_destination_rejects_missing_worktree() {
        let repo = RepoRoot(PathBuf::from("/repo"));
        let worktrees = vec![wt("/repo", Some("main"), true)];

        let error = resolve_merge_destination(&repo, &worktrees, Some("release/1.0"))
            .expect_err("missing destination should fail");

        assert!(error
            .message
            .contains("destination branch 'release/1.0' is not checked out"));
    }

    #[test]
    fn resolve_merge_destination_rejects_ambiguous_worktree() {
        let repo = RepoRoot(PathBuf::from("/repo"));
        let worktrees = vec![
            wt("/repo", Some("main"), true),
            wt(
                "/repo/.worktrees/release-a--12345678",
                Some("release/1.0"),
                false,
            ),
            wt(
                "/repo/.worktrees/release-b--87654321",
                Some("release/1.0"),
                false,
            ),
        ];

        let error = resolve_merge_destination(&repo, &worktrees, Some("release/1.0"))
            .expect_err("ambiguous destination should fail");

        assert!(error
            .message
            .contains("destination branch 'release/1.0' is checked out in multiple worktrees"));
        assert!(error.message.contains("release-a--12345678"));
        assert!(error.message.contains("release-b--87654321"));
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
