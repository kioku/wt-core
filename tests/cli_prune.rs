mod fixtures;

use std::process::Command as StdCommand;

use assert_cmd::Command;
use predicates::prelude::*;

use fixtures::{commit_file, find_worktree_dir, run_git};

fn wt_core() -> Command {
    Command::new(assert_cmd::cargo_bin!("wt-core"))
}

// ── Dry-run tests ───────────────────────────────────────────────────

#[test]
fn prune_dry_run_no_worktrees() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    wt_core()
        .args(["prune", "--repo", &repo_str])
        .assert()
        .success()
        .stdout(predicate::str::contains("No worktrees to prune."));
}

#[test]
fn prune_dry_run_does_not_create_lifecycle_lock_state() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();
    let state_dir = repo.path().join(".git/wt-core");
    assert!(!state_dir.exists());

    wt_core()
        .args(["prune", "--repo", &repo_str])
        .assert()
        .success();

    assert!(
        !state_dir.exists(),
        "read-only prune created lifecycle state at {}",
        state_dir.display()
    );
}

#[test]
fn prune_dry_run_shows_integrated_merged() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    // Create a worktree with a branch
    wt_core()
        .args(["add", "feature/merged", "--repo", &repo_str])
        .assert()
        .success();

    // Make a commit on the feature branch
    let wt_dir = find_worktree_dir(&repo.path(), "feature-merged");
    commit_file(&wt_dir, "feat.txt", "feature work", "add feature");

    // Merge the branch into main
    run_git(&["checkout", "main"], &repo.path());
    run_git(&["merge", "feature/merged"], &repo.path());

    // Dry-run should show as integrated (merged)
    wt_core()
        .args(["prune", "--repo", &repo_str])
        .assert()
        .success()
        .stdout(predicate::str::contains("integrated (merged)"))
        .stdout(predicate::str::contains("can be pruned"));
}

#[test]
fn prune_dry_run_shows_not_integrated() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    // Create a worktree with unmerged work
    wt_core()
        .args(["add", "feature/wip", "--repo", &repo_str])
        .assert()
        .success();

    let wt_dir = find_worktree_dir(&repo.path(), "feature-wip");
    commit_file(&wt_dir, "wip.txt", "wip", "wip commit");

    wt_core()
        .args(["prune", "--repo", &repo_str])
        .assert()
        .success()
        .stdout(predicate::str::contains("not integrated"))
        .stdout(predicate::str::contains("No integrated worktrees found."));
}

#[test]
fn prune_dry_run_shows_rebase_integrated() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    // Create a worktree with a branch
    wt_core()
        .args(["add", "feature/rebased", "--repo", &repo_str])
        .assert()
        .success();

    // Make a commit on the feature branch
    let wt_dir = find_worktree_dir(&repo.path(), "feature-rebased");
    commit_file(
        &wt_dir,
        "rebased.txt",
        "rebased work",
        "add rebased feature",
    );

    // Make main diverge AFTER the feature branch was created, so that
    // cherry-pick creates a genuinely new commit (not a fast-forward).
    commit_file(
        &repo.path(),
        "mainline.txt",
        "mainline work",
        "mainline commit",
    );

    // Cherry-pick the feature commit into main (simulates rebase merge)
    let commit_hash = git_log_hash(&repo.path(), "feature/rebased");
    run_git(&["cherry-pick", &commit_hash], &repo.path());

    // Dry-run should show as integrated (rebase)
    wt_core()
        .args(["prune", "--repo", &repo_str])
        .assert()
        .success()
        .stdout(predicate::str::contains("integrated (rebase)"));
}

