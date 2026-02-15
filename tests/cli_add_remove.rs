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

// ── Interactive picker fallback tests ───────────────────────────────
//
// These tests run in a non-TTY (CI) context, so the picker never opens.
// They verify the fallback routing: machine formats use cwd inference,
// non-TTY human format uses cwd inference, and appropriate errors are
// shown when neither the picker nor cwd inference can resolve a branch.

#[test]
fn remove_no_branch_non_tty_inside_worktree_uses_cwd_inference() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    // Create a worktree
    let output = wt_core()
        .args(["add", "infer-rm", "--repo", &repo_str, "--print-cd-path"])
        .output()
        .expect("add failed");
    let wt_path = String::from_utf8(output.stdout)
        .expect("invalid utf8")
        .trim()
        .to_string();

    // Remove from inside the worktree without specifying a branch.
    // Non-TTY → falls back to cwd inference → removes current worktree.
    wt_core()
        .args(["remove"])
        .current_dir(&wt_path)
        .assert()
        .success()
        .stdout(predicate::str::contains("infer-rm"));
}

#[test]
fn remove_no_branch_non_tty_from_main_worktree_errors() {
    let repo = fixtures::TestRepo::new();

    // Running from the main worktree root without a branch.
    // Non-TTY → cwd inference resolves to main → invariant error.
    wt_core()
        .args(["remove"])
        .current_dir(repo.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "refusing to remove the main worktree",
        ))
        .code(4);
}

#[test]
fn remove_no_branch_non_tty_outside_any_worktree_errors() {
    let repo = fixtures::TestRepo::new();

    // Running with --repo but cwd is NOT inside the repo at all.
    // Non-TTY → cwd inference fails → usage error.
    wt_core()
        .args(["remove", "--repo", &repo.path().display().to_string()])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "no branch specified and cwd is not inside a worktree",
        ))
        .code(1);
}

#[test]
fn remove_no_branch_json_uses_cwd_inference() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    // Create a worktree
    let output = wt_core()
        .args(["add", "json-infer", "--repo", &repo_str, "--print-cd-path"])
        .output()
        .expect("add failed");
    let wt_path = String::from_utf8(output.stdout)
        .expect("invalid utf8")
        .trim()
        .to_string();

    // --json without a branch uses cwd inference
    let output = wt_core()
        .args(["remove", "--json"])
        .current_dir(&wt_path)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let json: serde_json::Value = serde_json::from_slice(&output).expect("invalid json");
    assert_eq!(json["ok"], true);
    assert_eq!(json["branch"], "json-infer");
}

#[test]
fn remove_no_branch_print_paths_uses_cwd_inference() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    // Create a worktree
    let output = wt_core()
        .args(["add", "paths-infer", "--repo", &repo_str, "--print-cd-path"])
        .output()
        .expect("add failed");
    let wt_path = String::from_utf8(output.stdout)
        .expect("invalid utf8")
        .trim()
        .to_string();

    // --print-paths without a branch uses cwd inference
    let output = wt_core()
        .args(["remove", "--print-paths"])
        .current_dir(&wt_path)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8(output).expect("invalid utf8");
    let lines: Vec<&str> = stdout.trim().lines().collect();
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[2], "paths-infer");
}

#[test]
fn remove_no_branch_print_paths_from_nested_dir_uses_cwd_inference() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    // Create a worktree.
    let output = wt_core()
        .args([
            "add",
            "paths-infer-nested",
            "--repo",
            &repo_str,
            "--print-cd-path",
        ])
        .output()
        .expect("add failed");
    let wt_path = String::from_utf8(output.stdout)
        .expect("invalid utf8")
        .trim()
        .to_string();

    // Move into a nested subdirectory inside that worktree.
    let nested = std::path::Path::new(&wt_path).join("src/nested");
    std::fs::create_dir_all(&nested).expect("failed to create nested test dir");

    // --print-paths without a branch should still infer the linked branch
    // from nested cwd, not the main worktree.
    let output = wt_core()
        .args(["remove", "--print-paths"])
        .current_dir(&nested)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8(output).expect("invalid utf8");
    let lines: Vec<&str> = stdout.trim().lines().collect();
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[2], "paths-infer-nested");
}

#[test]
fn remove_no_branch_no_worktrees_non_tty_errors() {
    let repo = fixtures::TestRepo::new();

    // No worktrees created, no branch specified, cwd outside the test repo.
    // Non-TTY → cwd inference fails (cwd is not inside any worktree) → usage error.
    wt_core()
        .args(["remove", "--repo", &repo.path().display().to_string()])
        .assert()
        .failure()
        .code(1);
}

// ── Remote tracking tests ───────────────────────────────────────────

/// Helper: push a branch to the bare origin from a temporary working copy,
/// then fetch in the clone so `origin/<branch>` exists.
fn push_remote_branch(
    origin_path: &std::path::Path,
    clone_path: &std::path::Path,
    branch: &str,
    filename: &str,
) {
    // Create a throwaway clone to push from (avoids contaminating the test clone)
    let pusher = tempfile::TempDir::new().expect("failed to create pusher dir");
    fixtures::run_git(
        &[
            "clone",
            &origin_path.display().to_string(),
            &pusher.path().display().to_string(),
        ],
        origin_path,
    );
    fixtures::run_git(&["config", "user.email", "test@test.com"], pusher.path());
    fixtures::run_git(&["config", "user.name", "Test"], pusher.path());
    fixtures::run_git(&["checkout", "-b", branch], pusher.path());
    fixtures::commit_file(
        pusher.path(),
        filename,
        "content",
        &format!("add {filename}"),
    );
    fixtures::run_git(&["push", "origin", branch], pusher.path());

    // Fetch in the test clone so origin/<branch> is visible
    fixtures::run_git(&["fetch", "origin"], clone_path);
}

