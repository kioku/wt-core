mod fixtures;

use assert_cmd::Command;
use predicates::prelude::*;

fn wt_core() -> Command {
    Command::new(assert_cmd::cargo_bin!("wt-core"))
}

fn add_worktree(repo: &fixtures::TestRepo, branch: &str) {
    wt_core()
        .args(["add", branch, "--repo", &repo.path().display().to_string()])
        .assert()
        .success();
}

#[cfg(unix)]
#[test]
fn exec_uses_worktree_cwd_and_preserves_argument_boundaries_and_stdio() {
    let repo = fixtures::TestRepo::new();
    add_worktree(&repo, "exec-target");
    let worktree = fixtures::find_worktree_dir(&repo.path(), "exec-target")
        .canonicalize()
        .expect("worktree should exist");

    let output = wt_core()
        .args([
            "exec",
            "exec-target",
            "--repo",
            &repo.path().display().to_string(),
            "--",
            "sh",
            "-c",
            "printf '%s\\n' \"$PWD\"; printf '<%s>\\n' \"$1\"; printf 'child-stderr\\n' >&2",
            "exec-test",
            "argument with spaces",
        ])
        .output()
        .expect("exec should start");

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("stdout should be utf8");
    assert_eq!(
        stdout,
        format!("{}\n<argument with spaces>\n", worktree.display())
    );
    assert_eq!(
        String::from_utf8(output.stderr).expect("stderr should be utf8"),
        "child-stderr\n"
    );
}

#[test]
fn exec_json_metadata_is_on_stderr_and_child_stdout_is_unchanged() {
    let repo = fixtures::TestRepo::new();
    add_worktree(&repo, "exec-json");

    let output = wt_core()
        .args([
            "exec",
            "exec-json",
            "--repo",
            &repo.path().display().to_string(),
            "--json",
            "--",
            "git",
            "rev-parse",
            "--show-toplevel",
        ])
        .output()
        .expect("exec should start");

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("stdout should be utf8");
    let expected = fixtures::find_worktree_dir(&repo.path(), "exec-json")
        .canonicalize()
        .expect("worktree should exist");
    assert_eq!(stdout.trim(), expected.to_string_lossy());

    let stderr = String::from_utf8(output.stderr).expect("stderr should be utf8");
    let metadata: serde_json::Value = serde_json::from_str(stderr.trim())
        .expect("--json should emit one metadata object on stderr");
    assert_eq!(metadata["ok"], true);
    assert_eq!(metadata["branch"], "exec-json");
    assert_eq!(
        metadata["repo_root"],
        repo.path().to_string_lossy().to_string()
    );
    assert_eq!(
        metadata["worktree_path"],
        expected.to_string_lossy().to_string()
    );
}

#[test]
fn exec_fails_clearly_when_worktree_is_missing() {
    let repo = fixtures::TestRepo::new();

    wt_core()
        .args([
            "exec",
            "missing-worktree",
            "--repo",
            &repo.path().display().to_string(),
            "--",
            "git",
            "--version",
        ])
        .assert()
        .failure()
        .code(1)
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::contains(
            "no worktree found for branch 'missing-worktree'",
        ));
}

#[cfg(unix)]
#[test]
fn exec_returns_nonzero_child_exit_status() {
    let repo = fixtures::TestRepo::new();
    add_worktree(&repo, "exec-status");

    wt_core()
        .args([
            "exec",
            "exec-status",
            "--repo",
            &repo.path().display().to_string(),
            "--",
            "sh",
            "-c",
            "exit 23",
        ])
        .assert()
        .code(23);
}

#[cfg(unix)]
#[test]
fn exec_reports_signal_termination_with_shell_status() {
    let repo = fixtures::TestRepo::new();
    add_worktree(&repo, "exec-signal");

    wt_core()
        .args([
            "exec",
            "exec-signal",
            "--repo",
            &repo.path().display().to_string(),
            "--",
            "sh",
            "-c",
            "kill -TERM $$",
        ])
        .assert()
        .code(143);
}
