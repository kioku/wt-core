use std::path::Path;

use serde::Serialize;

use crate::domain::{Worktree, WorktreeStatsStatus};

/// Output format for commands that produce a navigable path (add, go).
///
/// JSON is selected before the legacy path-only mode when both flags are
/// present; this keeps wrapper-added path flags compatible with machine calls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavigationFormat {
    Human,
    Json,
    CdPath,
}

/// Output format for commands that produce status/list output (list, doctor).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusFormat {
    Human,
    Json,
}

/// Output format for the remove command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoveFormat {
    Human,
    Json,
    /// Stable legacy `--print-paths`: removed_path, repo_root, and branch
    /// (exactly three lines). Lifecycle status is exposed by JSON.
    PrintPaths,
}

/// JSON envelope for single-operation responses.
#[derive(Debug, Serialize)]
pub struct JsonResponse {
    pub ok: bool,
    /// Lifecycle event emitted by mutating commands.
    /// `"switch"` — consumer should cd to `cd_path`.
    /// `"reset"` — worktree removed; consumer should cd to `repo_root`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event: Option<String>,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo_root: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worktree_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cd_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub removed_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// Whether the worktree was removed (only set for `remove`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worktree_removed: Option<bool>,
    /// Whether the local branch was deleted (only set for `remove`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch_deleted: Option<bool>,
    /// Whether the branch tracks a remote branch (only set for `add`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tracking: Option<bool>,
    /// Symlinks created during `add` (only set when config exists).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symlinks: Option<Vec<String>>,
}

impl JsonResponse {
    pub fn success(message: impl Into<String>) -> Self {
        Self {
            ok: true,
            event: None,
            message: message.into(),
            repo_root: None,
            worktree_path: None,
            cd_path: None,
            removed_path: None,
            branch: None,
            worktree_removed: None,
            branch_deleted: None,
            tracking: None,
            symlinks: None,
        }
    }

    pub fn with_repo_root(mut self, root: impl Into<String>) -> Self {
        self.repo_root = Some(root.into());
        self
    }

    pub fn with_worktree_path(mut self, path: impl Into<String>) -> Self {
        self.worktree_path = Some(path.into());
        self
    }

    pub fn with_cd_path(mut self, path: impl Into<String>) -> Self {
        self.cd_path = Some(path.into());
        self
    }

    pub fn with_removed_path(mut self, path: impl Into<String>) -> Self {
        self.removed_path = Some(path.into());
        self
    }

    pub fn with_branch(mut self, branch: impl Into<String>) -> Self {
        self.branch = Some(branch.into());
        self
    }

    pub fn with_worktree_removed(mut self, removed: bool) -> Self {
        self.worktree_removed = Some(removed);
        self
    }

    pub fn with_branch_deleted(mut self, deleted: bool) -> Self {
        self.branch_deleted = Some(deleted);
        self
    }

    pub fn with_tracking(mut self, tracking: bool) -> Self {
        self.tracking = Some(tracking);
        self
    }

    pub fn with_symlinks(mut self, symlinks: Vec<String>) -> Self {
        if !symlinks.is_empty() {
            self.symlinks = Some(symlinks);
        }
        self
    }

    pub fn with_event(mut self, event: impl Into<String>) -> Self {
        self.event = Some(event.into());
        self
    }
}

/// JSON envelope for list responses.
#[derive(Debug, Serialize)]
pub struct JsonListResponse {
    pub ok: bool,
    pub worktrees: Vec<JsonWorktreeEntry>,
}

#[derive(Debug, Serialize)]
pub struct JsonWorktreeEntry {
    pub path: String,
    pub branch: Option<String>,
    pub commit: String,
    pub is_main: bool,
    pub is_current: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stats: Option<JsonWorktreeStats>,
}

