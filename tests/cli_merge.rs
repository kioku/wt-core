mod fixtures;

use std::process::Command as StdCommand;

use assert_cmd::Command;
use predicates::prelude::*;

use fixtures::{commit_file, find_worktree_dir, run_git};

fn wt_core() -> Command {
    Command::new(assert_cmd::cargo_bin!("wt-core"))
}

/// Environment variables cleared for raw git commands in tests.
const GIT_ENV_OVERRIDES: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_PREFIX",
];

// ── Clean merge tests ───────────────────────────────────────────────

#[test]
fn merge_clean_succeeds_and_cleans_up() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    // Create worktree and commit
    wt_core()
        .args(["add", "feature/auth", "--repo", &repo_str])
        .assert()
        .success();

    let wt_dir = find_worktree_dir(&repo.path(), "feature-auth");
    commit_file(&wt_dir, "auth.txt", "auth feature", "add auth");

    // Merge
    wt_core()
        .args(["merge", "feature/auth", "--repo", &repo_str])
        .assert()
        .success()
        .stdout(predicate::str::contains("Merged 'feature/auth' into main"))
        .stdout(predicate::str::contains(
            "Removed worktree and branch 'feature/auth'",
        ));

    // Verify worktree is gone
    let entries: Vec<_> = std::fs::read_dir(repo.path().join(".worktrees"))
        .into_iter()
        .flat_map(|rd| rd.flatten())
        .filter(|e| e.path().is_dir())
        .collect();
    assert_eq!(entries.len(), 0, "worktree should be removed");

    // Verify branch is deleted
    assert_branch_deleted(&repo.path(), "feature/auth");

    // Verify the merge commit exists on main
    let log = git_log_oneline(&repo.path(), "main");
    assert!(
        log.contains("Merge branch 'feature/auth'"),
        "merge commit should exist on main: {log}"
    );
}

#[test]
fn merge_no_cleanup_keeps_worktree_and_branch() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    wt_core()
        .args(["add", "feature/keep", "--repo", &repo_str])
        .assert()
        .success();

    let wt_dir = find_worktree_dir(&repo.path(), "feature-keep");
    commit_file(&wt_dir, "keep.txt", "keep feature", "add keep");

    wt_core()
        .args(["merge", "feature/keep", "--no-cleanup", "--repo", &repo_str])
        .assert()
        .success()
        .stdout(predicate::str::contains("Merged 'feature/keep' into main"))
        .stdout(predicate::str::contains("Removed").not());

    // Worktree should still exist
    let wt_dir = find_worktree_dir(&repo.path(), "feature-keep");
    assert!(wt_dir.exists(), "worktree should still exist");

    // Branch should still exist
    assert_branch_exists(&repo.path(), "feature/keep");
}

// ── Conflict tests ──────────────────────────────────────────────────

#[test]
fn merge_conflict_aborts_and_leaves_everything_untouched() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    wt_core()
        .args(["add", "feature/conflict", "--repo", &repo_str])
        .assert()
        .success();

    // Create conflicting changes on both branches
    let wt_dir = find_worktree_dir(&repo.path(), "feature-conflict");
    commit_file(&wt_dir, "shared.txt", "feature version", "feature change");
    commit_file(&repo.path(), "shared.txt", "main version", "main change");

    // Merge should fail with conflict details from git.
    wt_core()
        .args(["merge", "feature/conflict", "--repo", &repo_str])
        .assert()
        .failure()
        .stderr(predicate::str::contains("merge conflicts"))
        .stderr(predicate::str::contains("merge aborted, resolve manually"));

    // Worktree should still exist
    let wt_dir = find_worktree_dir(&repo.path(), "feature-conflict");
    assert!(
        wt_dir.exists(),
        "worktree should still exist after conflict"
    );

    // Branch should still exist
    assert_branch_exists(&repo.path(), "feature/conflict");

    // Main worktree should be clean (merge was aborted)
    let status = git_status(&repo.path());
    assert!(
        status.is_empty(),
        "main worktree should be clean after abort: {status}"
    );
}

// ── Main worktree protection ────────────────────────────────────────

#[test]
fn merge_refuses_main_worktree() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    wt_core()
        .args(["merge", "main", "--repo", &repo_str])
        .assert()
        .failure()
        .code(4) // Invariant violation
        .stderr(predicate::str::contains(
            "refusing to merge the main worktree",
        ));
}

