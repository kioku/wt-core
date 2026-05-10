use std::io::IsTerminal;
use std::path::PathBuf;

use crate::cli::{Cli, ColorChoice, Command, Shell};
use crate::domain::{self, BranchName, WorktreeStatsStatus};
use crate::error::{AppError, Result};
use crate::git;
use crate::output::{
    find_current_worktree, print_json, JsonDoctorResponse, JsonListResponse, JsonMergeResponse,
    JsonPruneDryRunEntry, JsonPruneDryRunResponse, JsonPruneExecuteResponse, JsonPrunedEntry,
    JsonResponse, JsonSkippedEntry, MergeFormat, NavigationFormat, PruneFormat, RemoveFormat,
    StatusFormat,
};
use crate::worktree;

pub fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Command::List {
            repo,
            json,
            stats,
            against,
            color,
        } => cmd_list(repo, status_fmt(json), stats, against.as_deref(), color),
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
        Command::Merge {
            branch,
            push,
            no_cleanup,
            repo,
            json,
            print_paths,
        } => cmd_merge(
            branch.as_deref().map(BranchName::new),
            push,
            no_cleanup,
            repo,
            merge_fmt(json, print_paths),
        ),
        Command::Diff {
            branch,
            against,
            dirty,
            staged,
            unstaged,
            tool,
            dry_run,
            print_command,
            repo,
        } => cmd_diff(
            branch.as_deref().map(BranchName::new),
            against.as_deref(),
            DiffMode::from_flags(dirty, staged, unstaged)?,
            tool.as_deref(),
            dry_run || print_command,
            repo,
        ),
        Command::Prune {
            execute,
            force,
            mainline,
            repo,
            json,
        } => cmd_prune(execute, force, mainline.as_deref(), repo, prune_fmt(json)),
        Command::Setup { repo, json } => cmd_setup(repo, status_fmt(json)),
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

fn merge_fmt(json: bool, print_paths: bool) -> MergeFormat {
    if print_paths {
        MergeFormat::PrintPaths
    } else if json {
        MergeFormat::Json
    } else {
        MergeFormat::Human
    }
}