#[derive(Debug, Serialize)]
pub struct JsonWorktreeStats {
    pub available: bool,
    pub base: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commits_ahead: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commits_behind: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub files_changed: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub insertions: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deletions: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl JsonWorktreeStats {
    fn from_status(status: &WorktreeStatsStatus) -> Self {
        match status {
            WorktreeStatsStatus::Available(stats) => Self {
                available: true,
                base: stats.base.clone(),
                commits_ahead: Some(stats.commits_ahead),
                commits_behind: Some(stats.commits_behind),
                files_changed: Some(stats.files_changed),
                insertions: Some(stats.insertions),
                deletions: Some(stats.deletions),
                reason: None,
            },
            WorktreeStatsStatus::Unavailable { base, reason } => Self {
                available: false,
                base: base.clone(),
                commits_ahead: None,
                commits_behind: None,
                files_changed: None,
                insertions: None,
                deletions: None,
                reason: Some(reason.clone()),
            },
        }
    }
}

impl JsonListResponse {
    /// Build a list response, marking the worktree whose path is the
    /// longest prefix of `cwd` as `is_current`.
    pub fn from_worktrees(worktrees: &[Worktree], cwd: Option<&Path>) -> Self {
        let current_idx = cwd.and_then(|cwd| find_current_worktree(worktrees, cwd));

        let entries = worktrees
            .iter()
            .enumerate()
            .map(|(i, wt)| JsonWorktreeEntry {
                path: wt.path.display().to_string(),
                branch: wt.branch.clone(),
                commit: wt.commit.clone(),
                is_main: wt.is_main,
                is_current: current_idx == Some(i),
                stats: None,
            })
            .collect();

        Self {
            ok: true,
            worktrees: entries,
        }
    }

    /// Build a list response with per-worktree stats.
    pub fn from_worktrees_with_stats(
        worktrees: &[Worktree],
        cwd: Option<&Path>,
        stats: &[WorktreeStatsStatus],
    ) -> Self {
        let current_idx = cwd.and_then(|cwd| find_current_worktree(worktrees, cwd));

        let entries = worktrees
            .iter()
            .zip(stats)
            .enumerate()
            .map(|(i, (wt, stat))| JsonWorktreeEntry {
                path: wt.path.display().to_string(),
                branch: wt.branch.clone(),
                commit: wt.commit.clone(),
                is_main: wt.is_main,
                is_current: current_idx == Some(i),
                stats: Some(JsonWorktreeStats::from_status(stat)),
            })
            .collect();

        Self {
            ok: true,
            worktrees: entries,
        }
    }
}

/// Find the index of the worktree whose path is the longest prefix of `cwd`.
/// Returns `None` if no worktree path is a prefix of `cwd`.
///
/// Both `cwd` (canonicalized by the caller) and each `wt.path` are compared
/// in canonical form so symlinks in the repository path do not break the match.
pub fn find_current_worktree(worktrees: &[Worktree], cwd: &Path) -> Option<usize> {
    worktrees
        .iter()
        .enumerate()
        .filter_map(|(i, wt)| {
            let canonical = wt.path.canonicalize().unwrap_or_else(|_| wt.path.clone());
            cwd.starts_with(&canonical).then_some((i, canonical))
        })
        .max_by_key(|(_, p)| p.as_os_str().len())
        .map(|(idx, _)| idx)
}

/// JSON envelope for doctor responses.
#[derive(Debug, Serialize)]
pub struct JsonDoctorResponse {
    pub ok: bool,
    pub diagnostics: Vec<JsonDiagEntry>,
}

#[derive(Debug, Serialize)]
pub struct JsonDiagEntry {
    pub level: crate::worktree::DiagLevel,
    pub message: String,
}

impl JsonDoctorResponse {
    pub fn from_diagnostics(diags: &[crate::worktree::Diagnostic]) -> Self {
        let has_errors = diags
            .iter()
            .any(|d| d.level == crate::worktree::DiagLevel::Error);
        Self {
            ok: !has_errors,
            diagnostics: diags
                .iter()
                .map(|d| JsonDiagEntry {
                    level: d.level,
                    message: d.message.clone(),
                })
                .collect(),
        }
    }
}

/// Output format for the prune command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PruneFormat {
    Human,
    Json,
}

// ── Prune JSON types ────────────────────────────────────────────────

/// JSON response for prune dry-run.
#[derive(Debug, Serialize)]
pub struct JsonPruneDryRunResponse {
    pub ok: bool,
    pub mainline: String,
    pub worktrees: Vec<JsonPruneDryRunEntry>,
    pub prunable: usize,
}

#[derive(Debug, Serialize)]
pub struct JsonPruneDryRunEntry {
    pub branch: Option<String>,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    pub path: Option<String>,
    /// Whether executing this entry would remove a worktree.
    pub worktree_present: bool,
    /// Whether executing this entry would delete the local branch.
    pub branch_will_be_deleted: bool,
}

