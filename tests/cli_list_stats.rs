mod fixtures;

use std::path::Path;

use assert_cmd::Command;

fn wt_core() -> Command {
    Command::new(assert_cmd::cargo_bin!("wt-core"))
}

fn add_worktree(repo_path: &Path, branch: &str) -> String {
    let output = wt_core()
        .args([
            "add",
            branch,
            "--repo",
            &repo_path.display().to_string(),
            "--print-cd-path",
        ])
        .output()
        .expect("failed to run wt-core add");
    assert!(output.status.success());
    String::from_utf8(output.stdout)
        .expect("invalid utf8")
        .trim()
        .to_string()
}

fn list_json(repo_path: &Path, extra_args: &[&str]) -> serde_json::Value {
    let repo_arg = repo_path.display().to_string();
    let mut args = vec!["list", "--repo", &repo_arg, "--json"];
    args.extend_from_slice(extra_args);
    let output = wt_core()
        .args(args)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    serde_json::from_slice(&output).expect("invalid json")
}

fn worktree_entry<'a>(json: &'a serde_json::Value, branch: &str) -> &'a serde_json::Value {
    json["worktrees"]
        .as_array()
        .expect("worktrees array")
        .iter()
        .find(|entry| entry["branch"] == branch)
        .expect("worktree entry")
}

fn stats_for<'a>(json: &'a serde_json::Value, branch: &str) -> &'a serde_json::Value {
    &worktree_entry(json, branch)["stats"]
}

#[test]
fn list_stats_reports_ahead_only_and_diff_totals() {
    let repo = fixtures::TestRepo::new();
    let repo_path = repo.path();
    let wt_path = add_worktree(&repo_path, "feat-ahead");
    fixtures::commit_file(
        Path::new(&wt_path),
        "feature.txt",
        "one\ntwo\n",
        "feature commit",
    );

    let json = list_json(&repo_path, &["--stats"]);
    let stats = stats_for(&json, "feat-ahead");

    assert_eq!(stats["available"], true);
    assert_eq!(stats["base"], "main");
    assert_eq!(stats["commits_ahead"], 1);
    assert_eq!(stats["commits_behind"], 0);
    assert_eq!(stats["files_changed"], 1);
    assert_eq!(stats["insertions"], 2);
    assert_eq!(stats["deletions"], 0);
}

#[test]
fn list_stats_reports_behind_only() {
    let repo = fixtures::TestRepo::new();
    let repo_path = repo.path();
    add_worktree(&repo_path, "feat-behind");
    fixtures::commit_file(&repo_path, "main.txt", "main\n", "main commit");

    let json = list_json(&repo_path, &["--stats"]);
    let stats = stats_for(&json, "feat-behind");

    assert_eq!(stats["commits_ahead"], 0);
    assert_eq!(stats["commits_behind"], 1);
}

#[test]
fn list_stats_reports_ahead_and_behind() {
    let repo = fixtures::TestRepo::new();
    let repo_path = repo.path();
    let wt_path = add_worktree(&repo_path, "feat-diverged");
    fixtures::commit_file(
        Path::new(&wt_path),
        "feature.txt",
        "feature\n",
        "feature commit",
    );
    fixtures::commit_file(&repo_path, "main.txt", "main\n", "main commit");

    let json = list_json(&repo_path, &["--stats"]);
    let stats = stats_for(&json, "feat-diverged");

    assert_eq!(stats["commits_ahead"], 1);
    assert_eq!(stats["commits_behind"], 1);
}

#[test]
fn list_stats_reports_no_difference() {
    let repo = fixtures::TestRepo::new();
    let repo_path = repo.path();
    add_worktree(&repo_path, "feat-even");

    let json = list_json(&repo_path, &["--stats"]);
    let stats = stats_for(&json, "feat-even");

    assert_eq!(stats["commits_ahead"], 0);
    assert_eq!(stats["commits_behind"], 0);
    assert_eq!(stats["files_changed"], 0);
}

