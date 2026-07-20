mod fixtures;

use std::process::Command as StdCommand;
#[cfg(unix)]
use std::process::Stdio;

use assert_cmd::Command;
use predicates::prelude::*;

use fixtures::{commit_file, find_worktree_dir, run_git};

fn wt_core() -> Command {
    Command::new(assert_cmd::cargo_bin!("wt-core"))
}

#[derive(Debug, PartialEq, Eq)]
struct FilesystemMetadata {
    file_type: (bool, bool, bool),
    len: u64,
    readonly: bool,
    modified: Option<std::time::SystemTime>,
    #[cfg(unix)]
    mode: u32,
}

fn filesystem_metadata(path: &std::path::Path) -> Option<FilesystemMetadata> {
    let metadata = std::fs::symlink_metadata(path).ok()?;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    Some(FilesystemMetadata {
        file_type: (
            metadata.file_type().is_file(),
            metadata.file_type().is_dir(),
            metadata.file_type().is_symlink(),
        ),
        len: metadata.len(),
        readonly: metadata.permissions().readonly(),
        modified: metadata.modified().ok(),
        #[cfg(unix)]
        mode: metadata.permissions().mode() & 0o7777,
    })
}

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
fn merge_status_does_not_initialize_missing_state_namespace() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();
    let git_dir = repo.path().join(".git");
    let state_dir = git_dir.join("wt-core");
    let state_path = state_dir.join("merge-operation.json");
    let lock_path = state_dir.join("merge-operation.lock");
    let paths = [git_dir, state_dir, state_path, lock_path];
    let before: Vec<_> = paths.iter().map(|path| filesystem_metadata(path)).collect();
    assert!(
        before[1].is_none(),
        "fixture must start without managed state"
    );

    let output = wt_core()
        .args(["merge", "--status", "--json", "--repo", &repo_str])
        .output()
        .expect("status should run without managed state");
    assert!(
        output.status.success(),
        "status failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let status: serde_json::Value = serde_json::from_slice(&output.stdout).expect("status JSON");
    assert_eq!(status["state"], "none");
    assert_eq!(status["ok"], true);

    let after: Vec<_> = paths.iter().map(|path| filesystem_metadata(path)).collect();
    assert_eq!(before, after, "status changed managed filesystem metadata");
}

#[test]
fn merge_conflict_preserves_destination_and_managed_state() {
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

    // Merge should fail with conflict details but preserve Git's merge state.
    wt_core()
        .args(["merge", "feature/conflict", "--repo", &repo_str])
        .assert()
        .failure()
        .stderr(predicate::str::contains("merge conflicts"))
        .stderr(predicate::str::contains("wt merge --continue"))
        .stderr(predicate::str::contains("merge aborted").not());

    // Worktree and branch remain available for the eventual continuation.
    let wt_dir = find_worktree_dir(&repo.path(), "feature-conflict");
    assert!(
        wt_dir.exists(),
        "worktree should still exist after conflict"
    );
    assert_branch_exists(&repo.path(), "feature/conflict");

    let status = git_status(&repo.path());
    assert!(
        status.contains("AA shared.txt"),
        "main worktree should preserve the conflict: {status}"
    );
    assert!(
        repo.path()
            .join(".git/wt-core/merge-operation.json")
            .is_file(),
        "managed merge state should be durable"
    );
}

#[test]
fn remove_refuses_managed_merge_source_while_conflicted() {
    let repo = fixtures::TestRepo::new();
    let source = create_conflicted_merge(&repo, "feature/remove-while-conflicted");
    let repo_str = repo.path().display().to_string();

    wt_core()
        .args([
            "remove",
            "feature/remove-while-conflicted",
            "--force",
            "--json",
            "--repo",
            &repo_str,
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "while managed merge 'feature/remove-while-conflicted' -> 'main' is active",
        ));

    assert!(source.exists(), "managed merge source must remain present");
    assert_branch_exists(&repo.path(), "feature/remove-while-conflicted");
}

#[test]
fn prune_refuses_cleanup_while_managed_merge_is_active() {
    let repo = fixtures::TestRepo::new();
    let source = create_conflicted_merge(&repo, "feature/prune-while-conflicted");
    let repo_str = repo.path().display().to_string();

    wt_core()
        .args(["prune", "--execute", "--json", "--repo", &repo_str])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "refusing prune while managed merge 'feature/prune-while-conflicted' -> 'main' is active",
        ));

    assert!(source.exists(), "managed merge source must remain present");
    assert_branch_exists(&repo.path(), "feature/prune-while-conflicted");
}

#[test]
fn merge_status_and_continue_report_and_finish_conflict() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();
    let source = create_conflicted_merge(&repo, "feature/continue");

    let status = wt_core()
        .args(["merge", "--status", "--json", "--repo", &repo_str])
        .output()
        .expect("status should run");
    assert!(status.status.success());
    let status_json: serde_json::Value =
        serde_json::from_slice(&status.stdout).expect("status JSON");
    assert_eq!(status_json["ok"], true);
    assert_eq!(status_json["state"], "conflicted");
    assert_eq!(
        status_json["unresolved_paths"],
        serde_json::json!(["shared.txt"])
    );
    assert!(status_json["pending_actions"]
        .as_array()
        .is_some_and(|actions| actions.iter().any(|action| action
            .as_str()
            .is_some_and(|action| action.contains("resolve")))));

    wt_core()
        .args(["merge", "--continue", "--repo", &repo_str])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unresolved paths remain"));

    std::fs::write(repo.path().join("shared.txt"), "resolved\n").expect("resolve conflict");
    run_git(&["add", "shared.txt"], &repo.path());
    wt_core()
        .args(["merge", "--continue", "--json", "--repo", &repo_str])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"cleaned_up\":true"))
        .stdout(predicate::str::contains("Merge branch 'feature/continue'").not());

    assert!(
        !source.exists(),
        "continue should apply the original cleanup policy"
    );
    assert_branch_deleted(&repo.path(), "feature/continue");
    assert!(!repo
        .path()
        .join(".git/wt-core/merge-operation.json")
        .exists());
}

#[test]
fn merge_linked_destination_conflict_continue_preserves_common_state_and_cleanup() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();
    let destination = add_linked_destination(&repo, "release/conflict");

    wt_core()
        .args(["add", "feature/linked-conflict", "--repo", &repo_str])
        .assert()
        .success();
    let source = find_worktree_dir(&repo.path(), "feature-linked-conflict");
    commit_file(&source, "shared.txt", "source", "source conflict");
    commit_file(
        &destination,
        "shared.txt",
        "destination",
        "destination conflict",
    );

    wt_core()
        .args([
            "merge",
            "feature/linked-conflict",
            "--into",
            "release/conflict",
            "--repo",
            &repo_str,
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("wt merge --continue"));
    let status_path = repo.path().join(".git/wt-core/merge-operation.json");
    assert!(
        status_path.is_file(),
        "linked merge state should use common Git dir"
    );

    std::fs::write(destination.join("shared.txt"), "resolved\n").expect("resolve linked conflict");
    run_git(&["add", "shared.txt"], &destination);
    wt_core()
        .args(["merge", "--continue", "--json", "--repo", &repo_str])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "\"mainline\":\"release/conflict\"",
        ))
        .stdout(predicate::str::contains("\"cleaned_up\":true"));

    assert!(
        !source.exists(),
        "linked destination continuation cleans source"
    );
    assert_branch_deleted(&repo.path(), "feature/linked-conflict");
    assert!(!status_path.exists());
}

#[test]
fn merge_abort_restores_destination_and_clears_matching_state() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();
    let source = create_conflicted_merge(&repo, "feature/abort");
    let original_head = git_rev_parse(&repo.path(), "HEAD");

    wt_core()
        .args(["merge", "--abort", "--json", "--repo", &repo_str])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"state\":\"aborted\""));

    assert_eq!(git_rev_parse(&repo.path(), "HEAD"), original_head);
    assert!(
        git_status(&repo.path()).is_empty(),
        "abort should restore a clean destination"
    );
    assert!(
        source.exists(),
        "abort must not clean up the source worktree"
    );
    assert_branch_exists(&repo.path(), "feature/abort");
    assert!(!repo
        .path()
        .join(".git/wt-core/merge-operation.json")
        .exists());
}

#[test]
fn merge_continue_keeps_source_when_original_merge_skipped_cleanup() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();
    let source = create_conflicted_merge_with_options(&repo, "feature/keep-conflict", true);
    std::fs::write(repo.path().join("shared.txt"), "resolved\n").expect("resolve conflict");
    run_git(&["add", "shared.txt"], &repo.path());

    wt_core()
        .args(["merge", "--continue", "--json", "--repo", &repo_str])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"cleaned_up\":false"));

    assert!(source.exists(), "--no-cleanup must survive continuation");
    assert_branch_exists(&repo.path(), "feature/keep-conflict");
    assert!(!repo
        .path()
        .join(".git/wt-core/merge-operation.json")
        .exists());
}

#[cfg(unix)]
#[test]
fn merge_continue_reconciles_commit_after_state_write_failure() {
    use std::os::unix::fs::PermissionsExt;

    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();
    create_conflicted_merge(&repo, "feature/state-write-commit");
    std::fs::write(repo.path().join("shared.txt"), "resolved\n").expect("resolve conflict");
    run_git(&["add", "shared.txt"], &repo.path());

    let state_dir = repo.path().join(".git/wt-core");
    std::fs::set_permissions(&state_dir, std::fs::Permissions::from_mode(0o500))
        .expect("make journal unwritable");
    wt_core()
        .args(["merge", "--continue", "--json", "--repo", &repo_str])
        .assert()
        .failure()
        .stdout(predicate::str::contains("\"state\":\"committed\""));
    std::fs::set_permissions(&state_dir, std::fs::Permissions::from_mode(0o700))
        .expect("restore journal permissions");

    let status = wt_core()
        .args(["merge", "--status", "--json", "--repo", &repo_str])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let status_json: serde_json::Value = serde_json::from_slice(&status).expect("status JSON");
    assert_eq!(status_json["state"], "committed");
    assert_eq!(status_json["worktree_removed"], false);

    wt_core()
        .args(["merge", "--continue", "--repo", &repo_str])
        .assert()
        .success();
    assert_branch_deleted(&repo.path(), "feature/state-write-commit");
    assert!(!repo
        .path()
        .join(".git/wt-core/merge-operation.json")
        .exists());
}

