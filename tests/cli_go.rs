mod fixtures;

use assert_cmd::Command;
use predicates::prelude::*;

fn wt_core() -> Command {
    Command::new(assert_cmd::cargo_bin!("wt-core"))
}

#[test]
fn go_resolves_existing_worktree() {
    let repo = fixtures::TestRepo::new();

    // Create a worktree first
    wt_core()
        .args(["add", "go-target", "--repo", &repo.path().display().to_string()])
        .assert()
        .success();

    // Go should resolve it
    let output = wt_core()
        .args(["go", "go-target", "--repo", &repo.path().display().to_string()])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8(output).expect("invalid utf8");
    assert!(stdout.contains("go-target"));
}

#[test]
fn go_print_cd_path_returns_bare_path() {
    let repo = fixtures::TestRepo::new();

    wt_core()
        .args(["add", "cd-go", "--repo", &repo.path().display().to_string()])
        .assert()
        .success();

    let output = wt_core()
        .args([
            "go",
            "cd-go",
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
}

#[test]
fn go_json_returns_structured_response() {
    let repo = fixtures::TestRepo::new();

    wt_core()
        .args(["add", "go-json", "--repo", &repo.path().display().to_string()])
        .assert()
        .success();

    let output = wt_core()
        .args([
            "go",
            "go-json",
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
    assert_eq!(json["branch"], "go-json");
}

#[test]
fn go_fails_for_nonexistent_branch() {
    let repo = fixtures::TestRepo::new();

    wt_core()
        .args([
            "go",
            "nonexistent-branch",
            "--repo",
            &repo.path().display().to_string(),
        ])
        .assert()
        .failure()
        .code(1) // Usage error
        .stderr(predicate::str::contains("no worktree found"));
}