/// Rebase-integrated branches use `-D` for deletion automatically, so
/// `--execute` without `--force` must fully remove worktree AND branch.
#[test]
fn prune_execute_rebase_deletes_branch_without_force() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    // Create a worktree with a branch
    wt_core()
        .args(["add", "feature/rebased-exec", "--repo", &repo_str])
        .assert()
        .success();

    // Commit on the feature branch
    let wt_dir = find_worktree_dir(&repo.path(), "feature-rebased-exec");
    commit_file(
        &wt_dir,
        "rebased.txt",
        "rebased work",
        "add rebased feature",
    );

    // Diverge main so cherry-pick produces a new (non-ff) commit
    commit_file(
        &repo.path(),
        "mainline.txt",
        "mainline work",
        "mainline commit",
    );

    // Cherry-pick the feature commit into main (simulates rebase merge)
    let commit_hash = git_log_hash(&repo.path(), "feature/rebased-exec");
    run_git(&["cherry-pick", &commit_hash], &repo.path());

    // Execute prune WITHOUT --force
    wt_core()
        .args(["prune", "--execute", "--repo", &repo_str])
        .assert()
        .success()
        .stdout(predicate::str::contains("Removed feature/rebased-exec"))
        .stdout(predicate::str::contains("Pruned 1 worktree."));

    // Branch must be deleted (auto-escalated to -D for rebase integration)
    assert_branch_deleted(&repo.path(), "feature/rebased-exec");
}

// ── Execute tests ───────────────────────────────────────────────────

#[test]
fn prune_execute_removes_integrated_worktrees() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    // Create two worktrees
    wt_core()
        .args(["add", "feature/done", "--repo", &repo_str])
        .assert()
        .success();
    wt_core()
        .args(["add", "feature/pending", "--repo", &repo_str])
        .assert()
        .success();

    // Commit on both
    let done_dir = find_worktree_dir(&repo.path(), "feature-done");
    commit_file(&done_dir, "done.txt", "done", "done commit");

    let pending_dir = find_worktree_dir(&repo.path(), "feature-pending");
    commit_file(&pending_dir, "pending.txt", "pending", "pending commit");

    // Merge only 'done' into main
    run_git(&["checkout", "main"], &repo.path());
    run_git(&["merge", "feature/done"], &repo.path());

    // Execute prune
    wt_core()
        .args(["prune", "--execute", "--repo", &repo_str])
        .assert()
        .success()
        .stdout(predicate::str::contains("Removed feature/done"))
        .stdout(predicate::str::contains("Skipped feature/pending"))
        .stdout(predicate::str::contains("Pruned 1 worktree."));

    // Verify only the pending worktree remains
    let worktrees_dir = repo.path().join(".worktrees");
    let entries: Vec<_> = std::fs::read_dir(&worktrees_dir)
        .expect("worktrees dir should exist")
        .flatten()
        .filter(|e| e.path().is_dir())
        .collect();
    assert_eq!(entries.len(), 1, "only pending worktree should remain");
}

#[test]
fn prune_execute_no_integrated_worktrees() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    wt_core()
        .args(["add", "feature/active", "--repo", &repo_str])
        .assert()
        .success();

    let wt_dir = find_worktree_dir(&repo.path(), "feature-active");
    commit_file(&wt_dir, "active.txt", "active", "active commit");

    wt_core()
        .args(["prune", "--execute", "--repo", &repo_str])
        .assert()
        .success()
        .stdout(predicate::str::contains("No worktrees pruned."));
}

// ── Mainline tests ──────────────────────────────────────────────────

/// Create a linked integration target and a candidate worktree merged into it.
/// The target is deliberately linked, so a raw revision comparison would
/// incorrectly classify it as a prune candidate.
fn setup_linked_integration_target(repo: &std::path::Path) -> (std::path::PathBuf, String) {
    run_git(&["branch", "integration/release"], repo);
    let target_dir = repo.join("integration-target");
    run_git(
        &[
            "worktree",
            "add",
            &target_dir.display().to_string(),
            "integration/release",
        ],
        repo,
    );

    let repo_str = repo.display().to_string();
    wt_core()
        .args(["add", "feature/target-candidate", "--repo", &repo_str])
        .assert()
        .success();
    let candidate_dir = find_worktree_dir(repo, "feature-target-candidate");
    commit_file(
        &candidate_dir,
        "target.txt",
        "target candidate",
        "target candidate",
    );
    run_git(
        &["merge", "--no-ff", "feature/target-candidate"],
        &target_dir,
    );

    let target_sha = git_ref_hash(&target_dir, "HEAD").expect("target should have a tip");
    (target_dir, target_sha)
}