#[cfg(unix)]
#[test]
fn merge_continue_reconciles_push_after_progress_write_failure() {
    use std::os::unix::fs::PermissionsExt;

    let (repo, upstream) = setup_repo_with_upstream();
    let repo_str = repo.path().display().to_string();
    create_conflicted_merge_at(&repo.path(), "feature/push-state-write", false, true);
    std::fs::write(repo.path().join("shared.txt"), "resolved\n").expect("resolve conflict");
    run_git(&["add", "shared.txt"], &repo.path());

    let state_dir = repo.path().join(".git/wt-core");
    install_hook(
        &repo,
        "pre-push",
        &format!(
            "#!/bin/sh\nchmod 0500 {}\n",
            shell_quote(&state_dir.display().to_string())
        ),
    );
    let output = wt_core()
        .args(["merge", "--continue", "--json", "--repo", &repo_str])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let json: serde_json::Value = serde_json::from_slice(&output).expect("merge JSON");
    assert_eq!(json["pushed"], true);
    assert_eq!(json["operation"]["push_done"], true);
    assert!(git_log_oneline(upstream.path(), "main")
        .contains("Merge branch 'feature/push-state-write'"));

    std::fs::set_permissions(&state_dir, std::fs::Permissions::from_mode(0o700))
        .expect("restore journal permissions");
    std::fs::remove_file(repo.path().join(".git/hooks/pre-push")).expect("remove hook");
    wt_core()
        .args(["merge", "--continue", "--repo", &repo_str])
        .assert()
        .success();
    assert!(!repo
        .path()
        .join(".git/wt-core/merge-operation.json")
        .exists());
}

