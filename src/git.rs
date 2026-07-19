use std::collections::HashSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command as Cmd;
use std::process::Stdio;

use crate::domain::{BranchName, RepoRoot, Worktree, WorktreeStats};
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

/// Remove inherited Git context that could redirect a command to another
/// repository (common when invoked from Git hooks).
pub(crate) fn sanitize_git_environment(cmd: &mut Cmd) {
    for var in GIT_ENV_OVERRIDES {
        cmd.env_remove(var);
    }
}

/// Run a git command and return stdout on success.
///
/// Clears inherited `GIT_*` environment variables that could redirect
/// operations to the wrong repository (common when invoked from git hooks).
fn git(args: &[&str], cwd: &Path) -> Result<String> {
    let mut cmd = Cmd::new("git");
    cmd.args(args).current_dir(cwd);
    sanitize_git_environment(&mut cmd);

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
        || lower.contains("merge conflict")
        || lower.contains("automatic merge failed")
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
    list_worktrees_readonly(repo)
}

/// List worktrees without pruning Git's worktree metadata.
///
/// Merge preflight and `--inspect` use this variant so inspection never
/// changes repository state, including stale-worktree administrative files.
pub fn list_worktrees_readonly(repo: &RepoRoot) -> Result<Vec<Worktree>> {
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
    let mut seen_paths = HashSet::new();

    let worktrees = blocks
        .iter()
        .filter_map(|block| parse_porcelain_block(block))
        .filter(|entry| !entry.is_bare)
        .filter(|entry| seen_paths.insert(entry.path.clone()))
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

/// Delete a local branch, using the main worktree as the merge base for
/// Git's safe `-d` check.
pub fn delete_branch(repo: &RepoRoot, branch: &BranchName, force: bool) -> Result<()> {
    delete_branch_at(repo.as_ref(), branch, force)
}

/// Delete a local branch using `path` as the current worktree context.
///
/// The context matters for `git branch -d`: Git checks whether the branch is
/// merged into the currently checked-out branch. Merge cleanup uses the
/// destination worktree so linked-target merges satisfy that check.
pub fn delete_branch_at(path: &Path, branch: &BranchName, force: bool) -> Result<()> {
    let flag = if force { "-D" } else { "-d" };
    git(&["branch", flag, branch.as_str()], path)?;
    Ok(())
}

/// Record that a branch should survive worktree removal until a later prune.
///
/// The marker is a private ref rather than a file in the repository, so the
/// state follows clones only when explicitly copied and does not alter branch
/// names or worktree directories. Its object ID is the branch incarnation
/// that was explicitly preserved; prune must not authorize a later incarnation.
pub fn mark_preserved_branch(repo: &RepoRoot, branch: &BranchName) -> Result<()> {
    let marker = format!("refs/wt-core/preserved/{}", branch.as_str());
    let branch_ref = format!("refs/heads/{}", branch.as_str());
    git(&["update-ref", &marker, &branch_ref], repo.as_ref())?;
    Ok(())
}

/// A branch preserved by `remove --keep-branch`, including its exact marker
/// object ID. The ID is intentionally retained through prune planning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreservedBranch {
    pub name: String,
    pub oid: String,
}

/// List branches explicitly preserved by `remove --keep-branch`.
pub fn list_preserved_branches(repo: &RepoRoot) -> Result<Vec<PreservedBranch>> {
    let output = git(
        &[
            "for-each-ref",
            "--format=%(refname:strip=3)%09%(objectname)",
            "refs/wt-core/preserved/",
        ],
        repo.as_ref(),
    )?;

    Ok(output
        .lines()
        .filter_map(|line| {
            let (name, oid) = line.split_once('\t')?;
            Some(PreservedBranch {
                name: name.to_string(),
                oid: oid.to_string(),
            })
        })
        .collect())
}

/// Return the marker object ID for a preserved branch, if one exists.
pub fn preserved_branch_oid(repo: &RepoRoot, branch: &BranchName) -> Result<Option<String>> {
    Ok(list_preserved_branches(repo)?
        .into_iter()
        .find(|preserved| preserved.name == branch.as_str())
        .map(|preserved| preserved.oid))
}

/// Restore a previously captured lifecycle marker object ID.
pub fn restore_preserved_branch(repo: &RepoRoot, branch: &BranchName, oid: &str) -> Result<()> {
    let marker = format!("refs/wt-core/preserved/{}", branch.as_str());
    git(&["update-ref", &marker, oid], repo.as_ref())?;
    Ok(())
}

/// Resolve the current object ID of a local branch.
pub fn branch_oid(repo: &RepoRoot, branch: &BranchName) -> Option<String> {
    let branch_ref = format!("refs/heads/{}^{{commit}}", branch.as_str());
    git(&["rev-parse", "--verify", &branch_ref], repo.as_ref()).ok()
}

/// Resolve a revision to its peeled commit object ID.
pub fn resolve_commit(repo: &RepoRoot, revision: &str) -> Result<String> {
    let peeled = format!("{revision}^{{commit}}");
    git(&["rev-parse", "--verify", &peeled], repo.as_ref())
}

/// Return a canonical short remote-tracking ref (`origin/topic`) when the
/// supplied revision names one directly or through its full ref name.
pub fn remote_branch_revision(repo: &RepoRoot, revision: &str) -> Option<String> {
    let short = revision.strip_prefix("refs/remotes/").unwrap_or(revision);
    let reference = format!("refs/remotes/{short}^{{commit}}");
    git(&["rev-parse", "--verify", &reference], repo.as_ref())
        .ok()
        .map(|_| short.to_string())
}