fn assert_target_survives_prune(
    repo: &std::path::Path,
    target_dir: &std::path::Path,
    target_revision: &str,
    expected_mainline: &str,
) {
    let repo_str = repo.display().to_string();
    let output = wt_core()
        .args([
            "prune",
            "--integrated-into",
            target_revision,
            "--execute",
            "--json",
            "--repo",
            &repo_str,
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let json: serde_json::Value = serde_json::from_slice(&output).expect("invalid json");
    assert_eq!(json["mainline"], expected_mainline);
    assert!(target_dir.exists(), "selected target worktree was pruned");
    assert_branch_exists(repo, "integration/release");
    assert_branch_deleted(repo, "feature/target-candidate");
}

#[test]
fn prune_never_prunes_linked_target_branch() {
    let repo = fixtures::TestRepo::new();
    let (target_dir, _) = setup_linked_integration_target(&repo.path());

    assert_target_survives_prune(
        &repo.path(),
        &target_dir,
        "integration/release",
        "integration/release",
    );
}

#[test]
fn prune_canonicalizes_full_target_ref_and_protects_linked_target() {
    let repo = fixtures::TestRepo::new();
    let (target_dir, _) = setup_linked_integration_target(&repo.path());

    assert_target_survives_prune(
        &repo.path(),
        &target_dir,
        "refs/heads/integration/release",
        "integration/release",
    );
}

#[test]
fn prune_protects_linked_target_selected_by_remote_ref() {
    let repo = fixtures::TestRepo::new();
    let (target_dir, target_sha) = setup_linked_integration_target(&repo.path());
    run_git(
        &[
            "update-ref",
            "refs/remotes/origin/integration/release",
            &target_sha,
        ],
        &repo.path(),
    );

    assert_target_survives_prune(
        &repo.path(),
        &target_dir,
        "origin/integration/release",
        "origin/integration/release",
    );
}

#[test]
fn prune_conservatively_protects_target_selected_by_commit_sha() {
    let repo = fixtures::TestRepo::new();
    let (target_dir, target_sha) = setup_linked_integration_target(&repo.path());

    assert_target_survives_prune(&repo.path(), &target_dir, &target_sha, &target_sha);
}

#[test]
fn prune_integrated_into_removes_preserved_branch_without_worktree() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    wt_core()
        .args(["add", "feature/staged", "--repo", &repo_str])
        .assert()
        .success();
    let staged_dir = find_worktree_dir(&repo.path(), "feature-staged");
    commit_file(&staged_dir, "staged.txt", "staged", "staged feature");

    run_git(&["branch", "integration/release"], &repo.path());
    run_git(&["checkout", "integration/release"], &repo.path());
    run_git(&["merge", "feature/staged"], &repo.path());

    // The source worktree is no longer needed, but the branch must survive
    // until the integration target has landed.
    wt_core()
        .args([
            "remove",
            "feature/staged",
            "--keep-branch",
            "--repo",
            &repo_str,
        ])
        .assert()
        .success();
    assert_branch_exists(&repo.path(), "feature/staged");

    let output = wt_core()
        .args([
            "prune",
            "--integrated-into",
            "integration/release",
            "--json",
            "--repo",
            &repo_str,
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let json: serde_json::Value = serde_json::from_slice(&output).expect("invalid json");
    assert_eq!(json["mainline"], "integration/release");
    assert_eq!(json["prunable"], 1);
    let candidates = json["worktrees"].as_array().expect("worktrees array");
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0]["branch"], "feature/staged");
    assert_eq!(candidates[0]["path"], serde_json::Value::Null);
    assert_eq!(candidates[0]["worktree_present"], false);
    assert_eq!(candidates[0]["branch_will_be_deleted"], true);

    let output = wt_core()
        .args([
            "prune",
            "--integrated-into",
            "integration/release",
            "--execute",
            "--json",
            "--repo",
            &repo_str,
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let json: serde_json::Value = serde_json::from_slice(&output).expect("invalid json");
    let pruned = json["pruned"].as_array().expect("pruned array");
    assert_eq!(pruned.len(), 1);
    assert_eq!(pruned[0]["worktree_removed"], false);
    assert_eq!(pruned[0]["branch_deleted"], true);
    assert_eq!(pruned[0]["path"], serde_json::Value::Null);
    assert_branch_deleted(&repo.path(), "feature/staged");
}

#[test]
fn prune_dry_run_does_not_clear_stale_preservation_marker() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    wt_core()
        .args(["add", "feature/dry-marker", "--repo", &repo_str])
        .assert()
        .success();
    wt_core()
        .args([
            "remove",
            "feature/dry-marker",
            "--keep-branch",
            "--repo",
            &repo_str,
        ])
        .assert()
        .success();
    let marker_ref = "refs/wt-core/preserved/feature/dry-marker";
    let marker = git_ref_hash(&repo.path(), marker_ref).expect("preservation marker");

    fixtures::commit_file(&repo.path(), "dry-marker.txt", "advanced\n", "advance main");
    run_git(
        &["branch", "-f", "feature/dry-marker", "main"],
        &repo.path(),
    );

    wt_core()
        .args(["prune", "--json", "--repo", &repo_str])
        .assert()
        .success();

    assert_eq!(
        git_ref_hash(&repo.path(), marker_ref).as_deref(),
        Some(marker.as_str()),
        "dry-run must not clear stale preservation refs"
    );
}

#[test]
fn prune_rejects_moved_preserved_branch_and_clears_marker() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    wt_core()
        .args(["add", "feature/moved-marker", "--repo", &repo_str])
        .assert()
        .success();
    let feature_dir = find_worktree_dir(&repo.path(), "feature-moved-marker");
    commit_file(&feature_dir, "moved.txt", "moved", "moved feature");
    run_git(&["checkout", "main"], &repo.path());
    run_git(&["merge", "feature/moved-marker"], &repo.path());

    wt_core()
        .args([
            "remove",
            "feature/moved-marker",
            "--keep-branch",
            "--repo",
            &repo_str,
        ])
        .assert()
        .success();

    let marker = git_ref_hash(&repo.path(), "refs/wt-core/preserved/feature/moved-marker")
        .expect("preservation marker should exist");
    assert_eq!(marker, git_log_hash(&repo.path(), "feature/moved-marker"));
    commit_file(
        &repo.path(),
        "after-preserve-move.txt",
        "after preserve",
        "advance target",
    );

    // A moved ref is a different branch incarnation and must not be pruned,
    // even though its new tip is already integrated into main.
    run_git(
        &["branch", "-f", "feature/moved-marker", "main"],
        &repo.path(),
    );
    let reattached = repo.path().join("moved-marker-reattached");
    run_git(
        &[
            "worktree",
            "add",
            &reattached.display().to_string(),
            "feature/moved-marker",
        ],
        &repo.path(),
    );
    wt_core()
        .args(["prune", "--execute", "--json", "--repo", &repo_str])
        .assert()
        .success();

    assert_branch_exists(&repo.path(), "feature/moved-marker");
    assert!(
        reattached.exists(),
        "stale reattached worktree should survive"
    );
    assert!(
        git_ref_hash(&repo.path(), "refs/wt-core/preserved/feature/moved-marker").is_none(),
        "stale marker should be cleared"
    );
}

#[test]
fn prune_rejects_recreated_preserved_branch_and_clears_marker() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    wt_core()
        .args(["add", "feature/recreated-marker", "--repo", &repo_str])
        .assert()
        .success();
    let feature_dir = find_worktree_dir(&repo.path(), "feature-recreated-marker");
    commit_file(
        &feature_dir,
        "recreated.txt",
        "recreated",
        "recreated feature",
    );
    run_git(&["checkout", "main"], &repo.path());
    run_git(&["merge", "feature/recreated-marker"], &repo.path());

    wt_core()
        .args([
            "remove",
            "feature/recreated-marker",
            "--keep-branch",
            "--repo",
            &repo_str,
        ])
        .assert()
        .success();

    commit_file(
        &repo.path(),
        "after-preserve-recreate.txt",
        "after preserve",
        "advance target",
    );
    run_git(&["branch", "-D", "feature/recreated-marker"], &repo.path());
    run_git(
        &["branch", "feature/recreated-marker", "main"],
        &repo.path(),
    );

    wt_core()
        .args(["prune", "--execute", "--json", "--repo", &repo_str])
        .assert()
        .success();

    assert_branch_exists(&repo.path(), "feature/recreated-marker");
    assert!(
        git_ref_hash(
            &repo.path(),
            "refs/wt-core/preserved/feature/recreated-marker"
        )
        .is_none(),
        "recreated branch marker should be cleared"
    );
}

