mod fixtures;

use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;

use assert_cmd::Command;
use predicates::prelude::*;

const GIT_ENV_OVERRIDES: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_PREFIX",
];

fn wt_core() -> Command {
    Command::new(assert_cmd::cargo_bin!("wt-core"))
}

fn git_output(args: &[&str], cwd: &Path) -> String {
    let output = git_command(args, cwd).output().expect("failed to run git");
    assert!(
        output.status.success(),
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("git stdout was not utf8")
        .trim()
        .to_string()
}

fn git_success(args: &[&str], cwd: &Path) -> bool {
    git_command(args, cwd)
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn git_command(args: &[&str], cwd: &Path) -> StdCommand {
    let mut command = StdCommand::new("git");
    command.args(args).current_dir(cwd);
    for var in GIT_ENV_OVERRIDES {
        command.env_remove(var);
    }
    command
}

fn file_url(path: &Path) -> String {
    format!("file://{}", path.display())
}

fn materialize_cached(
    repo: &fixtures::ClonedTestRepo,
    sha: &str,
    cache_root: &Path,
    workspace_root: &Path,
) -> serde_json::Value {
    let output = wt_core()
        .arg("materialize")
        .arg("--repo-slug")
        .arg("owner/repo")
        .arg("--remote-url")
        .arg(file_url(&repo.origin_path()))
        .arg("--ref")
        .arg("refs/heads/main")
        .arg("--sha")
        .arg(sha)
        .arg("--cache-root")
        .arg(cache_root)
        .arg("--workspace-root")
        .arg(workspace_root)
        .arg("--json")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    serde_json::from_slice(&output).expect("invalid materialize json")
}

#[test]
fn materialize_help_documents_contract() {
    wt_core()
        .args(["materialize", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("detached checkout"))
        .stdout(predicate::str::contains("bare mirror cache"))
        .stdout(predicate::str::contains("JSON output"))
        .stdout(predicate::str::contains("--object-source"));
}

#[test]
fn materialize_remote_with_cache_json_creates_detached_clean_workspace() {
    let repo = fixtures::ClonedTestRepo::new();
    let sha = git_output(&["rev-parse", "HEAD"], &repo.path());
    let root = tempfile::tempdir().expect("temp dir");
    let cache_root = root.path().join("cache");
    let workspace_one = root.path().join("workspace-one");
    let workspace_two = root.path().join("workspace-two");

    let first = materialize_cached(&repo, &sha, &cache_root, &workspace_one);
    assert_eq!(first["ok"], true);
    assert_eq!(first["repository"], "owner/repo");
    assert_eq!(first["workspace_path"], workspace_one.display().to_string());
    assert_eq!(
        first["cache_path"],
        cache_root.join("owner__repo.git").display().to_string()
    );
    assert_eq!(first["requested_ref"], "refs/heads/main");
    assert_eq!(first["requested_sha"], sha);
    assert_eq!(first["resolved_commit"], sha);
    assert_eq!(first["mode"], "detached");
    assert_eq!(first["cache_status"], "cold");
    assert_eq!(first["source"], "cache");
    assert!(first["timings_ms"]["total"].is_number());

    let second = materialize_cached(&repo, &sha, &cache_root, &workspace_two);
    assert_eq!(second["cache_status"], "refreshed");
    assert!(cache_root.join("owner__repo.git").is_dir());

    assert_eq!(git_output(&["rev-parse", "HEAD"], &workspace_one), sha);
    assert_eq!(git_output(&["status", "--porcelain"], &workspace_one), "");
    assert!(!git_success(
        &["symbolic-ref", "-q", "HEAD"],
        &workspace_one
    ));
}

#[test]
fn materialize_from_object_source_without_permanent_alternates() {
    let repo = fixtures::ClonedTestRepo::new();
    let sha = git_output(&["rev-parse", "HEAD"], &repo.path());
    let root = tempfile::tempdir().expect("temp dir");
    let workspace = root.path().join("workspace");

    let output = wt_core()
        .arg("materialize")
        .arg("--repo-slug")
        .arg("owner/repo")
        .arg("--object-source")
        .arg(repo.origin_path())
        .arg("--sha")
        .arg(&sha)
        .arg("--workspace-root")
        .arg(&workspace)
        .arg("--json")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let json: serde_json::Value = serde_json::from_slice(&output).expect("invalid json");

    assert_eq!(json["source"], "object_source");
    assert_eq!(json["cache_status"], "bypassed");
    assert_eq!(git_output(&["rev-parse", "HEAD"], &workspace), sha);
    assert!(!workspace.join(".git/objects/info/alternates").exists());
}

#[test]
fn materialize_rejects_existing_non_empty_workspace() {
    let repo = fixtures::ClonedTestRepo::new();
    let sha = git_output(&["rev-parse", "HEAD"], &repo.path());
    let root = tempfile::tempdir().expect("temp dir");
    let workspace = root.path().join("workspace");
    std::fs::create_dir(&workspace).expect("create workspace");
    std::fs::write(workspace.join("file.txt"), "occupied").expect("write file");

    wt_core()
        .arg("materialize")
        .arg("--repo-slug")
        .arg("owner/repo")
        .arg("--object-source")
        .arg(repo.origin_path())
        .arg("--sha")
        .arg(sha)
        .arg("--workspace-root")
        .arg(workspace)
        .assert()
        .failure()
        .code(5)
        .stderr(predicate::str::contains("not empty"));
}

#[test]
fn materialize_rejects_unsafe_repo_slugs_before_git() {
    let root = tempfile::tempdir().expect("temp dir");
    let valid_sha = "0123456789abcdef0123456789abcdef01234567";

    for slug in ["owner/repo/sub", "owner/re po", "owner/repo?", "../repo"] {
        wt_core()
            .arg("materialize")
            .arg("--repo-slug")
            .arg(slug)
            .arg("--object-source")
            .arg(root.path().join("missing.git"))
            .arg("--sha")
            .arg(valid_sha)
            .arg("--workspace-root")
            .arg(root.path().join("workspace"))
            .assert()
            .failure()
            .code(1)
            .stderr(predicate::str::contains("invalid --repo-slug"));
    }
}

#[test]
fn materialize_rejects_invalid_sha_and_unsafe_ref_before_git() {
    let repo = fixtures::ClonedTestRepo::new();
    let sha = git_output(&["rev-parse", "HEAD"], &repo.path());
    let root = tempfile::tempdir().expect("temp dir");

    wt_core()
        .arg("materialize")
        .arg("--repo-slug")
        .arg("owner/repo")
        .arg("--object-source")
        .arg(repo.origin_path())
        .arg("--sha")
        .arg("abc123")
        .arg("--workspace-root")
        .arg(root.path().join("bad-sha"))
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("40-character"));

    for (ref_name, workspace_name) in [
        ("refs/heads/main..evil", "bad-ref-dotdot"),
        ("refs/heads/.hidden", "bad-ref-hidden"),
        ("refs/heads/main.", "bad-ref-trailing-dot"),
        ("@", "bad-ref-at"),
    ] {
        let workspace = root.path().join(workspace_name);
        wt_core()
            .arg("materialize")
            .arg("--repo-slug")
            .arg("owner/repo")
            .arg("--remote-url")
            .arg(file_url(&repo.origin_path()))
            .arg("--ref")
            .arg(ref_name)
            .arg("--sha")
            .arg(&sha)
            .arg("--workspace-root")
            .arg(&workspace)
            .assert()
            .failure()
            .code(1)
            .stderr(predicate::str::contains("invalid --ref"));
        assert!(
            !workspace.exists(),
            "validation should happen before git setup"
        );
    }
}

#[test]
fn materialize_rejects_credential_remote_url_without_leaking_secret() {
    let root = tempfile::tempdir().expect("temp dir");
    let stderr = wt_core()
        .arg("materialize")
        .arg("--repo-slug")
        .arg("owner/repo")
        .arg("--remote-url")
        .arg("https://user:secret@example.invalid/repo.git")
        .arg("--sha")
        .arg("0123456789abcdef0123456789abcdef01234567")
        .arg("--workspace-root")
        .arg(root.path().join("workspace"))
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("must not include credentials"))
        .get_output()
        .stderr
        .clone();
    let stderr = String::from_utf8(stderr).expect("stderr utf8");
    assert!(!stderr.contains("secret"));
}

#[test]
fn concurrent_cached_materializations_share_cache_safely() {
    let repo = fixtures::ClonedTestRepo::new();
    let sha = git_output(&["rev-parse", "HEAD"], &repo.path());
    let root = tempfile::tempdir().expect("temp dir");
    let cache_root = root.path().join("cache");
    let workspace_one = root.path().join("workspace-one");
    let workspace_two = root.path().join("workspace-two");
    let origin_url = file_url(&repo.origin_path());

    let first = spawn_materialize(&sha, &origin_url, &cache_root, &workspace_one);
    let second = spawn_materialize(&sha, &origin_url, &cache_root, &workspace_two);
    first.join().expect("first thread panicked");
    second.join().expect("second thread panicked");

    assert_eq!(git_output(&["rev-parse", "HEAD"], &workspace_one), sha);
    assert_eq!(git_output(&["rev-parse", "HEAD"], &workspace_two), sha);
    assert!(cache_root.join("owner__repo.git").is_dir());
}

fn spawn_materialize(
    sha: &str,
    origin_url: &str,
    cache_root: &Path,
    workspace_root: &Path,
) -> std::thread::JoinHandle<()> {
    let sha = sha.to_string();
    let origin_url = origin_url.to_string();
    let cache_root = PathBuf::from(cache_root);
    let workspace_root = PathBuf::from(workspace_root);

    std::thread::spawn(move || {
        wt_core()
            .arg("materialize")
            .arg("--repo-slug")
            .arg("owner/repo")
            .arg("--remote-url")
            .arg(origin_url)
            .arg("--ref")
            .arg("refs/heads/main")
            .arg("--sha")
            .arg(sha)
            .arg("--cache-root")
            .arg(cache_root)
            .arg("--workspace-root")
            .arg(workspace_root)
            .assert()
            .success();
    })
}