/// Return the local branch corresponding to a canonical remote ref, if one
/// exists. A remote ref itself is not a local worktree target.
pub fn local_branch_for_remote(repo: &RepoRoot, remote_revision: &str) -> Option<String> {
    let (_, branch) = remote_revision.split_once('/')?;
    let branch_name = BranchName::new(branch);
    branch_exists(repo, &branch_name).then_some(branch.to_string())
}

/// Resolve `HEAD` to its checked-out local branch when it is symbolic.
pub fn current_branch(repo: &RepoRoot) -> Option<String> {
    git(&["symbolic-ref", "--short", "HEAD"], repo.as_ref()).ok()
}

/// Remove the lifecycle marker for a preserved branch.
pub fn clear_preserved_branch(repo: &RepoRoot, branch: &BranchName) -> Result<()> {
    let marker = format!("refs/wt-core/preserved/{}", branch.as_str());
    git(&["update-ref", "-d", &marker], repo.as_ref())?;
    Ok(())
}

/// Run a git command and return true if it exits successfully.
///
/// Used for commands like `merge-base --is-ancestor` that communicate
/// their result via exit code rather than stdout.
fn git_success(args: &[&str], cwd: &Path) -> bool {
    let mut cmd = Cmd::new("git");
    cmd.args(args).current_dir(cwd);
    sanitize_git_environment(&mut cmd);

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

/// Return whether the source and destination have equivalent content.
///
/// `git cherry` detects rebased and cherry-picked integrations, while an
/// exact tree comparison covers a squash merge whose combined commit is not
/// equivalent to any one source commit. This is deliberately conservative:
/// it never treats a branch as integrated merely because a commit message or
/// object name appears in the destination log.
pub fn patch_equivalent(repo: &RepoRoot, destination: &str, source: &str) -> bool {
    cherry(repo, destination, source)
        || git_success(
            &["diff", "--quiet", destination, source, "--"],
            repo.as_ref(),
        )
}

/// Try to resolve `refs/remotes/origin/HEAD` to a usable branch name.
///
/// Returns the local branch name if it exists, otherwise the full remote
/// ref (e.g. `origin/main`) so git commands can still resolve it.
fn resolve_origin_head(repo: &RepoRoot) -> Option<String> {
    let symref = git(
        &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
        repo.as_ref(),
    )
    .ok()?;

    let local = symref
        .strip_prefix("origin/")
        .unwrap_or(&symref)
        .to_string();
    let local_bn = BranchName::new(&local);

    if branch_exists(repo, &local_bn) {
        return Some(local);
    }
    Some(symref)
}

/// Auto-detect the mainline branch.
///
/// Resolution order:
/// 1. `refs/remotes/origin/HEAD` → resolve symbolic ref
/// 2. Local branch named `main`
/// 3. Local branch named `master`
/// 4. The main worktree's branch (first entry from `git worktree list`)
pub fn resolve_mainline(repo: &RepoRoot) -> Result<String> {
    resolve_mainline_with_worktree_listing(repo, true)
}

/// Resolve the mainline without pruning worktree metadata.
///
/// This is used by merge preflight because inspection must be read-only.
pub fn resolve_mainline_readonly(repo: &RepoRoot) -> Result<String> {
    resolve_mainline_with_worktree_listing(repo, false)
}

fn resolve_mainline_with_worktree_listing(repo: &RepoRoot, prune: bool) -> Result<String> {
    // 1. Try origin/HEAD — prefer the local branch name if it exists,
    //    otherwise use the full remote ref so git commands can resolve it
    //    even when there is no local tracking branch.
    if let Some(name) = resolve_origin_head(repo) {
        return Ok(name);
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
    let worktrees = if prune {
        list_worktrees(repo)?
    } else {
        list_worktrees_readonly(repo)?
    };
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

/// Return the configured upstream ref for a checked-out branch.
///
/// `for-each-ref` reads branch configuration without requiring the upstream
/// object to exist. This keeps a configured-but-stale remote visible to
/// callers instead of misreporting it as a branch with no upstream.
pub fn branch_upstream(path: &Path, branch: &str) -> Result<Option<String>> {
    let branch_ref = format!("refs/heads/{branch}");
    let output = git(
        &["for-each-ref", "--format=%(upstream:short)", &branch_ref],
        path,
    )?;
    Ok((!output.is_empty()).then_some(output))
}

/// Return ahead and behind commit counts for a branch and its upstream.
///
/// The first value is commits the branch is behind; the second is commits it
/// is ahead, matching Git's `--left-right --count` ordering. `None` means the
/// configured upstream ref is unavailable locally; this is distinct from a
/// branch with no configured upstream.
pub fn upstream_counts(path: &Path, upstream: &str, branch: &str) -> Result<Option<(u32, u32)>> {
    if !git_success(&["rev-parse", "--verify", upstream], path) {
        return Ok(None);
    }

    let range = format!("{upstream}...{branch}");
    let output = git(&["rev-list", "--left-right", "--count", &range], path)?;
    let mut fields = output.split_whitespace();
    let behind = fields
        .next()
        .and_then(|value| value.parse::<u32>().ok())
        .ok_or_else(|| AppError::git("failed to parse upstream behind count".to_string()))?;
    let ahead = fields
        .next()
        .and_then(|value| value.parse::<u32>().ok())
        .ok_or_else(|| AppError::git("failed to parse upstream ahead count".to_string()))?;
    Ok(Some((ahead, behind)))
}

/// Compute a stable patch id for a revision range.
///
/// The caller can reverse the range endpoints when it needs to compare an
/// inverse patch. An empty diff has no patch id because it cannot prove that
/// any content was reverted.
fn diff_patch_id(path: &Path, from: &str, to: &str) -> Option<String> {
    let mut diff = Cmd::new("git");
    diff.args(["diff", "--binary", "--no-ext-diff", from, to, "--"])
        .current_dir(path)
        .stdout(Stdio::piped());
    for var in GIT_ENV_OVERRIDES {
        diff.env_remove(var);
    }
    let diff_output = diff.output().ok()?;
    if !diff_output.status.success() || diff_output.stdout.is_empty() {
        return None;
    }

    let mut patch_id = Cmd::new("git");
    patch_id.args(["patch-id", "--stable"]).current_dir(path);
    for var in GIT_ENV_OVERRIDES {
        patch_id.env_remove(var);
    }
    let mut child = patch_id
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    child.stdin.take()?.write_all(&diff_output.stdout).ok()?;
    let output = child.wait_with_output().ok()?;
    if !output.status.success() {
        return None;
    }

    output
        .stdout
        .split(|byte| byte.is_ascii_whitespace())
        .find(|field| !field.is_empty())
        .map(|field| String::from_utf8_lossy(field).into_owned())
}

/// Confirm that a revert commit actually reverses the referenced commit.
///
/// Git's standard revert message is user-controlled, so the message alone is
/// not evidence. Compare the referenced commit's forward patch with the
/// revert commit's reverse patch; an empty or unrelated commit is rejected.
fn revert_reverses_commit(path: &Path, revert_commit: &str, reverted: &str) -> bool {
    let reverted_parent = format!("{reverted}^1");
    let revert_parent = format!("{revert_commit}^1");
    diff_patch_id(path, &reverted_parent, reverted).is_some_and(|original| {
        diff_patch_id(path, revert_commit, &revert_parent)
            .is_some_and(|inverse| original == inverse)
    })
}

/// Find a standard Git revert commit that refers to source history.
///
/// Git records the reverted object in the body as `This reverts commit ...`.
/// A marker alone is not enough: the reverted object must be in the
/// destination history, the revert commit must actually reverse the object's
/// tree diff, and either the object itself or (for a reverted merge) its second
/// parent must be reachable from `source`. This avoids warning for a revert
/// message that merely names an unrelated object.
pub fn reverted_source_commit(
    path: &Path,
    source: &str,
    destination: &str,
) -> Result<Option<String>> {
    let log = git(&["log", "--format=%H%x00%P%x00%B%x1e", destination], path)?;
    let source_was_merged =
        git_success(&["merge-base", "--is-ancestor", source, destination], path);
    let common_base = git(&["merge-base", source, destination], path).ok();

    for record in log.split('\x1e') {
        let mut fields = record.splitn(3, '\0');
        let Some(revert_commit) = fields.next() else {
            continue;
        };
        let Some(_revert_parents) = fields.next() else {
            continue;
        };
        let Some(message) = fields.next() else {
            continue;
        };
        let Some(marker) = message.find("This reverts commit ") else {
            continue;
        };
        let start = marker + "This reverts commit ".len();
        let Some(reverted) = message[start..]
            .split(|character: char| !character.is_ascii_hexdigit())
            .next()
            .filter(|value| value.len() == 40)
        else {
            continue;
        };

        if !git_success(
            &["merge-base", "--is-ancestor", reverted, destination],
            path,
        ) || !revert_reverses_commit(path, revert_commit, reverted)
        {
            continue;
        }

        let reverted_parents = match git(&["show", "-s", "--format=%P", reverted], path) {
            Ok(parents) => parents,
            Err(_) => continue,
        };
        let parents: Vec<&str> = reverted_parents.split_whitespace().collect();
        let source_history_commit = parents
            .get(1)
            .copied()
            .filter(|second_parent| {
                git_success(
                    &["merge-base", "--is-ancestor", second_parent, source],
                    path,
                )
            })
            .unwrap_or(reverted);
        let is_shared_base = common_base.as_deref().is_some_and(|base| {
            git_success(
                &["merge-base", "--is-ancestor", source_history_commit, base],
                path,
            )
        });
        if !source_was_merged && is_shared_base {
            continue;
        }
        if git_success(
            &["merge-base", "--is-ancestor", source_history_commit, source],
            path,
        ) {
            return Ok(Some(reverted.to_string()));
        }
    }

    Ok(None)
}

/// Compute commit and diff stats for `branch` against `base`.
pub fn worktree_stats(repo: &RepoRoot, base: &str, branch: &str) -> Result<WorktreeStats> {
    let branch_ref = format!("refs/heads/{branch}");
    let range = format!("{base}...{branch_ref}");
    let (commits_behind, commits_ahead) = rev_list_counts(repo, &range)?;
    let (files_changed, insertions, deletions) = diff_numstat(repo, &range)?;

    Ok(WorktreeStats {
        base: base.to_string(),
        commits_ahead,
        commits_behind,
        files_changed,
        insertions,
        deletions,
    })
}

fn rev_list_counts(repo: &RepoRoot, range: &str) -> Result<(u32, u32)> {
    let output = git(
        &["rev-list", "--left-right", "--count", range],
        repo.as_ref(),
    )?;
    let mut fields = output.split_whitespace();
    let behind = fields
        .next()
        .and_then(|s| s.parse::<u32>().ok())
        .ok_or_else(|| AppError::git("failed to parse rev-list behind count".to_string()))?;
    let ahead = fields
        .next()
        .and_then(|s| s.parse::<u32>().ok())
        .ok_or_else(|| AppError::git("failed to parse rev-list ahead count".to_string()))?;
    Ok((behind, ahead))
}

fn diff_numstat(repo: &RepoRoot, range: &str) -> Result<(u32, u32, u32)> {
    let output = git(&["diff", "--numstat", range], repo.as_ref())?;
    let mut files_changed = 0;
    let mut insertions = 0;
    let mut deletions = 0;

    for line in output.lines() {
        let mut fields = line.splitn(3, '\t');
        let added = fields.next().unwrap_or_default();
        let removed = fields.next().unwrap_or_default();
        if fields.next().is_some() {
            files_changed += 1;
            // Git reports binary files as `-` insertions/deletions in --numstat.
            // Count the file as changed and expose stable zero line counts.
            insertions += added.parse::<u32>().unwrap_or(0);
            deletions += removed.parse::<u32>().unwrap_or(0);
        }
    }

    Ok((files_changed, insertions, deletions))
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

/// Return the path Git uses for a repository-local marker from `cwd`.
fn git_path(cwd: &Path, marker: &str) -> Result<PathBuf> {
    let path = PathBuf::from(git(&["rev-parse", "--git-path", marker], cwd)?);
    if path.is_absolute() {
        Ok(path)
    } else {
        Ok(cwd.join(path))
    }
}

/// Detect an operation already in progress in a particular worktree.
///
/// Git stores these markers in the per-worktree git directory when linked
/// worktrees are enabled, so resolving them from `path` avoids inspecting the
/// main worktree's state by mistake.
pub fn operation_state(path: &Path) -> Result<Option<&'static str>> {
    let markers = [
        ("merge", &["MERGE_HEAD"][..]),
        ("rebase", &["rebase-merge", "rebase-apply"][..]),
        ("cherry-pick", &["CHERRY_PICK_HEAD", "sequencer"][..]),
        ("revert", &["REVERT_HEAD"][..]),
    ];

    for (state, state_markers) in markers {
        for marker in state_markers {
            if git_path(path, marker)?.exists() {
                return Ok(Some(state));
            }
        }
    }

    Ok(None)
}

/// Check whether this invocation's merge left a merge state to abort.
pub fn merge_in_progress(path: &Path) -> bool {
    git_path(path, "MERGE_HEAD")
        .map(|marker| marker.exists())
        .unwrap_or(false)
}

/// Resolve a path returned by `git rev-parse` relative to its command cwd.
fn canonical_git_path(cwd: &Path, argument: &str) -> Result<PathBuf> {
    let path = PathBuf::from(git(&["rev-parse", argument], cwd)?);
    let path = if path.is_absolute() {
        path
    } else {
        cwd.join(path)
    };
    path.canonicalize().map_err(|error| {
        AppError::conflict(format!(
            "cannot canonicalize Git metadata path '{}': {error}",
            path.display()
        ))
    })
}

/// Reject aliases that could make a registered path resolve somewhere else.
fn has_symlink_component(path: &Path) -> bool {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        match std::env::current_dir() {
            Ok(cwd) => cwd.join(path),
            Err(_) => return true,
        }
    };

    let mut current = PathBuf::new();
    for component in absolute.components() {
        current.push(component.as_os_str());
        if fs::symlink_metadata(&current)
            .map(|metadata| metadata.file_type().is_symlink())
            .unwrap_or(false)
        {
            return true;
        }
    }
    false
}

/// Resolve a path stored in a worktree admin file.
fn resolve_admin_link(admin_dir: &Path, link: &str) -> PathBuf {
    let link = PathBuf::from(link);
    if link.is_absolute() {
        link
    } else {
        admin_dir.join(link)
    }
}

/// Find the one main-repository admin entry registered for `worktree`.
fn registered_worktree_admin(common_dir: &Path, worktree: &Path) -> Result<PathBuf> {
    let admin_root = common_dir.join("worktrees");
    let canonical_admin_root = admin_root.canonicalize().map_err(|error| {
        AppError::conflict(format!(
            "cannot canonicalize Git worktree admin directory '{}': {error}",
            admin_root.display()
        ))
    })?;
    let worktree_git = worktree.join(".git").canonicalize().map_err(|error| {
        AppError::conflict(format!(
            "cannot canonicalize worktree Git link '{}': {error}",
            worktree.join(".git").display()
        ))
    })?;

    let mut matches = Vec::new();
    for entry in fs::read_dir(&canonical_admin_root).map_err(|error| {
        AppError::conflict(format!(
            "cannot inspect Git worktree admin directory '{}': {error}",
            canonical_admin_root.display()
        ))
    })? {
        let entry = entry.map_err(|error| {
            AppError::conflict(format!("cannot inspect Git worktree admin entry: {error}"))
        })?;
        let admin_dir = entry.path();
        if !admin_dir.is_dir() {
            continue;
        }
        let canonical_admin = admin_dir.canonicalize().map_err(|error| {
            AppError::conflict(format!(
                "cannot canonicalize Git worktree admin entry '{}': {error}",
                admin_dir.display()
            ))
        })?;
        if canonical_admin.parent() != Some(canonical_admin_root.as_path()) {
            continue;
        }

        let link = match fs::read_to_string(canonical_admin.join("gitdir")) {
            Ok(link) => link,
            Err(_) => continue,
        };
        let target = resolve_admin_link(&canonical_admin, link.trim());
        let Ok(target) = target.canonicalize() else {
            continue;
        };
        if target == worktree_git {
            matches.push(canonical_admin);
        }
    }

    match matches.as_slice() {
        [admin] => Ok(admin.clone()),
        [] => Err(AppError::conflict(format!(
            "worktree '{}' has no matching main-repository admin entry",
            worktree.display()
        ))),
        _ => Err(AppError::conflict(format!(
            "worktree '{}' has multiple matching main-repository admin entries",
            worktree.display()
        ))),
    }
}

/// Stable registration identity for a worktree.
///
/// The main worktree uses the repository's common Git directory as its admin
/// directory. A linked worktree uses its unique entry below
/// `<common-dir>/worktrees`. The path is captured before a merge and compared
/// later; path, branch, and common-directory checks alone cannot distinguish a
/// replacement worktree registered during a hook.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum WorktreeIdentity {
    Main { admin_dir: PathBuf },
    Linked { admin_dir: PathBuf },
}

impl WorktreeIdentity {
    pub fn admin_dir(&self) -> &Path {
        match self {
            Self::Main { admin_dir } | Self::Linked { admin_dir } => admin_dir,
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Self::Main { .. } => "main",
            Self::Linked { .. } => "linked",
        }
    }
}

/// Resolve and validate the exact worktree registration owned by `repo`.
///
/// `git worktree list` reports branch and path metadata from the owning
/// repository even when the directory at that path has been replaced. Before
/// merge code uses a path, compare both Git's canonical common directory and
/// the per-worktree admin linkage. This prevents an unrelated, nested, or
/// symlink-aliased repository from receiving a merge or cleanup operation.
pub fn capture_worktree_identity(repo: &RepoRoot, worktree: &Worktree) -> Result<WorktreeIdentity> {
    let path = &worktree.path;
    if has_symlink_component(path) {
        return Err(AppError::conflict(format!(
            "worktree path '{}' contains a symlink component",
            path.display()
        )));
    }

    let canonical_repo = repo.as_ref().canonicalize().map_err(|error| {
        AppError::conflict(format!(
            "cannot canonicalize owning repository '{}': {error}",
            repo.display()
        ))
    })?;
    let canonical_path = path.canonicalize().map_err(|_| {
        AppError::conflict(format!("worktree path '{}' is unavailable", path.display()))
    })?;
    if !worktree.is_main && canonical_path == canonical_repo {
        return Err(AppError::conflict(format!(
            "linked worktree '{}' resolves to the owning repository root",
            path.display()
        )));
    }
    if worktree.is_main && canonical_path != canonical_repo {
        return Err(AppError::conflict(format!(
            "main worktree '{}' is not the owning repository root '{}'",
            path.display(),
            canonical_repo.display()
        )));
    }

    let common_dir = canonical_git_path(repo.as_ref(), "--git-common-dir")?;
    let worktree_common = canonical_git_path(path, "--git-common-dir")?;
    if worktree_common != common_dir {
        return Err(AppError::conflict(format!(
            "Git common directory '{}' does not match owning repository '{}'",
            worktree_common.display(),
            common_dir.display()
        )));
    }

    let worktree_git_file = path.join(".git");
    let identity = match worktree.is_main {
        true => {
            let worktree_git_dir = canonical_git_path(path, "--git-dir")?;
            if worktree_git_dir != common_dir {
                return Err(AppError::conflict(format!(
                    "main worktree Git directory '{}' does not match owning repository '{}'",
                    worktree_git_dir.display(),
                    common_dir.display()
                )));
            }
            WorktreeIdentity::Main {
                admin_dir: common_dir.clone(),
            }
        }
        false => {
            let metadata = fs::symlink_metadata(&worktree_git_file).map_err(|error| {
                AppError::conflict(format!(
                    "linked worktree Git link '{}' is unavailable: {error}",
                    worktree_git_file.display()
                ))
            })?;
            if !metadata.file_type().is_file() {
                return Err(AppError::conflict(format!(
                    "linked worktree '{}' does not have a regular .git link",
                    path.display()
                )));
            }

            let admin_dir = registered_worktree_admin(&common_dir, path)?;
            let actual_git_dir = canonical_git_path(path, "--git-dir")?;
            if actual_git_dir != admin_dir {
                return Err(AppError::conflict(format!(
                    "Git directory '{}' is not the registered admin entry '{}'",
                    actual_git_dir.display(),
                    admin_dir.display()
                )));
            }

            let link = fs::read_to_string(&worktree_git_file).map_err(|error| {
                AppError::conflict(format!(
                    "cannot read linked worktree Git link '{}': {error}",
                    worktree_git_file.display()
                ))
            })?;
            let link = link.strip_prefix("gitdir:").map(str::trim).ok_or_else(|| {
                AppError::conflict(format!(
                    "linked worktree Git link '{}' has an invalid format",
                    worktree_git_file.display()
                ))
            })?;
            let linked_admin = resolve_admin_link(path, link)
                .canonicalize()
                .map_err(|error| {
                    AppError::conflict(format!(
                        "cannot canonicalize linked worktree admin path '{}': {error}",
                        link
                    ))
                })?;
            if linked_admin != admin_dir {
                return Err(AppError::conflict(format!(
                    "linked worktree Git link '{}' does not point to registered admin entry '{}'",
                    linked_admin.display(),
                    admin_dir.display()
                )));
            }

            let admin_link = fs::read_to_string(admin_dir.join("gitdir")).map_err(|error| {
                AppError::conflict(format!(
                    "cannot read worktree admin link '{}': {error}",
                    admin_dir.join("gitdir").display()
                ))
            })?;
            let registered_path = resolve_admin_link(&admin_dir, admin_link.trim())
                .canonicalize()
                .map_err(|error| {
                    AppError::conflict(format!(
                        "cannot canonicalize worktree admin link '{}': {error}",
                        admin_link.trim()
                    ))
                })?;
            if registered_path
                != worktree_git_file.canonicalize().map_err(|error| {
                    AppError::conflict(format!(
                        "cannot canonicalize worktree Git link '{}': {error}",
                        worktree_git_file.display()
                    ))
                })?
            {
                return Err(AppError::conflict(format!(
                    "worktree admin entry '{}' is not linked back to '{}'",
                    admin_dir.display(),
                    worktree_git_file.display()
                )));
            }

            match fs::read_to_string(admin_dir.join("commondir")) {
                Ok(commondir) => {
                    let admin_common = resolve_admin_link(&admin_dir, commondir.trim())
                        .canonicalize()
                        .map_err(|error| {
                            AppError::conflict(format!(
                                "cannot canonicalize worktree admin common directory '{}': {error}",
                                commondir.trim()
                            ))
                        })?;
                    if admin_common != common_dir {
                        return Err(AppError::conflict(format!(
                            "worktree admin common directory '{}' does not match owning repository '{}'",
                            admin_common.display(),
                            common_dir.display()
                        )));
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(AppError::conflict(format!(
                        "cannot read worktree admin common directory '{}': {error}",
                        admin_dir.join("commondir").display()
                    )));
                }
            }

            let admin_head = fs::read_to_string(admin_dir.join("HEAD")).map_err(|error| {
                AppError::conflict(format!(
                    "cannot read worktree admin HEAD '{}': {error}",
                    admin_dir.join("HEAD").display()
                ))
            })?;
            let admin_branch = admin_head
                .trim()
                .strip_prefix("ref: refs/heads/")
                .map(str::to_string);
            let actual_branch = git(&["symbolic-ref", "--quiet", "--short", "HEAD"], path).ok();
            if admin_branch != worktree.branch || actual_branch != worktree.branch {
                return Err(AppError::conflict(format!(
                    "worktree branch metadata does not match registered branch '{}'",
                    worktree.branch.as_deref().unwrap_or("(detached)")
                )));
            }
            WorktreeIdentity::Linked { admin_dir }
        }
    };

    let actual_branch = git(&["symbolic-ref", "--quiet", "--short", "HEAD"], path).ok();
    if actual_branch != worktree.branch {
        return Err(AppError::conflict(format!(
            "worktree branch '{}' does not match the registered branch '{}'",
            actual_branch.as_deref().unwrap_or("(detached)"),
            worktree.branch.as_deref().unwrap_or("(detached)")
        )));
    }
    let actual_head = git(&["rev-parse", "--verify", "HEAD"], path)?;
    if !worktree.commit.is_empty() && !actual_head.starts_with(&worktree.commit) {
        return Err(AppError::conflict(format!(
            "worktree HEAD '{}' does not match registered HEAD '{}'",
            actual_head, worktree.commit
        )));
    }

    Ok(identity)
}

/// Return whether Git left unmerged index entries in a worktree.
///
/// A failed hook can leave `MERGE_HEAD` and a staged merge without any
/// unmerged entries, so the index is the authoritative signal for a content
/// conflict. This keeps hook and other Git failures out of the
/// `content_conflict` JSON category.
pub fn has_unmerged_entries(path: &Path) -> bool {
    git(&["ls-files", "--unmerged"], path)
        .map(|output| !output.is_empty())
        .unwrap_or(false)
}

/// Return each path that still has unmerged index stages, once.
pub fn unmerged_paths(path: &Path) -> Result<Vec<String>> {
    let mut cmd = Cmd::new("git");
    cmd.args(["ls-files", "--unmerged", "-z"]).current_dir(path);
    sanitize_git_environment(&mut cmd);
    let output = cmd
        .output()
        .map_err(|error| AppError::git(format!("failed to run git ls-files: {error}")))?;
    if !output.status.success() {
        return Err(classify_git_error(
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        ));
    }

    let mut paths = Vec::new();
    for record in output.stdout.split(|byte| *byte == 0) {
        let record = String::from_utf8_lossy(record);
        let Some((_, path)) = record.split_once('\t') else {
            continue;
        };
        if !paths.iter().any(|existing| existing == path) {
            paths.push(path.to_string());
        }
    }
    Ok(paths)
}

/// Check if a remote-tracking branch exists for `origin/<branch>`.
pub fn remote_branch_exists(repo: &RepoRoot, branch: &BranchName) -> bool {
    let refspec = format!("refs/remotes/origin/{}", branch.as_str());
    git(&["rev-parse", "--verify", &refspec], repo.as_ref()).is_ok()
}

/// Set the upstream tracking reference for a local branch.
///
/// Equivalent to `git branch --set-upstream-to=origin/<branch> <branch>`.
pub fn set_upstream(repo: &RepoRoot, branch: &BranchName) -> Result<()> {
    let upstream = format!("origin/{}", branch.as_str());
    git(
        &["branch", "--set-upstream-to", &upstream, branch.as_str()],
        repo.as_ref(),
    )?;
    Ok(())
}

/// Return the full commit currently checked out in a worktree.
pub fn head_commit(path: &Path) -> Result<String> {
    git(&["rev-parse", "--verify", "HEAD^{commit}"], path)
}

/// Return the parent commits of a commit, in Git's recorded order.
pub fn commit_parents(path: &Path, revision: &str) -> Result<Vec<String>> {
    let output = git(&["rev-list", "--parents", "-n", "1", revision], path)?;
    Ok(output
        .split_whitespace()
        .skip(1)
        .map(str::to_string)
        .collect())
}

/// Identify the merge commit created from the captured destination/source
/// heads. If a hook or concurrent writer advanced the destination once more,
/// return the merge commit as the expected result so callers can refuse the
/// newer HEAD rather than silently treating it as the merge result.
pub fn merge_result_head(
    path: &Path,
    destination_head: &str,
    source_head: &str,
) -> Result<Option<String>> {
    let current = head_commit(path)?;
    let parents = commit_parents(path, &current)?;
    if parents.len() == 2 && parents[0] == destination_head && parents[1] == source_head {
        return Ok(Some(current));
    }
    let Some(first_parent) = parents.first() else {
        return Ok(None);
    };
    let first_parents = commit_parents(path, first_parent)?;
    if first_parents.len() == 2
        && first_parents[0] == destination_head
        && first_parents[1] == source_head
    {
        return Ok(Some(first_parent.clone()));
    }
    Ok(None)
}

/// Read the authoritative remote branch head without changing local refs.
pub fn remote_branch_head(repo: &RepoRoot, branch: &str) -> Result<Option<String>> {
    let reference = format!("refs/heads/{branch}");
    let output = git(&["ls-remote", "origin", &reference], repo.as_ref())?;
    Ok(output.split_whitespace().next().map(str::to_string))
}

/// Return the source commit recorded by Git for an in-progress merge.
pub fn merge_head(path: &Path) -> Result<Option<String>> {
    let marker = git_path(path, "MERGE_HEAD")?;
    match fs::read_to_string(marker) {
        Ok(value) => Ok(value
            .lines()
            .next()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(AppError::conflict(format!(
            "cannot read merge state: {error}"
        ))),
    }
}

/// Merge a branch into the current branch using `--no-ff`.
///
/// `path` identifies the worktree whose checked-out branch is the merge
/// destination. Returns `Ok(())` on a clean merge or an error if conflicts
/// arise (or any other git failure).
pub fn merge_no_ff(path: &Path, branch: &str) -> Result<()> {
    git(
        &[
            "merge",
            "--no-ff",
            branch,
            "-m",
            &format!("Merge branch '{branch}'"),
        ],
        path,
    )?;
    Ok(())
}

/// Continue an in-progress merge through Git's normal commit and hook path.
pub fn merge_continue(path: &Path) -> Result<()> {
    let mut cmd = Cmd::new("git");
    cmd.args(["merge", "--continue"]).current_dir(path);
    if std::env::var_os("GIT_EDITOR").is_none() {
        // The merge message was created by the initial merge. Avoid opening
        // an editor in non-interactive callers without changing Git's commit
        // or hook behavior.
        cmd.env("GIT_EDITOR", "true");
    }
    sanitize_git_environment(&mut cmd);
    let output = cmd
        .output()
        .map_err(|error| AppError::git(format!("failed to run git merge --continue: {error}")))?;
    if !output.status.success() {
        return Err(classify_git_error(
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        ));
    }
    Ok(())
}

/// Abort an in-progress merge and report failures to managed callers.
pub fn merge_abort_checked(path: &Path) -> Result<()> {
    git(&["merge", "--abort"], path)?;
    Ok(())
}

/// Abort an in-progress merge in the destination worktree.
///
/// Best-effort: if there is no merge to abort, git returns an error that
/// we silently ignore.
pub fn merge_abort(path: &Path) {
    let _ = merge_abort_checked(path);
}

/// Location of wt-core's repository-local managed merge state.
pub fn merge_operation_path(repo: &RepoRoot) -> Result<PathBuf> {
    git_path(repo.as_ref(), "wt-core/merge-operation.json")
}

/// Location of the OS-backed owner lock for mutating merge lifecycles.
pub fn merge_operation_lock_path(repo: &RepoRoot) -> Result<PathBuf> {
    git_path(repo.as_ref(), "wt-core/merge-operation.lock")
}

/// Verify Git has a usable difftool before launching an interactive diff.
pub fn ensure_difftool_available(path: &Path, tool: Option<&str>) -> Result<()> {
    match tool {
        Some(tool) => ensure_explicit_difftool_available(path, tool),
        None => ensure_default_difftool_available(path),
    }
}

fn ensure_explicit_difftool_available(path: &Path, tool: &str) -> Result<()> {
    if is_difftool_available(path, tool)? {
        return Ok(());
    }

    Err(AppError::usage(format!(
        "git difftool '{tool}' is not configured or available\n\n\
Configure it, for example:\n  git config --global diff.tool {tool}\n\n\
Or choose an available tool explicitly:\n  wt diff --tool nvimdiff <branch>\n\n\
Available tools can be inspected with:\n  git difftool --tool-help"
    )))
}

fn ensure_default_difftool_available(path: &Path) -> Result<()> {
    let Some(tool) = configured_difftool(path)? else {
        if !available_difftools(path)?.is_empty() {
            return Ok(());
        }

        return Err(AppError::usage(
            "no git difftool is configured or available\n\n\
Configure one, for example:\n  git config --global diff.tool nvimdiff\n\n\
Or run explicitly:\n  wt diff --tool nvimdiff <branch>\n\n\
Available tools can be inspected with:\n  git difftool --tool-help"
                .to_string(),
        ));
    };

    if is_difftool_available(path, &tool)? {
        return Ok(());
    }

    Err(AppError::usage(format!(
        "configured git difftool '{tool}' is not available\n\n\
Configure one, for example:\n  git config --global diff.tool nvimdiff\n\n\
Or run explicitly:\n  wt diff --tool nvimdiff <branch>\n\n\
Available tools can be inspected with:\n  git difftool --tool-help"
    )))
}

fn configured_difftool(path: &Path) -> Result<Option<String>> {
    let Some(tool) = git_config_get(path, "diff.tool")? else {
        return git_config_get(path, "merge.tool");
    };

    Ok(Some(tool))
}

fn is_difftool_available(path: &Path, tool: &str) -> Result<bool> {
    if git_config_get(path, &format!("difftool.{tool}.cmd"))?.is_some() {
        return Ok(true);
    }

    Ok(available_difftools(path)?.contains(tool))
}

fn available_difftools(path: &Path) -> Result<HashSet<String>> {
    let output = git_tool_help(path)?;
    Ok(parse_available_difftools(&output))
}

fn parse_available_difftools(output: &str) -> HashSet<String> {
    let mut tools = HashSet::new();

    for line in output.lines() {
        if line.starts_with("The following tools are valid, but not currently available:") {
            break;
        }

        if !line.starts_with(char::is_whitespace) {
            continue;
        }

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let Some(name) = trimmed.split_whitespace().next() else {
            continue;
        };

        if name.ends_with(':') {
            continue;
        }

        tools.insert(name.strip_suffix(".cmd").unwrap_or(name).to_string());
    }

    tools
}

fn git_config_get(path: &Path, key: &str) -> Result<Option<String>> {
    let mut cmd = Cmd::new("git");
    cmd.arg("-C").arg(path).arg("config").arg("--get").arg(key);
    sanitize_git_environment(&mut cmd);

    let output = cmd
        .output()
        .map_err(|e| AppError::git(format!("failed to run git config: {e}")))?;

    if !output.status.success() {
        return Ok(None);
    }

    let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Ok((!value.is_empty()).then_some(value))
}

fn git_tool_help(path: &Path) -> Result<String> {
    let mut cmd = Cmd::new("git");
    cmd.arg("-C").arg(path).arg("difftool").arg("--tool-help");
    sanitize_git_environment(&mut cmd);

    let output = cmd
        .output()
        .map_err(|e| AppError::git(format!("failed to run git difftool --tool-help: {e}")))?;

    if !output.status.success() {
        return Err(classify_git_error(
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        ));
    }

    let mut combined = String::from_utf8_lossy(&output.stdout).to_string();
    combined.push_str(&String::from_utf8_lossy(&output.stderr));
    Ok(combined)
}

/// Run Git's configured difftool for a branch comparison.
pub fn difftool(repo: &RepoRoot, tool: Option<&str>, range: &str) -> Result<()> {
    let mut cmd = base_difftool(repo.as_ref(), tool);
    cmd.arg(range);
    run_difftool(cmd)
}

/// Run Git's configured difftool for dirty changes in a linked worktree.
pub fn difftool_dirty(
    worktree_path: &Path,
    mode: crate::worktree::DirtyDiffMode,
    tool: Option<&str>,
) -> Result<()> {
    let mut cmd = base_difftool(worktree_path, tool);

    match mode {
        crate::worktree::DirtyDiffMode::Dirty => {
            cmd.arg("HEAD");
        }
        crate::worktree::DirtyDiffMode::Staged => {
            cmd.arg("--staged");
        }
        crate::worktree::DirtyDiffMode::Unstaged => {}
    }

    run_difftool(cmd)
}

fn base_difftool(path: &Path, tool: Option<&str>) -> Cmd {
    let mut cmd = Cmd::new("git");
    cmd.arg("-C")
        .arg(path)
        .arg("difftool")
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());

    if let Some(tool) = tool {
        cmd.arg("--tool").arg(tool);
    }

    cmd.arg("--dir-diff");
    cmd
}

fn run_difftool(mut cmd: Cmd) -> Result<()> {
    sanitize_git_environment(&mut cmd);

    let status = cmd
        .status()
        .map_err(|e| AppError::git(format!("failed to run git difftool: {e}")))?;

    if !status.success() {
        return Err(AppError::git(format!(
            "git difftool failed with status {status}"
        )));
    }

    Ok(())
}

/// Push a branch to `origin` from the worktree where it is checked out.
pub fn push(path: &Path, branch: &str) -> Result<()> {
    git(&["push", "origin", branch], path)?;
    Ok(())
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
    fn parse_porcelain_deduplicates_by_worktree_path() {
        let repo = RepoRoot(PathBuf::from("/repo"));
        let raw = "\
worktree /repo
HEAD abc1234567890
branch refs/heads/main

worktree /repo/.worktrees/feat
HEAD def4567890abc
branch refs/heads/feat

worktree /repo/.worktrees/feat
HEAD fedcba0987654
branch refs/heads/feat-duplicate

worktree /repo/.worktrees/other
HEAD 9876543210fed
branch refs/heads/other

";
        let result = parse_worktree_porcelain(raw, &repo).expect("should parse");

        assert_eq!(result.len(), 3);
        assert_eq!(result[0].path, PathBuf::from("/repo"));
        assert_eq!(result[0].branch.as_deref(), Some("main"));
        assert_eq!(result[0].commit, "abc1234");
        assert!(result[0].is_main);

        assert_eq!(result[1].path, PathBuf::from("/repo/.worktrees/feat"));
        assert_eq!(result[1].branch.as_deref(), Some("feat"));
        assert_eq!(result[1].commit, "def4567");
        assert!(!result[1].is_main);

        assert_eq!(result[2].path, PathBuf::from("/repo/.worktrees/other"));
        assert_eq!(result[2].branch.as_deref(), Some("other"));
        assert!(!result[2].is_main);
    }

    #[test]
    fn parse_available_difftools_skips_headings_and_unavailable_tools() {
        let raw = "\
'git difftool --tool=<tool>' may be set to one of the following:\n\
\t\tnvimdiff         Use Neovim\n\
\n\
\tuser-defined:\n\
\t\tdelta.cmd git diff --no-index -- $LOCAL $REMOTE | delta\n\
\n\
The following tools are valid, but not currently available:\n\
\t\tvimdiff          Use Vim\n\
\n\
Some of the tools listed above only work in a windowed environment.\n";

        let tools = parse_available_difftools(raw);

        assert!(tools.contains("nvimdiff"));
        assert!(tools.contains("delta"));
        assert!(!tools.contains("user-defined:"));
        assert!(!tools.contains("vimdiff"));
        assert!(!tools.contains("Some"));
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