fn prune_fmt(json: bool) -> PruneFormat {
    if json {
        PruneFormat::Json
    } else {
        PruneFormat::Human
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

fn cmd_list(
    repo: Option<PathBuf>,
    fmt: StatusFormat,
    stats: bool,
    against: Option<&str>,
    color: ColorChoice,
) -> Result<()> {
    let repo = resolve_repo(repo)?;
    let worktrees = git::list_worktrees(&repo)?;
    let cwd = std::env::current_dir()
        .ok()
        .and_then(|p| p.canonicalize().ok());
    let stats = if stats {
        Some(list_stats(&repo, &worktrees, against)?)
    } else {
        None
    };

    match fmt {
        StatusFormat::Json => match &stats {
            Some(stats) => print_json(&JsonListResponse::from_worktrees_with_stats(
                &worktrees,
                cwd.as_deref(),
                stats,
            ))?,
            None => print_json(&JsonListResponse::from_worktrees(
                &worktrees,
                cwd.as_deref(),
            ))?,
        },
        StatusFormat::Human => {
            if worktrees.is_empty() {
                println!("No worktrees found.");
                return Ok(());
            }
            if let Some(stats) = &stats {
                let color = ColorPolicy::from_env(color);
                print_list_with_stats(&worktrees, stats, color);
            } else {
                print_list_default(&worktrees, cwd.as_deref());
            }
        }
    }
    Ok(())
}

fn list_stats(
    repo: &domain::RepoRoot,
    worktrees: &[domain::Worktree],
    against: Option<&str>,
) -> Result<Vec<WorktreeStatsStatus>> {
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

    Ok(worktrees
        .iter()
        .map(|wt| match &wt.branch {
            Some(branch) => git::worktree_stats(repo, &base, branch).map_or_else(
                |_| WorktreeStatsStatus::Unavailable {
                    base: base.clone(),
                    reason: "git_error".to_string(),
                },
                WorktreeStatsStatus::Available,
            ),
            None => WorktreeStatsStatus::Unavailable {
                base: base.clone(),
                reason: "no_branch".to_string(),
            },
        })
        .collect())
}

fn print_list_default(worktrees: &[domain::Worktree], cwd: Option<&std::path::Path>) {
    let current_idx = cwd.and_then(|cwd| find_current_worktree(worktrees, cwd));
    for (i, wt) in worktrees.iter().enumerate() {
        let branch_str = wt.branch.as_deref().unwrap_or("(detached)");
        let main_tag = if wt.is_main { " [main]" } else { "" };
        let here_tag = if current_idx == Some(i) {
            " ← here"
        } else {
            ""
        };
        println!(
            "{:<50} {:<20} {}{}{}",
            wt.path.display(),
            branch_str,
            wt.commit,
            main_tag,
            here_tag
        );
    }
}

fn print_list_with_stats(
    worktrees: &[domain::Worktree],
    stats: &[WorktreeStatsStatus],
    color: ColorPolicy,
) {
    println!(
        "{:<20} {:<12} {:<10} {:<7} {:<14} PATH",
        "BRANCH", "BASE", "COMMITS", "FILES", "DIFF"
    );
    for (wt, stat) in worktrees.iter().zip(stats) {
        let branch = wt.branch.as_deref().unwrap_or("(detached)");
        let columns = format_stats_columns(stat, color);
        println!(
            "{} {} {} {} {} {}",
            pad_cell(branch, branch.len(), 20),
            pad_cell(&columns.base, columns.base.len(), 12),
            pad_cell(&columns.commits.rendered, columns.commits.visible_len, 10),
            pad_cell(&columns.files, columns.files.len(), 7),
            pad_cell(&columns.diff.rendered, columns.diff.visible_len, 14),
            wt.path.display()
        );
    }
}

fn pad_cell(text: &str, visible_len: usize, width: usize) -> String {
    format!("{}{}", text, " ".repeat(width.saturating_sub(visible_len)))
}

struct StatsColumns {
    base: String,
    commits: RenderedCell,
    files: String,
    diff: RenderedCell,
}

struct RenderedCell {
    rendered: String,
    visible_len: usize,
}

fn format_stats_columns(stat: &WorktreeStatsStatus, color: ColorPolicy) -> StatsColumns {
    match stat {
        WorktreeStatsStatus::Available(stats) => StatsColumns {
            base: stats.base.clone(),
            commits: format_commit_counts(stats.commits_ahead, stats.commits_behind, color),
            files: stats.files_changed.to_string(),
            diff: format_diff_counts(stats.insertions, stats.deletions, color),
        },
        WorktreeStatsStatus::Unavailable { base, .. } => StatsColumns {
            base: base.clone(),
            commits: plain_cell("unavailable"),
            files: "—".to_string(),
            diff: plain_cell("—"),
        },
    }
}

fn format_commit_counts(ahead: u32, behind: u32, color: ColorPolicy) -> RenderedCell {
    match (ahead, behind) {
        (0, 0) => plain_cell("0"),
        (a, 0) => color.signed_cell(&format!("+{a}"), StatSign::Positive),
        (0, b) => color.signed_cell(&format!("-{b}"), StatSign::Negative),
        (a, b) => joined_cell(&[
            color.signed_cell(&format!("+{a}"), StatSign::Positive),
            color.signed_cell(&format!("-{b}"), StatSign::Negative),
        ]),
    }
}

fn format_diff_counts(insertions: u32, deletions: u32, color: ColorPolicy) -> RenderedCell {
    joined_cell(&[
        signed_or_zero_cell(
            format!("+{insertions}"),
            insertions,
            StatSign::Positive,
            color,
        ),
        signed_or_zero_cell(
            format!("-{deletions}"),
            deletions,
            StatSign::Negative,
            color,
        ),
    ])
}

fn signed_or_zero_cell(
    text: String,
    value: u32,
    sign: StatSign,
    color: ColorPolicy,
) -> RenderedCell {
    if value == 0 {
        plain_cell(&text)
    } else {
        color.signed_cell(&text, sign)
    }
}

fn joined_cell(cells: &[RenderedCell]) -> RenderedCell {
    RenderedCell {
        rendered: cells
            .iter()
            .map(|cell| cell.rendered.as_str())
            .collect::<Vec<_>>()
            .join(" "),
        visible_len: cells.iter().map(|cell| cell.visible_len).sum::<usize>() + cells.len() - 1,
    }
}

fn plain_cell(text: &str) -> RenderedCell {
    RenderedCell {
        rendered: text.to_string(),
        visible_len: text.len(),
    }
}

#[derive(Clone, Copy)]
struct ColorPolicy {
    enabled: bool,
}

impl ColorPolicy {
    fn from_env(choice: ColorChoice) -> Self {
        Self::resolve(
            choice,
            std::io::stdout().is_terminal(),
            std::env::var_os("NO_COLOR").is_some(),
        )
    }

    fn resolve(choice: ColorChoice, stdout_is_tty: bool, no_color: bool) -> Self {
        let enabled = match choice {
            ColorChoice::Always => true,
            ColorChoice::Never => false,
            ColorChoice::Auto => stdout_is_tty && !no_color,
        };
        Self { enabled }
    }

    fn signed_cell(self, text: &str, sign: StatSign) -> RenderedCell {
        if self.enabled {
            RenderedCell {
                rendered: format!("{}{}\x1b[0m", sign.ansi_code(), text),
                visible_len: text.len(),
            }
        } else {
            plain_cell(text)
        }
    }
}

#[derive(Clone, Copy)]
enum StatSign {
    Positive,
    Negative,
}

impl StatSign {
    fn ansi_code(self) -> &'static str {
        match self {
            StatSign::Positive => "\x1b[32m",
            StatSign::Negative => "\x1b[31m",
        }
    }
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
    let tracking = result.tracking;

    let symlinked: Vec<String> = result
        .symlinks
        .as_ref()
        .map(|r| r.created.iter().map(|p| p.display().to_string()).collect())
        .unwrap_or_default();
    match fmt {
        NavigationFormat::CdPath => {
            println!("{path_str}");
        }
        NavigationFormat::Json => {
            let message = if tracking {
                format!(
                    "created worktree for branch '{branch_name}' tracking 'origin/{branch_name}'"
                )
            } else {
                format!("created worktree for branch '{branch_name}'")
            };
            let resp = JsonResponse::success(message)
                .with_event("switch")
                .with_repo_root(&root_str)
                .with_worktree_path(&path_str)
                .with_cd_path(&path_str)
                .with_branch(branch_name.as_str())
                .with_tracking(tracking)
                .with_symlinks(symlinked);
            print_json(&resp)?;
        }
        NavigationFormat::Human => {
            if tracking {
                println!("Created worktree for branch '{branch_name}' tracking 'origin/{branch_name}' at {path_str}");
            } else {
                println!("Created worktree for branch '{branch_name}' at {path_str}");
            }
            if let Some(report) = &result.symlinks {
                for path in &report.created {
                    println!("  Symlinked {}", path.display());
                }
            }
        }
    }
    if let Some(report) = &result.symlinks {
        for (path, reason) in &report.skipped {
            eprintln!("warning: symlink {}: {reason}", path.display());
        }
    }
    if let Some(recommendation) = &result.setup_recommendation {
        eprintln!("{recommendation}");
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
                    .with_event("switch")
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
    // JSON output is for machine consumers that pass an explicit branch.
    // --print-cd-path is allowed because shell bindings need it to cd
    // after the interactive picker (picker renders on stderr/tty).
    if fmt == NavigationFormat::Json {
        return Err(AppError::usage(
            "branch argument is required with --json".to_string(),
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

/// Resolve an optional branch for a destructive command (`remove`, `merge`)
/// when none was explicitly provided.
///
/// In TTY contexts (human and `--print-paths` formats), opens an interactive
/// picker excluding the main worktree and pre-selecting the current worktree
/// if applicable. For JSON and non-TTY contexts, returns `None` so the
/// caller falls back to cwd inference in the worktree layer.
///
/// `is_json` — whether the output format is machine-only (JSON).
/// `action`  — verb shown in picker prompt and error messages (e.g. "remove", "merge").
fn resolve_action_branch(
    repo: &domain::RepoRoot,
    is_json: bool,
    action: &str,
) -> Result<Option<BranchName>> {
    if is_json {
        return Ok(None);
    }

    if !std::io::stdin().is_terminal() {
        return Ok(None);
    }

    let worktrees = git::list_worktrees(repo)?;
    let candidates: Vec<_> = worktrees.iter().filter(|wt| !wt.is_main).collect();

    if candidates.is_empty() {
        return Err(AppError::usage(format!(
            "no worktrees to {action} (create one with `wt add`)"
        )));
    }

    // Pre-select the candidate whose path is the longest prefix of cwd.
    let preselect = std::env::current_dir().ok().and_then(|cwd| {
        candidates
            .iter()
            .enumerate()
            .filter(|(_, wt)| cwd.starts_with(&wt.path))
            .max_by_key(|(_, wt)| wt.path.as_os_str().len())
            .map(|(idx, _)| idx)
    });

    pick_action_worktree(&candidates, preselect, action).map(Some)
}

/// Present an interactive fuzzy picker for a destructive worktree action.
///
/// Only non-main worktrees are shown. `preselect` is the index into
/// `candidates` to highlight by default (e.g. the current worktree).
/// `action` is the verb displayed in the prompt (e.g. "Remove", "Merge").
#[cfg(feature = "interactive")]
fn pick_action_worktree(
    candidates: &[&domain::Worktree],
    preselect: Option<usize>,
    action: &str,
) -> Result<BranchName> {
    let worktree = pick_action_worktree_entry(candidates, preselect, action)?;
    let branch = worktree.branch.as_deref().ok_or_else(|| {
        AppError::usage("selected worktree has no branch (detached HEAD)".to_string())
    })?;
    Ok(BranchName::new(branch))
}

#[cfg(feature = "interactive")]
fn pick_action_worktree_entry(
    candidates: &[&domain::Worktree],
    preselect: Option<usize>,
    action: &str,
) -> Result<domain::Worktree> {
    use dialoguer::theme::ColorfulTheme;
    use dialoguer::FuzzySelect;

    let prompt = format!("{} worktree", capitalize(action));

    let items: Vec<String> = candidates
        .iter()
        .map(|wt| {
            let branch = wt.branch.as_deref().unwrap_or("(detached)");
            format!("{branch:<30} {:<50} {}", wt.path.display(), wt.commit)
        })
        .collect();

    let selection = FuzzySelect::with_theme(&ColorfulTheme::default())
        .with_prompt(&prompt)
        .items(&items)
        .default(preselect.unwrap_or(0))
        .interact_opt()
        .map_err(|e| AppError::usage(format!("picker failed: {e}")))?;

    match selection {
        Some(idx) => Ok(candidates[idx].clone()),
        // Esc / Ctrl-C: dialoguer has already restored the terminal state
        // before returning None, so destructors are not a concern here.
        // Exit 130 (128 + SIGINT) is the Unix convention for user cancellation.
        None => std::process::exit(130),
    }
}

#[cfg(not(feature = "interactive"))]
fn pick_action_worktree(
    candidates: &[&domain::Worktree],
    preselect: Option<usize>,
    action: &str,
) -> Result<BranchName> {
    let worktree = pick_action_worktree_entry(candidates, preselect, action)?;
    let branch = worktree.branch.as_deref().ok_or_else(|| {
        AppError::usage("selected worktree has no branch (detached HEAD)".to_string())
    })?;
    Ok(BranchName::new(branch))
}

#[cfg(not(feature = "interactive"))]
fn pick_action_worktree_entry(
    _candidates: &[&domain::Worktree],
    _preselect: Option<usize>,
    _action: &str,
) -> Result<domain::Worktree> {
    Err(AppError::usage(
        "interactive mode not available (compiled without 'interactive' feature)".to_string(),
    ))
}

/// Capitalize the first character of a string (ASCII only).
fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().to_string() + chars.as_str(),
        None => String::new(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DiffMode {
    Branch,
    Dirty,
    Staged,
    Unstaged,
}

impl DiffMode {
    fn from_flags(dirty: bool, staged: bool, unstaged: bool) -> Result<Self> {
        let selected = [dirty, staged, unstaged]
            .into_iter()
            .filter(|flag| *flag)
            .count();

        if selected > 1 {
            return Err(AppError::usage(
                "--dirty, --staged, and --unstaged are mutually exclusive".to_string(),
            ));
        }

        Ok(match (dirty, staged, unstaged) {
            (true, false, false) => Self::Dirty,
            (false, true, false) => Self::Staged,
            (false, false, true) => Self::Unstaged,
            _ => Self::Branch,
        })
    }
}

fn cmd_diff(
    branch: Option<BranchName>,
    against: Option<&str>,
    mode: DiffMode,
    tool: Option<&str>,
    dry_run: bool,
    repo: Option<PathBuf>,
) -> Result<()> {
    if matches!(tool, Some(name) if name.trim().is_empty()) {
        return Err(AppError::usage("--tool must not be empty".to_string()));
    }

    if mode != DiffMode::Branch && against.is_some() {
        return Err(AppError::usage(
            "--against can only be used with branch-vs-mainline diffs".to_string(),
        ));
    }

    let repo = resolve_repo(repo)?;

    if mode == DiffMode::Branch {
        let resolved_branch = match branch {
            Some(branch) => branch,
            None => resolve_diff_branch(&repo)?,
        };

        let result = worktree::diff(&repo, &resolved_branch, against, tool, dry_run)?;
        print_branch_diff_result(&result, dry_run);
        return Ok(());
    }

    let selected_worktree = resolve_diff_worktree(&repo, branch)?;
    let dirty_mode = match mode {
        DiffMode::Dirty => worktree::DirtyDiffMode::Dirty,
        DiffMode::Staged => worktree::DirtyDiffMode::Staged,
        DiffMode::Unstaged => worktree::DirtyDiffMode::Unstaged,
        DiffMode::Branch => unreachable!("branch diff handled above"),
    };
    let result = worktree::diff_dirty(&selected_worktree, dirty_mode, tool, dry_run)?;
    print_dirty_diff_result(&result, dry_run);

    Ok(())
}

fn print_branch_diff_result(result: &worktree::DiffResult, dry_run: bool) {
    if dry_run {
        println!("{}", result.command.join(" "));
        return;
    }

    println!(
        "Opened diff for '{}' against {}",
        result.branch, result.base
    );
}

fn print_dirty_diff_result(result: &worktree::DirtyDiffResult, dry_run: bool) {
    if dry_run {
        println!("{}", result.command.join(" "));
        return;
    }

    println!("Opened dirty diff for '{}'", result.label);
}

fn resolve_diff_worktree(
    repo: &domain::RepoRoot,
    branch: Option<BranchName>,
) -> Result<domain::Worktree> {
    let worktrees = git::list_worktrees(repo)?;

    if let Some(branch) = branch {
        return worktrees
            .into_iter()
            .find(|wt| !wt.is_main && wt.branch.as_deref() == Some(branch.as_str()))
            .ok_or_else(|| {
                AppError::usage(format!(
                    "branch '{}' has no associated worktree",
                    branch.as_str()
                ))
            });
    }

    let candidates: Vec<_> = worktrees.iter().filter(|wt| !wt.is_main).collect();

    if candidates.is_empty() {
        return Err(AppError::usage(
            "no worktrees to diff (create one with `wt add`)".to_string(),
        ));
    }

    if !std::io::stdin().is_terminal() {
        return Err(AppError::usage(
            "no branch specified; interactive mode requires a terminal".to_string(),
        ));
    }

    let preselect = std::env::current_dir().ok().and_then(|cwd| {
        candidates
            .iter()
            .enumerate()
            .filter(|(_, wt)| cwd.starts_with(&wt.path))
            .max_by_key(|(_, wt)| wt.path.as_os_str().len())
            .map(|(idx, _)| idx)
    });

    pick_action_worktree_entry(&candidates, preselect, "diff")
}

fn resolve_diff_branch(repo: &domain::RepoRoot) -> Result<BranchName> {
    let worktrees = git::list_worktrees(repo)?;
    let candidates: Vec<_> = worktrees.iter().filter(|wt| !wt.is_main).collect();

    if candidates.is_empty() {
        return Err(AppError::usage(
            "no worktrees to diff (create one with `wt add`)".to_string(),
        ));
    }

    if !std::io::stdin().is_terminal() {
        return Err(AppError::usage(
            "no branch specified; interactive mode requires a terminal".to_string(),
        ));
    }

    let preselect = std::env::current_dir().ok().and_then(|cwd| {
        candidates
            .iter()
            .enumerate()
            .filter(|(_, wt)| cwd.starts_with(&wt.path))
            .max_by_key(|(_, wt)| wt.path.as_os_str().len())
            .map(|(idx, _)| idx)
    });

    pick_action_worktree(&candidates, preselect, "diff")
}

fn cmd_merge(
    branch: Option<BranchName>,
    push: bool,
    no_cleanup: bool,
    repo: Option<PathBuf>,
    fmt: MergeFormat,
) -> Result<()> {
    let repo = resolve_repo(repo)?;

    let resolved_branch = match branch {
        Some(b) => Some(b),
        None => resolve_action_branch(&repo, fmt == MergeFormat::Json, "merge")?,
    };

    let result = worktree::merge(&repo, resolved_branch.as_ref(), push, no_cleanup)?;

    let root_str = result.repo_root.display().to_string();
    let branch_name = &result.branch;
    let removed_str = result
        .removed_path
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_default();

    match fmt {
        MergeFormat::PrintPaths => {
            println!("{root_str}");
            println!("{branch_name}");
            println!("{}", result.mainline);
            println!("{}", result.cleaned_up);
            println!("{removed_str}");
            println!("{}", result.pushed);
        }
        MergeFormat::Json => {
            let event = if result.cleaned_up {
                Some("reset".to_string())
            } else {
                None
            };
            print_json(&JsonMergeResponse {
                ok: true,
                event,
                message: format!("merged '{}' into {}", branch_name, result.mainline),
                branch: branch_name.to_string(),
                mainline: result.mainline.clone(),
                repo_root: root_str,
                cleaned_up: result.cleaned_up,
                removed_path: if result.cleaned_up {
                    Some(removed_str)
                } else {
                    None
                },
                pushed: result.pushed,
            })?;
        }
        MergeFormat::Human => {
            println!("Merged '{}' into {}", branch_name, result.mainline);
            if result.cleaned_up {
                println!("Removed worktree and branch '{}'", branch_name);
            }
            if result.pushed {
                println!("Pushed {} to origin", result.mainline);
            }
        }
    }
    for w in &result.warnings {
        eprintln!("warning: {w}");
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

    let resolved_branch = match branch {
        Some(b) => Some(b),
        None => resolve_action_branch(&repo, fmt == RemoveFormat::Json, "remove")?,
    };

    let result = worktree::remove(&repo, resolved_branch.as_ref(), force)?;

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
                    .with_event("reset")
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

fn cmd_prune(
    execute: bool,
    force: bool,
    mainline: Option<&str>,
    repo: Option<PathBuf>,
    fmt: PruneFormat,
) -> Result<()> {
    let repo = resolve_repo(repo)?;

    if execute {
        cmd_prune_execute(&repo, mainline, force, fmt)
    } else {
        cmd_prune_dry_run(&repo, mainline, fmt)
    }
}

fn format_prune_entry(entry: &worktree::WorktreePruneEntry) -> (String, Option<String>) {
    match &entry.status {
        worktree::IntegrationStatus::Integrated(m) => {
            let method_str = match m {
                worktree::IntegrationMethod::Merged => "merged",
                worktree::IntegrationMethod::Rebase => "rebase",
            };
            ("integrated".to_string(), Some(method_str.to_string()))
        }
        worktree::IntegrationStatus::NotIntegrated => ("not_integrated".to_string(), None),
        worktree::IntegrationStatus::NoBranch => ("no_branch".to_string(), None),
    }
}

fn print_prune_entry_human(entry: &worktree::WorktreePruneEntry) {
    match &entry.status {
        worktree::IntegrationStatus::Integrated(method) => {
            let method_str = match method {
                worktree::IntegrationMethod::Merged => "merged",
                worktree::IntegrationMethod::Rebase => "rebase",
            };
            let branch = entry.branch.as_deref().unwrap_or("(unknown)");
            println!("  ✓ {branch:<20} integrated ({method_str})");
        }
        worktree::IntegrationStatus::NotIntegrated => {
            let branch = entry.branch.as_deref().unwrap_or("(unknown)");
            println!("  ✗ {branch:<20} not integrated");
        }
        worktree::IntegrationStatus::NoBranch => {
            println!("  ⚠ {:<20} no branch (detached HEAD)", "(detached)");
        }
    }
}

fn cmd_prune_dry_run(
    repo: &domain::RepoRoot,
    mainline: Option<&str>,
    fmt: PruneFormat,
) -> Result<()> {
    let result = worktree::prune_dry_run(repo, mainline)?;

    let prunable = result
        .entries
        .iter()
        .filter(|e| matches!(e.status, worktree::IntegrationStatus::Integrated(_)))
        .count();

    match fmt {
        PruneFormat::Json => {
            let entries: Vec<JsonPruneDryRunEntry> = result
                .entries
                .iter()
                .map(|e| {
                    let (status, method) = format_prune_entry(e);
                    JsonPruneDryRunEntry {
                        branch: e.branch.clone(),
                        status,
                        method,
                        path: e.path.display().to_string(),
                    }
                })
                .collect();

            print_json(&JsonPruneDryRunResponse {
                ok: true,
                mainline: result.mainline,
                worktrees: entries,
                prunable,
            })?;
        }
        PruneFormat::Human => {
            println!("Mainline: {}", result.mainline);
            for entry in &result.entries {
                print_prune_entry_human(entry);
            }
            if result.entries.is_empty() {
                println!("\nNo worktrees to prune.");
            } else if prunable == 0 {
                println!("\nNo integrated worktrees found.");
            } else {
                println!(
                    "\n{prunable} integrated worktree{} can be pruned. Run with --execute to remove.",
                    if prunable == 1 { "" } else { "s" }
                );
            }
        }
    }
    Ok(())
}

fn cmd_prune_execute(
    repo: &domain::RepoRoot,
    mainline: Option<&str>,
    force: bool,
    fmt: PruneFormat,
) -> Result<()> {
    let result = worktree::prune_execute(repo, mainline, force)?;

    match fmt {
        PruneFormat::Json => {
            let pruned: Vec<JsonPrunedEntry> = result
                .pruned
                .iter()
                .map(|e| JsonPrunedEntry {
                    branch: e.branch.clone(),
                    path: e.path.display().to_string(),
                })
                .collect();

            let skipped: Vec<JsonSkippedEntry> = result
                .skipped
                .iter()
                .map(|e| JsonSkippedEntry {
                    branch: e.branch.clone(),
                    reason: e.reason.clone(),
                    path: e.path.display().to_string(),
                })
                .collect();

            print_json(&JsonPruneExecuteResponse {
                ok: true,
                mainline: result.mainline,
                pruned,
                skipped,
                warnings: result.warnings,
            })?;
        }
        PruneFormat::Human => {
            println!("Mainline: {}", result.mainline);
            for entry in &result.pruned {
                println!("  Removed {}", entry.branch);
            }
            for entry in &result.skipped {
                let label = entry.branch.as_deref().unwrap_or("(detached)");
                let reason = match entry.reason.as_str() {
                    "not_integrated" => "not integrated",
                    "no_branch" => "no branch",
                    "removal_failed" => "removal failed",
                    other => other,
                };
                println!("  Skipped {label} ({reason})");
            }
            for w in &result.warnings {
                eprintln!("warning: {w}");
            }
            let count = result.pruned.len();
            if count == 0 {
                println!("\nNo worktrees pruned.");
            } else {
                println!(
                    "\nPruned {count} worktree{}.",
                    if count == 1 { "" } else { "s" }
                );
            }
        }
    }
    Ok(())
}

fn cmd_setup(repo: Option<PathBuf>, fmt: StatusFormat) -> Result<()> {
    use crate::output::JsonSetupResponse;
    use crate::symlinks;

    let repo = resolve_repo(repo)?;
    let config_path = symlinks::config_path(&repo);

    if config_path.exists() {
        return Err(AppError::conflict(format!(
            ".wt/symlinks already exists at {}; edit it directly",
            config_path.display()
        )));
    }

    let config_content = symlinks::generate_config(&repo);
    let ecosystems = symlinks::detect_ecosystems(&repo);

    let config_dir = symlinks::config_dir(&repo);
    std::fs::create_dir_all(&config_dir)
        .map_err(|e| AppError::git(format!("failed to create .wt/ directory: {e}")))?;

    std::fs::write(&config_path, &config_content)
        .map_err(|e| AppError::git(format!("failed to write .wt/symlinks: {e}")))?;

    let gitignore_updated = symlinks::ensure_gitignore_entry(&repo)
        .map_err(|e| AppError::git(format!("failed to update .gitignore: {e}")))?;

    match fmt {
        StatusFormat::Json => {
            print_json(&JsonSetupResponse {
                ok: true,
                config_path: config_path.display().to_string(),
                ecosystems,
                gitignore_updated,
            })?;
        }
        StatusFormat::Human => {
            if ecosystems.is_empty() {
                eprintln!("Detected ecosystems: (none)");
            } else {
                eprintln!("Detected ecosystems: {}", ecosystems.join(", "));
            }
            eprintln!("Created {}", config_path.display());
            if gitignore_updated {
                eprintln!("Added .wt/symlinks.local to .gitignore");
            }
            eprintln!();
            eprintln!("Review the generated config and remove entries that don't apply.");
        }
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