#[cfg(unix)]
#[test]
fn merge_source_head_race_preserves_source_and_followup_intent() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();
    wt_core()
        .args(["add", "feature/source-head-race", "--repo", &repo_str])
        .assert()
        .success();
    let source = find_worktree_dir(&repo.path(), "feature-source-head-race");
    commit_file(&source, "source.txt", "source", "source");
    let source_str = source.display().to_string();
    install_hook(
        &repo,
        "post-merge",
        &format!(
            "#!/bin/sh\nunset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE GIT_OBJECT_DIRECTORY GIT_ALTERNATE_OBJECT_DIRECTORIES GIT_COMMON_DIR GIT_PREFIX\nprintf race > {}/race.txt\ngit -C {} add race.txt\ngit -C {} commit -m race\n",
            shell_quote(&source_str),
            shell_quote(&source_str),
            shell_quote(&source_str)
        ),
    );

    let output = wt_core()
        .args([
            "merge",
            "feature/source-head-race",
            "--json",
            "--repo",
            &repo_str,
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let json: serde_json::Value = serde_json::from_slice(&output).expect("merge JSON");
    assert_eq!(json["cleaned_up"], false);
    assert!(json["operation"]["state"] == "stale");
    assert!(
        source.exists(),
        "source HEAD race must preserve the source worktree"
    );
    assert_branch_exists(&repo.path(), "feature/source-head-race");
}

#[cfg(unix)]
#[test]
fn merge_destination_head_race_refuses_cleanup_and_push() {
    let (repo, _upstream) = setup_repo_with_upstream();
    let repo_str = repo.path().display().to_string();
    wt_core()
        .args(["add", "feature/destination-head-race", "--repo", &repo_str])
        .assert()
        .success();
    let source = find_worktree_dir(&repo.path(), "feature-destination-head-race");
    commit_file(&source, "destination-race.txt", "race", "race");
    install_hook(
        &repo,
        "post-merge",
        "#!/bin/sh\nunset GIT_EDITOR\ngit commit --allow-empty -m destination-race\n",
    );

    let output = wt_core()
        .args([
            "merge",
            "feature/destination-head-race",
            "--push",
            "--json",
            "--repo",
            &repo_str,
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let json: serde_json::Value = serde_json::from_slice(&output).expect("merge JSON");
    assert_eq!(json["cleaned_up"], false);
    assert_eq!(json["pushed"], false);
    assert!(
        source.exists(),
        "destination HEAD race must preserve cleanup intent"
    );
    assert_branch_exists(&repo.path(), "feature/destination-head-race");
}

#[cfg(unix)]
#[test]
fn merge_state_and_directory_are_private() {
    use std::os::unix::fs::PermissionsExt;

    let repo = fixtures::TestRepo::new();
    create_conflicted_merge(&repo, "feature/private-state");
    let state_dir = repo.path().join(".git/wt-core");
    let state = state_dir.join("merge-operation.json");
    assert_eq!(
        std::fs::metadata(&state_dir)
            .expect("state dir")
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(
        std::fs::metadata(&state)
            .expect("state")
            .permissions()
            .mode()
            & 0o777,
        0o600
    );

    let mut insecure = std::fs::metadata(&state)
        .expect("state metadata")
        .permissions();
    insecure.set_mode(0o644);
    std::fs::set_permissions(&state, insecure).expect("make state insecure");
    wt_core()
        .args([
            "merge",
            "--status",
            "--json",
            "--repo",
            &repo.path().display().to_string(),
        ])
        .assert()
        .failure()
        .stdout(predicate::str::contains("insecure permissions"));
}

#[cfg(unix)]
#[test]
fn merge_lifecycle_lock_blocks_hook_race_but_allows_read_only_status() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();
    wt_core()
        .args(["add", "feature/paused", "--repo", &repo_str])
        .assert()
        .success();
    let source = find_worktree_dir(&repo.path(), "feature-paused");
    commit_file(&source, "paused.txt", "paused", "paused source");
    wt_core()
        .args(["add", "feature/new", "--repo", &repo_str])
        .assert()
        .success();
    let new_source = find_worktree_dir(&repo.path(), "feature-new");
    commit_file(&new_source, "new.txt", "new", "new source");

    let entered = repo.path().join("post-merge-entered");
    let release = repo.path().join("post-merge-release");
    install_hook(
        &repo,
        "post-merge",
        &format!(
            "#!/bin/sh\nset -eu\nprintf entered > {}\nwhile [ ! -f {} ]; do sleep 0.05; done\n",
            shell_quote(&entered.display().to_string()),
            shell_quote(&release.display().to_string()),
        ),
    );

    let mut original = StdCommand::new(assert_cmd::cargo_bin!("wt-core"));
    original
        .args(["merge", "feature/paused", "--repo", &repo_str])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let original = original.spawn().expect("paused merge should start");
    wait_for_file(&entered);

    let status = wt_core()
        .args(["merge", "--status", "--json", "--repo", &repo_str])
        .output()
        .expect("status should run while the hook owns the lifecycle");
    assert!(
        status.status.success(),
        "status should remain read-only: {}",
        String::from_utf8_lossy(&status.stderr)
    );
    let status_json: serde_json::Value =
        serde_json::from_slice(&status.stdout).expect("busy status JSON");
    assert_eq!(
        status_json["state"], "busy",
        "live owner was misreported as an interrupted operation"
    );

    let continue_output = wt_core()
        .args(["merge", "--continue", "--repo", &repo_str])
        .output()
        .expect("continue should run");
    assert!(!continue_output.status.success());
    assert!(String::from_utf8_lossy(&continue_output.stderr).contains("busy"));
    assert!(String::from_utf8_lossy(&continue_output.stderr).contains("--status"));

    let abort_output = wt_core()
        .args(["merge", "--abort", "--repo", &repo_str])
        .output()
        .expect("abort should run");
    assert!(!abort_output.status.success());
    assert!(String::from_utf8_lossy(&abort_output.stderr).contains("busy"));

    let new_merge_output = wt_core()
        .args(["merge", "feature/new", "--repo", &repo_str])
        .output()
        .expect("new merge should run");
    assert!(!new_merge_output.status.success());
    assert!(String::from_utf8_lossy(&new_merge_output.stderr).contains("busy"));

    std::fs::write(&release, "release\n").expect("release paused hook");
    let output = original
        .wait_with_output()
        .expect("original merge should finish after hook release");
    assert!(
        output.status.success(),
        "original merge failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!repo
        .path()
        .join(".git/wt-core/merge-operation.json")
        .exists());
    assert!(!git_path(&repo.path(), "MERGE_HEAD").exists());
    assert!(!source.exists());
    assert!(new_source.exists());
}

#[cfg(unix)]
#[test]
fn merge_lifecycle_lock_recovers_after_owner_death_without_stale_finalization() {
    use std::thread::sleep;
    use std::time::Duration;

    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();
    wt_core()
        .args(["add", "feature/death", "--repo", &repo_str])
        .assert()
        .success();
    let source = find_worktree_dir(&repo.path(), "feature-death");
    commit_file(&source, "death.txt", "death", "death source");

    let entered = repo.path().join("post-merge-death-entered");
    let release = repo.path().join("post-merge-death-release");
    install_hook(
        &repo,
        "post-merge",
        &format!(
            "#!/bin/sh\nset -eu\nprintf entered > {}\nwhile [ ! -f {} ]; do sleep 0.05; done\n",
            shell_quote(&entered.display().to_string()),
            shell_quote(&release.display().to_string()),
        ),
    );

    let mut original = StdCommand::new(assert_cmd::cargo_bin!("wt-core"));
    original
        .args(["merge", "feature/death", "--repo", &repo_str])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut original = original.spawn().expect("merge should start");
    wait_for_file(&entered);
    original.kill().expect("owner should be terminable");

    // Git and its hook retain the inherited lifecycle lock after the owner
    // dies. Releasing the hook first must not let continuation race Git's
    // finalization; it may only recover after the child has exited.
    let busy = wt_core()
        .args(["merge", "--continue", "--repo", &repo_str])
        .output()
        .expect("continuation should run");
    assert!(!busy.status.success());
    assert!(String::from_utf8_lossy(&busy.stderr).contains("busy"));

    std::fs::write(&release, "release\n").expect("release orphaned hook");
    let _ = original.wait_with_output();

    let mut recovered = None;
    for _ in 0..100 {
        let output = wt_core()
            .args(["merge", "--continue", "--repo", &repo_str])
            .output()
            .expect("recovery continuation should run");
        if output.status.success() {
            recovered = Some(output);
            break;
        }
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("busy"),
            "unexpected recovery failure: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        sleep(Duration::from_millis(20));
    }
    assert!(recovered.is_some(), "lifecycle lock did not recover");
    assert!(!repo
        .path()
        .join(".git/wt-core/merge-operation.json")
        .exists());
    assert!(!source.exists());
}

#[cfg(unix)]
fn wait_for_file(path: &std::path::Path) {
    use std::thread::sleep;
    use std::time::Duration;

    for _ in 0..200 {
        if path.is_file() {
            return;
        }
        sleep(Duration::from_millis(10));
    }
    panic!("timed out waiting for {}", path.display());
}

#[cfg(unix)]
#[test]
fn merge_config_injection_cannot_bypass_merge_hooks() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();
    wt_core()
        .args(["add", "feature/config-hook", "--repo", &repo_str])
        .assert()
        .success();
    let source = find_worktree_dir(&repo.path(), "feature-config-hook");
    commit_file(&source, "config-hook.txt", "source", "source");
    install_hook(
        &repo,
        "pre-merge-commit",
        "#!/bin/sh\necho config hook ran >&2\nexit 42\n",
    );

    // These are the exact dynamic and packed Git config injection vectors that
    // a hook-inherited environment can use to set core.hooksPath=/dev/null.
    // wt-core must remove them from every Git child before the merge starts.
    let output = wt_core()
        .args(["merge", "feature/config-hook", "--repo", &repo_str])
        .env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", "core.hooksPath")
        .env("GIT_CONFIG_VALUE_0", "/dev/null")
        .env("GIT_CONFIG_PARAMETERS", "'core.hooksPath=/dev/null'")
        // Windows treats environment names case-insensitively; these lower
        // case spellings must be removed there as well.
        .env("git_config_count", "1")
        .env("git_config_key_0", "core.hooksPath")
        .env("git_config_value_0", "/dev/null")
        .env("git_config_parameters", "'core.hooksPath=/dev/null'")
        .env("GIT_NAMESPACE", "untrusted-namespace")
        .output()
        .expect("merge should run");
    assert!(
        !output.status.success(),
        "hook bypass unexpectedly succeeded"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("config hook ran"),
        "hook was bypassed: {stderr}"
    );
    assert_branch_exists(&repo.path(), "feature/config-hook");
}

#[cfg(unix)]
#[test]
fn merge_continue_destination_ref_lock_blocks_head_race() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();
    let source = create_conflicted_merge(&repo, "feature/continue-head-boundary");
    let destination_head = git_rev_parse(&repo.path(), "main");
    std::fs::write(repo.path().join("shared.txt"), "resolved\n").expect("resolve conflict");
    run_git(&["add", "shared.txt"], &repo.path());
    let marker = repo.path().join("continue-ref-race");
    install_hook(
        &repo,
        "pre-commit",
        &format!(
            "#!/bin/sh\nset -eu\nold={}\nrace=$(git commit-tree \"$(git rev-parse HEAD^{{tree}})\" -p \"$old\" -m race)\nif git update-ref refs/heads/main \"$race\" \"$old\"; then printf raced > {}; else printf blocked > {}; fi\n",
            shell_quote(&destination_head),
            shell_quote(&marker.display().to_string()),
            shell_quote(&marker.display().to_string()),
        ),
    );

    wt_core()
        .args(["merge", "--continue", "--repo", &repo_str])
        .assert()
        .success();
    assert_eq!(
        std::fs::read_to_string(&marker).expect("race marker"),
        "blocked"
    );
    let parents = git_rev_parse(&repo.path(), "main^@");
    assert!(parents.starts_with(&destination_head));
    assert!(
        !source.exists(),
        "successful continuation should clean source"
    );
}

#[cfg(unix)]
#[test]
fn merge_continue_restores_symbolic_head_when_final_cas_fails() {
    use std::os::unix::fs::PermissionsExt;

    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();
    let source = create_conflicted_merge(&repo, "feature/continue-final-cas");
    std::fs::write(repo.path().join("shared.txt"), "resolved\n").expect("resolve conflict");
    run_git(&["add", "shared.txt"], &repo.path());

    let shim = tempfile::TempDir::new().expect("git shim directory");
    let marker = shim.path().join("raced");
    let script = shim.path().join("git");
    std::fs::write(
        &script,
        r#"#!/bin/sh
set -eu
if [ "$#" -ge 4 ] && [ "$1" = "update-ref" ] && [ "$2" = "refs/heads/main" ] && [ ! -e "$WT_RACE_MARKER" ]; then
    : > "$WT_RACE_MARKER"
    tree=$("$WT_REAL_GIT" rev-parse HEAD^{tree})
    race=$("$WT_REAL_GIT" commit-tree "$tree" -p "$4" -m race)
    "$WT_REAL_GIT" update-ref "$2" "$race" "$4"
fi
exec "$WT_REAL_GIT" "$@"
"#,
    )
    .expect("write git shim");
    let mut permissions = std::fs::metadata(&script)
        .expect("git shim metadata")
        .permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(&script, permissions).expect("chmod git shim");
    let real_git = {
        let path = std::env::var_os("PATH").expect("PATH");
        std::env::split_paths(&path)
            .map(|component| component.join("git"))
            .find(|candidate| candidate.is_file())
            .expect("git executable")
    };
    let path = format!(
        "{}:{}",
        shim.path().display(),
        std::env::var("PATH").expect("PATH")
    );

    let output = wt_core()
        .args(["merge", "--continue", "--repo", &repo_str])
        .env("PATH", path)
        .env("WT_REAL_GIT", real_git)
        .env("WT_RACE_MARKER", &marker)
        .output()
        .expect("continuation should run");
    assert!(!output.status.success(), "final CAS race must be refused");
    assert!(marker.exists(), "final CAS shim did not run");
    let attached = git_allow_failure(
        &["symbolic-ref", "--quiet", "--short", "HEAD"],
        &repo.path(),
    );
    assert_eq!(
        String::from_utf8(attached.stdout)
            .expect("symbolic HEAD should be utf8")
            .trim(),
        "main"
    );
    assert!(!git_path(&repo.path(), "refs/heads/main.lock").exists());
    assert!(
        source.exists(),
        "failed final CAS must preserve cleanup intent"
    );
}

#[cfg(unix)]
#[test]
fn merge_continue_recovers_ref_lock_and_symbolic_head_after_kill() {
    use std::thread::sleep;
    use std::time::Duration;

    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();
    let source = create_conflicted_merge(&repo, "feature/continue-kill");
    std::fs::write(repo.path().join("shared.txt"), "resolved\n").expect("resolve conflict");
    run_git(&["add", "shared.txt"], &repo.path());

    let entered = repo.path().join("continue-kill-entered");
    let release = repo.path().join("continue-kill-release");
    install_hook(
        &repo,
        "pre-commit",
        &format!(
            "#!/bin/sh\nset -eu\nprintf entered > {}\nwhile [ ! -f {} ]; do sleep 0.05; done\n",
            shell_quote(&entered.display().to_string()),
            shell_quote(&release.display().to_string()),
        ),
    );

    let mut continuation = StdCommand::new(assert_cmd::cargo_bin!("wt-core"));
    continuation
        .args(["merge", "--continue", "--repo", &repo_str])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut continuation = continuation.spawn().expect("continuation should start");
    wait_for_file(&entered);
    let ref_lock = git_path(&repo.path(), "refs/heads/main.lock");
    assert!(
        ref_lock.exists(),
        "continuation should reserve the destination ref"
    );
    continuation
        .kill()
        .expect("continuation should be terminable");
    std::fs::write(&release, "release\n").expect("release continuation hook");
    let _ = continuation.wait_with_output();

    // The orphaned Git child may retain the lifecycle lock briefly. Wait for
    // it to finish before exercising stale ref-lock/head recovery.
    for _ in 0..200 {
        let status = wt_core()
            .args(["merge", "--status", "--json", "--repo", &repo_str])
            .output()
            .expect("status should run");
        if !String::from_utf8_lossy(&status.stdout).contains("\"state\":\"busy\"") {
            break;
        }
        sleep(Duration::from_millis(20));
    }
    assert!(
        ref_lock.exists(),
        "the terminated owner should leave a recoverable lock file"
    );
    let detached = git_allow_failure(&["symbolic-ref", "--quiet", "HEAD"], &repo.path());
    assert!(
        !detached.status.success(),
        "killed continuation should leave a detached HEAD before recovery"
    );

    wt_core()
        .args(["merge", "--continue", "--repo", &repo_str])
        .assert()
        .success();
    assert!(
        !ref_lock.exists(),
        "recovery should clear the stale ref lock"
    );
    let attached = git_allow_failure(
        &["symbolic-ref", "--quiet", "--short", "HEAD"],
        &repo.path(),
    );
    assert_eq!(
        String::from_utf8(attached.stdout)
            .expect("symbolic HEAD should be utf8")
            .trim(),
        "main"
    );
    assert!(
        !source.exists(),
        "recovered continuation should finish cleanup"
    );
    assert_branch_deleted(&repo.path(), "feature/continue-kill");
}

#[test]
fn merge_continue_recovers_recorded_detached_destination_head() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();
    let source = create_conflicted_merge(&repo, "feature/continue-recorded-head");
    let destination_head = git_rev_parse(&repo.path(), "main");
    run_git(
        &["update-ref", "--no-deref", "HEAD", &destination_head],
        &repo.path(),
    );
    std::fs::write(repo.path().join("shared.txt"), "resolved\n").expect("resolve conflict");
    run_git(&["add", "shared.txt"], &repo.path());

    wt_core()
        .args(["merge", "--continue", "--repo", &repo_str])
        .assert()
        .success();

    assert_eq!(
        git_rev_parse(&repo.path(), "main"),
        git_rev_parse(&repo.path(), "HEAD")
    );
    assert!(
        !source.exists(),
        "recorded detached HEAD should be recovered"
    );
    assert_branch_deleted(&repo.path(), "feature/continue-recorded-head");
}

