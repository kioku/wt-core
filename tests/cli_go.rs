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
        .args([
            "add",
            "go-target",
            "--repo",
            &repo.path().display().to_string(),
        ])
        .assert()
        .success();

    // Go should resolve it
    let output = wt_core()
        .args([
            "go",
            "go-target",
            "--repo",
            &repo.path().display().to_string(),
        ])
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
        .args([
            "add",
            "go-json",
            "--repo",
            &repo.path().display().to_string(),
        ])
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

#[test]
fn go_no_branch_non_tty_errors() {
    let repo = fixtures::TestRepo::new();

    // Create two worktrees so auto-select does not kick in
    for branch in &["picker-a", "picker-b"] {
        wt_core()
            .args(["add", branch, "--repo", &repo.path().display().to_string()])
            .assert()
            .success();
    }

    // Running without a branch in a non-TTY context should fail
    wt_core()
        .args(["go", "--repo", &repo.path().display().to_string()])
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains(
            "interactive mode requires a terminal",
        ));
}

#[test]
fn go_no_branch_json_errors() {
    let repo = fixtures::TestRepo::new();

    wt_core()
        .args(["go", "--json", "--repo", &repo.path().display().to_string()])
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains(
            "interactive picker cannot be used with --json or --print-cd-path",
        ));
}

#[test]
fn go_no_branch_print_cd_path_errors() {
    let repo = fixtures::TestRepo::new();

    wt_core()
        .args([
            "go",
            "--print-cd-path",
            "--repo",
            &repo.path().display().to_string(),
        ])
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains(
            "interactive picker cannot be used with --json or --print-cd-path",
        ));
}

#[test]
fn go_no_branch_auto_selects_single_worktree() {
    let repo = fixtures::TestRepo::new();

    // Create exactly one worktree
    wt_core()
        .args([
            "add",
            "only-one",
            "--repo",
            &repo.path().display().to_string(),
        ])
        .assert()
        .success();

    // With only one non-main worktree, auto-select should kick in
    // even without a TTY (it skips the picker entirely)
    let output = wt_core()
        .args(["go", "--repo", &repo.path().display().to_string()])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8(output).expect("invalid utf8");
    assert!(stdout.contains("only-one"));
}

#[test]
fn go_branch_with_interactive_flag_conflicts() {
    let repo = fixtures::TestRepo::new();

    // Providing both a branch and -i is a clap conflict
    wt_core()
        .args([
            "go",
            "some-branch",
            "-i",
            "--repo",
            &repo.path().display().to_string(),
        ])
        .assert()
        .failure()
        .code(2) // clap exits with code 2 for usage errors
        .stderr(predicate::str::contains("cannot be used with"));
}

#[test]
fn go_no_worktrees_to_select_errors() {
    let repo = fixtures::TestRepo::new();

    // No worktrees created — only main exists
    wt_core()
        .args(["go", "--repo", &repo.path().display().to_string()])
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("no worktrees to select"));
}
