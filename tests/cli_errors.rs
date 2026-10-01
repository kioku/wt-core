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
    assert!(std::path::Path::new(path).starts_with(repo.path().join(".worktrees")));

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

#[test]
fn json_repository_failures_emit_one_envelope() {
    let dir = tempfile::tempdir().expect("temp dir");
    for command in ["list", "doctor", "setup", "prune", "go", "remove", "merge"] {
        let output = wt_core()
            .args([command, "--json", "--repo"])
            .arg(dir.path())
            .output()
            .expect("run CLI");
        assert_eq!(output.status.code(), Some(3), "{command}");
        let value: serde_json::Value =
            serde_json::from_slice(&output.stdout).expect("one JSON response");
        assert_eq!(value["ok"], false);
        assert_eq!(value["exit_code"], 3);
    }
}

#[test]
fn doctor_reports_unreadable_managed_directory() {
    let repo = fixtures::TestRepo::new();
    std::fs::write(repo.path().join(".worktrees"), "not a directory").expect("fixture");
    let output = wt_core()
        .args(["doctor", "--json", "--repo"])
        .arg(repo.path())
        .output()
        .expect("doctor");
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).expect("JSON");
    assert!(value.to_string().contains("cannot inspect"));
    assert!(!value.to_string().contains("all worktrees healthy"));
}

#[test]
fn doctor_checks_external_stale_registration_without_managed_directory() {
    let repo = fixtures::TestRepo::new();
    let external = tempfile::tempdir().expect("external parent");
    let path = external.path().join("checkout");
    fixtures::run_git(
        &[
            "worktree",
            "add",
            "-b",
            "external",
            path.to_str().expect("path"),
        ],
        &repo.path(),
    );
    std::fs::remove_dir_all(&path).expect("remove disposable checkout");
    let before = std::fs::read_dir(repo.path().join(".git/worktrees"))
        .expect("registrations")
        .count();
    let output = wt_core()
        .args(["doctor", "--json", "--repo"])
        .arg(repo.path())
        .output()
        .expect("doctor");
    assert!(String::from_utf8_lossy(&output.stdout).contains("stale worktree metadata"));
    assert_eq!(
        before,
        std::fs::read_dir(repo.path().join(".git/worktrees"))
            .expect("registrations retained")
            .count()
    );
}

#[test]
fn long_branch_creates_bounded_collision_safe_directory() {
    let repo = fixtures::TestRepo::new();
    // Git for Windows needs its opt-in for the long ref path itself; wt only
    // bounds the separate worktree/admin-directory component it generates.
    #[cfg(windows)]
    assert!(std::process::Command::new("git")
        .args(["config", "core.longpaths", "true"])
        .current_dir(repo.path())
        .status()
        .expect("configure long ref paths")
        .success());
    let mut paths = Vec::new();
    for suffix in ["a", "b"] {
        let branch = format!("{}{}", "x".repeat(248), suffix);
        let output = wt_core()
            .args(["add", &branch, "--json", "--repo"])
            .arg(repo.path())
            .output()
            .expect("add");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).expect("JSON");
        let path = std::path::PathBuf::from(value["worktree_path"].as_str().expect("path"));
        assert!(path.is_dir());
        assert_eq!(path.file_name().expect("component").len(), 120);
        paths.push(path);
    }
    assert_ne!(paths[0], paths[1]);
}

#[test]
fn json_failure_after_repository_resolution_is_structured() {
    let repo = fixtures::TestRepo::new();
    let output = wt_core()
        .args([
            "list",
            "--json",
            "--stats",
            "--against",
            "missing-revision",
            "--repo",
        ])
        .arg(repo.path())
        .output()
        .expect("list");
    assert_eq!(output.status.code(), Some(1));
    let value: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("one JSON response");
    assert_eq!(value["ok"], false);
    assert_eq!(value["exit_code"], 1);
}