#[test]
fn successful_default_deletion_clears_an_existing_marker() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    wt_core()
        .args(["add", "feature/clear-marker", "--repo", &repo_str])
        .assert()
        .success();
    wt_core()
        .args([
            "remove",
            "feature/clear-marker",
            "--keep-branch",
            "--repo",
            &repo_str,
        ])
        .assert()
        .success();

    // Reattach the preserved branch, then take the normal deletion path.
    let reattached = repo.path().join("reattached-worktree");
    run_git(
        &[
            "worktree",
            "add",
            &reattached.display().to_string(),
            "feature/clear-marker",
        ],
        &repo.path(),
    );
    wt_core()
        .args(["remove", "feature/clear-marker", "--repo", &repo_str])
        .assert()
        .success();

    assert_branch_deleted(&repo.path(), "feature/clear-marker");
    assert!(
        git_ref_hash(&repo.path(), "refs/wt-core/preserved/feature/clear-marker").is_none(),
        "successful deletion should clear the marker"
    );
}

#[test]
fn prune_later_mainline_cleanup_deletes_preserved_branch() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    wt_core()
        .args(["add", "feature/eventual", "--repo", &repo_str])
        .assert()
        .success();
    let staged_dir = find_worktree_dir(&repo.path(), "feature-eventual");
    commit_file(&staged_dir, "eventual.txt", "eventual", "eventual feature");

    run_git(&["branch", "integration/release"], &repo.path());
    run_git(&["checkout", "integration/release"], &repo.path());
    run_git(&["merge", "feature/eventual"], &repo.path());
    wt_core()
        .args([
            "remove",
            "feature/eventual",
            "--keep-branch",
            "--repo",
            &repo_str,
        ])
        .assert()
        .success();

    // The staged target has the feature, but main does not yet. The branch
    // must remain preserved during this first prune.
    run_git(&["checkout", "main"], &repo.path());
    wt_core()
        .args(["prune", "--execute", "--repo", &repo_str])
        .assert()
        .success()
        .stdout(predicate::str::contains("No worktrees pruned."));
    assert_branch_exists(&repo.path(), "feature/eventual");

    // Once the integration target lands on main, the next mainline prune can
    // delete the preserved branch even though its worktree is already gone.
    run_git(&["merge", "integration/release"], &repo.path());
    let output = wt_core()
        .args(["prune", "--execute", "--json", "--repo", &repo_str])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let json: serde_json::Value = serde_json::from_slice(&output).expect("invalid json");
    let pruned = json["pruned"].as_array().expect("pruned array");
    assert_eq!(pruned.len(), 1);
    assert_eq!(pruned[0]["branch"], "feature/eventual");
    assert_eq!(pruned[0]["worktree_removed"], false);
    assert_eq!(pruned[0]["branch_deleted"], true);
    assert_branch_deleted(&repo.path(), "feature/eventual");
}

