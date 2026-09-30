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
    "GIT_COMMON_DIR",
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
fn materialize_sanitizes_inherited_git_config_injection() {
    let repo = fixtures::ClonedTestRepo::new();
    let sha = git_output(&["rev-parse", "HEAD"], &repo.path());
    let root = tempfile::tempdir().expect("temp dir");
    let output = wt_core()
        .args([
            "materialize",
            "--repo-slug",
            "owner/repo",
            "--remote-url",
            &file_url(&repo.origin_path()),
            "--ref",
            "refs/heads/main",
            "--sha",
            &sha,
            "--cache-root",
            &root.path().join("cache").display().to_string(),
            "--workspace-root",
            &root.path().join("workspace").display().to_string(),
            "--json",
        ])
        .env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", "protocol.file.allow")
        .env("GIT_CONFIG_VALUE_0", "never")
        .env("GIT_CONFIG_PARAMETERS", "'protocol.file.allow=never'")
        .output()
        .expect("materialize should run");
    assert!(
        output.status.success(),
        "config injection leaked into materialize: {}",
        String::from_utf8_lossy(&output.stderr)
    );
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
    git_output(
        &["push", "origin", "HEAD:refs/heads/obsolete"],
        &repo.path(),
    );
    let initial = root.path().join("initial");
    materialize_cached(&repo, &sha, &cache_root, &initial);
    git_output(&["push", "origin", ":refs/heads/obsolete"], &repo.path());

    let first = spawn_materialize(&sha, &origin_url, &cache_root, &workspace_one);
    let second = spawn_materialize(&sha, &origin_url, &cache_root, &workspace_two);
    first.join().expect("first thread panicked");
    second.join().expect("second thread panicked");

    assert_checkout(&workspace_one, &sha);
    assert_checkout(&workspace_two, &sha);
    let cache = cache_root.join("owner__repo.git");
    assert!(!git_success(
        &["show-ref", "--verify", "refs/heads/obsolete"],
        &cache
    ));
    #[cfg(unix)]
    assert_independent_files(&cache, &workspace_one.join(".git"));
    assert!(!cache_root.join("owner__repo.git.lock").exists());
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

fn materialize_object(source: &Path, sha: &str, workspace: &Path) -> Command {
    let mut command = wt_core();
    command
        .args([
            "materialize",
            "--repo-slug",
            "owner/repo",
            "--object-source",
        ])
        .arg(source)
        .args(["--sha", sha, "--workspace-root"])
        .arg(workspace);
    command
}

fn assert_checkout(workspace: &Path, sha: &str) {
    assert_eq!(git_output(&["rev-parse", "HEAD"], workspace), sha);
    assert_eq!(git_output(&["status", "--porcelain"], workspace), "");
    assert!(!git_success(&["symbolic-ref", "-q", "HEAD"], workspace));
    assert!(!workspace.join(".git/objects/info/alternates").exists());
    git_output(&["fsck", "--full"], workspace);
}

#[cfg(unix)]
fn assert_independent_files(source: &Path, destination: &Path) {
    use std::os::unix::fs::MetadataExt;
    for entry in std::fs::read_dir(destination).expect("read directory") {
        let entry = entry.expect("directory entry");
        let src = source.join(entry.file_name());
        let dst = entry.path();
        let metadata = std::fs::symlink_metadata(&dst).expect("metadata");
        if metadata.is_dir() {
            assert_independent_files(&src, &dst);
        } else if metadata.is_file() && src.is_file() {
            let original = std::fs::metadata(&src).expect("source metadata");
            assert_ne!(
                (metadata.dev(), metadata.ino()),
                (original.dev(), original.ino())
            );
            assert_eq!(metadata.nlink(), 1, "shared file: {}", dst.display());
        }
    }
}

#[test]
fn local_copy_owns_loose_and_packed_objects_and_metadata_after_source_pruning() {
    for packed in [false, true] {
        let repo = fixtures::ClonedTestRepo::new();
        let sha = git_output(&["rev-parse", "HEAD"], &repo.path());
        fixtures::commit_file(&repo.path(), "new.txt", "new head", "new commit");
        git_output(&["push", "origin", "main"], &repo.path());
        let root = tempfile::tempdir().expect("temp dir");
        let source = root.path().join("source.git");
        git_output(
            &[
                "clone",
                "--bare",
                "--no-hardlinks",
                &repo.origin_path().display().to_string(),
                &source.display().to_string(),
            ],
            root.path(),
        );
        if packed {
            git_output(&["repack", "-ad"], &source);
        }
        let workspace = root.path().join("workspace");
        // Existing empty destinations remain supported.
        std::fs::create_dir(&workspace).expect("empty workspace");
        materialize_object(&source, &sha, &workspace)
            .assert()
            .success();
        assert_checkout(&workspace, &sha);
        #[cfg(unix)]
        assert_independent_files(&source, &workspace.join(".git"));
        let original_config = std::fs::read(source.join("config")).expect("source config");
        git_output(&["config", "materialize.test", "changed"], &workspace);
        assert_eq!(
            std::fs::read(source.join("config")).expect("source config"),
            original_config
        );
        git_output(&["update-ref", "-d", "refs/heads/main"], &source);
        git_output(&["reflog", "expire", "--expire=now", "--all"], &source);
        git_output(&["gc", "--prune=now"], &source);
        assert_checkout(&workspace, &sha);
        std::fs::remove_dir_all(&source).expect("remove private source");
        assert_checkout(&workspace, &sha);
    }
}

#[test]
fn local_copy_accepts_empty_destination_with_trailing_dot() {
    let repo = fixtures::ClonedTestRepo::new();
    let sha = git_output(&["rev-parse", "HEAD"], &repo.path());
    let root = tempfile::tempdir().expect("temp dir");
    let workspace = root.path().join("workspace");
    std::fs::create_dir(&workspace).expect("empty workspace");
    let spelling = PathBuf::from(format!("{}/.", workspace.display()));
    materialize_object(&repo.origin_path(), &sha, &spelling)
        .assert()
        .success();
    assert_checkout(&workspace, &sha);
    assert_no_staging(root.path());
}

#[cfg(unix)]
#[test]
fn local_copy_preserves_private_empty_destination_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let repo = fixtures::ClonedTestRepo::new();
    let sha = git_output(&["rev-parse", "HEAD"], &repo.path());
    let root = tempfile::tempdir().expect("temp dir");
    let workspace = root.path().join("workspace");
    std::fs::create_dir(&workspace).expect("empty workspace");
    std::fs::set_permissions(&workspace, std::fs::Permissions::from_mode(0o700))
        .expect("private workspace");
    materialize_object(&repo.origin_path(), &sha, &workspace)
        .assert()
        .success();
    assert_eq!(
        std::fs::metadata(&workspace)
            .expect("workspace metadata")
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert_checkout(&workspace, &sha);
    assert_no_staging(root.path());
}

#[cfg(unix)]
#[test]
fn local_copy_preserves_empty_destination_group() {
    use std::os::unix::fs::{chown, MetadataExt, PermissionsExt};
    let repo = fixtures::ClonedTestRepo::new();
    let sha = git_output(&["rev-parse", "HEAD"], &repo.path());
    let root = tempfile::tempdir().expect("temp dir");
    let workspace = root.path().join("workspace");
    std::fs::create_dir(&workspace).expect("empty workspace");
    let original_group = std::fs::metadata(&workspace).expect("metadata").gid();
    let groups = StdCommand::new("id")
        .arg("-G")
        .output()
        .expect("list groups");
    assert!(groups.status.success());
    let Some(group) = String::from_utf8(groups.stdout)
        .expect("group ids")
        .split_whitespace()
        .filter_map(|group| group.parse::<u32>().ok())
        .find(|group| *group != original_group)
    else {
        // Changing directory groups requires membership in another group.
        return;
    };
    chown(&workspace, None, Some(group)).expect("set workspace group");
    std::fs::set_permissions(&workspace, std::fs::Permissions::from_mode(0o770))
        .expect("group-private workspace");
    materialize_object(&repo.origin_path(), &sha, &workspace)
        .assert()
        .success();
    let metadata = std::fs::metadata(&workspace).expect("workspace metadata");
    assert_eq!(metadata.gid(), group);
    assert_eq!(metadata.permissions().mode() & 0o777, 0o770);
    assert_checkout(&workspace, &sha);
    assert_no_staging(root.path());
}

#[test]
fn local_copy_rejects_alternates_without_leaving_workspace_or_staging() {
    let repo = fixtures::ClonedTestRepo::new();
    let sha = git_output(&["rev-parse", "HEAD"], &repo.path());
    let root = tempfile::tempdir().expect("temp dir");
    let source = root.path().join("shared.git");
    git_output(
        &[
            "clone",
            "--bare",
            "--shared",
            &repo.origin_path().display().to_string(),
            &source.display().to_string(),
        ],
        root.path(),
    );
    let workspace = root.path().join("workspace");
    materialize_object(&source, &sha, &workspace)
        .assert()
        .failure()
        .stderr(predicate::str::contains("alternates"));
    assert!(!workspace.exists());
    assert_no_staging(root.path());
}

fn assert_no_staging(parent: &Path) {
    assert!(std::fs::read_dir(parent)
        .expect("read parent")
        .all(|entry| !entry
            .expect("entry")
            .file_name()
            .to_string_lossy()
            .starts_with(".wt-materialize-")));
}

#[test]
fn local_checkout_failure_preserves_empty_destination_and_cleans_staging() {
    let repo = fixtures::ClonedTestRepo::new();
    let sha = git_output(&["rev-parse", "HEAD"], &repo.path()).to_uppercase();
    let root = tempfile::tempdir().expect("temp dir");
    for existed in [false, true] {
        let workspace = root.path().join(if existed { "empty" } else { "missing" });
        if existed {
            std::fs::create_dir(&workspace).expect("empty workspace");
        }
        // Git accepts uppercase full SHAs, but the existing exact-SHA contract
        // rejects the lowercase resolved HEAD. This fails after clone/checkout.
        materialize_object(&repo.origin_path(), &sha, &workspace)
            .assert()
            .failure()
            .code(4);
        assert_eq!(workspace.exists(), existed);
        if existed {
            assert_eq!(
                std::fs::read_dir(&workspace)
                    .expect("empty workspace")
                    .count(),
                0
            );
        }
        assert_no_staging(root.path());
    }
}

#[cfg(unix)]
#[test]
fn local_copy_rejects_symlink_source_and_object_paths() {
    use std::os::unix::fs::symlink;
    let repo = fixtures::ClonedTestRepo::new();
    let sha = git_output(&["rev-parse", "HEAD"], &repo.path());
    let root = tempfile::tempdir().expect("temp dir");
    let source_link = root.path().join("source.git");
    symlink(repo.origin_path(), &source_link).expect("source symlink");
    let workspace = root.path().join("workspace");
    for suffix in ["", "/", "/."] {
        let spelling = PathBuf::from(format!("{}{suffix}", source_link.display()));
        materialize_object(&spelling, &sha, &workspace)
            .assert()
            .failure()
            .stderr(predicate::str::contains("non-symlink directory"));
        assert!(!workspace.exists());
        assert_no_staging(root.path());
    }
    let objects = repo.origin_path().join("objects");
    let moved = repo.origin_path().join("original-objects");
    std::fs::rename(&objects, &moved).expect("move objects");
    symlink(&moved, &objects).expect("objects symlink");
    materialize_object(&repo.origin_path(), &sha, &workspace)
        .assert()
        .failure();
    assert!(!workspace.exists());
    assert_no_staging(root.path());
}

#[test]
fn remote_only_materialization_keeps_detached_clean_contract() {
    let repo = fixtures::ClonedTestRepo::new();
    let sha = git_output(&["rev-parse", "HEAD"], &repo.path());
    let root = tempfile::tempdir().expect("temp dir");
    let workspace = root.path().join("workspace");
    let output = wt_core()
        .args(["materialize", "--repo-slug", "owner/repo", "--remote-url"])
        .arg(file_url(&repo.origin_path()))
        .args(["--sha", &sha, "--workspace-root"])
        .arg(&workspace)
        .arg("--json")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let json: serde_json::Value = serde_json::from_slice(&output).expect("json");
    assert_eq!(json["source"], "remote");
    assert_eq!(json["cache_status"], "bypassed");
    assert_checkout(&workspace, &sha);
}