#[test]
fn list_stats_uses_against_override() {
    let repo = fixtures::TestRepo::new();
    let repo_path = repo.path();
    fixtures::run_git(&["branch", "base-point"], &repo_path);
    let wt_path = add_worktree(&repo_path, "feat-base-override");
    fixtures::commit_file(
        Path::new(&wt_path),
        "feature.txt",
        "feature\n",
        "feature commit",
    );
    fixtures::commit_file(&repo_path, "main.txt", "main\n", "main commit");

    let json = list_json(&repo_path, &["--stats", "--against", "base-point"]);
    let stats = stats_for(&json, "feat-base-override");

    assert_eq!(stats["base"], "base-point");
    assert_eq!(stats["commits_ahead"], 1);
    assert_eq!(stats["commits_behind"], 0);
}

#[test]
fn list_json_omits_stats_without_flag() {
    let repo = fixtures::TestRepo::new();
    let repo_path = repo.path();
    add_worktree(&repo_path, "feat-default");

    let json = list_json(&repo_path, &[]);
    let entry = worktree_entry(&json, "feat-default");

    assert!(entry.get("stats").is_none());
}

#[test]
fn list_stats_human_output_contains_stats_columns() {
    let repo = fixtures::TestRepo::new();
    let repo_path = repo.path();
    let wt_path = add_worktree(&repo_path, "feat-human");
    fixtures::commit_file(
        Path::new(&wt_path),
        "feature.txt",
        "feature\n",
        "feature commit",
    );

    wt_core()
        .args([
            "list",
            "--repo",
            &repo_path.display().to_string(),
            "--stats",
        ])
        .assert()
        .success()
        .stdout(predicates::str::contains("COMMITS"))
        .stdout(predicates::str::contains("+1"))
        .stdout(predicates::str::contains("+1 -0"));
}

#[test]
fn list_stats_counts_binary_numstat_rows_as_changed_files() {
    let repo = fixtures::TestRepo::new();
    let repo_path = repo.path();
    let wt_path = add_worktree(&repo_path, "feat-binary");
    std::fs::write(Path::new(&wt_path).join("image.bin"), [0, 159, 146, 150])
        .expect("write binary file");
    fixtures::run_git(&["add", "."], Path::new(&wt_path));
    fixtures::run_git(&["commit", "-m", "binary commit"], Path::new(&wt_path));

    let json = list_json(&repo_path, &["--stats"]);
    let stats = stats_for(&json, "feat-binary");

    assert_eq!(stats["files_changed"], 1);
    assert_eq!(stats["insertions"], 0);
    assert_eq!(stats["deletions"], 0);
}

#[test]
fn list_stats_rejects_invalid_against_revision() {
    let repo = fixtures::TestRepo::new();
    let repo_path = repo.path();

    wt_core()
        .args([
            "list",
            "--repo",
            &repo_path.display().to_string(),
            "--stats",
            "--against",
            "definitely-not-a-revision",
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "base revision 'definitely-not-a-revision' does not exist",
        ));
}

#[test]
fn list_stats_handles_detached_worktree_as_unavailable() {
    let repo = fixtures::TestRepo::new();
    let repo_path = repo.path();
    let detached_path = repo_path.join(".worktrees").join("detached");
    let detached_arg = detached_path.display().to_string();
    fixtures::run_git(
        &["worktree", "add", "--detach", &detached_arg, "HEAD"],
        &repo_path,
    );

    let json = list_json(&repo_path, &["--stats"]);
    let detached = json["worktrees"]
        .as_array()
        .expect("worktrees array")
        .iter()
        .find(|entry| entry["branch"].is_null())
        .expect("detached worktree");
    let stats = &detached["stats"];

    assert_eq!(stats["available"], false);
    assert_eq!(stats["reason"], "no_branch");
}
