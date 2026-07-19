mod fixtures;

use assert_cmd::Command;
use predicates::prelude::*;
use std::path::PathBuf;
#[cfg(unix)]
use std::process::{Command as ProcessCommand, Stdio};
#[cfg(unix)]
use std::{fs, thread, time::Duration};

fn wt_core() -> Command {
    Command::new(assert_cmd::cargo_bin!("wt-core"))
}

fn add_worktree(repo: &fixtures::TestRepo, branch: &str) {
    wt_core()
        .args(["add", branch, "--repo", &repo.path().display().to_string()])
        .assert()
        .success();
}

fn canonicalize_reported_path(value: &str) -> PathBuf {
    PathBuf::from(value)
        .canonicalize()
        .expect("reported path should exist")
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

#[cfg(windows)]
#[test]
fn exec_uses_windows_command_cwd_and_status() {
    let repo = fixtures::TestRepo::new();
    add_worktree(&repo, "exec-windows");
    let expected = fixtures::find_worktree_dir(&repo.path(), "exec-windows")
        .canonicalize()
        .expect("worktree should exist");

    let output = wt_core()
        .args([
            "exec",
            "exec-windows",
            "--repo",
            &repo.path().display().to_string(),
            "--",
            "cmd",
            "/C",
            "echo %CD% & echo argument with spaces",
        ])
        .output()
        .expect("exec should start");

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("stdout should be utf8");
    let lines: Vec<_> = stdout.lines().map(str::trim).collect();
    assert_eq!(canonicalize_reported_path(lines[0]), expected);
    assert_eq!(lines[1], "argument with spaces");
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
    assert_eq!(canonicalize_reported_path(stdout.trim()), expected);

    let stderr = String::from_utf8(output.stderr).expect("stderr should be utf8");
    let metadata_line = stderr.lines().next().expect("metadata line should exist");
    let metadata: serde_json::Value =
        serde_json::from_str(metadata_line).expect("--json metadata should be valid JSON");
    assert_eq!(metadata["event"], "exec_resolved");
    assert_eq!(metadata["resolved"], true);
    assert_eq!(metadata["branch"], "exec-json");
    assert_eq!(
        canonicalize_reported_path(
            metadata["repo_root"]
                .as_str()
                .expect("repo_root metadata should be a string")
        ),
        repo.path()
    );
    assert_eq!(
        canonicalize_reported_path(
            metadata["worktree_path"]
                .as_str()
                .expect("worktree_path metadata should be a string")
        ),
        expected
    );
}

#[cfg(unix)]
#[test]
fn exec_json_metadata_precedes_child_stderr() {
    let repo = fixtures::TestRepo::new();
    add_worktree(&repo, "exec-json-stderr");

    let output = wt_core()
        .args([
            "exec",
            "exec-json-stderr",
            "--repo",
            &repo.path().display().to_string(),
            "--json",
            "--",
            "sh",
            "-c",
            "printf 'child-stderr\\n' >&2",
        ])
        .output()
        .expect("exec should start");

    assert!(output.status.success());
    let stderr = String::from_utf8(output.stderr).expect("stderr should be utf8");
    let mut lines = stderr.lines();
    let metadata: serde_json::Value =
        serde_json::from_str(lines.next().expect("metadata line should exist"))
            .expect("metadata should be the first stderr line");
    assert_eq!(metadata["event"], "exec_resolved");
    assert_eq!(lines.next(), Some("child-stderr"));
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

#[test]
fn exec_json_metadata_stays_resolution_only_when_spawn_fails() {
    let repo = fixtures::TestRepo::new();
    add_worktree(&repo, "exec-json-spawn-failure");
    let missing_program = repo.path().join("definitely-missing-exec-program");

    let output = wt_core()
        .args([
            "exec",
            "exec-json-spawn-failure",
            "--repo",
            &repo.path().display().to_string(),
            "--json",
            "--",
            &missing_program.display().to_string(),
        ])
        .output()
        .expect("exec should start");

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).expect("stderr should be utf8");
    let mut lines = stderr.lines();
    let metadata: serde_json::Value =
        serde_json::from_str(lines.next().expect("metadata line should exist"))
            .expect("metadata should be valid JSON");
    assert_eq!(metadata["event"], "exec_resolved");
    assert_eq!(metadata["resolved"], true);
    assert!(
        lines.any(|line| line.contains("failed to execute")),
        "spawn failure should be reported after resolution metadata"
    );
}

#[test]
fn exec_returns_nonzero_child_exit_status() {
    let repo = fixtures::TestRepo::new();
    add_worktree(&repo, "exec-status");

    let mut command = wt_core();
    command.args([
        "exec",
        "exec-status",
        "--repo",
        &repo.path().display().to_string(),
        "--",
    ]);
    if cfg!(windows) {
        command.args(["cmd", "/C", "exit 23"]);
    } else {
        command.args(["sh", "-c", "exit 23"]);
    }
    command.assert().code(23);
}

#[test]
fn exec_sanitizes_inherited_git_context() {
    let repo = fixtures::TestRepo::new();
    let other_repo = fixtures::TestRepo::new();
    add_worktree(&repo, "exec-git-env");
    let git_dir = repo.path().join(".git");
    let other_common_dir = other_repo.path().join(".git");

    let output = wt_core()
        .args([
            "exec",
            "exec-git-env",
            "--repo",
            &repo.path().display().to_string(),
            "--",
            "git",
            "branch",
            "--show-current",
        ])
        .env("GIT_DIR", &git_dir)
        .env("GIT_COMMON_DIR", &other_common_dir)
        .output()
        .expect("exec should start");

    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout)
            .expect("stdout should be utf8")
            .trim(),
        "exec-git-env"
    );
}