#[test]
fn merge_continue_recovers_detached_expected_result_before_ref_update() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();
    let source = create_conflicted_merge(&repo, "feature/continue-result-head");
    let destination_head = git_rev_parse(&repo.path(), "main");
    let source_head = git_rev_parse(&repo.path(), "MERGE_HEAD");
    std::fs::write(repo.path().join("shared.txt"), "resolved\n").expect("resolve conflict");
    run_git(&["add", "shared.txt"], &repo.path());
    let tree = git_output(&["write-tree"], &repo.path());
    let result = git_output(
        &[
            "commit-tree",
            &tree,
            "-p",
            &destination_head,
            "-p",
            &source_head,
            "-m",
            "recovered merge",
        ],
        &repo.path(),
    );
    run_git(&["update-ref", "--no-deref", "HEAD", &result], &repo.path());

    wt_core()
        .args(["merge", "--continue", "--repo", &repo_str])
        .assert()
        .success();

    assert_eq!(git_rev_parse(&repo.path(), "main"), result);
    assert!(
        !source.exists(),
        "expected merge result should be recovered"
    );
    assert_branch_deleted(&repo.path(), "feature/continue-result-head");
}

#[test]
fn merge_continue_recovers_detached_expected_result_after_ref_update() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();
    let source = create_conflicted_merge(&repo, "feature/continue-result-ref");
    let destination_head = git_rev_parse(&repo.path(), "main");
    let source_head = git_rev_parse(&repo.path(), "MERGE_HEAD");
    std::fs::write(repo.path().join("shared.txt"), "resolved\n").expect("resolve conflict");
    run_git(&["add", "shared.txt"], &repo.path());
    let tree = git_output(&["write-tree"], &repo.path());
    let result = git_output(
        &[
            "commit-tree",
            &tree,
            "-p",
            &destination_head,
            "-p",
            &source_head,
            "-m",
            "recovered merge",
        ],
        &repo.path(),
    );
    run_git(
        &["update-ref", "refs/heads/main", &result, &destination_head],
        &repo.path(),
    );
    run_git(&["update-ref", "--no-deref", "HEAD", &result], &repo.path());

    wt_core()
        .args(["merge", "--continue", "--repo", &repo_str])
        .assert()
        .success();

    assert_eq!(git_rev_parse(&repo.path(), "main"), result);
    assert!(
        !source.exists(),
        "updated expected result should be recovered"
    );
    assert_branch_deleted(&repo.path(), "feature/continue-result-ref");
}

#[test]
fn merge_continue_rejects_an_unrecognized_detached_head() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();
    let source = create_conflicted_merge(&repo, "feature/continue-unrelated-head");
    let destination_head = git_rev_parse(&repo.path(), "main");
    let tree = git_rev_parse(&repo.path(), "HEAD^{tree}");
    let unrelated = git_output(
        &["commit-tree", &tree, "-m", "unrelated detached state"],
        &repo.path(),
    );
    assert_ne!(unrelated, destination_head);
    run_git(
        &["update-ref", "--no-deref", "HEAD", &unrelated],
        &repo.path(),
    );

    let output = wt_core()
        .args(["merge", "--continue", "--repo", &repo_str])
        .output()
        .expect("continuation should run");
    assert!(
        !output.status.success(),
        "unrecognized HEAD must be refused"
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("neither the recorded destination"));
    assert!(
        !git_allow_failure(&["symbolic-ref", "--quiet", "HEAD"], &repo.path())
            .status
            .success(),
        "unrecognized HEAD must remain detached"
    );
    assert_eq!(
        git_rev_parse(&repo.path(), "refs/heads/main"),
        destination_head
    );
    assert!(
        source.exists(),
        "unrecognized HEAD must preserve the source"
    );
    assert_branch_exists(&repo.path(), "feature/continue-unrelated-head");
    assert!(
        repo.path()
            .join(".git/wt-core/merge-operation.json")
            .is_file(),
        "unrecognized HEAD must preserve the lifecycle journal"
    );
}

#[cfg(unix)]
#[test]
fn merge_continue_hook_failure_preserves_merge_for_retry() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();
    create_conflicted_merge(&repo, "feature/continue-hook");
    std::fs::write(repo.path().join("shared.txt"), "resolved\n").expect("resolve conflict");
    run_git(&["add", "shared.txt"], &repo.path());
    install_hook(
        &repo,
        "pre-commit",
        "#!/bin/sh\necho continue hook failed >&2\nexit 42\n",
    );

    wt_core()
        .args(["merge", "--continue", "--repo", &repo_str])
        .assert()
        .failure()
        .stderr(predicate::str::contains("continuation failed"));
    assert!(repo
        .path()
        .join(".git/wt-core/merge-operation.json")
        .is_file());
    assert!(git_path(&repo.path(), "MERGE_HEAD").exists());

    std::fs::remove_file(repo.path().join(".git/hooks/pre-commit")).expect("remove hook");
    wt_core()
        .args(["merge", "--continue", "--repo", &repo_str])
        .assert()
        .success();
    assert!(!repo
        .path()
        .join(".git/wt-core/merge-operation.json")
        .exists());
}

#[test]
fn merge_continue_failed_push_preserves_committed_operation_for_retry() {
    let remote = fixtures::ClonedTestRepo::new();
    let repo = remote.path();
    let repo_str = repo.display().to_string();
    create_conflicted_merge_at(&repo, "feature/push-retry", false, true);
    // The helper above needs only the repository fixture API; remove origin to
    // force the original push intent to fail after the merge commit.
    run_git(&["remote", "remove", "origin"], &repo);

    std::fs::write(repo.join("shared.txt"), "resolved\n").expect("resolve conflict");
    run_git(&["add", "shared.txt"], &repo);
    wt_core()
        .args(["merge", "--continue", "--json", "--repo", &repo_str])
        .assert()
        .success()
        .stderr(predicate::str::contains("push failed"));
    assert!(repo.join(".git/wt-core/merge-operation.json").is_file());

    let origin = remote.origin_path();
    let origin_str = origin.display().to_string();
    run_git(&["remote", "add", "origin", &origin_str], &repo);
    run_git(&["push", "origin", "main"], &repo);
    wt_core()
        .args(["merge", "--continue", "--repo", &repo_str])
        .assert()
        .success();
    assert!(!repo.join(".git/wt-core/merge-operation.json").exists());
}

#[test]
fn merge_status_guides_recovery_for_stale_interrupted_and_corrupt_state() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();
    let source = create_conflicted_merge(&repo, "feature/recovery");
    commit_file(
        &source,
        "after-conflict.txt",
        "changed",
        "change while paused",
    );

    let stale = wt_core()
        .args(["merge", "--status", "--json", "--repo", &repo_str])
        .output()
        .expect("status should run");
    assert!(!stale.status.success());
    let stale_json: serde_json::Value = serde_json::from_slice(&stale.stdout).expect("stale JSON");
    assert_eq!(stale_json["state"], "stale");
    assert!(stale_json["recovery"]
        .as_str()
        .is_some_and(|message| message.contains("source branch HEAD changed")));
    run_git(&["reset", "--hard", "HEAD^"], &source);

    run_git(&["merge", "--abort"], &repo.path());

    let interrupted = wt_core()
        .args(["merge", "--status", "--json", "--repo", &repo_str])
        .output()
        .expect("status should run");
    assert!(!interrupted.status.success());
    let interrupted_json: serde_json::Value =
        serde_json::from_slice(&interrupted.stdout).expect("interrupted JSON");
    assert_eq!(interrupted_json["state"], "interrupted");
    assert!(interrupted_json["recovery"]
        .as_str()
        .is_some_and(|message| message.contains("Git no longer has the merge")));

    let state_path = repo.path().join(".git/wt-core/merge-operation.json");
    std::fs::write(&state_path, "{not-json").expect("corrupt state");
    let corrupt = wt_core()
        .args(["merge", "--status", "--json", "--repo", &repo_str])
        .output()
        .expect("status should run");
    assert!(!corrupt.status.success());
    let corrupt_json: serde_json::Value =
        serde_json::from_slice(&corrupt.stdout).expect("corrupt JSON");
    assert_eq!(corrupt_json["state"], "corrupt");
    assert!(corrupt_json["recovery"]
        .as_str()
        .is_some_and(|message| message.contains("preserve")));
}