#[test]
fn add_tracks_remote_branch_when_exists() {
    let repos = fixtures::ClonedTestRepo::new();
    let clone_str = repos.path().display().to_string();

    // Push a branch to origin from a separate clone
    push_remote_branch(
        &repos.origin_path(),
        &repos.path(),
        "feature/review",
        "review.txt",
    );

    // Now `wt add feature/review` should auto-track origin/feature/review
    wt_core()
        .args(["add", "feature/review", "--repo", &clone_str])
        .assert()
        .success()
        .stdout(predicate::str::contains("tracking 'origin/feature/review'"));
}

#[test]
fn add_remote_tracking_json_includes_tracking_field() {
    let repos = fixtures::ClonedTestRepo::new();
    let clone_str = repos.path().display().to_string();

    push_remote_branch(
        &repos.origin_path(),
        &repos.path(),
        "feature/json-track",
        "track.txt",
    );

    let output = wt_core()
        .args(["add", "feature/json-track", "--repo", &clone_str, "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let json: serde_json::Value = serde_json::from_slice(&output).expect("invalid json");
    assert_eq!(json["ok"], true);
    assert_eq!(json["tracking"], true);
    assert_eq!(json["branch"], "feature/json-track");
}

#[test]
fn add_with_base_ignores_remote_tracking() {
    let repos = fixtures::ClonedTestRepo::new();
    let clone_str = repos.path().display().to_string();

    push_remote_branch(
        &repos.origin_path(),
        &repos.path(),
        "feature/base-override",
        "base.txt",
    );

    // Even though origin/feature/base-override exists, --base forces a new branch
    let output = wt_core()
        .args([
            "add",
            "feature/base-override",
            "--base",
            "HEAD",
            "--repo",
            &clone_str,
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let json: serde_json::Value = serde_json::from_slice(&output).expect("invalid json");
    assert_eq!(
        json["tracking"], false,
        "--base should skip remote tracking"
    );
}

#[test]
fn add_new_branch_when_no_remote() {
    let repos = fixtures::ClonedTestRepo::new();
    let clone_str = repos.path().display().to_string();

    // No remote branch exists for "feature/brand-new"
    let output = wt_core()
        .args(["add", "feature/brand-new", "--repo", &clone_str, "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let json: serde_json::Value = serde_json::from_slice(&output).expect("invalid json");
    assert_eq!(json["tracking"], false);
}

#[test]
fn add_still_errors_when_local_branch_exists() {
    let repos = fixtures::ClonedTestRepo::new();
    let clone_str = repos.path().display().to_string();

    push_remote_branch(
        &repos.origin_path(),
        &repos.path(),
        "feature/local-conflict",
        "conflict.txt",
    );

    // Create the local branch manually (simulating a previous checkout)
    fixtures::run_git(
        &[
            "branch",
            "feature/local-conflict",
            "origin/feature/local-conflict",
        ],
        &repos.path(),
    );

    // Should fail with conflict error even though remote exists
    wt_core()
        .args(["add", "feature/local-conflict", "--repo", &clone_str])
        .assert()
        .failure()
        .code(5) // Conflict
        .stderr(predicate::str::contains("already exists"));
}

#[test]
fn add_remote_tracking_sets_correct_upstream() {
    let repos = fixtures::ClonedTestRepo::new();
    let clone_str = repos.path().display().to_string();

    push_remote_branch(
        &repos.origin_path(),
        &repos.path(),
        "feature/upstream-check",
        "upstream.txt",
    );

    let output = wt_core()
        .args([
            "add",
            "feature/upstream-check",
            "--repo",
            &clone_str,
            "--print-cd-path",
        ])
        .output()
        .expect("add failed");
    let wt_path = String::from_utf8(output.stdout)
        .expect("invalid utf8")
        .trim()
        .to_string();

    // Verify the upstream is set correctly by checking @{u}.
    // We need to use the run_git-style env clearing so hooks don't interfere.
    let mut cmd = std::process::Command::new("git");
    cmd.args(["rev-parse", "--abbrev-ref", "feature/upstream-check@{u}"])
        .current_dir(&wt_path);
    for var in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_PREFIX",
    ] {
        cmd.env_remove(var);
    }
    let upstream_output = cmd.output().expect("git rev-parse failed");
    assert!(
        upstream_output.status.success(),
        "git rev-parse @{{u}} failed: {}",
        String::from_utf8_lossy(&upstream_output.stderr)
    );
    let upstream = String::from_utf8(upstream_output.stdout)
        .expect("invalid utf8")
        .trim()
        .to_string();
    assert_eq!(upstream, "origin/feature/upstream-check");

    // Also verify the worktree has the remote branch's content
    assert!(
        std::path::Path::new(&wt_path).join("upstream.txt").exists(),
        "tracked worktree should contain the remote branch's files"
    );
}
