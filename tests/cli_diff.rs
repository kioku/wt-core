mod fixtures;

use std::path::Path;

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

use fixtures::commit_file;

fn wt_core() -> Command {
    Command::new(assert_cmd::cargo_bin!("wt-core"))
}

struct IsolatedDifftoolEnv {
    _temp: TempDir,
    path: String,
    home: String,
    xdg_config_home: String,
}

impl IsolatedDifftoolEnv {
    fn new() -> Self {
        let temp = TempDir::new().expect("failed to create isolated git env");
        let bin = temp.path().join("bin");
        std::fs::create_dir(&bin).expect("failed to create isolated bin dir");
        link_git_binary(&bin);

        let home = temp.path().join("home");
        let xdg_config_home = temp.path().join("xdg");
        std::fs::create_dir(&home).expect("failed to create isolated home dir");
        std::fs::create_dir(&xdg_config_home).expect("failed to create isolated xdg dir");

        Self {
            path: bin.display().to_string(),
            home: home.display().to_string(),
            xdg_config_home: xdg_config_home.display().to_string(),
            _temp: temp,
        }
    }

    fn apply(&self, cmd: &mut Command) {
        cmd.env("PATH", &self.path)
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", &self.xdg_config_home)
            .env("GIT_CONFIG_GLOBAL", os_null_path())
            .env("GIT_CONFIG_NOSYSTEM", "1");
    }
}

fn isolated_wt_core(env: &IsolatedDifftoolEnv) -> Command {
    let mut cmd = wt_core();
    env.apply(&mut cmd);
    cmd
}

#[cfg(unix)]
fn link_git_binary(bin: &Path) {
    let git = std::process::Command::new("git")
        .arg("--exec-path")
        .output()
        .expect("failed to locate git");
    assert!(git.status.success(), "git --exec-path failed");

    let exec_path = String::from_utf8_lossy(&git.stdout).trim().to_string();
    std::os::unix::fs::symlink(Path::new(&exec_path).join("git"), bin.join("git"))
        .expect("failed to link git into isolated PATH");
}

#[cfg(not(unix))]
fn link_git_binary(_bin: &Path) {
    panic!("isolated difftool PATH tests require unix symlinks");
}

fn os_null_path() -> &'static str {
    if cfg!(windows) {
        "NUL"
    } else {
        "/dev/null"
    }
}

#[test]
fn diff_dry_run_explicit_branch_uses_mainline_three_dot_range() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    wt_core()
        .args(["add", "feature/diff", "--repo", &repo_str])
        .assert()
        .success();

    wt_core()
        .args(["diff", "feature/diff", "--repo", &repo_str, "--dry-run"])
        .assert()
        .success()
        .stdout(predicate::str::contains(format!(
            "git -C {} difftool",
            repo_str
        )))
        .stdout(predicate::str::contains("--dir-diff"))
        .stdout(predicate::str::contains("main...feature/diff"));
}

#[test]
fn diff_dry_run_respects_base_override_and_tool() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    wt_core()
        .args(["add", "feature/tool", "--repo", &repo_str])
        .assert()
        .success();

    wt_core()
        .args([
            "diff",
            "feature/tool",
            "--repo",
            &repo_str,
            "--against",
            "HEAD",
            "--tool",
            "vimdiff",
            "--dry-run",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("--tool vimdiff"))
        .stdout(predicate::str::contains("HEAD...feature/tool"));
}

#[test]
fn diff_non_dry_run_errors_before_launch_when_no_difftool_is_available() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();
    let env = IsolatedDifftoolEnv::new();

    wt_core()
        .args(["add", "feature/no-difftool", "--repo", &repo_str])
        .assert()
        .success();

    isolated_wt_core(&env)
        .args(["diff", "feature/no-difftool", "--repo", &repo_str])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "no git difftool is configured or available",
        ))
        .stderr(predicate::str::contains(
            "git config --global diff.tool nvimdiff",
        ))
        .stderr(predicate::str::contains("git difftool --tool-help"))
        .stderr(predicate::str::contains("difftool..cmd").not());
}

#[test]
fn diff_dirty_modes_error_before_launch_when_no_difftool_is_available() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();
    let env = IsolatedDifftoolEnv::new();

    wt_core()
        .args(["add", "feature/dirty-no-difftool", "--repo", &repo_str])
        .assert()
        .success();

    for mode in ["--dirty", "--staged", "--unstaged"] {
        isolated_wt_core(&env)
            .args([
                "diff",
                mode,
                "feature/dirty-no-difftool",
                "--repo",
                &repo_str,
            ])
            .assert()
            .failure()
            .stderr(predicate::str::contains(
                "no git difftool is configured or available",
            ));
    }
}

#[test]
fn diff_explicit_unusable_tool_returns_wt_core_error() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    wt_core()
        .args(["add", "feature/bad-tool", "--repo", &repo_str])
        .assert()
        .success();

    wt_core()
        .args([
            "diff",
            "feature/bad-tool",
            "--repo",
            &repo_str,
            "--tool",
            "definitely-not-a-wt-difftool",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "git difftool 'definitely-not-a-wt-difftool' is not configured or available",
        ))
        .stderr(predicate::str::contains("wt diff --tool nvimdiff <branch>"));
}

