use std::collections::HashSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::domain::{BranchName, RepoRoot, Worktree};
use crate::error::{AppError, Result};
use crate::git;
use crate::operation_state::{self, MergePhase, MergeProgress};
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
    // Removal shares the merge lifecycle lock so it cannot race a managed
    // merge's journal, Git operation, or post-commit cleanup. A live journal
    // also owns its source and destination until continue/abort finishes.
    let _lifecycle_lock = acquire_merge_lifecycle_lock(repo)?;
    let active_merge = active_merge_operation(repo)?;
    let worktrees = if active_merge.is_some() {
        git::list_worktrees_readonly(repo)?
    } else {
        git::list_worktrees(repo)?
    };

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
    if let Some(active_merge) = active_merge
        .as_ref()
        .filter(|active| active.protects(target_branch.as_str()))
    {
        return Err(AppError::conflict(format!(
            "refusing to remove branch '{}' while managed merge '{}' -> '{}' is active; use `wt merge --continue` or `wt merge --abort` first",
            target_branch, active_merge.source, active_merge.destination
        )));
    }

    let removed_path = wt.path.clone();
    // Keep both halves of the destructive plan. A path and branch name are
    // not stable identities: Git may replace the registration while a hook
    // or another process is running.
    let planned_worktree = wt.clone();
    let planned_identity = git::capture_worktree_identity(repo, wt).map_err(|error| {
        AppError::conflict(format!(
            "stale remove worktree metadata for branch '{}' at {}: {error}",
            target_branch,
            wt.path.display()
        ))
    })?;
    let planned_branch_oid = git::branch_oid(repo, &target_branch).ok_or_else(|| {
        AppError::conflict(format!(
            "branch '{}' disappeared while planning removal",
            target_branch
        ))
    })?;

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

    // Revalidate the exact registration and branch tip immediately before
    // removal. The branch tip is a CAS token for the later deletion too;
    // neither path replacement nor a moved branch may be authorized by this
    // earlier plan.
    validate_worktree_cleanup_plan(
        repo,
        &planned_worktree,
        &planned_identity,
        &target_branch,
        &planned_branch_oid,
        "remove",
    )?;

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
        match git::delete_branch_at_cas(branch_context, &target_branch, force, &planned_branch_oid)
        {
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
    /// Branch tip captured with the prune plan. Execution uses it as a CAS
    /// token before both worktree removal and branch deletion.
    planned_branch_oid: Option<String>,
    /// Exact worktree registration captured with the prune plan. It is kept
    /// private so the established prune output protocol cannot change.
    planned_worktree: Option<Worktree>,
    planned_identity: Option<git::WorktreeIdentity>,
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
    force_branch_delete: bool,
}