#[cfg(unix)]
#[test]
fn exec_replacement_receives_parent_signal_without_orphaning_child() {
    use std::os::unix::process::ExitStatusExt;

    let repo = fixtures::TestRepo::new();
    add_worktree(&repo, "exec-parent-signal");
    let pid_file = repo.path().join("exec-parent-signal.pid");
    let repo_arg = repo.path().display().to_string();
    let pid_file_arg = pid_file.display().to_string();
    let mut child = ProcessCommand::new(assert_cmd::cargo_bin!("wt-core"))
        .args([
            "exec",
            "exec-parent-signal",
            "--repo",
            &repo_arg,
            "--",
            "sh",
            "-c",
            "printf '%s\\n' \"$$\" > \"$1\"; exec sleep 30",
            "exec-parent-signal-test",
            &pid_file_arg,
        ])
        .spawn()
        .expect("exec should start");
    let parent_pid = child.id();

    let target_pid = (0..100).find_map(|_| {
        let value = fs::read_to_string(&pid_file)
            .ok()
            .and_then(|value| value.trim().parse::<u32>().ok());
        if value.is_none() {
            thread::sleep(Duration::from_millis(10));
        }
        value
    });
    if target_pid.is_none() {
        let _ = child.kill();
        let _ = child.wait();
    }
    let target_pid = target_pid.expect("child pid should be written");

    ProcessCommand::new("kill")
        .stderr(Stdio::null())
        .args(["-TERM", &parent_pid.to_string()])
        .status()
        .expect("kill should start")
        .success()
        .then_some(())
        .expect("parent signal should be delivered");
    let status = child.wait().expect("wt-core should exit after signal");
    let target_alive = ProcessCommand::new("kill")
        .stderr(Stdio::null())
        .args(["-0", &target_pid.to_string()])
        .status()
        .map(|status| status.success())
        .unwrap_or(false);
    if target_alive {
        let _ = ProcessCommand::new("kill")
            .stderr(Stdio::null())
            .args(["-KILL", &target_pid.to_string()])
            .status();
    }

    assert_eq!(target_pid, parent_pid, "Unix exec should preserve the PID");
    assert_eq!(
        status.signal(),
        Some(15),
        "SIGTERM should reach the command"
    );
    assert!(!target_alive, "the command must not outlive wt-core");
}

#[cfg(unix)]
#[test]
fn exec_reports_signal_termination_directly() {
    use std::os::unix::process::ExitStatusExt;

    let repo = fixtures::TestRepo::new();
    add_worktree(&repo, "exec-signal");

    let status = ProcessCommand::new(assert_cmd::cargo_bin!("wt-core"))
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
        .status()
        .expect("exec should start");
    assert_eq!(status.signal(), Some(15));
}