#[test]
fn prune_mainline_auto_detects_main() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    wt_core()
        .args(["prune", "--repo", &repo_str])
        .assert()
        .success()
        .stdout(predicate::str::contains("Mainline: main"));
}

#[test]
fn prune_mainline_override() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    // Create a custom mainline branch
    run_git(&["branch", "develop"], &repo.path());

    wt_core()
        .args(["prune", "--mainline", "develop", "--repo", &repo_str])
        .assert()
        .success()
        .stdout(predicate::str::contains("Mainline: develop"));
}

#[test]
fn prune_mainline_override_invalid_fails() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    wt_core()
        .args(["prune", "--mainline", "nonexistent", "--repo", &repo_str])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "mainline branch 'nonexistent' does not exist",
        ));
}

#[test]
fn prune_mainline_detects_master() {
    // Create repo with 'master' as default branch
    let dir = tempfile::TempDir::new().expect("failed to create temp dir");
    let path = dir.path().canonicalize().expect("failed to canonicalize");

    run_git(&["init", "-b", "master"], &path);
    run_git(&["config", "user.email", "test@test.com"], &path);
    run_git(&["config", "user.name", "Test"], &path);
    std::fs::write(path.join("README.md"), "# test\n").expect("write failed");
    run_git(&["add", "."], &path);
    run_git(&["commit", "-m", "initial"], &path);

    let path_str = path.display().to_string();

    wt_core()
        .args(["prune", "--repo", &path_str])
        .assert()
        .success()
        .stdout(predicate::str::contains("Mainline: master"));
}

