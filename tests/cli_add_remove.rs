mod fixtures;

use assert_cmd::Command;
use predicates::prelude::*;

fn wt_core() -> Command {
    Command::new(assert_cmd::cargo_bin!("wt-core"))
}

#[test]
fn add_creates_worktree_and_branch() {
    let repo = fixtures::TestRepo::new();

    let output = wt_core()
        .args([
            "add",
            "feature/login",
            "--repo",
            &repo.path().display().to_string(),
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8(output).expect("invalid utf8");
    assert!(stdout.contains("feature/login"));
    assert!(stdout.contains(".worktrees/"));

    // Verify the worktree directory exists
    let entries: Vec<_> = std::fs::read_dir(repo.path().join(".worktrees"))
        .expect("no .worktrees dir")
        .flatten()
        .collect();
    assert_eq!(entries.len(), 1);
}

#[test]
fn add_json_returns_structured_response() {
    let repo = fixtures::TestRepo::new();

    let output = wt_core()
        .args([
            "add",
            "feature/json-test",
            "--repo",
            &repo.path().display().to_string(),
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let json: serde_json::Value = serde_json::from_slice(&output).expect("invalid json");
    assert_eq!(json["ok"], true);
    assert!(json["cd_path"].as_str().is_some());
    assert!(json["worktree_path"].as_str().is_some());
    assert!(json["repo_root"].as_str().is_some());
    assert_eq!(json["branch"], "feature/json-test");
}

#[test]
fn add_print_cd_path_returns_bare_path() {
    let repo = fixtures::TestRepo::new();

    let output = wt_core()
        .args([
            "add",
            "feature/cd-test",
            "--repo",
            &repo.path().display().to_string(),
            "--print-cd-path",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let path = String::from_utf8(output).expect("invalid utf8");
    let path = path.trim();
    assert!(path.starts_with('/'));
    assert!(path.contains(".worktrees/"));
    // Must not be JSON
    assert!(!path.starts_with('{'));
}

#[test]
fn add_fails_when_branch_exists() {
    let repo = fixtures::TestRepo::new();

    // Create branch first
    wt_core()
        .args([
            "add",
            "dupe-branch",
            "--repo",
            &repo.path().display().to_string(),
        ])
        .assert()
        .success();

    // Second add should fail
    wt_core()
        .args([
            "add",
            "dupe-branch",
            "--repo",
            &repo.path().display().to_string(),
        ])
        .assert()
        .failure()
        .code(5) // Conflict exit code
        .stderr(predicate::str::contains("already exists"));
}

#[test]
fn add_with_base_revision() {
    let repo = fixtures::TestRepo::new();

    wt_core()
        .args([
            "add",
            "from-head",
            "--base",
            "HEAD",
            "--repo",
            &repo.path().display().to_string(),
        ])
        .assert()
        .success();
}

#[test]
fn add_with_invalid_base_fails() {
    let repo = fixtures::TestRepo::new();

    wt_core()
        .args([
            "add",
            "bad-base",
            "--base",
            "nonexistent-ref-xyz",
            "--repo",
            &repo.path().display().to_string(),
        ])
        .assert()
        .failure();
}

#[test]
fn remove_deletes_worktree_and_branch() {
    let repo = fixtures::TestRepo::new();

    // Add a worktree first
    wt_core()
        .args([
            "add",
            "to-remove",
            "--repo",
            &repo.path().display().to_string(),
        ])
        .assert()
        .success();

    // Remove it
    wt_core()
        .args([
            "remove",
            "to-remove",
            "--repo",
            &repo.path().display().to_string(),
        ])
        .assert()
        .success();

    // Verify worktree is gone
    let entries: Vec<_> = std::fs::read_dir(repo.path().join(".worktrees"))
        .unwrap_or_else(|_| std::fs::read_dir(repo.path()).expect("repo gone"))
        .flatten()
        .filter(|e| e.path().is_dir())
        .collect();
    assert_eq!(entries.len(), 0);
}

#[test]
fn remove_refuses_main_worktree() {
    let repo = fixtures::TestRepo::new();

    // Try to remove main branch (which is the main worktree)
    wt_core()
        .args([
            "remove",
            "main",
            "--repo",
            &repo.path().display().to_string(),
        ])
        .assert()
        .failure()
        .code(4); // Invariant violation
}

#[test]
fn remove_json_includes_removed_path() {
    let repo = fixtures::TestRepo::new();

    wt_core()
        .args([
            "add",
            "json-rm",
            "--repo",
            &repo.path().display().to_string(),
        ])
        .assert()
        .success();

    let output = wt_core()
        .args([
            "remove",
            "json-rm",
            "--repo",
            &repo.path().display().to_string(),
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let json: serde_json::Value = serde_json::from_slice(&output).expect("invalid json");
    assert_eq!(json["ok"], true);
    assert!(json["removed_path"].as_str().is_some());
    assert!(json["repo_root"].as_str().is_some());
}

#[test]
fn remove_print_paths_returns_three_lines() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    // Add with a slashed branch name to verify we get the real name, not the slug
    wt_core()
        .args(["add", "feature/paths-rm", "--repo", &repo_str])
        .assert()
        .success();

    let output = wt_core()
        .args([
            "remove",
            "feature/paths-rm",
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
    assert_eq!(lines.len(), 3, "expected exactly 3 lines: {stdout}");

    // Line 1: removed worktree path (under .worktrees/)
    assert!(
        lines[0].contains(".worktrees/"),
        "line 1 should be removed path: {}",
        lines[0]
    );

    // Line 2: repo root (not under .worktrees/)
    assert!(
        !lines[1].contains(".worktrees/"),
        "line 2 should be repo root, not a worktree path: {}",
        lines[1]
    );

    // Line 3: actual branch name (not the sanitized slug)
    assert_eq!(
        lines[2], "feature/paths-rm",
        "line 3 should be the real branch name, not the slug"
    );

    // No line should be JSON
    assert!(!lines[0].starts_with('{'));
}

#[test]
fn remove_print_paths_conflicts_with_json() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    wt_core()
        .args([
            "remove",
            "any-branch",
            "--repo",
            &repo_str,
            "--print-paths",
            "--json",
        ])
        .assert()
        .failure()
        .stderr(predicates::prelude::predicate::str::contains(
            "cannot be used with",
        ));
}
