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

#[test]
fn diff_dirty_dry_run_explicit_branch_uses_worktree_path() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    wt_core()
        .args(["add", "feature/dirty", "--repo", &repo_str])
        .assert()
        .success();

    let worktree = fixtures::find_worktree_dir(&repo.path(), "feature-dirty");

    wt_core()
        .args([
            "diff",
            "--dirty",
            "feature/dirty",
            "--repo",
            &repo_str,
            "--dry-run",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(format!(
            "git -C {} difftool",
            worktree.display()
        )))
        .stdout(predicate::str::contains("--dir-diff HEAD"));
}

#[test]
fn diff_staged_and_unstaged_dry_run_construct_worktree_commands() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    wt_core()
        .args(["add", "feature/index", "--repo", &repo_str])
        .assert()
        .success();

    let worktree = fixtures::find_worktree_dir(&repo.path(), "feature-index");

    wt_core()
        .args([
            "diff",
            "--staged",
            "feature/index",
            "--repo",
            &repo_str,
            "--tool",
            "vimdiff",
            "--dry-run",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(format!(
            "git -C {} difftool --tool vimdiff --dir-diff --staged",
            worktree.display()
        )));

    wt_core()
        .args([
            "diff",
            "--unstaged",
            "feature/index",
            "--repo",
            &repo_str,
            "--dry-run",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(format!(
            "git -C {} difftool --dir-diff",
            worktree.display()
        )))
        .stdout(predicate::str::contains("--staged").not())
        .stdout(predicate::str::contains(" HEAD").not());
}

#[test]
fn diff_print_command_alias_prints_dirty_command() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    wt_core()
        .args(["add", "feature/print", "--repo", &repo_str])
        .assert()
        .success();

    wt_core()
        .args([
            "diff",
            "--dirty",
            "feature/print",
            "--repo",
            &repo_str,
            "--print-command",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("git -C"))
        .stdout(predicate::str::contains("difftool"));
}

#[test]
fn diff_dirty_errors_when_requested_branch_has_no_worktree() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    commit_file(&repo.path(), "local.txt", "local", "local branch base");
    fixtures::run_git(&["branch", "feature/no-dirty-worktree"], &repo.path());

    wt_core()
        .args([
            "diff",
            "--dirty",
            "feature/no-dirty-worktree",
            "--repo",
            &repo_str,
            "--dry-run",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "branch 'feature/no-dirty-worktree' has no associated worktree",
        ));
}

#[test]
fn diff_rejects_ambiguous_dirty_mode_flags() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    wt_core()
        .args([
            "diff",
            "--dirty",
            "--staged",
            "feature/ambiguous",
            "--repo",
            &repo_str,
            "--dry-run",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "--dirty, --staged, and --unstaged are mutually exclusive",
        ));
}

#[test]
fn diff_rejects_against_with_dirty_mode() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    wt_core()
        .args([
            "diff",
            "--dirty",
            "feature/against",
            "--against",
            "HEAD",
            "--repo",
            &repo_str,
            "--dry-run",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "--against can only be used with branch-vs-mainline diffs",
        ));
}