#[test]
fn merge_refuses_when_main_worktree_not_on_mainline() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    // Create a feature worktree and commit to it.
    wt_core()
        .args(["add", "feature/diverged", "--repo", &repo_str])
        .assert()
        .success();

    let wt_dir = find_worktree_dir(&repo.path(), "feature-diverged");
    commit_file(&wt_dir, "d.txt", "diverged work", "diverged commit");

    // Switch the main worktree off mainline to simulate HEAD divergence.
    run_git(&["checkout", "-b", "other-branch"], &repo.path());

    // Merge should refuse because HEAD != mainline.
    wt_core()
        .args(["merge", "feature/diverged", "--repo", &repo_str])
        .assert()
        .failure()
        .code(4) // Invariant violation
        .stderr(predicate::str::contains(
            "main worktree is on 'other-branch'",
        ))
        .stderr(predicate::str::contains("checkout mainline first"));

    // Switch back so cleanup doesn't fail.
    run_git(&["checkout", "main"], &repo.path());
}

// ── Push tests ──────────────────────────────────────────────────────

#[test]
fn merge_with_push_pushes_mainline() {
    let (repo, _upstream) = setup_repo_with_upstream();
    let repo_str = repo.path().display().to_string();

    wt_core()
        .args(["add", "feature/pushed", "--repo", &repo_str])
        .assert()
        .success();

    let wt_dir = find_worktree_dir(&repo.path(), "feature-pushed");
    commit_file(&wt_dir, "pushed.txt", "pushed work", "add pushed");

    wt_core()
        .args(["merge", "feature/pushed", "--push", "--repo", &repo_str])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Merged 'feature/pushed' into main",
        ))
        .stdout(predicate::str::contains("Pushed main to origin"));
}

#[test]
fn merge_push_failure_reports_warning() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    // No upstream configured → push will fail
    wt_core()
        .args(["add", "feature/no-remote", "--repo", &repo_str])
        .assert()
        .success();

    let wt_dir = find_worktree_dir(&repo.path(), "feature-no-remote");
    commit_file(&wt_dir, "nr.txt", "no remote work", "add no-remote");

    // Merge succeeds but push fails → success with warning on stderr
    wt_core()
        .args(["merge", "feature/no-remote", "--push", "--repo", &repo_str])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Merged 'feature/no-remote' into main",
        ))
        .stderr(predicate::str::contains("warning:"));
}

// ── JSON output tests ───────────────────────────────────────────────

#[test]
fn merge_json_output_structure() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    wt_core()
        .args(["add", "feature/json-merge", "--repo", &repo_str])
        .assert()
        .success();

    let wt_dir = find_worktree_dir(&repo.path(), "feature-json-merge");
    commit_file(&wt_dir, "j.txt", "json merge", "json commit");

    let output = wt_core()
        .args(["merge", "feature/json-merge", "--repo", &repo_str, "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let json: serde_json::Value = serde_json::from_slice(&output).expect("invalid json");
    assert_eq!(json["ok"], true);
    assert_eq!(json["branch"], "feature/json-merge");
    assert_eq!(json["mainline"], "main");
    assert!(json["repo_root"].as_str().is_some());
    assert_eq!(json["cleaned_up"], true);
    assert_eq!(json["pushed"], false);
}

#[test]
fn merge_json_no_cleanup_shows_false() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    wt_core()
        .args(["add", "feature/json-nc", "--repo", &repo_str])
        .assert()
        .success();

    let wt_dir = find_worktree_dir(&repo.path(), "feature-json-nc");
    commit_file(&wt_dir, "nc.txt", "no cleanup", "nc commit");

    let output = wt_core()
        .args([
            "merge",
            "feature/json-nc",
            "--no-cleanup",
            "--repo",
            &repo_str,
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let json: serde_json::Value = serde_json::from_slice(&output).expect("invalid json");
    assert_eq!(json["ok"], true);
    assert_eq!(json["cleaned_up"], false);
}

// ── Print-paths output tests ────────────────────────────────────────

#[test]
fn merge_print_paths_returns_five_lines() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    wt_core()
        .args(["add", "feature/paths-merge", "--repo", &repo_str])
        .assert()
        .success();

    let wt_dir = find_worktree_dir(&repo.path(), "feature-paths-merge");
    commit_file(&wt_dir, "p.txt", "paths work", "paths commit");

    let output = wt_core()
        .args([
            "merge",
            "feature/paths-merge",
            "--repo",
            &repo_str,
            "--print-paths",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8(output).expect("invalid utf8");
    let lines: Vec<&str> = stdout.trim().lines().collect();
    assert_eq!(lines.len(), 5, "expected 5 lines: {stdout}");

    // Line 1: repo root
    assert!(
        !lines[0].contains(".worktrees/"),
        "line 1 should be repo root: {}",
        lines[0]
    );

    // Line 2: branch name
    assert_eq!(lines[1], "feature/paths-merge");

    // Line 3: mainline
    assert_eq!(lines[2], "main");

    // Line 4: cleaned_up
    assert_eq!(lines[3], "true");

    // Line 5: pushed
    assert_eq!(lines[4], "false");
}

#[test]
fn merge_print_paths_conflicts_with_json() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    wt_core()
        .args([
            "merge",
            "any-branch",
            "--repo",
            &repo_str,
            "--print-paths",
            "--json",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("cannot be used with"));
}

// ── Mainline detection ──────────────────────────────────────────────

#[test]
fn merge_auto_detects_mainline() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    wt_core()
        .args(["add", "feature/mainline-test", "--repo", &repo_str])
        .assert()
        .success();

    let wt_dir = find_worktree_dir(&repo.path(), "feature-mainline-test");
    commit_file(&wt_dir, "m.txt", "mainline test", "mainline commit");

    wt_core()
        .args(["merge", "feature/mainline-test", "--repo", &repo_str])
        .assert()
        .success()
        .stdout(predicate::str::contains("into main"));
}

// ── Branch resolution ───────────────────────────────────────────────

#[test]
fn merge_no_branch_non_tty_inside_worktree_uses_cwd_inference() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    let output = wt_core()
        .args(["add", "infer-merge", "--repo", &repo_str, "--print-cd-path"])
        .output()
        .expect("add failed");
    let wt_path = String::from_utf8(output.stdout)
        .expect("invalid utf8")
        .trim()
        .to_string();

    // Commit something so merge has content
    commit_file(
        std::path::Path::new(&wt_path),
        "infer.txt",
        "infer",
        "infer commit",
    );

    // Merge from inside the worktree without specifying a branch
    wt_core()
        .args(["merge"])
        .current_dir(&wt_path)
        .assert()
        .success()
        .stdout(predicate::str::contains("infer-merge"));
}

#[test]
fn merge_no_branch_non_tty_from_main_worktree_errors() {
    let repo = fixtures::TestRepo::new();

    wt_core()
        .args(["merge"])
        .current_dir(repo.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "refusing to merge the main worktree",
        ))
        .code(4);
}