impl IntegrationTarget {
    fn protects_branch(&self, _repo: &RepoRoot, branch: &str) -> bool {
        self.protected_branches.contains(branch)
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
    readonly: bool,
) -> Result<IntegrationTarget> {
    let requested = match mainline_override {
        Some(revision) => revision.to_string(),
        None if readonly => git::resolve_mainline_readonly(repo)?,
        None => git::resolve_mainline(repo)?,
    };

    if let Some(branch) = local_branch_revision(repo, &requested) {
        let force_branch_delete = git::current_branch(repo).as_deref() != Some(branch.as_str());
        return Ok(IntegrationTarget {
            revision: branch.clone(),
            protected_branches: [branch].into_iter().collect(),
            force_branch_delete,
        });
    }

    if let Some(remote) = git::remote_branch_revision(repo, &requested) {
        let commit = git::resolve_commit(repo, &requested)?;
        let mut protected_branches = git::local_branches_at_oid(repo, &commit)?;
        protected_branches.extend(git::local_branch_for_remote(repo, &remote));
        return Ok(IntegrationTarget {
            revision: remote,
            protected_branches,
            force_branch_delete: true,
        });
    }

    let commit = git::resolve_commit(repo, &requested)
        .map_err(|_| AppError::usage(format!("mainline branch '{requested}' does not exist")))?;

    Ok(IntegrationTarget {
        revision: commit.clone(),
        protected_branches: git::local_branches_at_oid(repo, &commit)?,
        force_branch_delete: true,
    })
}

/// Dry-run: scan worktrees and preserved local branches without removing anything.
pub fn prune_dry_run(repo: &RepoRoot, mainline_override: Option<&str>) -> Result<PruneDryRun> {
    // Dry-run is strictly read-only. In particular, do not acquire the
    // mutating lifecycle lock: acquiring it would create `.git/wt-core/` and
    // its lock file in an otherwise untouched repository. A concurrent
    // mutator is represented by the journal/lock-aware status checks, while
    // execution takes the lock again before acting on this plan.
    let active_merge = active_merge_operation(repo)?;
    prune_dry_run_inner(repo, mainline_override, false, active_merge.as_ref())
}

fn prune_dry_run_inner(
    repo: &RepoRoot,
    mainline_override: Option<&str>,
    repair_stale_markers: bool,
    active_merge: Option<&ActiveMergeOperation>,
) -> Result<PruneDryRun> {
    let target = resolve_integration_target(repo, mainline_override, !repair_stale_markers)?;
    let mainline = target.revision.clone();

    let worktrees = git::list_worktrees_readonly(repo)?;
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
            // Dry-run is read-only. Execution repairs stale markers only when
            // the branch is not owned by a live managed merge journal.
            repair_stale_markers
                .then(|| active_merge.is_some_and(|active| active.protects(&preserved.name)))
                .filter(|protected| !protected)
                .map(|_| git::clear_preserved_branch(repo, &branch))
                .transpose()?;
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
            || wt
                .branch
                .as_deref()
                .is_some_and(|branch| active_merge.is_some_and(|active| active.protects(branch)))
        {
            continue;
        }

        let status = match &wt.branch {
            Some(branch) => classify_integration(repo, branch, &mainline),
            None => IntegrationStatus::NoBranch,
        };

        let planned_branch_oid = wt
            .branch
            .as_deref()
            .and_then(|branch| git::branch_oid(repo, &BranchName::new(branch)));
        let planned_identity = if wt.branch.is_some() {
            Some(git::capture_worktree_identity(repo, wt)?)
        } else {
            None
        };
        entries.push(WorktreePruneEntry {
            branch: wt.branch.clone(),
            path: Some(wt.path.clone()),
            status,
            preserved_oid: None,
            planned_branch_oid,
            planned_worktree: Some(wt.clone()),
            planned_identity,
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
            || active_merge.is_some_and(|active| active.protects(&preserved.name))
        {
            continue;
        }

        entries.push(WorktreePruneEntry {
            status: classify_integration(repo, &preserved.name, &mainline),
            branch: Some(preserved.name),
            path: None,
            planned_branch_oid: Some(preserved.oid.clone()),
            preserved_oid: Some(preserved.oid),
            planned_worktree: None,
            planned_identity: None,
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

    let Some(planned_branch_oid) = entry.planned_branch_oid.as_deref() else {
        acc.skipped.push(SkippedEntry {
            branch: Some(branch_name),
            path: entry.path,
            reason: "branch_missing".to_string(),
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

    // Validate the planned admin identity and branch OID immediately before
    // removing a registered worktree. Branch-only marker entries still get
    // the same tip CAS below.
    match (
        entry.planned_worktree.as_ref(),
        entry.planned_identity.as_ref(),
    ) {
        (Some(planned_worktree), Some(planned_identity)) => {
            if let Err(error) = validate_worktree_cleanup_plan(
                repo,
                planned_worktree,
                planned_identity,
                &BranchName::new(&branch_name),
                planned_branch_oid,
                "prune",
            ) {
                let reason = match error.message.contains("identity") {
                    true => "identity_changed",
                    false => "branch_changed",
                };
                acc.skipped.push(SkippedEntry {
                    branch: Some(branch_name),
                    path: entry.path,
                    reason: reason.to_string(),
                });
                return;
            }
        }
        _ => {
            if let Err(error) = git::verify_branch_ref_cas(
                repo.as_ref(),
                &BranchName::new(&branch_name),
                planned_branch_oid,
            ) {
                acc.warnings.push(format!(
                    "branch '{branch_name}' changed before prune: {error}"
                ));
                acc.skipped.push(SkippedEntry {
                    branch: Some(branch_name),
                    path: entry.path,
                    reason: "branch_changed".to_string(),
                });
                return;
            }
        }
    }

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
    if let Err(error) = git::verify_branch_ref_cas(repo.as_ref(), &bn, planned_branch_oid) {
        acc.warnings.push(format!(
            "worktree {} but branch '{branch_name}' changed before deletion; refusing to delete the newer branch: {error}",
            if worktree_removed { "removed" } else { "was not removed" }
        ));
        match worktree_removed {
            true => acc.pruned.push(PrunedEntry {
                branch: branch_name,
                path: entry.path,
                worktree_removed,
                branch_deleted: false,
            }),
            false => acc.skipped.push(SkippedEntry {
                branch: Some(branch_name),
                path: entry.path,
                reason: "branch_changed".to_string(),
            }),
        }
        return;
    }
    let branch_deleted =
        match git::delete_branch_at_cas(repo.as_ref(), &bn, force_branch, planned_branch_oid) {
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
    // Hold the repository lifecycle lock across planning, stale-marker repair,
    // worktree removal, and branch deletion. A managed merge owns its source
    // and destination until recovery completes, so fail closed rather than
    // allowing prune to invalidate that journal.
    let _lifecycle_lock = acquire_merge_lifecycle_lock(repo)?;
    if let Some(active_merge) = active_merge_operation(repo)? {
        return Err(AppError::conflict(format!(
            "refusing prune while managed merge '{}' -> '{}' is active; use `wt merge --continue` or `wt merge --abort` first",
            active_merge.source, active_merge.destination
        )));
    }
    let dry_run = prune_dry_run_inner(repo, mainline_override, true, None)?;
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceHistory {
    NotMerged,
    AlreadyMerged,
    MergedThenReverted,
}

/// The repository-local record that binds a conflicted Git merge to its
/// original source and destination worktree registrations.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct MergeOperationState {
    schema: u32,
    /// Stable identity for one merge lifecycle. Journal mutations must never
    /// cross this boundary, even if a stale process wakes after recovery.
    #[serde(default)]
    operation_id: String,
    /// Monotonic journal generation used as the compare-and-swap version.
    #[serde(default)]
    generation: u64,
    phase: MergePhase,
    source: String,
    destination: String,
    source_path: PathBuf,
    destination_path: PathBuf,
    source_identity: git::WorktreeIdentity,
    destination_identity: git::WorktreeIdentity,
    source_head: String,
    destination_head: String,
    merge_head: Option<String>,
    completed_destination_head: Option<String>,
    upstream: Option<String>,
    ahead: Option<u32>,
    behind: Option<u32>,
    topology: MergeTopology,
    source_history: SourceHistory,
    source_was_merged: bool,
    source_was_reverted: bool,
    reverted_commit: Option<String>,
    push: bool,
    cleanup: bool,
    keep_branch: bool,
    #[serde(flatten)]
    progress: MergeProgress,
}

const MERGE_OPERATION_SCHEMA: u32 = 1;

/// Read-only status of the managed merge operation.
#[derive(Debug, Clone)]
pub struct MergeOperationReport {
    pub state: String,
    pub source: Option<String>,
    pub destination: Option<String>,
    pub source_path: Option<PathBuf>,
    pub destination_path: Option<PathBuf>,
    pub unresolved_paths: Vec<String>,
    pub push: bool,
    pub cleanup: bool,
    pub keep_branch: bool,
    pub worktree_removed: bool,
    pub branch_deleted: bool,
    pub push_done: bool,
    pub pending_actions: Vec<String>,
    pub recovery: Option<String>,
    pub state_path: PathBuf,
}

enum MergeOperationFile {
    Missing,
    Valid(Box<MergeOperationState>),
    Corrupt { path: PathBuf, reason: String },
}

/// Branches owned by a live managed merge journal. Destructive commands must
/// not remove either side while the merge lifecycle can still recover them.
#[derive(Debug, Clone)]
struct ActiveMergeOperation {
    source: String,
    destination: String,
}

impl ActiveMergeOperation {
    fn protects(&self, branch: &str) -> bool {
        self.source == branch || self.destination == branch
    }
}

fn active_merge_operation(repo: &RepoRoot) -> Result<Option<ActiveMergeOperation>> {
    match merge_operation_file(repo)? {
        MergeOperationFile::Missing => Ok(None),
        MergeOperationFile::Valid(state) => Ok(Some(ActiveMergeOperation {
            source: state.source.clone(),
            destination: state.destination.clone(),
        })),
        MergeOperationFile::Corrupt { path, reason } => Err(AppError::conflict(format!(
            "managed merge state is corrupt ({reason}); preserve '{}', recover the destination manually, and do not run destructive cleanup until it is repaired",
            path.display()
        ))),
    }
}

fn merge_operation_file(repo: &RepoRoot) -> Result<MergeOperationFile> {
    let path = git::merge_operation_path(repo)?;
    match fs::symlink_metadata(&path) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            // A status query must not initialize the journal namespace just to
            // discover that there is no operation to report.
            return Ok(MergeOperationFile::Missing);
        }
        Err(error) => {
            return Ok(MergeOperationFile::Corrupt {
                path,
                reason: format!("cannot inspect state file: {error}"),
            });
        }
    }
    if let Some(error) = path
        .parent()
        .and_then(|parent| operation_state::ensure_private_directory(parent).err())
    {
        return Ok(MergeOperationFile::Corrupt {
            path,
            reason: error.message,
        });
    }
    if let Err(error) = operation_state::ensure_private_file(&path) {
        return Ok(MergeOperationFile::Corrupt {
            path,
            reason: error.message,
        });
    }
    let contents = match fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(error) => {
            return Ok(MergeOperationFile::Corrupt {
                path,
                reason: format!("cannot read state file: {error}"),
            });
        }
    };

    match serde_json::from_str::<MergeOperationState>(&contents) {
        Ok(state) if state.schema == MERGE_OPERATION_SCHEMA && !state.operation_id.is_empty() => {
            Ok(MergeOperationFile::Valid(Box::new(state)))
        }
        Ok(state) if state.schema == MERGE_OPERATION_SCHEMA => Ok(MergeOperationFile::Corrupt {
            path,
            reason: "managed merge state has no operation identity".to_string(),
        }),
        Ok(state) => Ok(MergeOperationFile::Corrupt {
            path,
            reason: format!("unsupported state schema {}", state.schema),
        }),
        Err(error) => Ok(MergeOperationFile::Corrupt {
            path,
            reason: format!("invalid JSON: {error}"),
        }),
    }
}

fn operation_report_base(state: &MergeOperationState, state_path: PathBuf) -> MergeOperationReport {
    MergeOperationReport {
        state: state.phase.as_str().to_string(),
        source: Some(state.source.clone()),
        destination: Some(state.destination.clone()),
        source_path: Some(state.source_path.clone()),
        destination_path: Some(state.destination_path.clone()),
        unresolved_paths: Vec::new(),
        push: state.push,
        cleanup: state.cleanup,
        keep_branch: state.keep_branch,
        worktree_removed: state.progress.worktree_removed,
        branch_deleted: state.progress.branch_deleted,
        push_done: state.progress.push_done,
        pending_actions: Vec::new(),
        recovery: None,
        state_path,
    }
}

fn no_operation_report(state_path: PathBuf) -> MergeOperationReport {
    MergeOperationReport {
        state: "none".to_string(),
        source: None,
        destination: None,
        source_path: None,
        destination_path: None,
        unresolved_paths: Vec::new(),
        push: false,
        cleanup: false,
        keep_branch: false,
        worktree_removed: false,
        branch_deleted: false,
        push_done: false,
        pending_actions: Vec::new(),
        recovery: None,
        state_path,
    }
}

fn corrupt_operation_report(path: PathBuf, reason: String) -> MergeOperationReport {
    MergeOperationReport {
        state: "corrupt".to_string(),
        source: None,
        destination: None,
        source_path: None,
        destination_path: None,
        unresolved_paths: Vec::new(),
        push: false,
        cleanup: false,
        keep_branch: false,
        worktree_removed: false,
        branch_deleted: false,
        push_done: false,
        pending_actions: Vec::new(),
        recovery: Some(format!(
            "managed merge state is corrupt ({reason}); preserve '{}', inspect the destination, and repair or remove the state only after recovery",
            path.display()
        )),
        state_path: path,
    }
}

fn pending_operation_actions(
    state: &MergeOperationState,
    phase: MergePhase,
    unresolved: &[String],
) -> Vec<String> {
    let mut actions = Vec::new();
    let merge_action = match (
        matches!(phase, MergePhase::Starting | MergePhase::Conflicted),
        unresolved.is_empty(),
    ) {
        (true, true) => Some("run `wt merge --continue` to create the merge commit"),
        (true, false) => Some("resolve the listed paths, then run `wt merge --continue`"),
        _ => None,
    };
    if let Some(action) = merge_action {
        actions.push(action.to_string());
    }
    let cleanup_action = match (
        state.cleanup,
        state.progress.worktree_removed,
        state.progress.branch_deleted,
    ) {
        (true, true, false) => Some("remove the source branch"),
        (true, false, _) => Some("remove the source worktree and branch"),
        _ => None,
    };
    if let Some(action) = cleanup_action {
        actions.push(action.to_string());
    }
    if state.push && !state.progress.push_done {
        actions.push(format!("push {} to origin", state.destination));
    }
    actions
}

fn stale_operation_report(
    state: &MergeOperationState,
    state_path: PathBuf,
    reason: impl Into<String>,
) -> MergeOperationReport {
    let mut report = operation_report_base(state, state_path.clone());
    report.state = "stale".to_string();
    report.recovery = Some(format!(
        "managed merge state is stale ({}) and was not changed; inspect '{}' and the destination, then finish or abort the Git operation manually before removing the state",
        reason.into(),
        state_path.display()
    ));
    report
}

fn interrupted_operation_report(
    state: &MergeOperationState,
    state_path: PathBuf,
) -> MergeOperationReport {
    let mut report = operation_report_base(state, state_path.clone());
    report.state = "interrupted".to_string();
    report.recovery = Some(format!(
        "Git no longer has the merge recorded for this state; inspect '{}' and the destination before manually completing or restoring it, then clear the state",
        state_path.display()
    ));
    report
}

fn operation_preflight(state: &MergeOperationState) -> MergePreflight {
    MergePreflight {
        source: state.source.clone(),
        source_path: state.source_path.clone(),
        source_identity: state.source_identity.clone(),
        destination: state.destination.clone(),
        destination_path: state.destination_path.clone(),
        destination_identity: state.destination_identity.clone(),
        upstream: state.upstream.clone(),
        ahead: state.ahead,
        behind: state.behind,
        topology: state.topology,
        source_history: state.source_history,
        source_was_merged: state.source_was_merged,
        source_was_reverted: state.source_was_reverted,
        reverted_commit: state.reverted_commit.clone(),
        allowed: true,
        refusal: None,
    }
}

#[cfg(unix)]
fn sync_operation_parent(parent: &Path) -> Result<()> {
    fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| {
            AppError::git(format!(
                "cannot flush managed merge state directory: {error}"
            ))
        })
}

#[cfg(not(unix))]
fn sync_operation_parent(_parent: &Path) -> Result<()> {
    Ok(())
}

fn verify_operation_generation(repo: &RepoRoot, expected: &MergeOperationState) -> Result<()> {
    match merge_operation_file(repo)? {
        MergeOperationFile::Valid(current)
            if current.operation_id == expected.operation_id
                && current.generation == expected.generation =>
        {
            Ok(())
        }
        MergeOperationFile::Valid(current) => Err(AppError::conflict(format!(
            "managed merge state belongs to operation '{}' generation {}; refusing to overwrite operation '{}' generation {}",
            current.operation_id,
            current.generation,
            expected.operation_id,
            expected.generation
        ))),
        MergeOperationFile::Missing => Err(AppError::conflict(
            "managed merge state disappeared before its compare-and-swap update; preserving the Git operation"
                .to_string(),
        )),
        MergeOperationFile::Corrupt { path, reason } => Err(AppError::conflict(format!(
            "managed merge state at '{}' changed before its compare-and-swap update ({reason}); preserving it",
            path.display()
        ))),
    }
}

fn write_operation_state_inner(
    repo: &RepoRoot,
    state: &MergeOperationState,
    create_only: bool,
    expected: Option<&MergeOperationState>,
) -> Result<()> {
    let path = git::merge_operation_path(repo)?;
    let parent = path.parent().ok_or_else(|| {
        AppError::invariant(format!(
            "managed merge state path '{}' has no parent",
            path.display()
        ))
    })?;
    operation_state::ensure_private_directory(parent)?;
    if let Some(expected) = expected {
        verify_operation_generation(repo, expected)?;
    }
    let bytes = serde_json::to_vec_pretty(state).map_err(|error| {
        AppError::invariant(format!("cannot encode managed merge state: {error}"))
    })?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let temp = path.with_file_name(format!(
        ".merge-operation.json.tmp.{}.{}",
        std::process::id(),
        nonce
    ));
    let mut options = fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&temp)
        .map_err(|error| AppError::git(format!("cannot write managed merge state: {error}")))?;
    file.write_all(&bytes)
        .and_then(|_| file.sync_all())
        .map_err(|error| AppError::git(format!("cannot flush managed merge state: {error}")))?;
    drop(file);
    match create_only {
        true => match fs::hard_link(&temp, &path) {
            Ok(()) => {
                fs::remove_file(&temp).map_err(|error| {
                    AppError::git(format!(
                        "cannot remove temporary managed merge state: {error}"
                    ))
                })?;
            }
            Err(error) => {
                let _ = fs::remove_file(&temp);
                return Err(AppError::conflict(format!(
                    "cannot claim managed merge state: {error}"
                )));
            }
        },
        false => {
            // Recheck immediately before replacement. The lifecycle lock is
            // the synchronization boundary, while this generation check makes
            // stale callers fail closed even if they bypass that boundary.
            if let Some(expected) = expected {
                verify_operation_generation(repo, expected).map_err(|error| {
                    let _ = fs::remove_file(&temp);
                    error
                })?;
            }
            if let Err(error) = operation_state::ensure_private_file(&path) {
                let _ = fs::remove_file(&temp);
                return Err(error);
            }
            if let Err(error) = operation_state::replace_existing(&temp, &path) {
                let _ = fs::remove_file(&temp);
                return Err(error);
            }
        }
    }
    sync_operation_parent(parent)?;
    Ok(())
}

fn write_operation_state(repo: &RepoRoot, state: &mut MergeOperationState) -> Result<()> {
    let next_generation = state.generation.checked_add(1).ok_or_else(|| {
        AppError::invariant("managed merge state generation exhausted".to_string())
    })?;
    let mut next = state.clone();
    next.generation = next_generation;
    write_operation_state_inner(repo, &next, false, Some(state))?;
    state.generation = next_generation;
    Ok(())
}

fn create_operation_state(repo: &RepoRoot, state: &MergeOperationState) -> Result<()> {
    write_operation_state_inner(repo, state, true, None)
}

/// Remove the operation record only after atomically detaching and verifying
/// the exact bytes that were expected. If another writer installs a
/// replacement between the read and rename, that replacement stays at the
/// canonical path and the expected record is retained as recovery evidence.
fn clear_operation_state(repo: &RepoRoot, expected: &MergeOperationState) -> Result<()> {
    let path = git::merge_operation_path(repo)?;
    let parent = path.parent().ok_or_else(|| {
        AppError::invariant(format!(
            "managed merge state path '{}' has no parent",
            path.display()
        ))
    })?;
    let tombstone = path.with_file_name(format!(
        ".merge-operation.json.removing.{}.{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default()
    ));

    match fs::symlink_metadata(&path) {
        Ok(_) => operation_state::ensure_private_file(&path)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(AppError::git(format!(
                "cannot inspect managed merge state '{}': {error}",
                path.display()
            )))
        }
    }
    fs::rename(&path, &tombstone).map_err(|error| {
        AppError::conflict(format!(
            "cannot detach managed merge state before clearing it: {error}"
        ))
    })?;

    let contents = fs::read_to_string(&tombstone);
    let current = contents
        .as_deref()
        .ok()
        .and_then(|value| serde_json::from_str::<MergeOperationState>(value).ok());
    let matches_generation = current.as_ref().is_some_and(|current| {
        current.operation_id == expected.operation_id && current.generation == expected.generation
    });
    if !matches_generation {
        match path.exists() {
            false => {
                let _ = fs::rename(&tombstone, &path);
            }
            true => {
                // The canonical path is another operation. Remove only the
                // detached record that we already proved was not its journal.
                let _ = fs::remove_file(&tombstone);
            }
        }
        return Err(AppError::conflict(
            "managed merge state changed while the operation was running; refusing to clear the replacement".to_string(),
        ));
    }

    fs::remove_file(&tombstone)
        .map_err(|error| AppError::git(format!("cannot clear managed merge state: {error}")))?;
    sync_operation_parent(parent)?;
    Ok(())
}

fn state_worktree<'a>(
    worktrees: &'a [Worktree],
    path: &Path,
    branch: &str,
) -> Option<&'a Worktree> {
    worktrees
        .iter()
        .find(|wt| wt.path == path && wt.branch.as_deref() == Some(branch))
}

fn validate_operation_worktree(
    repo: &RepoRoot,
    worktree: &Worktree,
    role: &str,
    expected: &git::WorktreeIdentity,
) -> Result<()> {
    let actual = validate_merge_worktree(repo, worktree, role)?;
    if actual != *expected {
        return Err(identity_changed(
            role,
            worktree.branch.as_deref().unwrap_or("(detached)"),
            &worktree.path,
            expected,
            &actual,
        ));
    }
    Ok(())
}

/// Reconcile effects that may have completed after the process lost the
/// opportunity to persist its progress record. This only recognizes effects
/// whose observable result is exactly the recorded intent; it never deletes a
/// replacement worktree/branch and never treats a diverged remote as pushed.
fn reconcile_operation_progress(repo: &RepoRoot, state: &mut MergeOperationState) -> Result<bool> {
    let mut changed = false;

    if state.phase != MergePhase::Committed {
        let source_head = state
            .merge_head
            .as_deref()
            .unwrap_or(state.source_head.as_str());
        let Some(expected) = git::merge_result_head(
            &state.destination_path,
            &state.destination_head,
            source_head,
        )?
        else {
            return Ok(false);
        };
        state.phase = MergePhase::Committed;
        state.completed_destination_head = Some(expected);
        changed = true;
    }

    let Some(expected) = state.completed_destination_head.clone() else {
        return Ok(changed);
    };
    if git::head_commit(&state.destination_path)? != expected {
        // The merge result exists, but the destination has advanced. Keep the
        // journal available for truthful stale-state reporting and refuse all
        // destructive follow-up actions.
        return Ok(changed);
    }

    if state.cleanup {
        changed |= reconcile_cleanup_progress(repo, state)?;
    }
    if state.push && !state.progress.push_done {
        changed |= reconcile_push_progress(repo, state, &expected);
    }

    Ok(changed)
}

fn reconcile_cleanup_progress(repo: &RepoRoot, state: &mut MergeOperationState) -> Result<bool> {
    let worktrees = git::list_worktrees_readonly(repo)?;
    let source_present = state_worktree(&worktrees, &state.source_path, &state.source).is_some();
    let branch = BranchName::new(&state.source);
    match (
        state.progress.worktree_removed,
        state.progress.branch_deleted,
    ) {
        (false, _) if !source_present => match git::branch_oid(repo, &branch) {
            Some(head) if head == state.source_head => {
                state.progress.worktree_removed = true;
                Ok(true)
            }
            None => {
                // Both cleanup effects are already visible. The requested
                // outcome is idempotently complete.
                state.progress.worktree_removed = true;
                state.progress.branch_deleted = true;
                Ok(true)
            }
            Some(_) => Ok(false),
        },
        (true, false) if git::branch_oid(repo, &branch).is_none() => {
            state.progress.branch_deleted = true;
            Ok(true)
        }
        _ => Ok(false),
    }
}

fn reconcile_push_progress(
    repo: &RepoRoot,
    state: &mut MergeOperationState,
    expected: &str,
) -> bool {
    match git::remote_branch_head(repo, &state.destination) {
        Ok(Some(remote_head)) if remote_head == expected => {
            state.progress.push_done = true;
            true
        }
        _ => false,
    }
}

fn finish_committed_report(
    mut report: MergeOperationReport,
    state: &MergeOperationState,
) -> MergeOperationReport {
    report.pending_actions = pending_operation_actions(state, MergePhase::Committed, &[]);
    report.state = if report.pending_actions.is_empty() {
        "complete".to_string()
    } else {
        "committed".to_string()
    };
    report
}

fn inspect_committed_operation(
    repo: &RepoRoot,
    state: &MergeOperationState,
    state_path: PathBuf,
    worktrees: &[Worktree],
    report: MergeOperationReport,
) -> Result<MergeOperationReport> {
    let Some(expected_head) = &state.completed_destination_head else {
        return Ok(stale_operation_report(
            state,
            state_path,
            "completed destination HEAD is missing",
        ));
    };
    let actual_head = git::head_commit(&state.destination_path)?;
    if &actual_head != expected_head {
        return Ok(stale_operation_report(
            state,
            state_path,
            "destination HEAD changed after the merge commit",
        ));
    }
    if state.progress.worktree_removed {
        return Ok(finish_committed_report(report, state));
    }

    let Some(source) = state_worktree(worktrees, &state.source_path, &state.source) else {
        return Ok(stale_operation_report(
            state,
            state_path,
            "source worktree disappeared before cleanup completed",
        ));
    };
    if let Err(error) = validate_operation_worktree(repo, source, "source", &state.source_identity)
    {
        return Ok(stale_operation_report(state, state_path, error.message));
    }
    if git::head_commit(&state.source_path)? != state.source_head {
        return Ok(stale_operation_report(
            state,
            state_path,
            "source branch HEAD changed after the merge began",
        ));
    }
    Ok(finish_committed_report(report, state))
}

fn inspect_operation_state(
    repo: &RepoRoot,
    state: &MergeOperationState,
    state_path: PathBuf,
) -> Result<MergeOperationReport> {
    let mut reconciled = state.clone();
    let _ = reconcile_operation_progress(repo, &mut reconciled)?;
    let state = &reconciled;
    let mut report = operation_report_base(state, state_path.clone());
    let worktrees = git::list_worktrees_readonly(repo)?;
    let destination = match state_worktree(&worktrees, &state.destination_path, &state.destination)
    {
        Some(destination) => destination,
        None => {
            return Ok(stale_operation_report(
                state,
                state_path,
                "destination path or branch no longer matches",
            ))
        }
    };
    if let Err(error) = validate_operation_worktree(
        repo,
        destination,
        "destination",
        &state.destination_identity,
    ) {
        return Ok(stale_operation_report(state, state_path, error.message));
    }

    if state.phase == MergePhase::Committed {
        return inspect_committed_operation(repo, state, state_path, &worktrees, report);
    }

    let Some(source) = state_worktree(&worktrees, &state.source_path, &state.source) else {
        return Ok(stale_operation_report(
            state,
            state_path,
            "source path or branch no longer matches",
        ));
    };
    if let Err(error) = validate_operation_worktree(repo, source, "source", &state.source_identity)
    {
        return Ok(stale_operation_report(state, state_path, error.message));
    }
    if git::head_commit(&state.source_path)? != state.source_head {
        return Ok(stale_operation_report(
            state,
            state_path,
            "source branch HEAD changed while the merge was paused",
        ));
    }
    if git::head_commit(&state.destination_path)? != state.destination_head {
        return Ok(stale_operation_report(
            state,
            state_path,
            "destination HEAD changed while the merge was paused",
        ));
    }

    match git::operation_state(&state.destination_path)? {
        Some("merge") => {
            if git::merge_head(&state.destination_path)? != state.merge_head {
                return Ok(stale_operation_report(
                    state,
                    state_path,
                    "MERGE_HEAD does not match the recorded source",
                ));
            }
            report.unresolved_paths = git::unmerged_paths(&state.destination_path)?;
            report.state = if report.unresolved_paths.is_empty() {
                "ready".to_string()
            } else {
                "conflicted".to_string()
            };
            report.pending_actions =
                pending_operation_actions(state, state.phase, &report.unresolved_paths);
            Ok(report)
        }
        Some(other) => Ok(stale_operation_report(
            state,
            state_path,
            format!("destination has a different {other} operation in progress"),
        )),
        None => Ok(interrupted_operation_report(state, state_path)),
    }
}

/// Return the current managed merge status without pruning worktree metadata.
pub fn merge_operation_status(repo: &RepoRoot) -> Result<MergeOperationReport> {
    let path = git::merge_operation_path(repo)?;
    let lock_path = git::merge_operation_lock_path(repo)?;
    let busy = operation_state::lock_is_held(&lock_path)?;
    let mut report = match merge_operation_file(repo)? {
        MergeOperationFile::Missing => no_operation_report(path),
        MergeOperationFile::Corrupt { path, reason } => corrupt_operation_report(path, reason),
        MergeOperationFile::Valid(state) => inspect_operation_state(repo, &state, path)?,
    };
    if busy {
        report.state = "busy".to_string();
        report.recovery = Some(
            "a managed merge lifecycle owner is live; wait for it to finish before mutating the repository"
                .to_string(),
        );
    }
    Ok(report)
}

/// Return a report for an operation file after a merge failure, if one exists.
pub fn merge_operation_report_if_present(repo: &RepoRoot) -> Option<MergeOperationReport> {
    merge_operation_status(repo)
        .ok()
        .filter(|report| report.state != "none")
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
    pub branch_deleted: bool,
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

/// Validate a destructive remove/prune plan immediately before its worktree
/// operation. The registration identity protects the path; the branch OID
/// protects the ref incarnation and is reused for deletion CAS.
fn validate_worktree_cleanup_plan(
    repo: &RepoRoot,
    planned_worktree: &Worktree,
    planned_identity: &git::WorktreeIdentity,
    branch: &BranchName,
    planned_branch_oid: &str,
    role: &str,
) -> Result<()> {
    let actual = git::capture_worktree_identity(repo, planned_worktree).map_err(|error| {
        AppError::conflict(format!(
            "stale {role} worktree metadata for branch '{}' at {}: {error}",
            branch,
            planned_worktree.path.display()
        ))
    })?;
    if actual != *planned_identity {
        return Err(identity_changed(
            role,
            branch.as_str(),
            &planned_worktree.path,
            planned_identity,
            &actual,
        ));
    }
    if let Err(error) = git::verify_branch_ref_cas(repo.as_ref(), branch, planned_branch_oid) {
        return Err(AppError::conflict(format!(
            "branch '{}' changed before {role}; refusing to use the newer branch tip: {error}",
            branch
        )));
    }
    Ok(())
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
            "content merge conflicts with '{}' — resolve the listed paths, then run `wt merge --continue`; use `wt merge --abort` to restore the destination\n{error}",
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

fn merge_state_from_preflight(
    preflight: &MergePreflight,
    source_head: String,
    destination_head: String,
    push: bool,
    no_cleanup: bool,
    operation_id: &str,
) -> MergeOperationState {
    MergeOperationState {
        schema: MERGE_OPERATION_SCHEMA,
        operation_id: operation_id.to_string(),
        generation: 0,
        phase: MergePhase::Starting,
        source: preflight.source.clone(),
        destination: preflight.destination.clone(),
        source_path: preflight.source_path.clone(),
        destination_path: preflight.destination_path.clone(),
        source_identity: preflight.source_identity.clone(),
        destination_identity: preflight.destination_identity.clone(),
        source_head,
        destination_head,
        merge_head: None,
        completed_destination_head: None,
        upstream: preflight.upstream.clone(),
        ahead: preflight.ahead,
        behind: preflight.behind,
        topology: preflight.topology,
        source_history: preflight.source_history,
        source_was_merged: preflight.source_was_merged,
        source_was_reverted: preflight.source_was_reverted,
        reverted_commit: preflight.reverted_commit.clone(),
        push,
        cleanup: !no_cleanup,
        keep_branch: no_cleanup,
        // These flags describe completed cleanup actions, not the policy. A
        // no-cleanup operation still needs its source identity revalidated
        // while it is paused.
        progress: MergeProgress {
            push_done: !push,
            ..MergeProgress::default()
        },
    }
}

/// Acquire the one owner allowed to mutate a repository's merge lifecycle.
///
/// Read-only status intentionally does not acquire this lock.
pub(crate) fn acquire_merge_lifecycle_lock(
    repo: &RepoRoot,
) -> Result<operation_state::MergeLifecycleLock> {
    let path = git::merge_operation_lock_path(repo)?;
    operation_state::acquire_merge_lifecycle_lock(&path)
}

fn load_valid_merge_state(repo: &RepoRoot) -> Result<(PathBuf, MergeOperationState)> {
    let path = git::merge_operation_path(repo)?;
    match merge_operation_file(repo)? {
        MergeOperationFile::Valid(state) => Ok((path, *state)),
        MergeOperationFile::Missing => Err(AppError::conflict(
            "no managed merge operation is recorded; start or inspect the merge manually, then use `wt merge --status`"
                .to_string(),
        )),
        MergeOperationFile::Corrupt { path, reason } => Err(AppError::conflict(format!(
            "managed merge state is corrupt ({reason}); preserve '{}', recover the destination manually, and do not retry until it is repaired",
            path.display()
        ))),
    }
}

fn operation_recovery_error(report: &MergeOperationReport) -> AppError {
    AppError::conflict(
        report
            .recovery
            .clone()
            .unwrap_or_else(|| format!("managed merge operation is in '{}' state", report.state)),
    )
}

fn validate_committed_destination(
    repo: &RepoRoot,
    state: &MergeOperationState,
) -> Result<Worktree> {
    let expected = state.completed_destination_head.as_deref().ok_or_else(|| {
        AppError::conflict(
            "managed merge has no recorded destination merge-result HEAD; state was preserved"
                .to_string(),
        )
    })?;
    let worktrees = git::list_worktrees_readonly(repo)?;
    let destination = state_worktree(&worktrees, &state.destination_path, &state.destination)
        .ok_or_else(|| {
            AppError::conflict(
                "destination worktree changed before cleanup or push; managed state was preserved"
                    .to_string(),
            )
        })?;
    validate_operation_worktree(
        repo,
        destination,
        "destination",
        &state.destination_identity,
    )?;
    let actual = git::head_commit(&state.destination_path)?;
    if actual != expected {
        return Err(AppError::conflict(
            "destination HEAD changed after the merge result; refusing cleanup or push and preserving managed state"
                .to_string(),
        ));
    }
    Ok(destination.clone())
}

fn validate_committed_source(repo: &RepoRoot, state: &MergeOperationState) -> Result<Worktree> {
    let worktrees = git::list_worktrees_readonly(repo)?;
    let source = state_worktree(&worktrees, &state.source_path, &state.source).ok_or_else(|| {
        AppError::conflict(
            "source worktree changed before cleanup; refusing to remove a replacement and preserving managed state"
                .to_string(),
        )
    })?;
    validate_operation_worktree(repo, source, "source", &state.source_identity)?;
    if git::head_commit(&source.path)? != state.source_head
        || git::branch_oid(repo, &BranchName::new(&state.source)).as_deref()
            != Some(state.source_head.as_str())
    {
        return Err(AppError::conflict(
            "source HEAD changed after the merge; refusing cleanup and preserving managed state"
                .to_string(),
        ));
    }
    Ok(source.clone())
}

fn cleanup_merge_operation(
    repo: &RepoRoot,
    state: &mut MergeOperationState,
    warnings: &mut Vec<String>,
) -> Result<Option<PathBuf>> {
    if !state.cleanup {
        return Ok(None);
    }

    let removed_path = if state.progress.worktree_removed {
        Some(state.source_path.clone())
    } else {
        // Validate both captured heads immediately before the destructive
        // action. The branch check closes the race between worktree removal
        // and branch deletion.
        let _ = validate_committed_destination(repo, state)?;
        let source = validate_committed_source(repo, state)?;
        let path = source.path.clone();
        git::remove_worktree(repo, &path, false)?;
        state.progress.worktree_removed = true;
        // Do not continue to branch deletion until the journal records the
        // completed worktree removal durably.
        write_operation_state(repo, state)?;
        Some(path)
    };

    if state.progress.branch_deleted {
        return Ok(removed_path);
    }

    let destination = validate_committed_destination(repo, state)?;
    let branch = BranchName::new(&state.source);
    match git::branch_oid(repo, &branch) {
        Some(head) if head != state.source_head => {
            return Err(AppError::conflict(
                "source branch HEAD changed before branch cleanup; refusing to delete the newer branch"
                    .to_string(),
            ));
        }
        _ => {}
    }
    match git::delete_branch_at_cas(&destination.path, &branch, false, &state.source_head) {
        Ok(()) => {
            state.progress.branch_deleted = true;
            let marker_warning = git::clear_preserved_branch(repo, &branch)
                .err()
                .map(|error| {
                    format!(
                        "branch '{}' deleted but lifecycle marker cleanup failed: {error}",
                        state.source
                    )
                });
            if let Some(warning) = marker_warning {
                warnings.push(warning);
            }
            write_operation_state(repo, state)?;
        }
        Err(_error) if git::branch_oid(repo, &branch).is_none() => {
            // A concurrent non-force deletion has already completed the same
            // requested action. Record it and reconcile on the next retry.
            state.progress.branch_deleted = true;
            write_operation_state(repo, state)?;
        }
        Err(error) => {
            warnings.push(format!(
                "worktree removed but branch deletion failed: {error}"
            ));
        }
    }

    Ok(removed_path)
}

fn finish_push(
    repo: &RepoRoot,
    state: &mut MergeOperationState,
    destination: &Worktree,
    warnings: &mut Vec<String>,
) -> bool {
    match git::push(&destination.path, &state.destination) {
        Ok(()) => {
            state.progress.push_done = true;
            match write_operation_state(repo, state) {
                Ok(()) => true,
                Err(error) => {
                    warnings.push(format!(
                        "push completed but managed progress could not be recorded; retry `wt merge --continue`: {error}"
                    ));
                    false
                }
            }
        }
        Err(error) => {
            warnings.push(format!("merge succeeded but push failed: {error}"));
            true
        }
    }
}

fn clear_completed_operation(
    repo: &RepoRoot,
    state: &MergeOperationState,
    warnings: &mut Vec<String>,
) {
    let cleanup_complete =
        !state.cleanup || (state.progress.worktree_removed && state.progress.branch_deleted);
    let complete = cleanup_complete && (!state.push || state.progress.push_done);
    if !complete {
        return;
    }
    match clear_operation_state(repo, state) {
        Ok(()) => {}
        Err(error) => warnings.push(format!(
            "merge completed but managed state could not be cleared: {error}"
        )),
    }
}

fn finish_committed_operation(
    repo: &RepoRoot,
    state: &mut MergeOperationState,
    warnings: &mut Vec<String>,
) -> Option<PathBuf> {
    let _destination = match validate_committed_destination(repo, state) {
        Ok(destination) => destination,
        Err(error) => {
            warnings.push(partial_success_warning(
                "cleanup and push",
                "completed",
                &error,
            ));
            return state
                .progress
                .worktree_removed
                .then(|| state.source_path.clone());
        }
    };

    let removed_path = match cleanup_merge_operation(repo, state, warnings) {
        Ok(path) => path,
        Err(error) => {
            warnings.push(partial_success_warning("cleanup", "completed", &error));
            return state
                .progress
                .worktree_removed
                .then(|| state.source_path.clone());
        }
    };

    let push_recorded = match (state.push, state.progress.push_done) {
        (true, false) => match validate_committed_destination(repo, state) {
            Ok(destination) => finish_push(repo, state, &destination, warnings),
            Err(error) => {
                warnings.push(partial_success_warning(
                    "destination push",
                    "pushed",
                    &error,
                ));
                true
            }
        },
        _ => true,
    };
    if !push_recorded {
        return removed_path;
    }

    clear_completed_operation(repo, state, warnings);
    removed_path
}

fn merge_result_from_operation(
    repo: &RepoRoot,
    state: &MergeOperationState,
    preflight: MergePreflight,
    removed_path: Option<PathBuf>,
    warnings: Vec<String>,
) -> MergeResult {
    MergeResult {
        branch: BranchName::new(&state.source),
        mainline: state.destination.clone(),
        destination_path: state.destination_path.clone(),
        repo_root: repo.to_path_buf(),
        cleaned_up: state.cleanup
            && state.progress.worktree_removed
            && state.progress.branch_deleted,
        removed_path,
        branch_deleted: state.progress.branch_deleted,
        pushed: state.push && state.progress.push_done,
        preflight,
        warnings,
    }
}

fn continue_merge_commit(
    repo: &RepoRoot,
    state: &mut MergeOperationState,
    preflight: &MergePreflight,
    report: &MergeOperationReport,
) -> Result<()> {
    if !matches!(report.state.as_str(), "ready") {
        return Err(AppError::conflict(format!(
            "cannot continue while unresolved paths remain: {}; resolve them and run `wt merge --continue`",
            report.unresolved_paths.join(", ")
        )));
    }
    validate_preflight_worktrees(repo, preflight, "source", "destination")?;
    if git::merge_head(&state.destination_path)? != state.merge_head {
        return Err(AppError::conflict(
            "destination MERGE_HEAD changed before continuation; managed state was preserved for recovery".to_string(),
        ));
    }
    if git::head_commit(&state.destination_path)? != state.destination_head
        || git::branch_oid(repo, &BranchName::new(&state.destination)).as_deref()
            != Some(state.destination_head.as_str())
    {
        return Err(AppError::conflict(
            "destination HEAD changed before continuation; managed state was preserved for recovery"
                .to_string(),
        ));
    }

    let source_head = state.merge_head.clone().ok_or_else(|| {
        AppError::invariant("managed merge has no recorded MERGE_HEAD".to_string())
    })?;

    // Reserve the destination ref and detach only this worktree's HEAD. Git's
    // normal continue path still runs all configured hooks, but it cannot
    // attach the merge commit to a ref that another writer advances while the
    // hooks run. The final update-ref is an old-value CAS after the lock is
    // released, covering writers that do not cooperate with the lock.
    let _destination_ref_lock = git::acquire_branch_ref_lock(
        &state.destination_path,
        &BranchName::new(&state.destination),
        &state.destination_head,
    )?;
    git::detach_head(&state.destination_path, &state.destination_head)?;
    if let Err(error) = git::merge_continue(&state.destination_path) {
        let _ = git::restore_head(
            &state.destination_path,
            &BranchName::new(&state.destination),
        );
        return Err(AppError {
            code: error.code,
            message: format!(
                "managed merge continuation failed; state was preserved — resolve hook or Git errors and retry `wt merge --continue`\n{error}"
            ),
        });
    }
    let merge_head_commit = git::head_commit(&state.destination_path)?;
    let expected = git::merge_result_head(
        &state.destination_path,
        &state.destination_head,
        &source_head,
    )?
    .filter(|expected| expected == &merge_head_commit)
    .ok_or_else(|| {
        AppError::conflict(
            "managed merge continuation produced an unexpected HEAD; destination ref was not updated and managed state was preserved"
                .to_string(),
        )
    })?;
    drop(_destination_ref_lock);
    if let Err(error) = git::update_branch_ref_cas(
        &state.destination_path,
        &BranchName::new(&state.destination),
        &expected,
        &state.destination_head,
    ) {
        return Err(AppError::conflict(format!(
            "destination HEAD changed during continuation; merge result was not installed and managed state was preserved: {error}"
        )));
    }
    git::restore_head(
        &state.destination_path,
        &BranchName::new(&state.destination),
    )?;

    let expected = git::merge_result_head(
        &state.destination_path,
        &state.destination_head,
        &source_head,
    )?
    .ok_or_else(|| {
        AppError::conflict(
            "managed merge committed, but its expected merge-result HEAD could not be identified; state was preserved"
                .to_string(),
        )
    })?;
    state.phase = MergePhase::Committed;
    state.completed_destination_head = Some(expected);
    write_operation_state(repo, state)
}

/// Continue a managed merge after its conflicts have been resolved.
pub fn merge_continue(repo: &RepoRoot) -> Result<MergeResult> {
    let _lifecycle_lock = acquire_merge_lifecycle_lock(repo)?;
    let (_, mut state) = load_valid_merge_state(repo)?;
    if reconcile_operation_progress(repo, &mut state)? {
        // A prior action or the commit itself may have completed before its
        // progress write. Persist reconciliation before taking another action.
        write_operation_state(repo, &mut state)?;
    }
    let report = merge_operation_status(repo)?;
    if matches!(report.state.as_str(), "stale" | "interrupted" | "corrupt") {
        return Err(operation_recovery_error(&report));
    }
    if report.state == "none" {
        return Err(AppError::conflict(
            "no managed merge operation is recorded; nothing to continue".to_string(),
        ));
    }

    let preflight = operation_preflight(&state);
    if state.phase != MergePhase::Committed {
        continue_merge_commit(repo, &mut state, &preflight, &report)?;
    }

    let mut warnings = Vec::new();
    let removed_path = finish_committed_operation(repo, &mut state, &mut warnings);

    Ok(merge_result_from_operation(
        repo,
        &state,
        preflight,
        removed_path,
        warnings,
    ))
}

/// Abort a managed merge and clear only the matching operation record.
pub fn merge_abort_operation(repo: &RepoRoot) -> Result<MergeOperationReport> {
    let _lifecycle_lock = acquire_merge_lifecycle_lock(repo)?;
    let (_, state) = load_valid_merge_state(repo)?;
    let report = merge_operation_status(repo)?;
    if matches!(report.state.as_str(), "stale" | "interrupted" | "corrupt") {
        return Err(operation_recovery_error(&report));
    }
    if state.phase == MergePhase::Committed {
        return Err(AppError::conflict(
            "the managed merge is already committed; it cannot be aborted — finish pending push or cleanup actions instead".to_string(),
        ));
    }
    if report.state == "none" {
        return Err(AppError::conflict(
            "no managed merge operation is recorded; nothing to abort".to_string(),
        ));
    }
    validate_preflight_worktrees(repo, &operation_preflight(&state), "source", "destination")?;
    if git::merge_head(&state.destination_path)? != state.merge_head {
        return Err(AppError::conflict(
            "destination MERGE_HEAD no longer matches the managed operation; state was preserved for manual recovery".to_string(),
        ));
    }

    git::merge_abort_checked(&state.destination_path).map_err(|error| AppError {
        code: error.code,
        message: format!("managed merge abort failed; state was preserved\n{error}"),
    })?;
    if git::merge_in_progress(&state.destination_path)
        || git::head_commit(&state.destination_path)? != state.destination_head
    {
        return Err(AppError::conflict(
            "Git did not restore the original destination; managed state was preserved for recovery".to_string(),
        ));
    }
    validate_preflight_worktrees(repo, &operation_preflight(&state), "source", "destination")?;
    clear_operation_state(repo, &state)?;
    let mut aborted = report;
    aborted.state = "aborted".to_string();
    aborted.unresolved_paths.clear();
    aborted.pending_actions.clear();
    aborted.recovery = None;
    Ok(aborted)
}

fn record_conflicted_merge_state(
    repo: &RepoRoot,
    destination_path: &Path,
    operation: &mut MergeOperationState,
) -> Result<bool> {
    operation.phase = MergePhase::Conflicted;
    operation.merge_head = git::merge_head(destination_path)?;
    let Some(_) = operation.merge_head else {
        return Ok(false);
    };
    write_operation_state(repo, operation)?;
    Ok(true)
}

fn handle_merge_failure(
    repo: &RepoRoot,
    destination_path: &Path,
    target_branch: &BranchName,
    operation: &mut MergeOperationState,
    error: AppError,
) -> MergeFailure {
    let failure = classify_merge_failure(destination_path, target_branch, error);
    if failure.kind != MergeFailureKind::ContentConflict {
        abort_created_merge(destination_path);
        let _ = clear_operation_state(repo, operation);
        return failure;
    }

    match record_conflicted_merge_state(repo, destination_path, operation) {
        Ok(true) => failure,
        Ok(false) => {
            abort_created_merge(destination_path);
            let _ = clear_operation_state(repo, operation);
            failure
        }
        Err(state_error) => {
            abort_created_merge(destination_path);
            MergeFailure {
                kind: MergeFailureKind::GitFailure,
                error: AppError::conflict(format!(
                    "content merge conflict could not be recorded safely; merge was aborted: {state_error}"
                )),
            }
        }
    }
}

/// Run a merge using an already collected preflight.
pub(crate) fn merge_with_preflight(
    repo: &RepoRoot,
    preflight: MergePreflight,
    push: bool,
    no_cleanup: bool,
    lifecycle_lock: &operation_state::MergeLifecycleLock,
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
    let source_head = match git::head_commit(&preflight.source_path) {
        Ok(head) => head,
        Err(error) => {
            return Err(MergeFailure {
                kind: MergeFailureKind::GitFailure,
                error,
            });
        }
    };
    let destination_head = match git::head_commit(&destination_path) {
        Ok(head) => head,
        Err(error) => {
            return Err(MergeFailure {
                kind: MergeFailureKind::GitFailure,
                error,
            });
        }
    };
    let mut operation = merge_state_from_preflight(
        &preflight,
        source_head,
        destination_head,
        push,
        no_cleanup,
        lifecycle_lock.operation_id(),
    );

    // Never overwrite an operation that may require recovery. This also
    // keeps a stale or corrupt record from being silently detached from the
    // Git merge it describes.
    match merge_operation_file(repo) {
        Ok(MergeOperationFile::Missing) => {}
        Ok(MergeOperationFile::Valid(_)) | Ok(MergeOperationFile::Corrupt { .. }) => {
            return Err(MergeFailure {
                kind: MergeFailureKind::GitFailure,
                error: AppError::conflict(
                    "a managed merge operation already exists; inspect it with `wt merge --status` and recover it before starting another merge".to_string(),
                ),
            });
        }
        Err(error) => {
            return Err(MergeFailure {
                kind: MergeFailureKind::GitFailure,
                error,
            });
        }
    }
    if let Err(error) = create_operation_state(repo, &operation) {
        return Err(MergeFailure {
            kind: MergeFailureKind::GitFailure,
            error,
        });
    }

    // Attempt the merge from the selected destination worktree's context.
    if let Err(error) = git::merge_no_ff(&destination_path, target_branch.as_str()) {
        return Err(handle_merge_failure(
            repo,
            &destination_path,
            &target_branch,
            &mut operation,
            error,
        ));
    }

    // The merge commit is the durable point of no return. Record its exact
    // two-parent result before attempting cleanup or push; if this write
    // fails, leave the starting journal in place so the next status/continue
    // can identify and reconcile the completed commit.
    let expected = git::merge_result_head(&destination_path, &operation.destination_head, &operation.source_head)
        .map_err(|error| MergeFailure {
            kind: MergeFailureKind::GitFailure,
            error,
        })?
        .ok_or_else(|| MergeFailure {
            kind: MergeFailureKind::GitFailure,
            error: AppError::conflict(
                "merge committed, but the expected merge-result HEAD could not be identified; managed state was preserved"
                    .to_string(),
            ),
        })?;
    operation.phase = MergePhase::Committed;
    operation.completed_destination_head = Some(expected);
    if let Err(error) = write_operation_state(repo, &mut operation) {
        return Err(MergeFailure {
            kind: MergeFailureKind::GitFailure,
            error: AppError::git(format!(
                "merge committed but durable managed state could not be recorded; retry `wt merge --continue` without changing the source or destination: {error}"
            )),
        });
    }

    let mut warnings = Vec::new();
    let removed_path = finish_committed_operation(repo, &mut operation, &mut warnings);
    Ok(merge_result_from_operation(
        repo,
        &operation,
        preflight,
        removed_path,
        warnings,
    ))
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
