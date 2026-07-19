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
        .stderr(predicate::str::contains("merge aborted"));

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

// ── Dirty worktree tests ────────────────────────────────────────────

#[test]
fn merge_dirty_main_worktree_errors() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    wt_core()
        .args(["add", "feature/dirty-main", "--repo", &repo_str])
        .assert()
        .success();

    let wt_dir = find_worktree_dir(&repo.path(), "feature-dirty-main");
    // Modify the same file on the feature branch so the merge would touch it.
    commit_file(&wt_dir, "README.md", "feature changes", "feature change");

    // Modify the same tracked file on main without committing.
    // Git refuses to merge when tracked files with local changes would
    // be overwritten by the merge.
    std::fs::write(repo.path().join("README.md"), "dirty local changes").expect("write failed");

    wt_core()
        .args(["merge", "feature/dirty-main", "--repo", &repo_str])
        .assert()
        .failure();

    // Restore tracked file so TestRepo::drop doesn't fail.
    run_git(&["checkout", "--", "README.md"], &repo.path());
}

#[test]
fn merge_cleanup_failure_is_warning_not_error() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    wt_core()
        .args(["add", "feature/dirty-wt", "--repo", &repo_str])
        .assert()
        .success();

    let wt_dir = find_worktree_dir(&repo.path(), "feature-dirty-wt");
    commit_file(&wt_dir, "clean.txt", "clean commit", "add clean");

    // Create an uncommitted file in the feature worktree so removal fails.
    std::fs::write(wt_dir.join("dirty.txt"), "dirty").expect("write failed");

    // Merge should succeed (merge itself is on main), but cleanup fails.
    // Cleanup failure is a warning, not a hard error.
    wt_core()
        .args(["merge", "feature/dirty-wt", "--repo", &repo_str])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Merged 'feature/dirty-wt' into main",
        ))
        .stderr(predicate::str::contains("warning:"))
        .stderr(predicate::str::contains("cleanup failed"));

    // Verify merge commit exists on main despite cleanup failure.
    let log = git_log_oneline(&repo.path(), "main");
    assert!(
        log.contains("Merge branch 'feature/dirty-wt'"),
        "merge commit should exist on main: {log}"
    );

    // Worktree should still exist (cleanup failed).
    assert!(
        wt_dir.exists(),
        "worktree should still exist after cleanup failure"
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

#[test]
fn merge_into_with_push_pushes_destination_branch() {
    let (repo, upstream) = setup_repo_with_upstream();
    let repo_str = repo.path().display().to_string();

    run_git(&["checkout", "-b", "release/1.0"], &repo.path());
    run_git(&["push", "-u", "origin", "release/1.0"], &repo.path());
    run_git(&["checkout", "main"], &repo.path());

    wt_core()
        .args(["add", "feature/release-push", "--repo", &repo_str])
        .assert()
        .success();

    let wt_dir = find_worktree_dir(&repo.path(), "feature-release-push");
    commit_file(&wt_dir, "rp.txt", "release push", "add release push");

    run_git(&["checkout", "release/1.0"], &repo.path());

    wt_core()
        .args([
            "merge",
            "feature/release-push",
            "--into",
            "release/1.0",
            "--push",
            "--repo",
            &repo_str,
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Merged 'feature/release-push' into release/1.0",
        ))
        .stdout(predicate::str::contains("Pushed release/1.0 to origin"));

    let upstream_log = git_log_oneline(upstream.path(), "release/1.0");
    assert!(
        upstream_log.contains("Merge branch 'feature/release-push'"),
        "merge commit should be pushed to release branch: {upstream_log}"
    );

    let upstream_main_log = git_log_oneline(upstream.path(), "main");
    assert!(
        !upstream_main_log.contains("Merge branch 'feature/release-push'"),
        "merge commit should not be pushed to main: {upstream_main_log}"
    );

    run_git(&["checkout", "main"], &repo.path());
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
    assert_eq!(json["event"], "reset");
    assert_eq!(json["branch"], "feature/json-merge");
    assert_eq!(json["mainline"], "main");
    assert_eq!(json["destination_path"], repo_str);
    assert!(json["repo_root"].as_str().is_some());
    assert_eq!(json["cleaned_up"], true);
    assert!(
        json["removed_path"].as_str().is_some(),
        "removed_path should be present when cleaned_up is true"
    );
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
    assert!(
        json["event"].is_null(),
        "event should be absent when cleanup is skipped"
    );
    assert_eq!(json["cleaned_up"], false);
    assert!(
        json["removed_path"].is_null(),
        "removed_path should be absent when cleaned_up is false"
    );
}

#[test]
fn merge_json_into_reports_destination_branch() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    run_git(&["checkout", "-b", "release/json"], &repo.path());
    run_git(&["checkout", "main"], &repo.path());

    wt_core()
        .args(["add", "feature/json-release", "--repo", &repo_str])
        .assert()
        .success();

    let wt_dir = find_worktree_dir(&repo.path(), "feature-json-release");
    commit_file(&wt_dir, "json-release.txt", "json release", "json release");

    run_git(&["checkout", "release/json"], &repo.path());

    let output = wt_core()
        .args([
            "merge",
            "feature/json-release",
            "--into",
            "release/json",
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
    assert_eq!(json["branch"], "feature/json-release");
    assert_eq!(json["mainline"], "release/json");
    assert_eq!(
        json["message"],
        "merged 'feature/json-release' into release/json"
    );

    run_git(&["checkout", "main"], &repo.path());
}

// ── Print-paths output tests ────────────────────────────────────────

#[test]
fn merge_print_paths_returns_six_lines() {
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
    assert_eq!(lines.len(), 6, "expected 6 lines: {stdout}");

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

    // Line 5: removed_path (non-empty when cleaned_up)
    assert!(
        lines[4].contains(".worktrees/"),
        "line 5 should be the removed worktree path: {}",
        lines[4]
    );

    // Line 6: pushed
    assert_eq!(lines[5], "false");
}

#[test]
fn merge_print_paths_v2_appends_destination_path() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    wt_core()
        .args(["add", "feature/paths-v2", "--repo", &repo_str])
        .assert()
        .success();

    let wt_dir = find_worktree_dir(&repo.path(), "feature-paths-v2");
    commit_file(&wt_dir, "v2.txt", "v2 work", "v2 commit");

    let output = wt_core()
        .args([
            "merge",
            "feature/paths-v2",
            "--repo",
            &repo_str,
            "--print-paths-v2",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8(output).expect("invalid utf8");
    let lines: Vec<&str> = stdout.trim().lines().collect();
    assert_eq!(lines.len(), 7, "expected 7 lines: {stdout}");
    assert_eq!(lines[1], "feature/paths-v2");
    assert_eq!(lines[2], "main");
    assert_eq!(lines[3], "true");
    assert_eq!(lines[5], "false");
    assert_eq!(lines[6], repo_str);
}

#[test]
fn merge_print_paths_into_reports_destination_branch() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    run_git(&["checkout", "-b", "release/paths"], &repo.path());
    run_git(&["checkout", "main"], &repo.path());

    wt_core()
        .args(["add", "feature/paths-release", "--repo", &repo_str])
        .assert()
        .success();

    let wt_dir = find_worktree_dir(&repo.path(), "feature-paths-release");
    commit_file(
        &wt_dir,
        "paths-release.txt",
        "paths release",
        "paths release",
    );

    run_git(&["checkout", "release/paths"], &repo.path());

    let output = wt_core()
        .args([
            "merge",
            "feature/paths-release",
            "--into",
            "release/paths",
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
    assert_eq!(lines.len(), 6, "expected 6 lines: {stdout}");
    assert_eq!(lines[1], "feature/paths-release");
    assert_eq!(lines[2], "release/paths");

    run_git(&["checkout", "main"], &repo.path());
}

#[test]
fn merge_print_paths_v2_conflicts_with_json() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    wt_core()
        .args([
            "merge",
            "any-branch",
            "--repo",
            &repo_str,
            "--print-paths-v2",
            "--json",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("cannot be used with"));
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

#[test]
fn merge_into_checked_out_non_mainline_branch() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    run_git(&["checkout", "-b", "release/1.0"], &repo.path());
    run_git(&["checkout", "main"], &repo.path());

    wt_core()
        .args(["add", "feature/release-fix", "--repo", &repo_str])
        .assert()
        .success();

    let wt_dir = find_worktree_dir(&repo.path(), "feature-release-fix");
    commit_file(&wt_dir, "fix.txt", "release fix", "add release fix");

    run_git(&["checkout", "release/1.0"], &repo.path());

    wt_core()
        .args([
            "merge",
            "feature/release-fix",
            "--into",
            "release/1.0",
            "--repo",
            &repo_str,
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Merged 'feature/release-fix' into release/1.0",
        ));

    let release_log = git_log_oneline(&repo.path(), "release/1.0");
    assert!(
        release_log.contains("Merge branch 'feature/release-fix'"),
        "merge commit should exist on release branch: {release_log}"
    );

    let main_log = git_log_oneline(&repo.path(), "main");
    assert!(
        !main_log.contains("Merge branch 'feature/release-fix'"),
        "merge commit should not exist on main: {main_log}"
    );

    run_git(&["checkout", "main"], &repo.path());
}

#[test]
fn merge_into_linked_worktree_succeeds_and_cleans_only_source() {
    let (repo, upstream) = setup_repo_with_upstream();
    let repo_str = repo.path().display().to_string();
    let destination = add_linked_destination(&repo, "release/linked");
    let destination_str = destination.display().to_string();

    run_git(&["push", "-u", "origin", "release/linked"], &repo.path());

    wt_core()
        .args(["add", "feature/linked", "--repo", &repo_str])
        .assert()
        .success();

    let source = find_worktree_dir(&repo.path(), "feature-linked");
    commit_file(&source, "linked.txt", "linked merge", "linked merge");

    wt_core()
        .args([
            "merge",
            "feature/linked",
            "--into",
            "release/linked",
            "--push",
            "--repo",
            &repo_str,
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Merged 'feature/linked' into release/linked",
        ))
        .stdout(predicate::str::contains(format!(
            "Destination worktree: {destination_str}"
        )))
        .stdout(predicate::str::contains("Pushed release/linked to origin"));

    assert!(destination.exists(), "destination worktree must remain");
    assert_branch_exists(&repo.path(), "release/linked");
    assert_branch_deleted(&repo.path(), "feature/linked");

    let destination_log = git_log_oneline(&destination, "HEAD");
    assert!(
        destination_log.contains("Merge branch 'feature/linked'"),
        "merge commit should exist in linked destination: {destination_log}"
    );

    let upstream_log = git_log_oneline(upstream.path(), "release/linked");
    assert!(
        upstream_log.contains("Merge branch 'feature/linked'"),
        "linked destination branch should be pushed: {upstream_log}"
    );
}

#[test]
fn merge_into_linked_worktree_dirty_destination_fails_and_aborts_there() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();
    let destination = add_linked_destination(&repo, "release/dirty");

    wt_core()
        .args(["add", "feature/linked-dirty", "--repo", &repo_str])
        .assert()
        .success();

    let source = find_worktree_dir(&repo.path(), "feature-linked-dirty");
    commit_file(
        &source,
        "README.md",
        "source changes README",
        "source changes README",
    );
    std::fs::write(destination.join("README.md"), "dirty destination").expect("write failed");

    wt_core()
        .args([
            "merge",
            "feature/linked-dirty",
            "--into",
            "release/dirty",
            "--repo",
            &repo_str,
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("merge conflicts"))
        .stderr(predicate::str::contains("merge aborted"));

    let destination_status = git_status(&destination);
    assert!(
        destination_status.contains("README.md"),
        "destination dirty state should be preserved: {destination_status}"
    );
    assert_branch_exists(&repo.path(), "feature/linked-dirty");
    assert!(
        !git_log_oneline(&destination, "HEAD").contains("Merge branch 'feature/linked-dirty'"),
        "dirty destination must not receive a merge commit"
    );
}

#[test]
fn merge_into_unchecked_out_destination_fails_before_merge() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    wt_core()
        .args(["add", "feature/missing-destination", "--repo", &repo_str])
        .assert()
        .success();
    let source = find_worktree_dir(&repo.path(), "feature-missing-destination");
    commit_file(
        &source,
        "missing.txt",
        "missing destination",
        "missing destination",
    );

    wt_core()
        .args([
            "merge",
            "feature/missing-destination",
            "--into",
            "release/not-checked-out",
            "--repo",
            &repo_str,
        ])
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains(
            "destination branch 'release/not-checked-out' is not checked out in a worktree",
        ));

    assert_branch_exists(&repo.path(), "feature/missing-destination");
    assert!(
        source.exists(),
        "source worktree must remain after preflight failure"
    );
}

#[test]
fn merge_into_wrong_checked_out_destination_errors_before_merge() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    run_git(&["checkout", "-b", "release/1.0"], &repo.path());
    run_git(&["checkout", "main"], &repo.path());

    wt_core()
        .args(["add", "feature/wrong-target", "--repo", &repo_str])
        .assert()
        .success();

    let wt_dir = find_worktree_dir(&repo.path(), "feature-wrong-target");
    commit_file(&wt_dir, "wrong.txt", "wrong target", "add wrong target");

    wt_core()
        .args([
            "merge",
            "feature/wrong-target",
            "--into",
            "release/1.0",
            "--repo",
            &repo_str,
        ])
        .assert()
        .failure()
        .code(4)
        .stderr(predicate::str::contains("main worktree is on 'main'"))
        .stderr(predicate::str::contains("expected 'release/1.0'"))
        .stderr(predicate::str::contains("checkout target branch first"));

    let release_log = git_log_oneline(&repo.path(), "release/1.0");
    assert!(
        !release_log.contains("Merge branch 'feature/wrong-target'"),
        "merge commit should not exist on release branch: {release_log}"
    );
    assert_branch_exists(&repo.path(), "feature/wrong-target");
}

#[test]
fn merge_into_refuses_source_equal_destination() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    wt_core()
        .args(["add", "feature/self", "--repo", &repo_str])
        .assert()
        .success();

    wt_core()
        .args([
            "merge",
            "feature/self",
            "--into",
            "feature/self",
            "--repo",
            &repo_str,
        ])
        .assert()
        .failure()
        .code(4)
        .stderr(predicate::str::contains(
            "refusing to merge a branch into itself",
        ));

    assert_branch_exists(&repo.path(), "feature/self");
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

#[test]
fn merge_refuses_preexisting_destination_merge_without_aborting_it() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();
    let destination = add_linked_destination(&repo, "release/preexisting-merge");

    wt_core()
        .args(["add", "feature/preexisting-merge", "--repo", &repo_str])
        .assert()
        .success();
    let source = find_worktree_dir(&repo.path(), "feature-preexisting-merge");
    commit_file(&source, "README.md", "source conflict", "source conflict");
    commit_file(
        &destination,
        "README.md",
        "destination conflict",
        "destination conflict",
    );

    let merge_output = git_allow_failure(
        &["merge", "--no-ff", "feature/preexisting-merge"],
        &destination,
    );
    assert!(
        !merge_output.status.success(),
        "manual merge should leave a conflict"
    );
    let merge_head = git_path(&destination, "MERGE_HEAD");
    let merge_head_before = std::fs::read(&merge_head).expect("MERGE_HEAD should exist");
    let status_before = git_status(&destination);

    wt_core()
        .args([
            "merge",
            "feature/preexisting-merge",
            "--into",
            "release/preexisting-merge",
            "--repo",
            &repo_str,
        ])
        .assert()
        .failure()
        .code(5)
        .stderr(predicate::str::contains("in-progress merge"))
        .stderr(predicate::str::contains("finish or abort it"))
        .stderr(predicate::str::contains("merge aborted").not());

    assert_eq!(
        std::fs::read(&merge_head).expect("MERGE_HEAD should remain"),
        merge_head_before,
        "pre-existing merge marker must not be changed"
    );
    assert_eq!(
        git_status(&destination),
        status_before,
        "pre-existing conflict state must be preserved"
    );
}

