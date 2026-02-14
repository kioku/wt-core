use std::path::{Path, PathBuf};
use std::process::Command as Cmd;

use crate::domain::{BranchName, RepoRoot, Worktree};
use crate::error::{AppError, Result};

/// Environment variables that can leak from parent git processes (e.g. hooks)
/// and interfere with our subprocess calls.
const GIT_ENV_OVERRIDES: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_PREFIX",
];

/// Run a git command and return stdout on success.
///
/// Clears inherited `GIT_*` environment variables that could redirect
/// operations to the wrong repository (common when invoked from git hooks).
fn git(args: &[&str], cwd: &Path) -> Result<String> {
    let mut cmd = Cmd::new("git");
    cmd.args(args).current_dir(cwd);

    for var in GIT_ENV_OVERRIDES {
        cmd.env_remove(var);
    }

    let output = cmd
        .output()
        .map_err(|e| AppError::git(format!("failed to run git: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let msg = stderr.trim().to_string();
        return Err(classify_git_error(msg));
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Inspect git stderr to map known error patterns to the correct exit code.
fn classify_git_error(msg: String) -> AppError {
    let lower = msg.to_lowercase();

    if lower.contains("not a git repository") {
        return AppError::not_a_repo(msg);
    }

    if lower.contains("unmerged")
        || lower.contains("modified")
        || lower.contains("dirty")
        || lower.contains("already exists")
        || lower.contains("already checked out")
        || lower.contains("is not fully merged")
    {
        return AppError::conflict(msg);
    }

    AppError::git(msg)
}

/// Resolve the main repository root from a starting path.
///
/// Uses `--git-common-dir` so this returns the main worktree root even
/// when invoked from inside a linked worktree.
pub fn repo_root(start: &Path) -> Result<RepoRoot> {
    // First confirm we are inside a git repo.
    let toplevel = git(&["rev-parse", "--show-toplevel"], start)
        .map_err(|_| AppError::not_a_repo(format!("not a git repository: {}", start.display())))?;

    // --git-common-dir returns the shared .git directory.  For the main
    // worktree this is `<repo>/.git`; for a linked worktree it is
    // `<main-repo>/.git/worktrees/<name>` → common dir = `<main-repo>/.git`.
    // The returned path, when relative, is relative to the cwd of the git
    // process (i.e. `start`), so we must resolve it against `start`.
    let common =
        git(&["rev-parse", "--git-common-dir"], start).unwrap_or_else(|_| ".git".to_string());

    let common_path = PathBuf::from(start).join(&common);
    let common_canonical = common_path.canonicalize().unwrap_or(common_path);

    // The main repo root is the parent of the common .git directory.
    let root = common_canonical
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from(&toplevel));

    Ok(RepoRoot(root))
}

/// List all worktrees via `git worktree list --porcelain`.
pub fn list_worktrees(repo: &RepoRoot) -> Result<Vec<Worktree>> {
    // Prune stale worktrees first (matches current behavior expectation).
    let _ = git(&["worktree", "prune"], repo.as_ref());

    let raw = git(&["worktree", "list", "--porcelain"], repo.as_ref())?;
    parse_worktree_porcelain(&raw, repo)
}

/// A raw worktree entry parsed from porcelain lines.
struct RawEntry {
    path: PathBuf,
    commit: String,
    branch: Option<String>,
    is_bare: bool,
}

/// Parse a single porcelain block (lines between blank separators).
fn parse_porcelain_block(block: &str) -> Option<RawEntry> {
    let mut path: Option<PathBuf> = None;
    let mut commit = String::new();
    let mut branch = None;
    let mut is_bare = false;

    for line in block.lines() {
        apply_porcelain_line(line, &mut path, &mut commit, &mut branch, &mut is_bare);
    }

    path.map(|p| RawEntry {
        path: p,
        commit,
        branch,
        is_bare,
    })
}

fn apply_porcelain_line(
    line: &str,
    path: &mut Option<PathBuf>,
    commit: &mut String,
    branch: &mut Option<String>,
    is_bare: &mut bool,
) {
    if let Some(p) = line.strip_prefix("worktree ") {
        *path = Some(PathBuf::from(p));
        return;
    }
    if let Some(h) = line.strip_prefix("HEAD ") {
        *commit = h[..7.min(h.len())].to_string();
        return;
    }
    if let Some(b) = line.strip_prefix("branch ") {
        *branch = Some(b.strip_prefix("refs/heads/").unwrap_or(b).to_string());
        return;
    }
    if line == "bare" {
        *is_bare = true;
    }
}

/// Parse porcelain output from `git worktree list --porcelain`.
///
/// The first entry in `git worktree list` is always the main worktree
/// (per Git spec), so we use index position rather than path comparison
/// to set `is_main`.  This avoids mismatches when paths differ in
/// canonicalization (e.g. symlinks).
fn parse_worktree_porcelain(raw: &str, _repo: &RepoRoot) -> Result<Vec<Worktree>> {
    let blocks: Vec<&str> = raw.split("\n\n").collect();

    let worktrees = blocks
        .iter()
        .filter_map(|block| parse_porcelain_block(block))
        .filter(|entry| !entry.is_bare)
        .enumerate()
        .map(|(idx, entry)| Worktree {
            path: entry.path,
            branch: entry.branch,
            commit: entry.commit,
            is_main: idx == 0,
        })
        .collect();

    Ok(worktrees)
}

/// Add a new worktree.
pub fn add_worktree(
    repo: &RepoRoot,
    dir: &Path,
    branch: &BranchName,
    base: Option<&str>,
) -> Result<()> {
    let base_rev = base.unwrap_or("HEAD");
    let branch_str = branch.as_str();
    let mut args = vec!["worktree", "add", "-b", branch_str];
    let dir_str = dir.display().to_string();
    args.push(&dir_str);
    args.push(base_rev);

    git(&args, repo.as_ref())?;
    Ok(())
}

/// Remove a worktree directory.
pub fn remove_worktree(repo: &RepoRoot, dir: &Path, force: bool) -> Result<()> {
    let dir_str = dir.display().to_string();
    let mut args = vec!["worktree", "remove"];
    if force {
        args.push("--force");
    }
    args.push(&dir_str);

    git(&args, repo.as_ref())?;
    Ok(())
}

/// Delete a local branch.
pub fn delete_branch(repo: &RepoRoot, branch: &BranchName, force: bool) -> Result<()> {
    let flag = if force { "-D" } else { "-d" };
    git(&["branch", flag, branch.as_str()], repo.as_ref())?;
    Ok(())
}

/// Run a git command and return true if it exits successfully.
///
/// Used for commands like `merge-base --is-ancestor` that communicate
/// their result via exit code rather than stdout.
fn git_success(args: &[&str], cwd: &Path) -> bool {
    let mut cmd = Cmd::new("git");
    cmd.args(args).current_dir(cwd);

    for var in GIT_ENV_OVERRIDES {
        cmd.env_remove(var);
    }

    cmd.output().map(|o| o.status.success()).unwrap_or(false)
}

/// Check whether `branch` is an ancestor of `mainline`.
///
/// Uses `git merge-base --is-ancestor <branch> <mainline>`.
/// Returns `true` if all commits on `branch` are reachable from `mainline`.
pub fn is_ancestor(repo: &RepoRoot, branch: &str, mainline: &str) -> bool {
    git_success(
        &["merge-base", "--is-ancestor", branch, mainline],
        repo.as_ref(),
    )
}

/// Run `git cherry <mainline> <branch>` and return true if every commit
/// is prefixed with `-`, meaning every patch has an equivalent in mainline
/// (covers rebase/cherry-pick merges).
///
/// Returns `false` if cherry produces no output or any line starts with `+`.
pub fn cherry(repo: &RepoRoot, mainline: &str, branch: &str) -> bool {
    match git(&["cherry", mainline, branch], repo.as_ref()) {
        Ok(output) => {
            let lines: Vec<&str> = output.lines().collect();
            !lines.is_empty() && lines.iter().all(|l| l.starts_with('-'))
        }
        Err(_) => false,
    }
}

/// Auto-detect the mainline branch.
///
/// Resolution order:
/// 1. `refs/remotes/origin/HEAD` → resolve symbolic ref
/// 2. Local branch named `main`
/// 3. Local branch named `master`
/// 4. The main worktree's branch (first entry from `git worktree list`)
pub fn resolve_mainline(repo: &RepoRoot) -> Result<String> {
    // 1. Try origin/HEAD
    if let Ok(symref) = git(
        &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
        repo.as_ref(),
    ) {
        // symref is like "origin/main" — strip the remote prefix
        let branch = symref
            .strip_prefix("origin/")
            .unwrap_or(&symref)
            .to_string();
        return Ok(branch);
    }

    // 2. Check for local 'main'
    let main_name = BranchName::new("main");
    if branch_exists(repo, &main_name) {
        return Ok("main".to_string());
    }

    // 3. Check for local 'master'
    let master_name = BranchName::new("master");
    if branch_exists(repo, &master_name) {
        return Ok("master".to_string());
    }

    // 4. Fall back to main worktree's branch
    let worktrees = list_worktrees(repo)?;
    worktrees
        .iter()
        .find(|wt| wt.is_main)
        .and_then(|wt| wt.branch.clone())
        .ok_or_else(|| {
            AppError::git(
                "could not determine mainline branch; use --mainline to specify".to_string(),
            )
        })
}

/// Check if a local branch exists.
pub fn branch_exists(repo: &RepoRoot, branch: &BranchName) -> bool {
    let refspec = format!("refs/heads/{}", branch.as_str());
    git(&["rev-parse", "--verify", &refspec], repo.as_ref()).is_ok()
}

/// Resolve a revision to confirm it exists.
pub fn rev_exists(repo: &RepoRoot, rev: &str) -> bool {
    git(&["rev-parse", "--verify", rev], repo.as_ref()).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_porcelain_basic() {
        // repo path intentionally differs from the worktree path to prove
        // is_main is determined by index position, not path comparison.
        let repo = RepoRoot(PathBuf::from("/different/path"));
        let raw = "\
worktree /home/user/project
HEAD abc1234567890
branch refs/heads/main

worktree /home/user/project/.worktrees/feat-x--12345678
HEAD def4567890abc
branch refs/heads/feat-x

";
        let result = parse_worktree_porcelain(raw, &repo).expect("should parse");
        assert_eq!(result.len(), 2);

        assert!(result[0].is_main, "first entry is always the main worktree");
        assert_eq!(result[0].branch.as_deref(), Some("main"));
        assert_eq!(result[0].commit, "abc1234");

        assert!(!result[1].is_main);
        assert_eq!(result[1].branch.as_deref(), Some("feat-x"));
    }

    #[test]
    fn parse_porcelain_bare_skipped() {
        let repo = RepoRoot(PathBuf::from("/repo"));
        let raw = "\
worktree /repo
HEAD abc1234
bare

";
        let result = parse_worktree_porcelain(raw, &repo).expect("should parse");
        assert!(result.is_empty());
    }

    #[test]
    fn parse_porcelain_no_trailing_newline() {
        let repo = RepoRoot(PathBuf::from("/repo"));
        let raw = "worktree /repo\nHEAD abc1234\nbranch refs/heads/main";
        let result = parse_worktree_porcelain(raw, &repo).expect("should parse");
        assert_eq!(result.len(), 1);
        assert!(result[0].is_main);
    }

    #[test]
    fn classify_not_a_repo() {
        let err = classify_git_error(
            "fatal: not a git repository (or any of the parent directories)".to_string(),
        );
        assert_eq!(err.code, crate::error::ExitCode::NotARepo);
    }

    #[test]
    fn classify_already_exists_is_conflict() {
        let err = classify_git_error("fatal: 'feature/x' already exists".to_string());
        assert_eq!(err.code, crate::error::ExitCode::Conflict);
    }

    #[test]
    fn classify_already_checked_out_is_conflict() {
        let err = classify_git_error(
            "fatal: 'feature/x' is already checked out at '/repo/.worktrees/feat'".to_string(),
        );
        assert_eq!(err.code, crate::error::ExitCode::Conflict);
    }

    #[test]
    fn classify_not_fully_merged() {
        let err = classify_git_error("error: the branch 'x' is not fully merged".to_string());
        assert_eq!(err.code, crate::error::ExitCode::Conflict);
    }

    #[test]
    fn classify_dirty_is_conflict() {
        let err = classify_git_error("error: dirty worktree, use --force".to_string());
        assert_eq!(err.code, crate::error::ExitCode::Conflict);
    }

    #[test]
    fn classify_unknown_falls_to_git() {
        let err = classify_git_error("fatal: something unexpected".to_string());
        assert_eq!(err.code, crate::error::ExitCode::Git);
    }
}