// ── Main worktree protection ────────────────────────────────────────

#[test]
fn prune_never_prunes_main_worktree() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    // Dry-run should not list main worktree
    let output = wt_core()
        .args(["prune", "--json", "--repo", &repo_str])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let json: serde_json::Value = serde_json::from_slice(&output).expect("invalid json");
    let worktrees = json["worktrees"].as_array().expect("worktrees array");
    // No entries — main is filtered out
    assert!(worktrees.is_empty());
}

// ── JSON output tests ───────────────────────────────────────────────

#[test]
fn prune_json_dry_run_structure() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    wt_core()
        .args(["add", "feature/json-test", "--repo", &repo_str])
        .assert()
        .success();

    let wt_dir = find_worktree_dir(&repo.path(), "feature-json-test");
    commit_file(&wt_dir, "j.txt", "json", "json commit");

    // Merge into main
    run_git(&["checkout", "main"], &repo.path());
    run_git(&["merge", "feature/json-test"], &repo.path());

    let output = wt_core()
        .args(["prune", "--json", "--repo", &repo_str])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let json: serde_json::Value = serde_json::from_slice(&output).expect("invalid json");
    assert_eq!(json["ok"], true);
    assert_eq!(json["mainline"], "main");
    assert_eq!(json["prunable"], 1);

    let wts = json["worktrees"].as_array().expect("worktrees array");
    assert_eq!(wts.len(), 1);
    assert_eq!(wts[0]["branch"], "feature/json-test");
    assert_eq!(wts[0]["status"], "integrated");
    assert_eq!(wts[0]["method"], "merged");
    assert!(wts[0]["path"].as_str().is_some());
}

#[test]
fn prune_json_execute_structure() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    wt_core()
        .args(["add", "feature/exec-json", "--repo", &repo_str])
        .assert()
        .success();

    let wt_dir = find_worktree_dir(&repo.path(), "feature-exec-json");
    commit_file(&wt_dir, "e.txt", "exec", "exec commit");

    run_git(&["checkout", "main"], &repo.path());
    run_git(&["merge", "feature/exec-json"], &repo.path());

    let output = wt_core()
        .args(["prune", "--execute", "--json", "--repo", &repo_str])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let json: serde_json::Value = serde_json::from_slice(&output).expect("invalid json");
    assert_eq!(json["ok"], true);
    assert_eq!(json["mainline"], "main");

    let pruned = json["pruned"].as_array().expect("pruned array");
    assert_eq!(pruned.len(), 1);
    assert_eq!(pruned[0]["branch"], "feature/exec-json");

    let skipped = json["skipped"].as_array().expect("skipped array");
    assert!(skipped.is_empty());
}

// ── Squash merge limitation ─────────────────────────────────────────

#[test]
fn prune_squash_merge_shows_not_integrated() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    wt_core()
        .args(["add", "feature/squashed", "--repo", &repo_str])
        .assert()
        .success();

    // Multiple commits so squash produces a combined patch that won't
    // match any individual commit's patch-id.
    let wt_dir = find_worktree_dir(&repo.path(), "feature-squashed");
    commit_file(&wt_dir, "s1.txt", "squash work 1", "squash commit 1");
    commit_file(&wt_dir, "s2.txt", "squash work 2", "squash commit 2");

    // Squash merge into main (no divergence needed; --squash never ff's)
    run_git(&["merge", "--squash", "feature/squashed"], &repo.path());
    run_git(&["commit", "-m", "squashed feature"], &repo.path());

    // Squash merges are a known limitation — should show as not integrated
    wt_core()
        .args(["prune", "--repo", &repo_str])
        .assert()
        .success()
        .stdout(predicate::str::contains("not integrated"));
}

