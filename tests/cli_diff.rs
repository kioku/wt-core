mod fixtures;

use assert_cmd::Command;
use predicates::prelude::*;

use fixtures::commit_file;

fn wt_core() -> Command {
    Command::new(assert_cmd::cargo_bin!("wt-core"))
}

#[test]
fn diff_dry_run_explicit_branch_uses_mainline_three_dot_range() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    wt_core()
        .args(["add", "feature/diff", "--repo", &repo_str])
        .assert()
        .success();

    wt_core()
        .args(["diff", "feature/diff", "--repo", &repo_str, "--dry-run"])
        .assert()
        .success()
        .stdout(predicate::str::contains(format!(
            "git -C {} difftool",
            repo_str
        )))
        .stdout(predicate::str::contains("--dir-diff"))
        .stdout(predicate::str::contains("main...feature/diff"));
}

#[test]
fn diff_dry_run_respects_base_override_and_tool() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    wt_core()
        .args(["add", "feature/tool", "--repo", &repo_str])
        .assert()
        .success();

    wt_core()
        .args([
            "diff",
            "feature/tool",
            "--repo",
            &repo_str,
            "--against",
            "HEAD",
            "--tool",
            "vimdiff",
            "--dry-run",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("--tool vimdiff"))
        .stdout(predicate::str::contains("HEAD...feature/tool"));
}

#[test]
fn diff_errors_when_requested_branch_has_no_worktree() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    commit_file(&repo.path(), "local.txt", "local", "local branch base");
    fixtures::run_git(&["branch", "feature/no-worktree"], &repo.path());

    wt_core()
        .args([
            "diff",
            "feature/no-worktree",
            "--repo",
            &repo_str,
            "--dry-run",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "branch 'feature/no-worktree' has no associated worktree",
        ));
}

#[test]
fn diff_without_branch_errors_when_no_non_main_worktrees() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    wt_core()
        .args(["diff", "--repo", &repo_str, "--dry-run"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("no worktrees to diff"));
}

#[test]
fn diff_rejects_empty_tool_name() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    wt_core()
        .args(["add", "feature/empty-tool", "--repo", &repo_str])
        .assert()
        .success();

    wt_core()
        .args([
            "diff",
            "feature/empty-tool",
            "--repo",
            &repo_str,
            "--tool",
            "   ",
            "--dry-run",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--tool must not be empty"));
}