// ── Helpers ─────────────────────────────────────────────────────────

/// Get the git log as one-line entries.
fn git_log_oneline(repo: &std::path::Path, branch: &str) -> String {
    let mut cmd = StdCommand::new("git");
    cmd.args(["log", branch, "--oneline"]).current_dir(repo);
    for var in GIT_ENV_OVERRIDES {
        cmd.env_remove(var);
    }
    let output = cmd.output().expect("git log failed");
    String::from_utf8_lossy(&output.stdout).to_string()
}

/// Get `git status --porcelain` output.
fn git_status(repo: &std::path::Path) -> String {
    let mut cmd = StdCommand::new("git");
    cmd.args(["status", "--porcelain"]).current_dir(repo);
    for var in GIT_ENV_OVERRIDES {
        cmd.env_remove(var);
    }
    let output = cmd.output().expect("git status failed");
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// Assert that a branch does not exist in the repo.
fn assert_branch_deleted(repo: &std::path::Path, branch: &str) {
    let mut cmd = StdCommand::new("git");
    cmd.args(["branch", "--list", branch]).current_dir(repo);
    for var in GIT_ENV_OVERRIDES {
        cmd.env_remove(var);
    }
    let output = cmd.output().expect("git branch failed");
    let branches = String::from_utf8_lossy(&output.stdout).trim().to_string();
    assert!(
        branches.is_empty(),
        "branch should be deleted but found: {branches}"
    );
}

/// Assert that a branch exists in the repo.
fn assert_branch_exists(repo: &std::path::Path, branch: &str) {
    let mut cmd = StdCommand::new("git");
    cmd.args(["branch", "--list", branch]).current_dir(repo);
    for var in GIT_ENV_OVERRIDES {
        cmd.env_remove(var);
    }
    let output = cmd.output().expect("git branch failed");
    let branches = String::from_utf8_lossy(&output.stdout).trim().to_string();
    assert!(
        !branches.is_empty(),
        "branch '{branch}' should exist but was not found"
    );
}

/// Create a repo with a bare upstream configured as `origin`.
fn setup_repo_with_upstream() -> (fixtures::TestRepo, tempfile::TempDir) {
    // Create bare upstream
    let upstream = tempfile::TempDir::new().expect("failed to create upstream dir");
    let upstream_path = upstream.path().canonicalize().expect("canonicalize failed");
    run_git(&["init", "--bare", "-b", "main"], &upstream_path);

    // Create the working repo
    let repo = fixtures::TestRepo::new();

    // Add remote and push
    run_git(
        &[
            "remote",
            "add",
            "origin",
            &upstream_path.display().to_string(),
        ],
        &repo.path(),
    );
    run_git(&["push", "-u", "origin", "main"], &repo.path());

    (repo, upstream)
}