#[test]
fn merge_status_rejects_a_changed_recorded_admin_identity() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();
    create_conflicted_merge(&repo, "feature/identity-status");
    let state_path = repo.path().join(".git/wt-core/merge-operation.json");
    let mut state: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&state_path).expect("read state"))
            .expect("state JSON");
    state["source_identity"]["Linked"]["admin_dir"] = serde_json::json!("/replacement/admin");
    std::fs::write(
        &state_path,
        serde_json::to_vec_pretty(&state).expect("encode state"),
    )
    .expect("write state");

    let output = wt_core()
        .args(["merge", "--status", "--json", "--repo", &repo_str])
        .output()
        .expect("status should run");
    assert!(!output.status.success());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).expect("status JSON");
    assert_eq!(json["state"], "stale");
    assert!(json["recovery"]
        .as_str()
        .is_some_and(|message| message.contains("identity changed")));
    assert!(git_path(&repo.path(), "MERGE_HEAD").exists());
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
fn merge_json_takes_precedence_over_print_paths() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    wt_core()
        .args(["add", "feature/json-precedence-merge", "--repo", &repo_str])
        .assert()
        .success();

    let wt_dir = find_worktree_dir(&repo.path(), "feature-json-precedence-merge");
    commit_file(&wt_dir, "precedence.txt", "precedence", "precedence commit");

    let output = wt_core()
        .args([
            "merge",
            "feature/json-precedence-merge",
            "--repo",
            &repo_str,
            "--print-paths",
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8(output).expect("invalid utf8");
    assert_eq!(stdout.lines().count(), 1, "expected one JSON document");
    let json: serde_json::Value = serde_json::from_str(stdout.trim()).expect("invalid json");
    assert_eq!(json["ok"], true);
    assert_eq!(json["branch"], "feature/json-precedence-merge");
    assert_eq!(json["cleaned_up"], true);
    assert!(json["removed_path"].as_str().is_some());
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
fn merge_rejects_replaced_linked_destination_before_mutation_or_cleanup() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();
    let destination = add_linked_destination(&repo, "release/stale");

    wt_core()
        .args(["add", "feature/stale", "--repo", &repo_str])
        .assert()
        .success();
    let source = find_worktree_dir(&repo.path(), "feature-stale");
    commit_file(&source, "source.txt", "source", "source");
    let main_before = git_rev_parse(&repo.path(), "main");
    let destination_branch_before = git_rev_parse(&repo.path(), "release/stale");
    let admin_before = worktree_admin_snapshot(&repo.path());

    let unrelated = fixtures::TestRepo::new();
    run_git(&["checkout", "-b", "release/stale"], &unrelated.path());
    commit_file(
        &unrelated.path(),
        "unrelated-release.txt",
        "unrelated release",
        "unrelated release",
    );
    run_git(&["checkout", "-b", "feature/stale"], &unrelated.path());
    commit_file(
        &unrelated.path(),
        "unrelated-feature.txt",
        "unrelated feature",
        "unrelated feature",
    );
    run_git(&["checkout", "release/stale"], &unrelated.path());

    std::fs::remove_dir_all(&destination).expect("remove registered destination");
    let unrelated_path = unrelated.path();
    std::fs::rename(&unrelated_path, &destination).expect("replace destination");
    let unrelated_before = git_rev_parse(&destination, "HEAD");

    wt_core()
        .args([
            "merge",
            "feature/stale",
            "--into",
            "release/stale",
            "--repo",
            &repo_str,
        ])
        .assert()
        .failure()
        .code(5)
        .stderr(predicate::str::contains(
            "stale destination worktree metadata",
        ))
        .stderr(predicate::str::contains("common directory"));

    assert_eq!(git_rev_parse(&repo.path(), "main"), main_before);
    assert_eq!(
        git_rev_parse(&repo.path(), "release/stale"),
        destination_branch_before
    );
    assert_eq!(git_rev_parse(&destination, "HEAD"), unrelated_before);
    assert_branch_exists(&repo.path(), "feature/stale");
    assert!(source.exists(), "source must not be cleaned up");
    assert_eq!(worktree_admin_snapshot(&repo.path()), admin_before);
}

#[test]
fn merge_rejects_replaced_source_before_mutation_or_cleanup() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    wt_core()
        .args(["add", "feature/stale-source", "--repo", &repo_str])
        .assert()
        .success();
    let source = find_worktree_dir(&repo.path(), "feature-stale-source");
    commit_file(&source, "source.txt", "source", "source");
    let main_before = git_rev_parse(&repo.path(), "main");
    let source_branch_before = git_rev_parse(&repo.path(), "feature/stale-source");
    let admin_before = worktree_admin_snapshot(&repo.path());

    let unrelated = fixtures::TestRepo::new();
    run_git(
        &["checkout", "-b", "feature/stale-source"],
        &unrelated.path(),
    );
    commit_file(&unrelated.path(), "unrelated.txt", "unrelated", "unrelated");
    std::fs::remove_dir_all(&source).expect("remove registered source");
    let unrelated_path = unrelated.path();
    std::fs::rename(&unrelated_path, &source).expect("replace source");
    let unrelated_before = git_rev_parse(&source, "HEAD");

    wt_core()
        .args(["merge", "feature/stale-source", "--repo", &repo_str])
        .assert()
        .failure()
        .code(5)
        .stderr(predicate::str::contains("stale source worktree metadata"))
        .stderr(predicate::str::contains("common directory"));

    assert_eq!(git_rev_parse(&repo.path(), "main"), main_before);
    assert_eq!(
        git_rev_parse(&repo.path(), "feature/stale-source"),
        source_branch_before
    );
    assert_eq!(git_rev_parse(&source, "HEAD"), unrelated_before);
    assert!(source.exists(), "replacement source must not be removed");
    assert_eq!(worktree_admin_snapshot(&repo.path()), admin_before);
}

#[test]
fn merge_inspect_rejects_symlinked_destination_without_pruning_metadata() {
    #[cfg(not(unix))]
    return;

    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;

        let repo = fixtures::TestRepo::new();
        let repo_str = repo.path().display().to_string();
        let destination = add_linked_destination(&repo, "release/symlink");

        wt_core()
            .args(["add", "feature/symlink", "--repo", &repo_str])
            .assert()
            .success();
        let source = find_worktree_dir(&repo.path(), "feature-symlink");
        commit_file(&source, "source.txt", "source", "source");
        let admin_before = worktree_admin_snapshot(&repo.path());

        let unrelated = fixtures::TestRepo::new();
        let unrelated_path = unrelated.path();
        std::fs::remove_dir_all(&destination).expect("remove registered destination");
        symlink(&unrelated_path, &destination).expect("replace destination with symlink");

        wt_core()
            .args([
                "merge",
                "feature/symlink",
                "--into",
                "release/symlink",
                "--inspect",
                "--repo",
                &repo_str,
            ])
            .assert()
            .failure()
            .code(5)
            .stderr(predicate::str::contains(
                "stale destination worktree metadata",
            ))
            .stderr(predicate::str::contains("symlink"));

        assert!(source.exists(), "inspect must preserve the source");
        assert!(destination.is_symlink(), "replacement symlink must remain");
        assert_eq!(worktree_admin_snapshot(&repo.path()), admin_before);
    }
}

#[test]
fn merge_rejects_same_repository_branch_spoof_before_cleanup() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    wt_core()
        .args(["add", "feature/spoof", "--repo", &repo_str])
        .assert()
        .success();
    let source = find_worktree_dir(&repo.path(), "feature-spoof");
    commit_file(&source, "source.txt", "source", "source");

    wt_core()
        .args(["add", "feature/other", "--repo", &repo_str])
        .assert()
        .success();
    let replacement = find_worktree_dir(&repo.path(), "feature-other");
    lock_worktree_metadata(&repo.path(), &replacement);
    let admin_before = worktree_admin_snapshot(&repo.path());
    std::fs::remove_dir_all(&source).expect("remove registered source");
    std::fs::rename(&replacement, &source).expect("move another worktree into source path");

    wt_core()
        .args(["merge", "feature/spoof", "--repo", &repo_str])
        .assert()
        .failure()
        .code(5)
        .stderr(predicate::str::contains("stale source worktree metadata"))
        .stderr(predicate::str::contains("registered admin entry"));

    assert_branch_exists(&repo.path(), "feature/spoof");
    assert!(source.exists(), "spoofed source must not be removed");
    assert_eq!(worktree_admin_snapshot(&repo.path()), admin_before);
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
        .stderr(predicate::str::contains(
            "merge of 'feature/linked-dirty' failed",
        ))
        .stderr(predicate::str::contains("merge conflicts").not())
        .stderr(predicate::str::contains("failed and was aborted"));

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

// ── Merge topology preflight tests ─────────────────────────────────

#[test]
fn merge_json_distinguishes_content_conflict_from_topology_refusal() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    wt_core()
        .args(["add", "feature/json-conflict", "--repo", &repo_str])
        .assert()
        .success();
    let source = find_worktree_dir(&repo.path(), "feature-json-conflict");
    commit_file(&source, "shared.txt", "source", "source conflict");
    commit_file(
        &repo.path(),
        "shared.txt",
        "destination",
        "destination conflict",
    );
    let destination_before = git_rev_parse(&repo.path(), "HEAD");

    let output = wt_core()
        .args([
            "merge",
            "feature/json-conflict",
            "--json",
            "--repo",
            &repo_str,
        ])
        .assert()
        .failure()
        .code(5)
        .get_output()
        .stdout
        .clone();
    let json: serde_json::Value = serde_json::from_slice(&output).expect("invalid json");

    assert_eq!(json["ok"], false);
    assert_eq!(json["preflight"]["topology"], "no_upstream");
    assert_eq!(json["refusal"]["kind"], "content");
    assert_eq!(json["refusal"]["reason"], "content_conflict");
    assert_eq!(json["operation"]["state"], "conflicted");
    assert_eq!(
        json["operation"]["unresolved_paths"],
        serde_json::json!(["shared.txt"])
    );
    assert!(json["operation"]["state_path"]
        .as_str()
        .is_some_and(|path| path.ends_with(".git/wt-core/merge-operation.json")));
    assert_eq!(git_rev_parse(&repo.path(), "HEAD"), destination_before);
    assert!(source.exists(), "content conflict must preserve the source");
}