// ── Force flag ──────────────────────────────────────────────────────

#[test]
fn prune_force_removes_dirty_integrated_worktree() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    wt_core()
        .args(["add", "feature/dirty", "--repo", &repo_str])
        .assert()
        .success();

    let wt_dir = find_worktree_dir(&repo.path(), "feature-dirty");
    commit_file(&wt_dir, "d.txt", "dirty work", "dirty commit");

    // Merge into main
    run_git(&["checkout", "main"], &repo.path());
    run_git(&["merge", "feature/dirty"], &repo.path());

    // Make the worktree dirty (uncommitted change)
    std::fs::write(wt_dir.join("dirty-uncommitted.txt"), "dirty").expect("write failed");

    // Without --force: removal may fail due to dirty worktree
    // With --force: should succeed
    wt_core()
        .args(["prune", "--execute", "--force", "--repo", &repo_str])
        .assert()
        .success()
        .stdout(predicate::str::contains("Removed feature/dirty"));
}

#[test]
fn prune_force_without_execute_rejected() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    wt_core()
        .args(["prune", "--force", "--repo", &repo_str])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--execute"));
}

// ── Detached HEAD ───────────────────────────────────────────────────

#[test]
fn prune_detached_head_skipped() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    // Create a detached HEAD worktree directly via git
    let detached_dir = repo.path().join(".worktrees").join("detached-test");
    std::fs::create_dir_all(detached_dir.parent().expect("parent")).ok();
    run_git(
        &[
            "worktree",
            "add",
            "--detach",
            &detached_dir.display().to_string(),
            "HEAD",
        ],
        &repo.path(),
    );

    wt_core()
        .args(["prune", "--repo", &repo_str])
        .assert()
        .success()
        .stdout(predicate::str::contains("no branch (detached HEAD)"));

    // Execute should skip it
    wt_core()
        .args(["prune", "--execute", "--repo", &repo_str])
        .assert()
        .success()
        .stdout(predicate::str::contains("Skipped (detached) (no branch)"));
}

// ── Empty repo (no extra worktrees) ─────────────────────────────────

#[test]
fn prune_empty_repo_no_worktrees() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    wt_core()
        .args(["prune", "--execute", "--repo", &repo_str])
        .assert()
        .success()
        .stdout(predicate::str::contains("No worktrees pruned."));
}

// ── Helpers ─────────────────────────────────────────────────────────

/// Environment variables cleared for raw git commands in tests.
const GIT_ENV_OVERRIDES: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_COMMON_DIR",
    "GIT_PREFIX",
];

/// Get the latest commit hash for a branch.
fn git_log_hash(repo: &std::path::Path, branch: &str) -> String {
    let mut cmd = StdCommand::new("git");
    cmd.args(["log", branch, "--format=%H", "-1"])
        .current_dir(repo);
    for var in GIT_ENV_OVERRIDES {
        cmd.env_remove(var);
    }
    let output = cmd.output().expect("git log failed");
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn git_ref_hash(repo: &std::path::Path, reference: &str) -> Option<String> {
    let mut cmd = StdCommand::new("git");
    cmd.args(["rev-parse", "--verify", reference])
        .current_dir(repo);
    for var in GIT_ENV_OVERRIDES {
        cmd.env_remove(var);
    }
    let output = cmd.output().expect("git rev-parse failed");
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Assert that a branch exists in the repo.
fn assert_branch_exists(repo: &std::path::Path, branch: &str) {
    let mut cmd = StdCommand::new("git");
    cmd.args(["show-ref", "--verify", &format!("refs/heads/{branch}")])
        .current_dir(repo);
    for var in GIT_ENV_OVERRIDES {
        cmd.env_remove(var);
    }
    assert!(
        cmd.output().expect("git show-ref failed").status.success(),
        "branch should exist: {branch}"
    );
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