/// JSON response for prune execute.
#[derive(Debug, Serialize)]
pub struct JsonPruneExecuteResponse {
    pub ok: bool,
    pub mainline: String,
    pub pruned: Vec<JsonPrunedEntry>,
    pub skipped: Vec<JsonSkippedEntry>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct JsonPrunedEntry {
    pub branch: String,
    pub path: Option<String>,
    pub worktree_removed: bool,
    pub branch_deleted: bool,
}

#[derive(Debug, Serialize)]
pub struct JsonSkippedEntry {
    pub branch: Option<String>,
    pub reason: String,
    pub path: Option<String>,
}

/// Output format for the merge command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeFormat {
    Human,
    Json,
    /// `--print-paths`: version 1, prints repo_root, branch, mainline, cleaned_up, removed_path, pushed (one per line).
    PrintPaths,
    /// `--print-paths-v2`: prints the version 1 fields followed by destination_path.
    PrintPathsV2,
}

/// Machine-readable refusal details for a merge preflight or content merge.
#[derive(Debug, Serialize)]
pub struct JsonMergeRefusal {
    pub kind: String,
    pub reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// Facts collected before a merge mutates the destination.
#[derive(Debug, Serialize)]
pub struct JsonMergePreflight {
    pub source: String,
    pub destination: String,
    pub destination_path: String,
    pub upstream: Option<String>,
    /// Alias kept explicit for consumers that name the destination side.
    pub destination_upstream: Option<String>,
    pub ahead: Option<u32>,
    pub behind: Option<u32>,
    pub topology: String,
    pub source_history: String,
    pub source_was_merged: bool,
    pub source_was_reverted: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reverted_commit: Option<String>,
    pub allowed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refusal_kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refusal_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refusal_message: Option<String>,
}

impl JsonMergePreflight {
    pub fn from_preflight(preflight: &crate::worktree::MergePreflight) -> Self {
        let (refusal_kind, refusal_reason, refusal_message) = preflight
            .refusal
            .as_ref()
            .map(|refusal| {
                (
                    Some(refusal.kind.clone()),
                    Some(refusal.reason.clone()),
                    Some(refusal.message.clone()),
                )
            })
            .unwrap_or((None, None, None));

        Self {
            source: preflight.source.clone(),
            destination: preflight.destination.clone(),
            destination_path: preflight.destination_path.display().to_string(),
            upstream: preflight.upstream.clone(),
            destination_upstream: preflight.upstream.clone(),
            ahead: preflight.ahead,
            behind: preflight.behind,
            topology: merge_topology_name(preflight.topology),
            source_history: source_history_name(preflight.source_history),
            source_was_merged: preflight.source_was_merged,
            source_was_reverted: preflight.source_was_reverted,
            reverted_commit: preflight.reverted_commit.clone(),
            allowed: preflight.allowed,
            refusal_kind,
            refusal_reason,
            refusal_message,
        }
    }
}

fn merge_topology_name(topology: crate::worktree::MergeTopology) -> String {
    match topology {
        crate::worktree::MergeTopology::NoUpstream => "no_upstream",
        crate::worktree::MergeTopology::UpstreamUnavailable => "upstream_unavailable",
        crate::worktree::MergeTopology::Synchronized => "synchronized",
        crate::worktree::MergeTopology::Ahead => "ahead",
        crate::worktree::MergeTopology::Behind => "behind",
        crate::worktree::MergeTopology::Diverged => "diverged",
    }
    .to_string()
}

fn source_history_name(history: crate::worktree::SourceHistory) -> String {
    match history {
        crate::worktree::SourceHistory::NotMerged => "not_merged",
        crate::worktree::SourceHistory::AlreadyMerged => "already_merged",
        crate::worktree::SourceHistory::MergedThenReverted => "merged_then_reverted",
    }
    .to_string()
}

fn is_false(value: &bool) -> bool {
    !value
}

/// Durable state and pending actions for a managed merge operation.
#[derive(Debug, Serialize)]
pub struct JsonMergeOperation {
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub destination: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub destination_path: Option<String>,
    pub unresolved_paths: Vec<String>,
    pub push: bool,
    pub cleanup: bool,
    pub keep_branch: bool,
    pub worktree_removed: bool,
    pub branch_deleted: bool,
    pub push_done: bool,
    pub pending_actions: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recovery: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state_path: Option<String>,
}

/// JSON response for `merge --status`, `--continue`, and `--abort` errors.
#[derive(Debug, Serialize)]
pub struct JsonMergeOperationResponse {
    pub ok: bool,
    pub message: String,
    #[serde(flatten)]
    pub operation: JsonMergeOperation,
}

/// JSON response for the merge command.
#[derive(Debug, Serialize)]
pub struct JsonMergeResponse {
    pub ok: bool,
    /// `"reset"` when the worktree was cleaned up; absent otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event: Option<String>,
    pub message: String,
    pub branch: String,
    pub mainline: String,
    pub destination_path: String,
    pub repo_root: String,
    pub cleaned_up: bool,
    pub branch_deleted: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub removed_path: Option<String>,
    pub pushed: bool,
    /// Partial-success diagnostics, such as an identity change after commit.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preflight: Option<JsonMergePreflight>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refusal: Option<JsonMergeRefusal>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operation: Option<JsonMergeOperation>,
    /// True when this response came from `merge --inspect`.
    #[serde(skip_serializing_if = "is_false")]
    pub inspect: bool,
}

/// Resolution metadata emitted before an `exec --json` child starts.
///
/// This is intentionally not an execution result: child stdout and stderr
/// remain inherited, and child stderr is appended to the same stderr stream.
#[derive(Debug, Serialize)]
pub struct JsonExecResponse {
    pub event: &'static str,
    pub resolved: bool,
    pub message: String,
    pub branch: String,
    pub repo_root: String,
    pub worktree_path: String,
}

/// JSON response for the materialize command.
#[derive(Debug, Serialize)]
pub struct JsonMaterializeResponse {
    pub ok: bool,
    pub repository: String,
    pub workspace_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requested_ref: Option<String>,
    pub requested_sha: String,
    pub resolved_commit: String,
    pub mode: String,
    pub cache_status: String,
    pub source: String,
    pub timings_ms: JsonMaterializeTimings,
}

#[derive(Debug, Serialize)]
pub struct JsonMaterializeTimings {
    pub cache_lock: u64,
    pub cache_refresh: u64,
    pub workspace_checkout: u64,
    pub total: u64,
}

/// JSON response for the setup command.
#[derive(Debug, Serialize)]
pub struct JsonSetupResponse {
    pub ok: bool,
    pub config_path: String,
    pub ecosystems: Vec<String>,
    pub gitignore_updated: bool,
}

/// Write the wrapper navigation side channel without involving JSON parsing.
///
/// The record is NUL-delimited as `action`, `removed_path`, and `repo_root`.
/// Paths are written as their display representation, so shell bindings can
/// read each field verbatim even when it contains JSON-significant characters.
/// `action` is `reset` when the parent shell should leave the removed
/// worktree, and `none` otherwise.
pub fn write_navigation_file(
    file: &Path,
    reset: bool,
    removed_path: Option<&Path>,
    repo_root: &Path,
) -> crate::error::Result<()> {
    write_navigation_file_with_cleanup(file, reset, removed_path, repo_root, None)
}

/// Write navigation metadata plus private cleanup status for legacy wrappers.
///
/// The optional fourth field is deliberately side-channel-only: `--print-paths`
/// keeps its established three-line stdout protocol while wrappers can avoid
/// claiming branch deletion when worktree removal succeeded but branch cleanup
/// was partial.
pub fn write_navigation_file_with_cleanup(
    file: &Path,
    reset: bool,
    removed_path: Option<&Path>,
    repo_root: &Path,
    branch_deleted: Option<bool>,
) -> crate::error::Result<()> {
    let action = if reset { "reset" } else { "none" };
    let removed = removed_path
        .map(|path| path.display().to_string())
        .unwrap_or_default();
    let mut record = format!("{action}\0{removed}\0{}\0", repo_root.display());
    branch_deleted.into_iter().for_each(|branch_deleted| {
        record.push_str(if branch_deleted { "true" } else { "false" });
        record.push('\0');
    });
    std::fs::write(file, record.as_bytes()).map_err(|error| {
        crate::error::AppError::git(format!(
            "could not write navigation metadata to {}: {error}",
            file.display()
        ))
    })?;
    Ok(())
}

/// Serialize a value as a compact single-line JSON object to stdout.
pub fn print_json(value: &impl Serialize) -> crate::error::Result<()> {
    println!(
        "{}",
        serde_json::to_string(value)
            .map_err(|e| crate::error::AppError::invariant(format!("json error: {e}")))?
    );
    Ok(())
}

/// Serialize pre-execution metadata to stderr so the child owns stdout
/// unchanged. The stream may contain child diagnostics after this line.
pub fn print_json_stderr(value: &impl Serialize) -> crate::error::Result<()> {
    eprintln!(
        "{}",
        serde_json::to_string(value)
            .map_err(|e| crate::error::AppError::invariant(format!("json error: {e}")))?
    );
    Ok(())
}
