use std::ffi::{OsStr, OsString};
use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::{Command as ProcessCommand, Stdio};

#[cfg(not(unix))]
use std::process::ExitStatus;

use crate::cli::{Cli, ColorChoice, Command, MaterializeMode, Shell};
use crate::domain::{self, BranchName, WorktreeStatsStatus};
use crate::error::{AppError, Result};
use crate::git;
use crate::output::{
    find_current_worktree, print_json, print_json_stderr, write_navigation_file,
    write_navigation_file_with_cleanup, JsonDoctorResponse, JsonExecResponse, JsonListResponse,
    JsonMaterializeResponse, JsonMaterializeTimings, JsonMergeOperation,
    JsonMergeOperationResponse, JsonMergePreflight, JsonMergeRefusal, JsonMergeResponse,
    JsonPruneDryRunEntry, JsonPruneDryRunResponse, JsonPruneExecuteResponse, JsonPrunedEntry,
    JsonResponse, JsonSkippedEntry, MergeFormat, NavigationFormat, PruneFormat, RemoveFormat,
    StatusFormat,
};
use crate::worktree;
use unicode_width::UnicodeWidthStr;

#[cfg(windows)]
mod windows_job {
    use std::io;
    use std::mem::size_of;
    use std::os::windows::io::AsRawHandle;
    use std::process::Child;

    use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };

    /// Keeps the child and its descendants in a kill-on-close job.
    ///
    /// Windows has no Unix-style `exec`, so a terminated `wt-core` process
    /// cannot otherwise guarantee that descendants do not outlive it.
    pub struct ChildJob(HANDLE);

    impl ChildJob {
        pub fn for_child(child: &Child) -> io::Result<Self> {
            // A private job with KILL_ON_JOB_CLOSE is closed automatically by
            // the OS if wt-core is terminated without unwinding.
            let handle =
                // SAFETY: null attributes and name request an unnamed private job.
                unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
            if handle.is_null() {
                return Err(last_error());
            }

            let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let configured =
                // SAFETY: handle is valid and limits remains alive for this call.
                unsafe {
                    SetInformationJobObject(
                        handle,
                        JobObjectExtendedLimitInformation,
                        (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION)
                            .cast::<core::ffi::c_void>(),
                        size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                    )
                } != 0;
            if !configured {
                let error = last_error();
                let _ =
                    // SAFETY: handle was returned by CreateJobObjectW and is owned here.
                    unsafe { CloseHandle(handle) };
                return Err(error);
            }

            let assigned =
                // SAFETY: handle is valid and the live child owns a valid process handle.
                unsafe { AssignProcessToJobObject(handle, child.as_raw_handle()) } != 0;
            if !assigned {
                let error = last_error();
                let _ =
                    // SAFETY: handle was returned by CreateJobObjectW and is owned here.
                    unsafe { CloseHandle(handle) };
                return Err(error);
            }

            Ok(Self(handle))
        }
    }

    impl Drop for ChildJob {
        fn drop(&mut self) {
            // Closing the final job handle terminates all remaining members.
            let _ =
                // SAFETY: self.0 is the owned handle returned by CreateJobObjectW.
                unsafe { CloseHandle(self.0) };
        }
    }

    fn last_error() -> io::Error {
        let error =
            // SAFETY: GetLastError has no preconditions and returns this thread's error.
            unsafe { GetLastError() };
        io::Error::from_raw_os_error(error as i32)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunOutcome {
    Success,
    #[cfg_attr(unix, allow(dead_code))]
    Exit(i32),
}

pub fn run(cli: Cli) -> Result<RunOutcome> {
    match cli.command {
        Command::List {
            repo,
            json,
            stats,
            against,
            color,
        } => success(cmd_list(
            repo,
            status_fmt(json),
            stats,
            against.as_deref(),
            color,
        )),
        Command::Add {
            branch,
            base,
            repo,
            json,
            print_cd_path,
        } => success(cmd_add(
            &BranchName::new(&branch),
            base.as_deref(),
            repo,
            nav_fmt(json, print_cd_path),
        )),
        Command::Go {
            branch,
            interactive,
            repo,
            json,
            print_cd_path,
        } => success(cmd_go(
            branch.as_deref(),
            interactive,
            repo,
            nav_fmt(json, print_cd_path),
        )),
        Command::Exec {
            branch,
            repo,
            json,
            command,
        } => cmd_exec(&branch, repo, json, command),
        Command::Remove {
            branch,
            force,
            keep_branch,
            repo,
            json,
            print_paths,
            navigation_file,
        } => success(cmd_remove(
            branch.as_deref().map(BranchName::new),
            force,
            keep_branch,
            repo,
            remove_fmt(json, print_paths),
            navigation_file.as_deref(),
        )),
        Command::Merge {
            branch,
            into,
            inspect,
            status,
            continue_merge,
            abort,
            push,
            no_cleanup,
            repo,
            json,
            print_paths,
            print_paths_v2,
            navigation_file,
        } => success(cmd_merge(MergeCommandOptions {
            branch: branch.as_deref().map(BranchName::new),
            into,
            inspect,
            status,
            continue_merge,
            abort,
            push,
            no_cleanup,
            repo,
            fmt: merge_fmt(json, print_paths, print_paths_v2),
            navigation_file,
        })),
        Command::Materialize {
            repo_slug,
            remote_url,
            ref_,
            sha,
            cache_root,
            workspace_root,
            object_source,
            mode,
            json,
        } => success(cmd_materialize(
            crate::materialize::MaterializeOptions {
                repo_slug,
                remote_url,
                ref_name: ref_,
                sha,
                cache_root,
                workspace_root,
                object_source,
                mode,
            },
            json,
        )),
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
        } => success(cmd_diff(
            branch.as_deref().map(BranchName::new),
            against.as_deref(),
            DiffMode::from_flags(dirty, staged, unstaged)?,
            tool.as_deref(),
            dry_run || print_command,
            repo,
        )),
        Command::Prune {
            execute,
            force,
            integrated_into,
            repo,
            json,
        } => success(cmd_prune(
            execute,
            force,
            integrated_into.as_deref(),
            repo,
            prune_fmt(json),
        )),
        Command::Setup { repo, json } => success(cmd_setup(repo, status_fmt(json))),
        Command::Init { shell } => success(cmd_init(shell)),
        Command::Doctor { repo, json } => success(cmd_doctor(repo, status_fmt(json))),
    }
}