// ── Helpers ─────────────────────────────────────────────────────────

/// Run a git command and return its raw output, allowing expected failures.
fn git_allow_failure(args: &[&str], cwd: &std::path::Path) -> std::process::Output {
    let mut cmd = StdCommand::new("git");
    cmd.args(args).current_dir(cwd);
    for var in GIT_ENV_OVERRIDES {
        cmd.env_remove(var);
    }
    cmd.output().expect("failed to run git")
}

/// Resolve a git marker path for a specific worktree.
fn git_path(cwd: &std::path::Path, marker: &str) -> std::path::PathBuf {
    let output = git_allow_failure(&["rev-parse", "--git-path", marker], cwd);
    assert!(output.status.success(), "git path lookup failed");
    let path = std::path::PathBuf::from(
        String::from_utf8(output.stdout)
            .expect("invalid utf8")
            .trim(),
    );
    if path.is_absolute() {
        path
    } else {
        cwd.join(path)
    }
}

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

/// Check out an existing branch in a linked worktree for merge integration tests.
fn add_linked_destination(repo: &fixtures::TestRepo, branch: &str) -> std::path::PathBuf {
    run_git(&["checkout", "-b", branch], &repo.path());
    run_git(&["checkout", "main"], &repo.path());

    let slug = branch.replace('/', "-");
    let destination = repo.path().join(format!(".linked-{slug}"));
    let destination_str = destination.display().to_string();
    run_git(&["worktree", "add", &destination_str, branch], &repo.path());
    destination
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