#[test]
fn diff_dry_run_and_print_command_bypass_missing_difftool_preflight() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();
    let env = IsolatedDifftoolEnv::new();

    wt_core()
        .args(["add", "feature/isolated-dry-run", "--repo", &repo_str])
        .assert()
        .success();

    isolated_wt_core(&env)
        .args([
            "diff",
            "feature/isolated-dry-run",
            "--repo",
            &repo_str,
            "--dry-run",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("git -C"))
        .stdout(predicate::str::contains("difftool"));

    isolated_wt_core(&env)
        .args([
            "diff",
            "--dirty",
            "feature/isolated-dry-run",
            "--repo",
            &repo_str,
            "--print-command",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("git -C"))
        .stdout(predicate::str::contains("difftool"));
}

#[test]
fn diff_errors_when_requested_branch_has_no_worktree() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    commit_file(&repo.path(), "local.txt", "local", "local branch base");
    fixtures::run_git(&["branch", "feature/no-worktree"], &repo.path());

    wt_core()
        .args([
            "diff",
            "feature/no-worktree",
            "--repo",
            &repo_str,
            "--dry-run",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "branch 'feature/no-worktree' has no associated worktree",
        ));
}

#[test]
fn diff_without_branch_errors_when_no_non_main_worktrees() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    wt_core()
        .args(["diff", "--repo", &repo_str, "--dry-run"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("no worktrees to diff"));
}

#[test]
fn diff_rejects_empty_tool_name() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    wt_core()
        .args(["add", "feature/empty-tool", "--repo", &repo_str])
        .assert()
        .success();

    wt_core()
        .args([
            "diff",
            "feature/empty-tool",
            "--repo",
            &repo_str,
            "--tool",
            "   ",
            "--dry-run",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--tool must not be empty"));
}

#[test]
fn diff_dirty_dry_run_explicit_branch_uses_worktree_path() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    wt_core()
        .args(["add", "feature/dirty", "--repo", &repo_str])
        .assert()
        .success();

    let worktree = fixtures::find_worktree_dir(&repo.path(), "feature-dirty");

    wt_core()
        .args([
            "diff",
            "--dirty",
            "feature/dirty",
            "--repo",
            &repo_str,
            "--dry-run",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(format!(
            "git -C {} difftool",
            worktree.display()
        )))
        .stdout(predicate::str::contains("--dir-diff HEAD"));
}

#[test]
fn diff_staged_and_unstaged_dry_run_construct_worktree_commands() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    wt_core()
        .args(["add", "feature/index", "--repo", &repo_str])
        .assert()
        .success();

    let worktree = fixtures::find_worktree_dir(&repo.path(), "feature-index");

    wt_core()
        .args([
            "diff",
            "--staged",
            "feature/index",
            "--repo",
            &repo_str,
            "--tool",
            "vimdiff",
            "--dry-run",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(format!(
            "git -C {} difftool --tool vimdiff --dir-diff --staged",
            worktree.display()
        )));

    wt_core()
        .args([
            "diff",
            "--unstaged",
            "feature/index",
            "--repo",
            &repo_str,
            "--dry-run",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(format!(
            "git -C {} difftool --dir-diff",
            worktree.display()
        )))
        .stdout(predicate::str::contains("--staged").not())
        .stdout(predicate::str::contains(" HEAD").not());
}

#[test]
fn diff_print_command_alias_prints_dirty_command() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    wt_core()
        .args(["add", "feature/print", "--repo", &repo_str])
        .assert()
        .success();

    wt_core()
        .args([
            "diff",
            "--dirty",
            "feature/print",
            "--repo",
            &repo_str,
            "--print-command",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("git -C"))
        .stdout(predicate::str::contains("difftool"));
}

#[test]
fn diff_dirty_errors_when_requested_branch_has_no_worktree() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    commit_file(&repo.path(), "local.txt", "local", "local branch base");
    fixtures::run_git(&["branch", "feature/no-dirty-worktree"], &repo.path());

    wt_core()
        .args([
            "diff",
            "--dirty",
            "feature/no-dirty-worktree",
            "--repo",
            &repo_str,
            "--dry-run",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "branch 'feature/no-dirty-worktree' has no associated worktree",
        ));
}

#[test]
fn diff_rejects_ambiguous_dirty_mode_flags() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    wt_core()
        .args([
            "diff",
            "--dirty",
            "--staged",
            "feature/ambiguous",
            "--repo",
            &repo_str,
            "--dry-run",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "--dirty, --staged, and --unstaged are mutually exclusive",
        ));
}

#[test]
fn diff_rejects_against_with_dirty_mode() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    wt_core()
        .args([
            "diff",
            "--dirty",
            "feature/against",
            "--against",
            "HEAD",
            "--repo",
            &repo_str,
            "--dry-run",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "--against can only be used with branch-vs-mainline diffs",
        ));
}
