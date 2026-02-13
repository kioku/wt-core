mod fixtures;

use assert_cmd::Command;
use predicates::prelude::*;

fn wt_core() -> Command {
    Command::new(assert_cmd::cargo_bin!("wt-core"))
}

#[test]
fn not_a_repo_exits_3() {
    let dir = tempfile::tempdir().expect("temp dir");

    wt_core()
        .args(["list", "--repo", &dir.path().display().to_string()])
        .assert()
        .failure()
        .code(3)
        .stderr(predicate::str::contains("not a git repository"));
}

#[test]
fn list_empty_repo_shows_main() {
    let repo = fixtures::TestRepo::new();

    let output = wt_core()
        .args(["list", "--repo", &repo.path().display().to_string()])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8(output).expect("invalid utf8");
    // Should show at least the main worktree
    assert!(stdout.contains("main") || stdout.contains(&repo.path().display().to_string()));
}

#[test]
fn list_json_returns_array() {
    let repo = fixtures::TestRepo::new();

    let output = wt_core()
        .args([
            "list",
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
    assert!(json["worktrees"].as_array().is_some());
    assert!(!json["worktrees"].as_array().expect("array").is_empty());
}

#[test]
fn doctor_on_clean_repo() {
    let repo = fixtures::TestRepo::new();

    wt_core()
        .args(["doctor", "--repo", &repo.path().display().to_string()])
        .assert()
        .success();
}

#[test]
fn doctor_json_output() {
    let repo = fixtures::TestRepo::new();

    let output = wt_core()
        .args([
            "doctor",
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
    assert!(json["diagnostics"].as_array().is_some());
}

#[test]
fn path_convention_worktrees_dir() {
    let repo = fixtures::TestRepo::new();

    let output = wt_core()
        .args([
            "add",
            "feature/nested",
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

    // Must be under .worktrees/
    assert!(path.contains("/.worktrees/"));

    // Must use collision-safe naming: slug--8hex
    let dir_name = std::path::Path::new(path)
        .file_name()
        .expect("dir name")
        .to_string_lossy();
    assert!(
        dir_name.contains("--"),
        "directory name should contain '--' separator: {dir_name}"
    );
}

#[test]
fn remove_nonexistent_branch_fails() {
    let repo = fixtures::TestRepo::new();

    wt_core()
        .args([
            "remove",
            "ghost-branch",
            "--repo",
            &repo.path().display().to_string(),
        ])
        .assert()
        .failure()
        .code(1) // Usage error
        .stderr(predicate::str::contains("no worktree found"));
}

#[test]
fn no_subcommand_shows_help() {
    wt_core()
        .assert()
        .failure()
        .stderr(predicate::str::contains("Usage"));
}
