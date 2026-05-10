mod fixtures;

use std::path::Path;

use assert_cmd::Command;
use predicates::prelude::*;

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

fn list_human(repo_path: &Path, extra_args: &[&str]) -> String {
    let repo_arg = repo_path.display().to_string();
    let mut args = vec!["list", "--repo", &repo_arg];
    args.extend_from_slice(extra_args);
    let output = wt_core()
        .args(args)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    String::from_utf8(output).expect("invalid utf8")
}

fn strip_stats_ansi(text: &str) -> String {
    text.replace("\x1b[32m", "")
        .replace("\x1b[31m", "")
        .replace("\x1b[0m", "")
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

fn column_start(line: &str, column: &str) -> usize {
    line.find(column)
        .unwrap_or_else(|| panic!("column '{column}' not found in '{line}'"))
}

fn char_column_start(line: &str, column: &str) -> usize {
    let byte_start = column_start(line, column);
    line[..byte_start].chars().count()
}

fn char_start_of_text(line: &str, text: &str) -> usize {
    let byte_start = line
        .find(text)
        .unwrap_or_else(|| panic!("text '{text}' not found in '{line}'"));
    line[..byte_start].chars().count()
}

#[test]
fn list_stats_human_output_dynamically_aligns_wide_stats_columns() {
    let repo = fixtures::TestRepo::new();
    let repo_path = repo.path();

    let long_branch = "2385/line-native-startup-continuation-extra-long";
    let long_wt_path = add_worktree(&repo_path, long_branch);
    let large_insertions = (0..1200)
        .map(|idx| format!("line {idx}\n"))
        .collect::<String>();
    fixtures::commit_file(
        Path::new(&long_wt_path),
        "large-feature.txt",
        &large_insertions,
        "large feature commit",
    );

    let diverged_branch = "diverged-large-counts";
    let diverged_wt_path = add_worktree(&repo_path, diverged_branch);
    fixtures::commit_file(
        Path::new(&diverged_wt_path),
        "feature.txt",
        "feature\n",
        "feature commit",
    );
    fixtures::commit_file(&repo_path, "main-1.txt", "main\n", "main commit 1");
    fixtures::commit_file(&repo_path, "main-2.txt", "main\n", "main commit 2");
    let base_branch = "base-with-a-long-name";
    fixtures::run_git(&["branch", base_branch], &repo_path);

    let detached_path = repo_path.join(".worktrees").join("detached-wide-stats");
    fixtures::run_git(
        &[
            "worktree",
            "add",
            "--detach",
            &detached_path.display().to_string(),
            "HEAD",
        ],
        &repo_path,
    );

    let base_name = base_branch;
    let plain = list_human(
        &repo_path,
        &["--stats", "--against", base_name, "--color", "never"],
    );
    let colored = list_human(
        &repo_path,
        &["--stats", "--against", base_name, "--color", "always"],
    );
    let colored_plain = strip_stats_ansi(&colored);

    assert_eq!(colored_plain, plain);

    let lines = plain.lines().collect::<Vec<_>>();
    let header = lines.first().expect("header line");
    let base_start = column_start(header, "BASE");
    let path_start = column_start(header, "PATH");
    let path_char_start = char_column_start(header, "PATH");
    let repo_prefix = repo_path.display().to_string();

    assert!(lines.iter().any(|line| line.contains("+1200 -0")));
    assert!(lines.iter().any(|line| line.contains("+1 -2")));
    assert!(lines.iter().any(|line| line.contains("unavailable")));

    for line in lines.iter().skip(1) {
        assert!(line[base_start..].starts_with(base_name), "{line}");
        assert!(line[..path_start].contains(base_name), "{line}");
        assert!(line.contains(&repo_prefix), "{line}");
        assert_eq!(
            char_start_of_text(line, &repo_prefix),
            path_char_start,
            "{line}"
        );
    }
}

#[test]
fn list_stats_color_always_colors_non_zero_signed_values() {
    let repo = fixtures::TestRepo::new();
    let repo_path = repo.path();
    let wt_path = add_worktree(&repo_path, "feat-color");
    fixtures::commit_file(&repo_path, "main.txt", "main\n", "main commit");
    fixtures::commit_file(
        Path::new(&wt_path),
        "README.md",
        "changed\n",
        "feature commit",
    );

    let output = list_human(&repo_path, &["--stats", "--color", "always"]);

    assert!(output.contains("\x1b[32m+1\x1b[0m"));
    assert!(output.contains("\x1b[31m-1\x1b[0m"));
    assert!(output.contains("\x1b[32m+1\x1b[0m \x1b[31m-1\x1b[0m"));
    let plain = strip_stats_ansi(&output);
    let row = plain
        .lines()
        .find(|line| line.starts_with("feat-color"))
        .expect("feat-color row");
    assert_eq!(
        row.split_whitespace().take(7).collect::<Vec<_>>(),
        ["feat-color", "main", "+1", "-1", "1", "+1", "-1"]
    );
}

#[test]
fn list_stats_color_never_and_auto_non_tty_do_not_emit_ansi() {
    let repo = fixtures::TestRepo::new();
    let repo_path = repo.path();
    let wt_path = add_worktree(&repo_path, "feat-plain");
    fixtures::commit_file(
        Path::new(&wt_path),
        "feature.txt",
        "feature\n",
        "feature commit",
    );

    let default_output = list_human(&repo_path, &["--stats"]);
    let never_output = list_human(&repo_path, &["--stats", "--color", "never"]);

    assert!(!default_output.contains("\x1b["));
    assert!(!never_output.contains("\x1b["));
}

#[test]
fn list_stats_no_color_disables_auto_but_not_always() {
    let repo = fixtures::TestRepo::new();
    let repo_path = repo.path();
    let wt_path = add_worktree(&repo_path, "feat-no-color");
    fixtures::commit_file(
        Path::new(&wt_path),
        "feature.txt",
        "feature\n",
        "feature commit",
    );
    let repo_arg = repo_path.display().to_string();

    wt_core()
        .args(["list", "--repo", &repo_arg, "--stats", "--color", "auto"])
        .env("NO_COLOR", "1")
        .assert()
        .success()
        .stdout(predicates::str::contains("\x1b[").not());

    wt_core()
        .args(["list", "--repo", &repo_arg, "--stats", "--color", "always"])
        .env("NO_COLOR", "1")
        .assert()
        .success()
        .stdout(predicates::str::contains("\x1b[32m+1\x1b[0m"));
}

#[test]
fn list_stats_does_not_color_zero_unavailable_or_json() {
    let repo = fixtures::TestRepo::new();
    let repo_path = repo.path();
    add_worktree(&repo_path, "feat-zero");
    let detached_path = repo_path.join(".worktrees").join("detached-color");
    fixtures::run_git(
        &[
            "worktree",
            "add",
            "--detach",
            &detached_path.display().to_string(),
            "HEAD",
        ],
        &repo_path,
    );

    let human = list_human(&repo_path, &["--stats", "--color", "always"]);
    let json = list_json(&repo_path, &["--stats", "--color", "always"]);
    let json_text = serde_json::to_string(&json).expect("json text");

    let zero_row = strip_stats_ansi(&human)
        .lines()
        .find(|line| line.starts_with("main") || line.starts_with("feat-zero"))
        .expect("zero stats row")
        .to_string();
    assert!(zero_row.contains(" 0"));
    assert!(zero_row.contains("+0 -0"));
    assert!(human.contains("unavailable"));
    assert!(!human.contains("\x1b[32m+0"));
    assert!(!human.contains("\x1b[31m-0"));
    assert!(!json_text.contains("\x1b["));
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
