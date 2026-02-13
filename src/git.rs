use std::path::{Path, PathBuf};
use std::process::Command as Cmd;

use crate::domain::{RepoRoot, Worktree};
use crate::error::{AppError, Result};

/// Run a git command and return stdout on success.
fn git(args: &[&str], cwd: &Path) -> Result<String> {
    let output = Cmd::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .map_err(|e| AppError::git(format!("failed to run git: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(AppError::git(stderr.trim().to_string()));
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Resolve the repository root from a starting path.
pub fn repo_root(start: &Path) -> Result<RepoRoot> {
    let root = git(&["rev-parse", "--show-toplevel"], start)
        .map_err(|_| AppError::not_a_repo(format!("not a git repository: {}", start.display())))?;
    Ok(RepoRoot(PathBuf::from(root)))
}

/// List all worktrees via `git worktree list --porcelain`.
pub fn list_worktrees(repo: &RepoRoot) -> Result<Vec<Worktree>> {
    // Prune stale worktrees first (matches current behavior expectation).
    let _ = git(&["worktree", "prune"], &repo.0);

    let raw = git(&["worktree", "list", "--porcelain"], &repo.0)?;
    parse_worktree_porcelain(&raw, repo)
}

/// Parse porcelain output from `git worktree list --porcelain`.
fn parse_worktree_porcelain(raw: &str, repo: &RepoRoot) -> Result<Vec<Worktree>> {
    let mut worktrees = Vec::new();
    let mut path: Option<PathBuf> = None;
    let mut commit = String::new();
    let mut branch: Option<String> = None;
    let mut is_bare = false;

    for line in raw.lines() {
        if let Some(p) = line.strip_prefix("worktree ") {
            path = Some(PathBuf::from(p));
            commit.clear();
            branch = None;
            is_bare = false;
        } else if let Some(h) = line.strip_prefix("HEAD ") {
            commit = h[..7.min(h.len())].to_string();
        } else if let Some(b) = line.strip_prefix("branch ") {
            // branch is like refs/heads/main
            branch = Some(b.strip_prefix("refs/heads/").unwrap_or(b).to_string());
        } else if line == "bare" {
            is_bare = true;
        } else if line.is_empty() {
            if let Some(wt_path) = path.take() {
                if is_bare {
                    continue;
                }
                let is_main = wt_path == repo.0;
                worktrees.push(Worktree {
                    path: wt_path,
                    branch: branch.take(),
                    commit: commit.clone(),
                    is_main,
                });
            }
        }
    }

    // Handle last entry if no trailing blank line.
    if let Some(wt_path) = path.take() {
        if !is_bare {
            let is_main = wt_path == repo.0;
            worktrees.push(Worktree {
                path: wt_path,
                branch,
                commit,
                is_main,
            });
        }
    }

    Ok(worktrees)
}

/// Add a new worktree.
pub fn add_worktree(repo: &RepoRoot, dir: &Path, branch: &str, base: Option<&str>) -> Result<()> {
    let base_rev = base.unwrap_or("HEAD");
    let mut args = vec!["worktree", "add", "-b", branch];
    let dir_str = dir.display().to_string();
    args.push(&dir_str);
    args.push(base_rev);

    git(&args, &repo.0)?;
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

    git(&args, &repo.0)?;
    Ok(())
}

/// Delete a local branch.
pub fn delete_branch(repo: &RepoRoot, branch: &str, force: bool) -> Result<()> {
    let flag = if force { "-D" } else { "-d" };
    git(&["branch", flag, branch], &repo.0)?;
    Ok(())
}

/// Check if a local branch exists.
pub fn branch_exists(repo: &RepoRoot, branch: &str) -> bool {
    git(
        &["rev-parse", "--verify", &format!("refs/heads/{branch}")],
        &repo.0,
    )
    .is_ok()
}

/// Resolve a revision to confirm it exists.
pub fn rev_exists(repo: &RepoRoot, rev: &str) -> bool {
    git(&["rev-parse", "--verify", rev], &repo.0).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_porcelain_basic() {
        let repo = RepoRoot(PathBuf::from("/home/user/project"));
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

        assert!(result[0].is_main);
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
}