fn success(result: Result<()>) -> Result<RunOutcome> {
    result.map(|()| RunOutcome::Success)
}

fn nav_fmt(json: bool, cd_path: bool) -> NavigationFormat {
    // JSON is the canonical machine format. The path flag remains accepted
    // so wrappers can append it without making --json invocations invalid.
    if json {
        NavigationFormat::Json
    } else if cd_path {
        NavigationFormat::CdPath
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
    // Keep the same precedence as add/go: JSON is canonical, while the
    // legacy line-oriented output remains available when requested alone.
    if json {
        RemoveFormat::Json
    } else if print_paths {
        RemoveFormat::PrintPaths
    } else {
        RemoveFormat::Human
    }
}

fn merge_fmt(json: bool, print_paths: bool, print_paths_v2: bool) -> MergeFormat {
    // JSON is authoritative when wrappers append either legacy selector.
    if json {
        MergeFormat::Json
    } else if print_paths_v2 {
        MergeFormat::PrintPathsV2
    } else if print_paths {
        MergeFormat::PrintPaths
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

fn cmd_materialize(options: crate::materialize::MaterializeOptions, json: bool) -> Result<()> {
    if options.mode != MaterializeMode::Detached {
        return Err(AppError::usage(
            "only detached materialize mode is supported".to_string(),
        ));
    }

    let result = crate::materialize::materialize(options)?;
    if json {
        print_json(&JsonMaterializeResponse {
            ok: true,
            repository: result.repository,
            workspace_path: result.workspace_path.display().to_string(),
            cache_path: result.cache_path.map(|p| p.display().to_string()),
            requested_ref: result.requested_ref,
            requested_sha: result.requested_sha,
            resolved_commit: result.resolved_commit,
            mode: result.mode.to_string(),
            cache_status: result.cache_status.to_string(),
            source: result.source.to_string(),
            timings_ms: JsonMaterializeTimings {
                cache_lock: result.timings.cache_lock,
                cache_refresh: result.timings.cache_refresh,
                workspace_checkout: result.timings.workspace_checkout,
                total: result.timings.total,
            },
        })?;
    } else {
        println!(
            "Materialized {} at {} ({})",
            result.resolved_commit,
            result.workspace_path.display(),
            result.mode
        );
    }
    Ok(())
}

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
    let rows = worktrees
        .iter()
        .zip(stats)
        .map(|(wt, stat)| StatsRow {
            branch: plain_cell(wt.branch.as_deref().unwrap_or("(detached)")),
            columns: format_stats_columns(stat, color),
            path: wt.path.display().to_string(),
        })
        .collect::<Vec<_>>();
    let widths = StatsColumnWidths::from_rows(&rows);

    println!(
        "{} {} {} {} {} PATH",
        align_left(&plain_cell("BRANCH"), widths.branch),
        align_left(&plain_cell("BASE"), widths.base),
        align_right(&plain_cell("COMMITS"), widths.commits),
        align_right(&plain_cell("FILES"), widths.files),
        align_right(&plain_cell("DIFF"), widths.diff)
    );

    for row in rows {
        println!(
            "{} {} {} {} {} {}",
            align_left(&row.branch, widths.branch),
            align_left(&row.columns.base, widths.base),
            align_right(&row.columns.commits, widths.commits),
            align_right(&row.columns.files, widths.files),
            align_right(&row.columns.diff, widths.diff),
            row.path
        );
    }
}

fn align_left(cell: &RenderedCell, width: usize) -> String {
    format!(
        "{}{}",
        cell.rendered,
        " ".repeat(width.saturating_sub(cell.visible_len))
    )
}

fn align_right(cell: &RenderedCell, width: usize) -> String {
    format!(
        "{}{}",
        " ".repeat(width.saturating_sub(cell.visible_len)),
        cell.rendered
    )
}

struct StatsRow {
    branch: RenderedCell,
    columns: StatsColumns,
    path: String,
}

struct StatsColumnWidths {
    branch: usize,
    base: usize,
    commits: usize,
    files: usize,
    diff: usize,
}

impl StatsColumnWidths {
    fn from_rows(rows: &[StatsRow]) -> Self {
        let mut widths = Self {
            branch: "BRANCH".len(),
            base: "BASE".len(),
            commits: "COMMITS".len(),
            files: "FILES".len(),
            diff: "DIFF".len(),
        };

        for row in rows {
            widths.branch = widths.branch.max(row.branch.visible_len);
            widths.base = widths.base.max(row.columns.base.visible_len);
            widths.commits = widths.commits.max(row.columns.commits.visible_len);
            widths.files = widths.files.max(row.columns.files.visible_len);
            widths.diff = widths.diff.max(row.columns.diff.visible_len);
        }

        widths
    }
}

struct StatsColumns {
    base: RenderedCell,
    commits: RenderedCell,
    files: RenderedCell,
    diff: RenderedCell,
}

struct RenderedCell {
    rendered: String,
    visible_len: usize,
}

fn format_stats_columns(stat: &WorktreeStatsStatus, color: ColorPolicy) -> StatsColumns {
    match stat {
        WorktreeStatsStatus::Available(stats) => StatsColumns {
            base: plain_cell(&stats.base),
            commits: format_commit_counts(stats.commits_ahead, stats.commits_behind, color),
            files: plain_cell(&stats.files_changed.to_string()),
            diff: format_diff_counts(stats.insertions, stats.deletions, color),
        },
        WorktreeStatsStatus::Unavailable { base, .. } => StatsColumns {
            base: plain_cell(base),
            commits: plain_cell("unavailable"),
            files: plain_cell("—"),
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
        visible_len: UnicodeWidthStr::width(text),
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
                visible_len: UnicodeWidthStr::width(text),
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

fn cmd_exec(
    branch: &str,
    repo: Option<PathBuf>,
    json: bool,
    command: Vec<OsString>,
) -> Result<RunOutcome> {
    let program = command
        .first()
        .ok_or_else(|| AppError::usage("command is required after `--`"))?;
    let repo = resolve_repo(repo)?;
    let result = worktree::go(&repo, &BranchName::new(branch))?;

    if json {
        print_json_stderr(&JsonExecResponse {
            event: "exec_resolved",
            resolved: true,
            message: format!("resolved worktree for branch '{branch}'"),
            branch: result.branch.to_string(),
            repo_root: result.repo_root.display().to_string(),
            worktree_path: result.worktree_path.display().to_string(),
        })?;
    }

    let mut process = ProcessCommand::new(program);
    process
        .args(&command[1..])
        .current_dir(&result.worktree_path)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    // Match internal Git invocations: inherited Git repository context must
    // not redirect the command away from the resolved worktree.
    git::sanitize_git_environment(&mut process);

    run_resolved_command(process, program)
}

#[cfg(unix)]
fn run_resolved_command(mut process: ProcessCommand, program: &OsStr) -> Result<RunOutcome> {
    use std::os::unix::process::CommandExt;

    // Resolution has already completed. Replacing this process gives the
    // command exact inherited stdio, exit status, and signal semantics; a
    // supervisor terminating wt-core therefore terminates the command itself.
    let error = process.exec();
    Err(execution_error(program, error))
}

#[cfg(windows)]
fn run_resolved_command(mut process: ProcessCommand, program: &OsStr) -> Result<RunOutcome> {
    let mut child = process
        .spawn()
        .map_err(|error| execution_error(program, error))?;
    let _job = match windows_job::ChildJob::for_child(&child) {
        Ok(job) => job,
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(AppError::usage(format!(
                "failed to contain '{}': {error}",
                program.to_string_lossy()
            )));
        }
    };

    let status = child
        .wait()
        .map_err(|error| execution_error(program, error))?;
    Ok(child_outcome(status))
}

#[cfg(not(any(unix, windows)))]
fn run_resolved_command(mut process: ProcessCommand, program: &OsStr) -> Result<RunOutcome> {
    let status = process
        .status()
        .map_err(|error| execution_error(program, error))?;
    Ok(child_outcome(status))
}

fn execution_error(program: &OsStr, error: std::io::Error) -> AppError {
    AppError::usage(format!(
        "failed to execute '{}': {error}",
        program.to_string_lossy()
    ))
}

#[cfg(not(unix))]
fn child_outcome(status: ExitStatus) -> RunOutcome {
    if status.success() {
        RunOutcome::Success
    } else {
        RunOutcome::Exit(status.code().unwrap_or(1))
    }
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

    // Target selection happens before destructive commands acquire their
    // lifecycle lock. Keep this observation strictly read-only; the command
    // performs any required metadata prune after it owns the lock.
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
#[cfg(feature = "interactive")]
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

/// Inputs for the merge command, kept together so every CLI option remains
/// visible without making the command implementation's call site fragile.
struct MergeCommandOptions {
    branch: Option<BranchName>,
    into: Option<String>,
    inspect: bool,
    status: bool,
    continue_merge: bool,
    abort: bool,
    push: bool,
    no_cleanup: bool,
    repo: Option<PathBuf>,
    fmt: MergeFormat,
    navigation_file: Option<PathBuf>,
}

fn cmd_merge(options: MergeCommandOptions) -> Result<()> {
    let MergeCommandOptions {
        branch,
        into,
        inspect,
        status,
        continue_merge,
        abort,
        push,
        no_cleanup,
        repo: repo_path,
        fmt,
        navigation_file,
    } = options;
    let repo = resolve_repo(repo_path)?;

    if status {
        return cmd_merge_status(&repo, fmt);
    }
    if abort {
        return cmd_merge_abort(&repo, fmt);
    }
    if continue_merge {
        return cmd_merge_continue(&repo, fmt, navigation_file.as_deref());
    }

    let resolved_branch = match branch {
        Some(branch) => Some(branch),
        None => resolve_action_branch(&repo, fmt == MergeFormat::Json, "merge")?,
    };
    if inspect {
        let preflight =
            worktree::merge_preflight(&repo, resolved_branch.as_ref(), into.as_deref())?;
        return print_merge_inspection(&repo, &preflight, fmt);
    }

    // Hold ownership before preflight and keep it through Git finalization and
    // every durable journal/cleanup update. A paused hook therefore blocks all
    // competing mutators while read-only status remains available.
    let lifecycle_lock = worktree::acquire_merge_lifecycle_lock(&repo)?;
    let preflight = worktree::merge_preflight_with_lifecycle_lock(
        &repo,
        resolved_branch.as_ref(),
        into.as_deref(),
        &lifecycle_lock,
    )?;

    if fmt == MergeFormat::Human {
        print_merge_preflight(&preflight);
    }

    if let Some(refusal) = &preflight.refusal {
        let error = AppError::conflict(refusal.message.clone());
        return report_merge_failure(&repo, &preflight, fmt, error, None);
    }

    let preflight_for_error = preflight.clone();
    let result =
        match worktree::merge_with_preflight(&repo, preflight, push, no_cleanup, &lifecycle_lock) {
            Ok(result) => result,
            Err(failure) => {
                let refusal = match failure.kind {
                    worktree::MergeFailureKind::ContentConflict => JsonMergeRefusal {
                        kind: "content".to_string(),
                        reason: "content_conflict".to_string(),
                        message: Some("destination content conflicts with the source".to_string()),
                    },
                    worktree::MergeFailureKind::GitFailure => JsonMergeRefusal {
                        kind: "git".to_string(),
                        reason: "git_error".to_string(),
                        message: Some(failure.error.message.clone()),
                    },
                };
                return report_merge_failure(
                    &repo,
                    &preflight_for_error,
                    fmt,
                    failure.error,
                    Some(refusal),
                );
            }
        };

    print_merge_result(&repo, result, fmt, navigation_file.as_deref())
}

fn print_merge_result(
    repo: &domain::RepoRoot,
    result: worktree::MergeResult,
    fmt: MergeFormat,
    navigation_file: Option<&std::path::Path>,
) -> Result<()> {
    let root_str = result.repo_root.display().to_string();
    let branch_name = &result.branch;
    let removed_str = result
        .removed_path
        .as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_default();

    if let Some(Err(error)) = navigation_file.map(|path| {
        write_navigation_file(
            path,
            result.removed_path.is_some(),
            result.removed_path.as_deref(),
            &result.repo_root,
        )
    }) {
        eprintln!("warning: could not write navigation metadata: {error}");
    }

    match fmt {
        MergeFormat::PrintPaths => {
            println!("{root_str}");
            println!("{branch_name}");
            println!("{}", result.mainline);
            println!("{}", result.cleaned_up);
            println!("{removed_str}");
            println!("{}", result.pushed);
        }
        MergeFormat::PrintPathsV2 => {
            println!("{root_str}");
            println!("{branch_name}");
            println!("{}", result.mainline);
            println!("{}", result.cleaned_up);
            println!("{removed_str}");
            println!("{}", result.pushed);
            println!("{}", result.destination_path.display());
        }
        MergeFormat::Json => {
            let event = result.removed_path.as_ref().map(|_| "reset".to_string());
            print_json(&JsonMergeResponse {
                ok: true,
                event,
                message: format!("merged '{}' into {}", branch_name, result.mainline),
                branch: branch_name.to_string(),
                mainline: result.mainline.clone(),
                destination_path: result.destination_path.display().to_string(),
                repo_root: root_str,
                cleaned_up: result.cleaned_up,
                branch_deleted: result.branch_deleted,
                removed_path: if result.removed_path.is_some() {
                    Some(removed_str)
                } else {
                    None
                },
                pushed: result.pushed,
                warnings: result.warnings.clone(),
                preflight: Some(JsonMergePreflight::from_preflight(&result.preflight)),
                refusal: None,
                operation: worktree::merge_operation_report_if_present(repo)
                    .map(|report| json_merge_operation(&report)),
                inspect: false,
            })?
        }
        MergeFormat::Human => {
            println!("Merged '{}' into {}", branch_name, result.mainline);
            println!(
                "Destination worktree: {}",
                result.destination_path.display()
            );
            match (&result.removed_path, result.branch_deleted) {
                (Some(_), true) => {
                    println!("Removed worktree and branch '{}'", branch_name);
                }
                (Some(path), false) => {
                    println!("Removed worktree: {}", path.display());
                    println!("Source branch cleanup remains pending");
                }
                (None, _) => {}
            }
            if result.pushed {
                println!("Pushed {} to origin", result.mainline);
            }
        }
    }
    for warning in &result.warnings {
        eprintln!("warning: {warning}");
    }
    Ok(())
}

fn json_merge_operation(report: &worktree::MergeOperationReport) -> JsonMergeOperation {
    JsonMergeOperation {
        state: report.state.clone(),
        source: report.source.clone(),
        destination: report.destination.clone(),
        source_path: report
            .source_path
            .as_ref()
            .map(|path| path.display().to_string()),
        destination_path: report
            .destination_path
            .as_ref()
            .map(|path| path.display().to_string()),
        unresolved_paths: report.unresolved_paths.clone(),
        push: report.push,
        cleanup: report.cleanup,
        keep_branch: report.keep_branch,
        worktree_removed: report.worktree_removed,
        branch_deleted: report.branch_deleted,
        push_done: report.push_done,
        pending_actions: report.pending_actions.clone(),
        recovery: report.recovery.clone(),
        state_path: Some(report.state_path.display().to_string()),
    }
}

fn empty_merge_operation(message: &str) -> JsonMergeOperation {
    JsonMergeOperation {
        state: "unknown".to_string(),
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
        recovery: Some(message.to_string()),
        state_path: None,
    }
}

fn print_operation_error(
    repo: &domain::RepoRoot,
    fmt: MergeFormat,
    error: &AppError,
) -> Result<()> {
    if fmt == MergeFormat::Json {
        let operation = worktree::merge_operation_status(repo)
            .ok()
            .map(|report| json_merge_operation(&report))
            .unwrap_or_else(|| empty_merge_operation(&error.message));
        print_json(&JsonMergeOperationResponse {
            ok: false,
            message: error.message.clone(),
            operation,
        })?;
    }
    Err(AppError {
        code: error.code,
        message: error.message.clone(),
    })
}

fn print_operation_human(report: &worktree::MergeOperationReport) {
    println!("Managed merge operation: {}", report.state);
    if let (Some(source), Some(destination)) = (&report.source, &report.destination) {
        println!("  Source: {source}");
        println!("  Destination: {destination}");
    }
    if let Some(path) = &report.destination_path {
        println!("  Destination worktree: {}", path.display());
    }
    if report.unresolved_paths.is_empty() {
        println!("  Unresolved paths: none");
    } else {
        println!("  Unresolved paths:");
        for path in &report.unresolved_paths {
            println!("    {path}");
        }
    }
    println!("  Pending actions:");
    if report.pending_actions.is_empty() {
        println!("    none");
    } else {
        for action in &report.pending_actions {
            println!("    {action}");
        }
    }
    if let Some(recovery) = &report.recovery {
        println!("  Recovery: {recovery}");
    }
}

fn cmd_merge_status(repo: &domain::RepoRoot, fmt: MergeFormat) -> Result<()> {
    if matches!(fmt, MergeFormat::PrintPaths | MergeFormat::PrintPathsV2) {
        return Err(AppError::usage(
            "--status cannot be used with path output formats".to_string(),
        ));
    }
    let report = worktree::merge_operation_status(repo)?;
    match fmt {
        MergeFormat::Json => print_json(&JsonMergeOperationResponse {
            ok: !matches!(report.state.as_str(), "stale" | "interrupted" | "corrupt"),
            message: format!("managed merge operation is {}", report.state),
            operation: json_merge_operation(&report),
        })?,
        MergeFormat::Human => print_operation_human(&report),
        MergeFormat::PrintPaths | MergeFormat::PrintPathsV2 => unreachable!(),
    }
    if matches!(report.state.as_str(), "stale" | "interrupted" | "corrupt") {
        return Err(AppError::conflict(report.recovery.unwrap_or_else(|| {
            "managed merge state requires manual recovery".to_string()
        })));
    }
    Ok(())
}

fn cmd_merge_abort(repo: &domain::RepoRoot, fmt: MergeFormat) -> Result<()> {
    if matches!(fmt, MergeFormat::PrintPaths | MergeFormat::PrintPathsV2) {
        return Err(AppError::usage(
            "--abort cannot be used with path output formats".to_string(),
        ));
    }
    let report = match worktree::merge_abort_operation(repo) {
        Ok(report) => report,
        Err(error) => return print_operation_error(repo, fmt, &error),
    };
    let message = format!(
        "aborted managed merge of '{}' into {}",
        report.source.as_deref().unwrap_or("(unknown)"),
        report.destination.as_deref().unwrap_or("(unknown)")
    );
    match fmt {
        MergeFormat::Json => print_json(&JsonMergeOperationResponse {
            ok: true,
            message,
            operation: json_merge_operation(&report),
        })?,
        MergeFormat::Human => {
            println!("{message}");
            print_operation_human(&report);
        }
        MergeFormat::PrintPaths | MergeFormat::PrintPathsV2 => unreachable!(),
    }
    Ok(())
}

fn cmd_merge_continue(
    repo: &domain::RepoRoot,
    fmt: MergeFormat,
    navigation_file: Option<&std::path::Path>,
) -> Result<()> {
    let result = match worktree::merge_continue(repo) {
        Ok(result) => result,
        Err(error) => return print_operation_error(repo, fmt, &error),
    };
    print_merge_result(repo, result, fmt, navigation_file)
}

fn print_merge_preflight(preflight: &worktree::MergePreflight) {
    println!(
        "Merge preflight: destination '{}' at {}",
        preflight.destination,
        preflight.destination_path.display()
    );
    match (&preflight.upstream, preflight.ahead, preflight.behind) {
        (Some(upstream), Some(ahead), Some(behind)) => {
            println!("  Destination upstream: {upstream}");
            println!(
                "  Topology: {} (ahead {ahead}, behind {behind})",
                merge_topology_label(preflight.topology)
            );
            if preflight.topology == worktree::MergeTopology::Ahead {
                println!(
                    "  WARNING: destination is AHEAD of upstream by {ahead} commit{}; merge/push will preserve those local commits",
                    if ahead == 1 { "" } else { "s" }
                );
            }
        }
        (Some(upstream), None, None) => {
            println!("  Destination upstream: {upstream} (unavailable locally)");
            println!(
                "  Topology: {} (ahead/behind counts unavailable)",
                merge_topology_label(preflight.topology)
            );
        }
        (None, _, _) => {
            println!("  Destination upstream: none");
            println!("  Topology: no upstream (ahead/behind counts unavailable)");
        }
        _ => {
            println!("  Destination upstream: unavailable");
            println!(
                "  Topology: {} (ahead/behind counts unavailable)",
                merge_topology_label(preflight.topology)
            );
        }
    }

    print_source_history(preflight);
    match &preflight.refusal {
        Some(refusal) => println!("  REFUSED ({}): {}", refusal.kind, refusal.message),
        None => println!("  Result: ready for content merge"),
    }
}

fn print_source_history(preflight: &worktree::MergePreflight) {
    match preflight.source_history {
        worktree::SourceHistory::NotMerged => {}
        worktree::SourceHistory::AlreadyMerged => println!(
            "  Source history: source '{}' is already merged into the destination",
            preflight.source
        ),
        worktree::SourceHistory::MergedThenReverted => {
            let commit = preflight
                .reverted_commit
                .as_deref()
                .unwrap_or("an earlier source commit");
            println!(
                "  WARNING: source '{}' was previously merged then reverted (revert of {commit}); merging again may intentionally reintroduce reverted changes",
                preflight.source
            );
        }
    }
}

fn merge_topology_label(topology: worktree::MergeTopology) -> &'static str {
    match topology {
        worktree::MergeTopology::NoUpstream => "no upstream",
        worktree::MergeTopology::UpstreamUnavailable => "upstream unavailable",
        worktree::MergeTopology::Synchronized => "synchronized",
        worktree::MergeTopology::Ahead => "ahead",
        worktree::MergeTopology::Behind => "behind",
        worktree::MergeTopology::Diverged => "diverged",
    }
}

fn print_merge_inspection(
    repo: &domain::RepoRoot,
    preflight: &worktree::MergePreflight,
    fmt: MergeFormat,
) -> Result<()> {
    match fmt {
        MergeFormat::Json => print_json(&JsonMergeResponse {
            ok: true,
            event: None,
            message: format!(
                "inspected merge of '{}' into {}",
                preflight.source, preflight.destination
            ),
            branch: preflight.source.clone(),
            mainline: preflight.destination.clone(),
            destination_path: preflight.destination_path.display().to_string(),
            repo_root: repo.display().to_string(),
            cleaned_up: false,
            branch_deleted: false,
            removed_path: None,
            pushed: false,
            warnings: Vec::new(),
            preflight: Some(JsonMergePreflight::from_preflight(preflight)),
            refusal: preflight.refusal.as_ref().map(|refusal| JsonMergeRefusal {
                kind: refusal.kind.clone(),
                reason: refusal.reason.clone(),
                message: Some(refusal.message.clone()),
            }),
            operation: None,
            inspect: true,
        }),
        MergeFormat::Human => {
            println!(
                "Inspecting merge of '{}' (no repository mutation)",
                preflight.source
            );
            print_merge_preflight(preflight);
            Ok(())
        }
        MergeFormat::PrintPaths | MergeFormat::PrintPathsV2 => Err(AppError::usage(
            "--inspect cannot be used with path output formats".to_string(),
        )),
    }
}

fn report_merge_failure(
    repo: &domain::RepoRoot,
    preflight: &worktree::MergePreflight,
    fmt: MergeFormat,
    error: AppError,
    refusal: Option<JsonMergeRefusal>,
) -> Result<()> {
    if fmt == MergeFormat::Json {
        let json_refusal = refusal.or_else(|| {
            preflight
                .refusal
                .as_ref()
                .map(|preflight_refusal| JsonMergeRefusal {
                    kind: preflight_refusal.kind.clone(),
                    reason: preflight_refusal.reason.clone(),
                    message: Some(preflight_refusal.message.clone()),
                })
        });
        print_json(&JsonMergeResponse {
            ok: false,
            event: None,
            message: error.message.clone(),
            branch: preflight.source.clone(),
            mainline: preflight.destination.clone(),
            destination_path: preflight.destination_path.display().to_string(),
            repo_root: repo.display().to_string(),
            cleaned_up: false,
            branch_deleted: false,
            removed_path: None,
            pushed: false,
            warnings: Vec::new(),
            preflight: Some(JsonMergePreflight::from_preflight(preflight)),
            refusal: json_refusal,
            operation: worktree::merge_operation_report_if_present(repo)
                .map(|report| json_merge_operation(&report)),
            inspect: false,
        })?;
    }
    Err(error)
}

fn cmd_remove(
    branch: Option<BranchName>,
    force: bool,
    keep_branch: bool,
    repo: Option<PathBuf>,
    fmt: RemoveFormat,
    navigation_file: Option<&std::path::Path>,
) -> Result<()> {
    let repo = resolve_repo(repo)?;

    let resolved_branch = match branch {
        Some(b) => Some(b),
        None => resolve_action_branch(&repo, fmt == RemoveFormat::Json, "remove")?,
    };

    let result = if keep_branch {
        worktree::remove_with_keep_branch(&repo, resolved_branch.as_ref(), force, true)?
    } else {
        worktree::remove(&repo, resolved_branch.as_ref(), force)?
    };

    let removed_str = result.removed_path.display().to_string();
    let root_str = result.repo_root.display().to_string();
    let branch_name = &result.branch;

    if let Some(Err(error)) = navigation_file.map(|path| match fmt {
        RemoveFormat::PrintPaths => write_navigation_file_with_cleanup(
            path,
            true,
            Some(&result.removed_path),
            &result.repo_root,
            Some(result.branch_deleted),
        ),
        _ => write_navigation_file(path, true, Some(&result.removed_path), &result.repo_root),
    }) {
        eprintln!("warning: could not write navigation metadata: {error}");
    }

    match fmt {
        RemoveFormat::PrintPaths => {
            // This is a stable legacy protocol used by installed shell
            // bindings: exactly removed_path, repo_root, and branch.
            // Lifecycle status is available in the explicit JSON response.
            println!("{removed_str}");
            println!("{root_str}");
            println!("{branch_name}");
        }
        RemoveFormat::Json => {
            let resp = JsonResponse::success(if result.branch_deleted {
                format!("removed worktree for branch '{branch_name}'")
            } else {
                format!("removed worktree and kept branch '{branch_name}'")
            })
            .with_event("reset")
            .with_repo_root(&root_str)
            .with_removed_path(&removed_str)
            .with_branch(branch_name.as_str())
            .with_worktree_removed(true)
            .with_branch_deleted(result.branch_deleted);
            print_json(&resp)?;
        }
        RemoveFormat::Human => {
            if result.branch_deleted {
                println!("Removed worktree and branch '{branch_name}' ({removed_str})");
            } else {
                println!("Removed worktree and kept branch '{branch_name}' ({removed_str})");
            }
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
    let location = if entry.path.is_some() {
        ""
    } else {
        " (branch only)"
    };

    match &entry.status {
        worktree::IntegrationStatus::Integrated(method) => {
            let method_str = match method {
                worktree::IntegrationMethod::Merged => "merged",
                worktree::IntegrationMethod::Rebase => "rebase",
            };
            let branch = entry.branch.as_deref().unwrap_or("(unknown)");
            println!("  ✓ {branch:<20} integrated ({method_str}){location}");
        }
        worktree::IntegrationStatus::NotIntegrated => {
            let branch = entry.branch.as_deref().unwrap_or("(unknown)");
            println!("  ✗ {branch:<20} not integrated{location}");
        }
        worktree::IntegrationStatus::NoBranch => {
            println!("  ⚠ {:<20} no branch (detached HEAD)", "(detached)");
        }
    }
}

fn print_prune_dry_run_summary(entries: &[worktree::WorktreePruneEntry], prunable: usize) {
    let has_branch_only = entries.iter().any(|entry| entry.path.is_none());
    match (entries.is_empty(), prunable, has_branch_only) {
        (true, _, _) => println!("\nNo worktrees to prune."),
        (false, 0, true) => println!("\nNo integrated worktrees or branches found."),
        (false, 0, false) => println!("\nNo integrated worktrees found."),
        (false, _, _) => println!(
            "\n{prunable} integrated worktree{} or branch{} can be pruned. Run with --execute to remove worktrees and delete branches.",
            if prunable == 1 { "" } else { "s" },
            if prunable == 1 { "" } else { "es" }
        ),
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
                        path: e.path.as_ref().map(|p| p.display().to_string()),
                        worktree_present: e.path.is_some(),
                        branch_will_be_deleted: matches!(
                            &e.status,
                            worktree::IntegrationStatus::Integrated(_)
                        ),
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
            print_prune_dry_run_summary(&result.entries, prunable);
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
                    path: e.path.as_ref().map(|p| p.display().to_string()),
                    worktree_removed: e.worktree_removed,
                    branch_deleted: e.branch_deleted,
                })
                .collect();

            let skipped: Vec<JsonSkippedEntry> = result
                .skipped
                .iter()
                .map(|e| JsonSkippedEntry {
                    branch: e.branch.clone(),
                    reason: e.reason.clone(),
                    path: e.path.as_ref().map(|p| p.display().to_string()),
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
                match (entry.worktree_removed, entry.branch_deleted) {
                    (true, true) => {
                        println!("  Removed {} worktree and branch", entry.branch);
                    }
                    (true, false) => {
                        println!("  Removed {} worktree; kept branch", entry.branch);
                    }
                    (false, true) => {
                        println!("  Deleted {} branch (no worktree)", entry.branch);
                    }
                    (false, false) => {
                        println!("  Kept {} branch (no worktree)", entry.branch);
                    }
                }
            }
            for entry in &result.skipped {
                let label = entry.branch.as_deref().unwrap_or("(detached)");
                let reason = match entry.reason.as_str() {
                    "not_integrated" => "not integrated",
                    "no_branch" => "no branch",
                    "removal_failed" => "removal failed",
                    "stale_marker" => "stale preservation marker",
                    other => other,
                };
                println!("  Skipped {label} ({reason})");
            }
            for w in &result.warnings {
                eprintln!("warning: {w}");
            }
            let worktrees_removed = result
                .pruned
                .iter()
                .filter(|entry| entry.worktree_removed)
                .count();
            let branches_deleted = result
                .pruned
                .iter()
                .filter(|entry| entry.branch_deleted)
                .count();
            if worktrees_removed == 0 && branches_deleted == 0 {
                println!("\nNo worktrees pruned.");
            } else if result.pruned.len() == worktrees_removed
                && branches_deleted == result.pruned.len()
            {
                println!(
                    "\nPruned {worktrees_removed} worktree{}.",
                    if worktrees_removed == 1 { "" } else { "s" }
                );
            } else {
                println!(
                    "\nPruned {worktrees_removed} worktree{} and deleted {branches_deleted} branch{}.",
                    if worktrees_removed == 1 { "" } else { "s" },
                    if branches_deleted == 1 { "" } else { "es" }
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