#[test]
fn merge_inspect_reports_synchronized_topology_without_mutation() {
    let (repo, _upstream) = setup_repo_with_upstream();
    let repo_str = repo.path().display().to_string();

    wt_core()
        .args(["add", "feature/inspect", "--repo", &repo_str])
        .assert()
        .success();
    let source = find_worktree_dir(&repo.path(), "feature-inspect");
    commit_file(&source, "inspect.txt", "inspect", "inspect");
    let main_before = git_rev_parse(&repo.path(), "main");

    let output = wt_core()
        .args([
            "merge",
            "feature/inspect",
            "--inspect",
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

    assert_eq!(json["ok"], true);
    assert_eq!(json["inspect"], true);
    assert_eq!(json["preflight"]["upstream"], "origin/main");
    assert_eq!(json["preflight"]["ahead"], 0);
    assert_eq!(json["preflight"]["behind"], 0);
    assert_eq!(json["preflight"]["topology"], "synchronized");
    assert_eq!(json["preflight"]["allowed"], true);
    assert_eq!(git_rev_parse(&repo.path(), "main"), main_before);
    assert!(
        source.exists(),
        "inspect must not clean up the source worktree"
    );
}

#[test]
fn merge_inspect_reports_absent_upstream_without_inventing_remote_state() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    wt_core()
        .args(["add", "feature/no-upstream", "--repo", &repo_str])
        .assert()
        .success();
    let source = find_worktree_dir(&repo.path(), "feature-no-upstream");
    commit_file(
        &source,
        "no-upstream.txt",
        "source",
        "source without upstream",
    );
    let status_before = git_status(&repo.path());
    let worktree_admin_before = worktree_admin_snapshot(&repo.path());

    let output = wt_core()
        .args([
            "merge",
            "feature/no-upstream",
            "--inspect",
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

    assert!(json["preflight"]["upstream"].is_null());
    assert_eq!(json["preflight"]["ahead"], serde_json::Value::Null);
    assert_eq!(json["preflight"]["behind"], serde_json::Value::Null);
    assert_eq!(json["preflight"]["topology"], "no_upstream");
    assert_eq!(json["preflight"]["allowed"], true);
    assert_eq!(git_status(&repo.path()), status_before);
    assert_eq!(worktree_admin_snapshot(&repo.path()), worktree_admin_before);
    assert!(
        source.exists(),
        "inspect must not clean up the source worktree"
    );
}

#[test]
fn merge_inspect_refuses_configured_upstream_missing_remote_ref() {
    let (repo, _upstream) = setup_repo_with_upstream();
    let repo_str = repo.path().display().to_string();
    run_git(
        &["update-ref", "-d", "refs/remotes/origin/main"],
        &repo.path(),
    );

    wt_core()
        .args(["add", "feature/stale-upstream", "--repo", &repo_str])
        .assert()
        .success();
    let source = find_worktree_dir(&repo.path(), "feature-stale-upstream");
    commit_file(&source, "stale.txt", "source", "source with stale upstream");

    let output = wt_core()
        .args([
            "merge",
            "feature/stale-upstream",
            "--inspect",
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

    assert_eq!(json["preflight"]["upstream"], "origin/main");
    assert_eq!(json["preflight"]["topology"], "upstream_unavailable");
    assert_eq!(json["preflight"]["allowed"], false);
    assert_eq!(json["refusal"]["kind"], "topology");
    assert_eq!(
        json["refusal"]["reason"],
        "destination_upstream_unavailable"
    );
    assert!(
        source.exists(),
        "inspect must not clean up the source worktree"
    );
}

#[test]
fn merge_reports_ahead_destination_before_merging() {
    let (repo, _upstream) = setup_repo_with_upstream();
    let repo_str = repo.path().display().to_string();

    commit_file(
        &repo.path(),
        "local.txt",
        "local",
        "local destination commit",
    );
    wt_core()
        .args(["add", "feature/ahead", "--repo", &repo_str])
        .assert()
        .success();
    let source = find_worktree_dir(&repo.path(), "feature-ahead");
    commit_file(&source, "ahead.txt", "ahead", "ahead source");

    wt_core()
        .args([
            "merge",
            "feature/ahead",
            "--no-cleanup",
            "--repo",
            &repo_str,
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("AHEAD"))
        .stdout(predicate::str::contains("ahead 1, behind 0"));
}

#[test]
fn merge_inspect_reports_behind_destination() {
    let (repo, _upstream) = setup_repo_with_upstream();
    let repo_str = repo.path().display().to_string();

    // Move origin/main forward, then leave the checked-out destination one
    // commit behind it. Preflight must report this without fetching.
    commit_file(
        &repo.path(),
        "remote.txt",
        "remote",
        "remote destination commit",
    );
    run_git(&["push", "origin", "main"], &repo.path());
    run_git(&["reset", "--hard", "HEAD~1"], &repo.path());

    wt_core()
        .args(["add", "feature/behind", "--repo", &repo_str])
        .assert()
        .success();
    let source = find_worktree_dir(&repo.path(), "feature-behind");
    commit_file(&source, "behind.txt", "behind", "behind source");

    let output = wt_core()
        .args([
            "merge",
            "feature/behind",
            "--inspect",
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

    assert_eq!(json["preflight"]["topology"], "behind");
    assert_eq!(json["preflight"]["ahead"], 0);
    assert_eq!(json["preflight"]["behind"], 1);
    assert_eq!(json["preflight"]["allowed"], false);
    assert_eq!(json["refusal"]["reason"], "destination_behind_upstream");
    assert!(
        source.exists(),
        "inspect must not clean up the source worktree"
    );
}

#[test]
fn merge_rejects_diverged_destination_before_mutation_with_json_reason() {
    let (repo, _upstream) = setup_repo_with_upstream();
    let repo_str = repo.path().display().to_string();

    wt_core()
        .args(["add", "feature/diverged-topology", "--repo", &repo_str])
        .assert()
        .success();
    let source = find_worktree_dir(&repo.path(), "feature-diverged-topology");
    commit_file(&source, "source.txt", "source", "source");

    run_git(&["checkout", "-b", "remote-simulation"], &repo.path());
    commit_file(&repo.path(), "remote.txt", "remote", "remote");
    let remote_head = git_rev_parse(&repo.path(), "HEAD");
    run_git(&["checkout", "main"], &repo.path());
    run_git(&["branch", "-D", "remote-simulation"], &repo.path());
    run_git(
        &["update-ref", "refs/remotes/origin/main", &remote_head],
        &repo.path(),
    );
    commit_file(&repo.path(), "local.txt", "local", "local");
    let main_before = git_rev_parse(&repo.path(), "main");

    let output = wt_core()
        .args([
            "merge",
            "feature/diverged-topology",
            "--json",
            "--repo",
            &repo_str,
        ])
        .assert()
        .failure()
        .code(5)
        .get_output()
        .stdout
        .clone();
    let json: serde_json::Value = serde_json::from_slice(&output).expect("invalid json");

    assert_eq!(json["ok"], false);
    assert_eq!(json["preflight"]["topology"], "diverged");
    assert_eq!(json["preflight"]["ahead"], 1);
    assert_eq!(json["preflight"]["behind"], 1);
    assert_eq!(json["preflight"]["allowed"], false);
    assert_eq!(json["refusal"]["kind"], "topology");
    assert_eq!(
        json["refusal"]["reason"],
        "destination_diverged_from_upstream"
    );
    assert_eq!(git_rev_parse(&repo.path(), "main"), main_before);
    assert!(source.exists(), "topology refusal must preserve the source");
}

#[test]
fn merge_inspect_explains_merged_then_reverted_source() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    wt_core()
        .args(["add", "feature/reverted", "--repo", &repo_str])
        .assert()
        .success();
    let source = find_worktree_dir(&repo.path(), "feature-reverted");
    commit_file(&source, "reverted.txt", "reverted", "reverted source");
    run_git(
        &[
            "merge",
            "--no-ff",
            "feature/reverted",
            "-m",
            "Merge branch 'feature/reverted'",
        ],
        &repo.path(),
    );
    run_git(&["revert", "-m", "1", "HEAD", "--no-edit"], &repo.path());

    let output = wt_core()
        .args([
            "merge",
            "feature/reverted",
            "--inspect",
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

    assert_eq!(json["preflight"]["source_history"], "merged_then_reverted");
    assert_eq!(json["preflight"]["source_was_merged"], true);
    assert_eq!(json["preflight"]["source_was_reverted"], true);
    assert!(
        json["preflight"]["reverted_commit"]
            .as_str()
            .is_some_and(|sha| sha.len() == 40),
        "reverted commit should be exposed"
    );
    assert!(
        source.exists(),
        "inspect must not clean up the source worktree"
    );
}

#[test]
fn merge_inspect_ignores_revert_marker_for_unmerged_source() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    wt_core()
        .args(["add", "feature/unmerged-revert", "--repo", &repo_str])
        .assert()
        .success();
    let source = find_worktree_dir(&repo.path(), "feature-unmerged-revert");
    commit_file(&source, "unmerged.txt", "source", "unmerged source");
    let shared_base = git_rev_parse(&repo.path(), "main");
    let message =
        format!("Revert a shared base, not the source\n\nThis reverts commit {shared_base}.");
    run_git(&["commit", "--allow-empty", "-m", &message], &repo.path());

    let output = wt_core()
        .args([
            "merge",
            "feature/unmerged-revert",
            "--inspect",
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

    assert_eq!(json["preflight"]["source_history"], "not_merged");
    assert_eq!(json["preflight"]["source_was_merged"], false);
    assert_eq!(json["preflight"]["source_was_reverted"], false);
    assert!(json["preflight"]["reverted_commit"].is_null());
    assert!(
        source.exists(),
        "inspect must not clean up the source worktree"
    );
}

#[test]
fn merge_inspect_reports_linked_destination_topology() {
    let (repo, _upstream) = setup_repo_with_upstream();
    let repo_str = repo.path().display().to_string();
    let destination = add_linked_destination(&repo, "release/inspect");
    run_git(&["push", "-u", "origin", "release/inspect"], &repo.path());

    wt_core()
        .args(["add", "feature/linked-inspect", "--repo", &repo_str])
        .assert()
        .success();
    let source = find_worktree_dir(&repo.path(), "feature-linked-inspect");
    commit_file(&source, "linked-inspect.txt", "linked", "linked inspect");
    let destination_before = git_rev_parse(&destination, "HEAD");

    let output = wt_core()
        .args([
            "merge",
            "feature/linked-inspect",
            "--into",
            "release/inspect",
            "--inspect",
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

    assert_eq!(json["preflight"]["destination"], "release/inspect");
    assert_eq!(
        json["preflight"]["destination_path"],
        destination.display().to_string()
    );
    assert_eq!(json["preflight"]["topology"], "synchronized");
    assert_eq!(git_rev_parse(&destination, "HEAD"), destination_before);
    assert!(
        source.exists(),
        "linked inspect must not clean up the source"
    );
}

#[test]
fn merge_normal_rejects_locked_stale_source_before_preflight() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    wt_core()
        .args(["add", "feature/stale-source", "--repo", &repo_str])
        .assert()
        .success();
    let source = find_worktree_dir(&repo.path(), "feature-stale-source");
    lock_worktree_metadata(&repo.path(), &source);
    std::fs::remove_dir_all(&source).expect("remove stale source");

    wt_core()
        .args([
            "merge",
            "feature/stale-source",
            "--json",
            "--repo",
            &repo_str,
        ])
        .assert()
        .failure()
        .code(5)
        .stderr(predicate::str::contains("stale source worktree metadata"))
        .stderr(predicate::str::contains("No such file or directory").not());

    assert_branch_exists(&repo.path(), "feature/stale-source");
    assert_eq!(
        worktree_admin_snapshot(&repo.path()).len(),
        1,
        "locked stale source metadata must be retained for explicit repair"
    );
}

#[test]
fn merge_normal_rejects_locked_stale_destination_before_content_merge() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();
    let destination = add_linked_destination(&repo, "release/stale-destination");
    lock_worktree_metadata(&repo.path(), &destination);
    std::fs::remove_dir_all(&destination).expect("remove stale destination");

    wt_core()
        .args(["add", "feature/stale-destination", "--repo", &repo_str])
        .assert()
        .success();
    let source = find_worktree_dir(&repo.path(), "feature-stale-destination");
    commit_file(&source, "stale-destination.txt", "source", "source");

    wt_core()
        .args([
            "merge",
            "feature/stale-destination",
            "--into",
            "release/stale-destination",
            "--json",
            "--repo",
            &repo_str,
        ])
        .assert()
        .failure()
        .code(5)
        .stderr(predicate::str::contains(
            "stale destination worktree metadata",
        ))
        .stderr(predicate::str::contains("No such file or directory").not());

    assert_branch_exists(&repo.path(), "feature/stale-destination");
    assert_branch_exists(&repo.path(), "release/stale-destination");
}

#[test]
fn merge_inspect_does_not_prune_locked_stale_metadata() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    wt_core()
        .args(["add", "feature/inspect-stale", "--repo", &repo_str])
        .assert()
        .success();
    let source = find_worktree_dir(&repo.path(), "feature-inspect-stale");
    lock_worktree_metadata(&repo.path(), &source);
    std::fs::remove_dir_all(&source).expect("remove stale source");
    let admin_before = worktree_admin_snapshot(&repo.path());

    wt_core()
        .args([
            "merge",
            "feature/inspect-stale",
            "--inspect",
            "--json",
            "--repo",
            &repo_str,
        ])
        .assert()
        .failure()
        .code(5)
        .stderr(predicate::str::contains("stale source worktree metadata"));

    assert_eq!(
        worktree_admin_snapshot(&repo.path()),
        admin_before,
        "inspect must not prune stale worktree metadata"
    );
}

#[test]
fn merge_inspect_detects_squash_integration_by_equivalent_tree() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    wt_core()
        .args(["add", "feature/squashed-history", "--repo", &repo_str])
        .assert()
        .success();
    let source = find_worktree_dir(&repo.path(), "feature-squashed-history");
    commit_file(&source, "squash-one.txt", "one", "one");
    commit_file(&source, "squash-two.txt", "two", "two");
    run_git(
        &["merge", "--squash", "feature/squashed-history"],
        &repo.path(),
    );
    run_git(&["commit", "-m", "squashed feature"], &repo.path());

    let output = wt_core()
        .args([
            "merge",
            "feature/squashed-history",
            "--inspect",
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

    assert_eq!(json["preflight"]["source_history"], "already_merged");
    assert_eq!(json["preflight"]["source_was_merged"], false);
    assert!(source.exists(), "inspect must preserve the source worktree");
}

#[test]
fn merge_inspect_rejects_forged_revert_marker_without_tree_change() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    wt_core()
        .args(["add", "feature/forged-revert", "--repo", &repo_str])
        .assert()
        .success();
    let source = find_worktree_dir(&repo.path(), "feature-forged-revert");
    commit_file(&source, "forged.txt", "source", "source");
    let source_head = git_rev_parse(&source, "HEAD");
    run_git(
        &["merge", "--ff-only", "feature/forged-revert"],
        &repo.path(),
    );
    run_git(
        &[
            "commit",
            "--allow-empty",
            "-m",
            &format!("Forged marker\n\nThis reverts commit {source_head}."),
        ],
        &repo.path(),
    );

    let output = wt_core()
        .args([
            "merge",
            "feature/forged-revert",
            "--inspect",
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

    assert_eq!(json["preflight"]["source_history"], "already_merged");
    assert_eq!(json["preflight"]["source_was_merged"], true);
    assert_eq!(json["preflight"]["source_was_reverted"], false);
    assert!(json["preflight"]["reverted_commit"].is_null());
}

#[cfg(unix)]
#[test]
fn merge_json_distinguishes_pre_merge_hook_failure_from_content_conflict() {
    use std::os::unix::fs::PermissionsExt;

    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    wt_core()
        .args(["add", "feature/hook-failure", "--repo", &repo_str])
        .assert()
        .success();
    let source = find_worktree_dir(&repo.path(), "feature-hook-failure");
    commit_file(&source, "hook.txt", "source", "source");

    let hook = repo.path().join(".git/hooks/pre-merge-commit");
    std::fs::write(
        &hook,
        "#!/bin/sh\necho pre-merge hook failed >&2\nexit 42\n",
    )
    .expect("write hook");
    let mut permissions = std::fs::metadata(&hook)
        .expect("hook metadata")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&hook, permissions).expect("chmod hook");

    let output = wt_core()
        .args([
            "merge",
            "feature/hook-failure",
            "--json",
            "--repo",
            &repo_str,
        ])
        .assert()
        .failure()
        .code(2)
        .get_output()
        .stdout
        .clone();
    let json: serde_json::Value = serde_json::from_slice(&output).expect("invalid json");

    assert_eq!(json["ok"], false);
    assert_eq!(json["refusal"]["kind"], "git");
    assert_eq!(json["refusal"]["reason"], "git_error");
    assert!(json["message"]
        .as_str()
        .is_some_and(|message| message.contains("merge of 'feature/hook-failure' failed")));
    assert!(json["message"]
        .as_str()
        .is_some_and(|message| !message.contains("content merge conflicts")));
    assert_branch_exists(&repo.path(), "feature/hook-failure");
}

#[cfg(unix)]
#[test]
fn merge_source_identity_race_skips_cleanup_after_commit() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    wt_core()
        .args(["add", "feature/source-identity-race", "--repo", &repo_str])
        .assert()
        .success();
    let source = find_worktree_dir(&repo.path(), "feature-source-identity-race");
    commit_file(&source, "source-race.txt", "source", "source race");

    install_identity_replacement_hook(
        &repo,
        "pre-merge-commit",
        &source,
        "feature/source-identity-race",
        "replacement-source-identity",
    );

    let output = wt_core()
        .args([
            "merge",
            "feature/source-identity-race",
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

    assert_eq!(json["ok"], true);
    assert_eq!(json["cleaned_up"], false);
    assert_eq!(json["pushed"], false);
    assert!(json["warnings"]
        .as_array()
        .is_some_and(|warnings| warnings.iter().any(|warning| {
            warning
                .as_str()
                .is_some_and(|message| message.contains("identity changed"))
        })));
    assert!(source.exists(), "replacement source must remain untouched");
    assert_branch_exists(&repo.path(), "feature/source-identity-race");
    assert!(
        git_log_oneline(&repo.path(), "main")
            .contains("Merge branch 'feature/source-identity-race'"),
        "successful merge must remain committed"
    );
}

#[cfg(unix)]
#[test]
fn merge_linked_destination_identity_race_skips_cleanup_and_push() {
    let (repo, upstream) = setup_repo_with_upstream();
    let repo_str = repo.path().display().to_string();
    let destination = add_linked_destination(&repo, "release/destination-identity-race");
    run_git(
        &["push", "-u", "origin", "release/destination-identity-race"],
        &repo.path(),
    );

    wt_core()
        .args([
            "add",
            "feature/destination-identity-race",
            "--repo",
            &repo_str,
        ])
        .assert()
        .success();
    let source = find_worktree_dir(&repo.path(), "feature-destination-identity-race");
    commit_file(
        &source,
        "destination-race.txt",
        "source",
        "destination race",
    );

    install_identity_replacement_hook(
        &repo,
        "post-merge",
        &destination,
        "release/destination-identity-race",
        "replacement-destination-identity",
    );

    let output = wt_core()
        .args([
            "merge",
            "feature/destination-identity-race",
            "--into",
            "release/destination-identity-race",
            "--push",
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

    assert_eq!(json["ok"], true);
    assert_eq!(json["cleaned_up"], false);
    assert_eq!(json["pushed"], false);
    assert!(json["warnings"].as_array().is_some_and(|warnings| {
        warnings.iter().any(|warning| {
            warning
                .as_str()
                .is_some_and(|message| message.contains("identity changed"))
        })
    }));
    assert!(
        destination.exists(),
        "replacement destination must remain untouched"
    );
    assert!(
        source.exists(),
        "cleanup must stop when destination identity changed"
    );
    assert_branch_exists(&repo.path(), "feature/destination-identity-race");
    assert!(
        !git_log_oneline(upstream.path(), "release/destination-identity-race")
            .contains("Merge branch 'feature/destination-identity-race'")
    );
}

#[cfg(unix)]
#[test]
fn merge_source_identity_control_keeps_cleanup_with_non_replacing_hook() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    wt_core()
        .args([
            "add",
            "feature/source-identity-control",
            "--repo",
            &repo_str,
        ])
        .assert()
        .success();
    let source = find_worktree_dir(&repo.path(), "feature-source-identity-control");
    commit_file(&source, "source-control.txt", "source", "source control");
    install_hook(&repo, "pre-merge-commit", "#!/bin/sh\nexit 0\n");

    wt_core()
        .args([
            "merge",
            "feature/source-identity-control",
            "--repo",
            &repo_str,
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains("identity changed").not());

    assert!(
        !source.exists(),
        "stable source identity should be cleaned up"
    );
    assert_branch_deleted(&repo.path(), "feature/source-identity-control");
}

#[cfg(unix)]
#[test]
fn merge_linked_destination_identity_control_keeps_push_with_non_replacing_hook() {
    let (repo, upstream) = setup_repo_with_upstream();
    let repo_str = repo.path().display().to_string();
    let destination = add_linked_destination(&repo, "release/destination-identity-control");
    run_git(
        &[
            "push",
            "-u",
            "origin",
            "release/destination-identity-control",
        ],
        &repo.path(),
    );

    wt_core()
        .args([
            "add",
            "feature/destination-identity-control",
            "--repo",
            &repo_str,
        ])
        .assert()
        .success();
    let source = find_worktree_dir(&repo.path(), "feature-destination-identity-control");
    commit_file(
        &source,
        "destination-control.txt",
        "source",
        "destination control",
    );
    install_hook(&repo, "post-merge", "#!/bin/sh\nexit 0\n");

    wt_core()
        .args([
            "merge",
            "feature/destination-identity-control",
            "--into",
            "release/destination-identity-control",
            "--push",
            "--repo",
            &repo_str,
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains("identity changed").not());

    assert!(
        !source.exists(),
        "stable source identity should be cleaned up"
    );
    assert_branch_deleted(&repo.path(), "feature/destination-identity-control");
    assert!(
        git_log_oneline(upstream.path(), "release/destination-identity-control")
            .contains("Merge branch 'feature/destination-identity-control'")
    );
    assert!(destination.exists(), "linked destination must remain");
}

// ── Helpers ─────────────────────────────────────────────────────────

fn create_conflicted_merge(repo: &fixtures::TestRepo, branch: &str) -> std::path::PathBuf {
    create_conflicted_merge_at(&repo.path(), branch, false, false)
}

fn create_conflicted_merge_with_options(
    repo: &fixtures::TestRepo,
    branch: &str,
    no_cleanup: bool,
) -> std::path::PathBuf {
    create_conflicted_merge_at(&repo.path(), branch, no_cleanup, false)
}

fn create_conflicted_merge_at(
    repo: &std::path::Path,
    branch: &str,
    no_cleanup: bool,
    push: bool,
) -> std::path::PathBuf {
    let repo_str = repo.display().to_string();
    wt_core()
        .args(["add", branch, "--repo", &repo_str])
        .assert()
        .success();
    let prefix = branch.replace('/', "-");
    let source = find_worktree_dir(repo, &prefix);
    commit_file(&source, "shared.txt", "feature version", "feature change");
    commit_file(repo, "shared.txt", "main version", "main change");

    let mut merge = wt_core();
    merge.args(["merge", branch, "--repo", &repo_str]);
    if no_cleanup {
        merge.arg("--no-cleanup");
    }
    if push {
        merge.arg("--push");
    }
    merge.assert().failure();
    source
}

#[cfg(unix)]
fn install_hook(repo: &fixtures::TestRepo, name: &str, script: &str) {
    use std::os::unix::fs::PermissionsExt;

    let hook = repo.path().join(".git/hooks").join(name);
    std::fs::write(&hook, script).expect("write hook");
    let mut permissions = std::fs::metadata(&hook)
        .expect("hook metadata")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&hook, permissions).expect("chmod hook");
}

#[test]
fn read_only_commands_preserve_stale_worktree_metadata() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();
    wt_core()
        .args(["add", "feature/read-only-stale", "--repo", &repo_str])
        .assert()
        .success();
    let stale_path = find_worktree_dir(&repo.path(), "feature-read-only-stale");
    lock_worktree_metadata(&repo.path(), &stale_path);
    std::fs::remove_dir_all(&stale_path).expect("remove stale worktree directory");
    let admin_before = worktree_admin_snapshot(&repo.path());

    wt_core()
        .args(["list", "--repo", &repo_str])
        .assert()
        .success();

    wt_core()
        .args(["go", "feature/read-only-stale", "--repo", &repo_str])
        .assert()
        .failure()
        .code(5)
        .stderr(predicate::str::contains("worktree for branch"))
        .stderr(predicate::str::contains("is unavailable"));

    wt_core()
        .args([
            "diff",
            "feature/read-only-stale",
            "--dry-run",
            "--repo",
            &repo_str,
        ])
        .assert()
        .success();

    wt_core()
        .args(["doctor", "--repo", &repo_str])
        .assert()
        .success()
        .stdout(predicate::str::contains("git worktree unlock"));

    assert_eq!(
        worktree_admin_snapshot(&repo.path()),
        admin_before,
        "read-only commands must not prune stale metadata"
    );
}

#[cfg(unix)]
#[test]
fn stale_guidance_shell_quotes_apostrophe_and_whitespace_paths() {
    let parent = fixtures::TestRepo::new();
    let repo_path = parent.path().join("repo with ' apostrophe");
    std::fs::create_dir(&repo_path).expect("create repository directory");
    run_git(&["init", "-b", "main"], &repo_path);
    run_git(&["config", "user.email", "test@test.com"], &repo_path);
    run_git(&["config", "user.name", "Test"], &repo_path);
    std::fs::write(repo_path.join("README.md"), "# test\n").expect("write readme");
    run_git(&["add", "."], &repo_path);
    run_git(&["commit", "-m", "initial commit"], &repo_path);

    let repo_str = repo_path.display().to_string();
    wt_core()
        .args(["add", "feature/quoted-stale", "--repo", &repo_str])
        .assert()
        .success();
    let stale_path = find_worktree_dir(&repo_path, "feature-quoted-stale");
    lock_worktree_metadata(&repo_path, &stale_path);
    std::fs::remove_dir_all(&stale_path).expect("remove stale worktree directory");
    let expected_path = stale_path.display().to_string();

    let doctor = wt_core()
        .args(["doctor", "--repo", &repo_str])
        .output()
        .expect("run doctor");
    assert!(doctor.status.success());
    let doctor_output = String::from_utf8(doctor.stdout).expect("doctor output is utf8");

    let parse_guidance_path = |output: &str| {
        let command = output
            .split_once("git worktree unlock ")
            .and_then(|(_, rest)| rest.split('`').next())
            .expect("unlock command in stale guidance");
        let script = format!("set -- {command}; printf \"%s\" \"$1\"");
        let parsed = StdCommand::new("sh")
            .args(["-c", &script, "shell-parse"])
            .output()
            .expect("parse shell guidance");
        assert!(parsed.status.success());
        String::from_utf8(parsed.stdout).expect("parsed path is utf8")
    };

    assert_eq!(parse_guidance_path(&doctor_output), expected_path);

    let go = wt_core()
        .args(["go", "feature/quoted-stale", "--repo", &repo_str])
        .output()
        .expect("run go");
    assert!(!go.status.success());
    let go_output = String::from_utf8(go.stderr).expect("go output is utf8");
    assert_eq!(parse_guidance_path(&go_output), expected_path);
}

#[cfg(unix)]
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(unix)]
fn install_identity_replacement_hook(
    repo: &fixtures::TestRepo,
    hook_name: &str,
    target: &std::path::Path,
    branch: &str,
    replacement_name: &str,
) {
    let repo_path = repo.path().display().to_string();
    let target_path = target.display().to_string();
    let script = format!(
        "#!/bin/sh\nset -eu\nunset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE GIT_OBJECT_DIRECTORY GIT_ALTERNATE_OBJECT_DIRECTORIES GIT_COMMON_DIR GIT_PREFIX\nrepo={}\ntarget={}\nreplacement=\"$target.{}\"\ngit -C \"$repo\" worktree remove --force \"$target\"\ngit -C \"$repo\" worktree add \"$replacement\" {}\nadmin=$(git -C \"$replacement\" rev-parse --git-dir)\nmv \"$replacement\" \"$target\"\nprintf '%s\\n' \"$target/.git\" > \"$admin/gitdir\"\n",
        shell_quote(&repo_path),
        shell_quote(&target_path),
        replacement_name,
        shell_quote(branch),
    );
    install_hook(repo, hook_name, &script);
}

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

/// Resolve a revision to its full commit ID.
fn git_rev_parse(repo: &std::path::Path, revision: &str) -> String {
    let mut cmd = StdCommand::new("git");
    cmd.args(["rev-parse", revision]).current_dir(repo);
    for var in GIT_ENV_OVERRIDES {
        cmd.env_remove(var);
    }
    let output = cmd.output().expect("git rev-parse failed");
    assert!(
        output.status.success(),
        "git rev-parse {revision} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("invalid utf8")
        .trim()
        .to_string()
}

/// Run Git and return its trimmed stdout for tests that need an object ID.
fn git_output(args: &[&str], cwd: &std::path::Path) -> String {
    let output = git_allow_failure(args, cwd);
    assert!(
        output.status.success(),
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("invalid utf8")
        .trim()
        .to_string()
}

/// Snapshot Git's per-worktree administrative entries without pruning them.
fn worktree_admin_snapshot(repo: &std::path::Path) -> Vec<(String, String)> {
    let admin_dir = repo.join(".git/worktrees");
    let Ok(entries) = std::fs::read_dir(admin_dir) else {
        return Vec::new();
    };

    let mut snapshot = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        let gitdir = std::fs::read_to_string(path.join("gitdir")).unwrap_or_default();
        let head = std::fs::read_to_string(path.join("HEAD")).unwrap_or_default();
        snapshot.push((name, format!("{gitdir}\0{head}")));
    }
    snapshot.sort();
    snapshot
}

/// Mark a worktree admin entry locked so Git's prune cannot silently remove
/// the stale record during a normal merge preflight.
fn lock_worktree_metadata(repo: &std::path::Path, worktree: &std::path::Path) {
    let admin_dir = repo.join(".git/worktrees");
    let worktree_prefix = worktree.display().to_string();
    for entry in std::fs::read_dir(&admin_dir)
        .expect("worktree admin directory")
        .flatten()
    {
        let path = entry.path();
        let gitdir = std::fs::read_to_string(path.join("gitdir")).unwrap_or_default();
        if gitdir.trim().starts_with(&worktree_prefix) {
            std::fs::write(path.join("locked"), "repair test").expect("lock worktree");
            return;
        }
    }
    panic!("no worktree admin entry for {}", worktree.display());
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
