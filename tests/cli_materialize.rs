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
fn materialize_large_checkout_preserves_older_sha_and_file_modes() {
    let repo = fixtures::ClonedTestRepo::new();
    // Assert exact LF bytes on every platform, independently of the runner
    // Git installation's core.autocrlf default. Checkout must honor attributes.
    std::fs::write(repo.path().join(".gitattributes"), "* text eol=lf\n")
        .expect("write checkout attributes");
    let nested = repo.path().join("nested");
    std::fs::create_dir(&nested).expect("create nested directory");
    // Exceed Git's default checkout and status parallelism thresholds.
    for index in 0..1_200 {
        std::fs::write(
            nested.join(format!("file-{index}.txt")),
            format!("content {index}\n"),
        )
        .expect("write checkout fixture");
    }
    git_output(&["add", "."], &repo.path());
    git_output(
        &["update-index", "--chmod=+x", "nested/file-0.txt"],
        &repo.path(),
    );
    git_output(&["commit", "-m", "large checkout fixture"], &repo.path());
    let sha = git_output(&["rev-parse", "HEAD"], &repo.path());
    fixtures::commit_file(&repo.path(), "later.txt", "not requested", "later commit");
    git_output(&["push", "origin", "main"], &repo.path());

    let root = tempfile::tempdir().expect("temp dir");
    for mode in ["object_source", "cache", "remote"] {
        let workspace = root.path().join(mode);
        let mut command = wt_core();
        command.args(["materialize", "--repo-slug", "owner/repo", "--sha", &sha]);
        if mode == "object_source" {
            command.arg("--object-source").arg(repo.origin_path());
        } else {
            command
                .arg("--remote-url")
                .arg(file_url(&repo.origin_path()));
        }
        if mode == "cache" {
            command
                .arg("--cache-root")
                .arg(root.path().join("cache-root"));
        }
        command
            .arg("--workspace-root")
            .arg(&workspace)
            .arg("--json")
            .assert()
            .success();
        assert_eq!(git_output(&["rev-parse", "HEAD"], &workspace), sha);
        assert_eq!(git_output(&["status", "--porcelain"], &workspace), "");
        assert!(!git_success(&["symbolic-ref", "-q", "HEAD"], &workspace));
        assert!(!workspace.join("later.txt").exists());
        assert!(!workspace.join(".git/objects/info/alternates").exists());
        // The optimization must not persist a repository config override.
        assert!(!git_success(
            &["config", "--local", "--get", "checkout.workers"],
            &workspace
        ));
        assert!(
            git_output(&["ls-files", "--stage", "nested/file-0.txt"], &workspace)
                .starts_with("100755 ")
        );
        for index in 0..1_200 {
            assert_eq!(
                std::fs::read_to_string(workspace.join(format!("nested/file-{index}.txt")))
                    .expect("read materialized file"),
                format!("content {index}\n")
            );
        }
    }
}

#[cfg(unix)]
#[test]
fn parallel_materialize_status_rejects_dirty_paths_and_worker_failures() {
    use std::os::unix::fs::PermissionsExt;

    let repo = fixtures::ClonedTestRepo::new();
    for index in 0..1_000 {
        std::fs::write(
            repo.path().join(format!("file-{index}")),
            "clean contents\n",
        )
        .expect("write large fixture");
    }
    std::fs::create_dir(repo.path().join("nested")).expect("nested directory");
    let paths = [
        "alpha",
        "fox",
        "november",
        "sierra",
        ".hidden",
        "123",
        "Éclair",
        "nested/é\nfile",
    ];
    for path in paths {
        std::fs::write(repo.path().join(path), "clean contents\n").expect("write probe");
    }
    git_output(&["add", "."], &repo.path());
    git_output(&["commit", "-m", "large dirty-state fixture"], &repo.path());
    git_output(&["push", "origin", "main"], &repo.path());
    let sha = git_output(&["rev-parse", "HEAD"], &repo.path());
    let real_git = StdCommand::new("sh")
        .args(["-c", "command -v git"])
        .output()
        .expect("locate git");
    assert!(real_git.status.success());
    let real_git = String::from_utf8(real_git.stdout).expect("git path");
    let root = tempfile::tempdir().expect("temp dir");
    let bin = root.path().join("bin");
    std::fs::create_dir(&bin).expect("wrapper directory");
    let wrapper = bin.join("git");
    std::fs::write(
        &wrapper,
        r#"#!/bin/sh
if [ "$1" = rev-parse ] && [ "$2" = HEAD ] && [ ! -e "$WT_MUTATED" ]; then
    touch "$WT_MUTATED" || exit 90
    case "$WT_PROBE_MODE" in
        edit)
            cp -p "$WT_WORKSPACE/$WT_PROBE_PATH" "$WT_TIMESTAMP" || exit 91
            printf 'dirty contents\n' > "$WT_WORKSPACE/$WT_PROBE_PATH" || exit 92
            touch -r "$WT_TIMESTAMP" "$WT_WORKSPACE/$WT_PROBE_PATH" || exit 93
            ;;
        untracked)
            "$WT_REAL_GIT" -C "$WT_WORKSPACE" config status.showUntrackedFiles no || exit 94
            printf 'untracked' > "$WT_WORKSPACE/$WT_PROBE_PATH-new" || exit 94
            ;;
        delete) rm "$WT_WORKSPACE/$WT_PROBE_PATH" || exit 95 ;;
        staged) "$WT_REAL_GIT" -C "$WT_WORKSPACE" mv alpha sierra-staged || exit 96 ;;
        symlink) rm "$WT_WORKSPACE/alpha" && ln -s fox "$WT_WORKSPACE/alpha" || exit 97 ;;
    esac
fi
if [ "$1" = symbolic-ref ] && [ "$WT_PROBE_MODE" = detached_failure ]; then
    echo 'injected detached check failure' >&2
    exit 99
fi
if [ "$1" = --no-optional-locks ]; then
    printf 'worker\n' >> "$WT_WORKERS" || exit 98
    if [ "$WT_PROBE_MODE" = failure ]; then
        echo 'injected status worker failure' >&2
        exit 99
    fi
fi
exec "$WT_REAL_GIT" "$@"
"#,
    )
    .expect("write wrapper");
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755))
        .expect("executable wrapper");
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").expect("PATH"));
    let probes: Vec<_> = paths
        .iter()
        .flat_map(|path| [("edit", *path), ("untracked", *path)])
        .chain([
            ("delete", "alpha"),
            ("staged", "alpha"),
            ("symlink", "alpha"),
            ("failure", "alpha"),
            ("detached_failure", "alpha"),
        ])
        .collect();
    for (index, (mode, probe_path)) in probes.into_iter().enumerate() {
        let workspace = root.path().join(format!("workspace-{index}"));
        let workers = root.path().join(format!("workers-{index}"));
        let mut command = materialize_object(&repo.origin_path(), &sha, &workspace);
        let pathspec_override = [
            "GIT_LITERAL_PATHSPECS",
            "GIT_GLOB_PATHSPECS",
            "GIT_NOGLOB_PATHSPECS",
            "GIT_ICASE_PATHSPECS",
        ][index % 4];
        command
            .env("PATH", &path)
            .env(pathspec_override, "1")
            .env("WT_REAL_GIT", real_git.trim())
            .env("WT_WORKSPACE", &workspace)
            .env("WT_PROBE_PATH", probe_path)
            .env("WT_PROBE_MODE", mode)
            .env("WT_MUTATED", root.path().join(format!("mutated-{index}")))
            .env(
                "WT_TIMESTAMP",
                root.path().join(format!("timestamp-{index}")),
            )
            .env("WT_WORKERS", &workers);
        let assertion = command.assert().failure();
        if mode == "failure" {
            assertion.stderr(predicate::str::contains("injected status worker failure"));
        } else if mode == "detached_failure" {
            assertion.stderr(predicate::str::contains("injected detached check failure"));
        } else {
            assertion.stderr(predicate::str::contains("not clean"));
        }
        assert_eq!(
            std::fs::read_to_string(workers)
                .expect("worker log")
                .lines()
                .count(),
            4
        );
        assert!(!workspace.exists(), "failed owned checkout must be cleaned");
    }
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
    assert!(cache_root.join("owner__repo.git.lock").is_file());
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
    for (packed, existed, copy_mode) in [
        (false, false, "auto"),
        (false, true, "auto"),
        (true, false, "auto"),
        (true, true, "auto"),
        (false, false, "copy"),
        (false, true, "copy"),
        (true, false, "copy"),
        (true, true, "copy"),
    ] {
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
        if existed {
            std::fs::create_dir(&workspace).expect("empty workspace");
        }
        materialize_object(&source, &sha, &workspace)
            .args(["--copy-mode", copy_mode])
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

#[cfg(target_os = "linux")]
#[test]
fn local_copy_preserves_empty_destination_access_and_default_acls() {
    use std::os::unix::fs::MetadataExt;
    let repo = fixtures::ClonedTestRepo::new();
    let sha = git_output(&["rev-parse", "HEAD"], &repo.path());
    // The normal test filesystem may have ACLs disabled; Linux tmpfs supports
    // them, so exercise the security regression there when available.
    let root = tempfile::tempdir_in("/dev/shm")
        .or_else(|_| tempfile::tempdir())
        .expect("temp dir");
    let workspace = root.path().join("workspace");
    std::fs::create_dir(&workspace).expect("empty workspace");
    let owner = std::fs::metadata(&workspace).expect("metadata").uid();
    let denied_user = if owner == 65534 { 65533 } else { 65534 };
    // Linux POSIX ACL xattr format: version 2, then (tag, permissions, id).
    // A named-user deny must survive even though mode bits permit others.
    let mut acl = 2_u32.to_le_bytes().to_vec();
    for (tag, permissions, id) in [
        (1_u16, 7_u16, u32::MAX),
        (2, 0, denied_user),
        (4, 5, u32::MAX),
        (16, 5, u32::MAX),
        (32, 5, u32::MAX),
    ] {
        acl.extend(tag.to_le_bytes());
        acl.extend(permissions.to_le_bytes());
        acl.extend(id.to_le_bytes());
    }
    if let Err(error) = xattr::set(&workspace, "system.posix_acl_access", &acl) {
        if error.raw_os_error() == Some(libc::EOPNOTSUPP) {
            return;
        }
        panic!("set access ACL: {error}");
    }
    xattr::set(&workspace, "system.posix_acl_default", &acl).expect("set default ACL");
    materialize_object(&repo.origin_path(), &sha, &workspace)
        .assert()
        .success();
    for path in [&workspace, &workspace.join(".git")] {
        assert_eq!(
            xattr::get(path, "system.posix_acl_access").expect("access ACL"),
            Some(acl.clone())
        );
        assert_eq!(
            xattr::get(path, "system.posix_acl_default").expect("default ACL"),
            Some(acl.clone())
        );
    }
    // Regular files inherit the named-user restriction, with a narrower mask.
    let file_acl = xattr::get(workspace.join(".git/config"), "system.posix_acl_access")
        .expect("file ACL")
        .expect("inherited file ACL");
    assert_eq!(&file_acl[12..20], &acl[12..20]);
    assert_checkout(&workspace, &sha);
    assert_no_staging(root.path());
}

#[cfg(target_os = "linux")]
#[test]
fn local_copy_does_not_add_parent_acls_to_existing_destination() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let repo = fixtures::ClonedTestRepo::new();
    let sha = git_output(&["rev-parse", "HEAD"], &repo.path());
    let root = tempfile::tempdir_in("/dev/shm")
        .or_else(|_| tempfile::tempdir())
        .expect("temp dir");
    let owner = std::fs::metadata(root.path()).expect("metadata").uid();
    let extra_user = if owner == 65534 { 65533 } else { 65534 };
    let mut acl = 2_u32.to_le_bytes().to_vec();
    for (tag, permissions, id) in [
        (1_u16, 7_u16, u32::MAX),
        (2, 7, extra_user),
        (4, 5, u32::MAX),
        (16, 7, u32::MAX),
        (32, 0, u32::MAX),
    ] {
        acl.extend(tag.to_le_bytes());
        acl.extend(permissions.to_le_bytes());
        acl.extend(id.to_le_bytes());
    }
    if let Err(error) = xattr::set(root.path(), "system.posix_acl_default", &acl) {
        if error.raw_os_error() == Some(libc::EOPNOTSUPP) {
            return;
        }
        panic!("set parent default ACL: {error}");
    }
    let workspace = root.path().join("workspace");
    std::fs::create_dir(&workspace).expect("empty workspace");
    // Deliberately opt this destination out of the parent's named-user grant.
    for name in ["system.posix_acl_access", "system.posix_acl_default"] {
        xattr::remove(&workspace, name).expect("remove inherited ACL");
    }
    std::fs::set_permissions(&workspace, std::fs::Permissions::from_mode(0o770))
        .expect("group-private workspace");
    materialize_object(&repo.origin_path(), &sha, &workspace)
        .assert()
        .success();
    for path in [
        &workspace,
        &workspace.join(".git"),
        &workspace.join(".git/config"),
    ] {
        for name in ["system.posix_acl_access", "system.posix_acl_default"] {
            assert_eq!(xattr::get(path, name).expect("inspect ACL"), None);
        }
    }
    assert_eq!(
        std::fs::metadata(&workspace)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777,
        0o770
    );
    assert_checkout(&workspace, &sha);
    assert_no_staging(root.path());
}

#[test]
fn local_copy_populates_existing_destination_without_changing_access_controls() {
    let repo = fixtures::ClonedTestRepo::new();
    let sha = git_output(&["rev-parse", "HEAD"], &repo.path());
    let root = tempfile::tempdir().expect("temp dir");
    let workspace = root.path().join("workspace");
    std::fs::create_dir(&workspace).expect("empty workspace");
    #[cfg(target_os = "macos")]
    {
        // A named-user deny is not represented by fs::Permissions or uid/gid.
        assert!(StdCommand::new("chmod")
            .args(["+a", "nobody deny read,execute"])
            .arg(&workspace)
            .status()
            .expect("set native ACL")
            .success());
    }
    let before = std::fs::metadata(&workspace).expect("metadata");
    #[cfg(target_os = "macos")]
    let acl_before = StdCommand::new("ls")
        .arg("-lde")
        .arg(&workspace)
        .output()
        .expect("inspect native ACL");
    materialize_object(&repo.origin_path(), &sha, &workspace)
        .assert()
        .success();
    assert_checkout(&workspace, &sha);
    assert_eq!(
        git_output(&["remote", "get-url", "origin"], &workspace),
        dunce::canonicalize(repo.origin_path())
            .expect("source path")
            .display()
            .to_string()
    );
    let after = std::fs::metadata(&workspace).expect("metadata");
    assert_eq!(before.permissions(), after.permissions());
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        assert_eq!((before.dev(), before.ino()), (after.dev(), after.ino()));
    }
    #[cfg(target_os = "macos")]
    {
        let acl_after = StdCommand::new("ls")
            .arg("-lde")
            .arg(&workspace)
            .output()
            .expect("inspect native ACL");
        assert!(acl_before.status.success() && acl_after.status.success());
        // Directory size/timestamps change when populated; compare ACL entries.
        let entries = |bytes: &[u8]| {
            String::from_utf8_lossy(bytes)
                .lines()
                .skip(1)
                .collect::<Vec<_>>()
                .join("\n")
        };
        assert_eq!(entries(&acl_before.stdout), entries(&acl_after.stdout));
    }
    assert_no_staging(root.path());
}

#[test]
fn existing_destination_receives_unreferenced_commit_from_verified_snapshot() {
    let repo = fixtures::ClonedTestRepo::new();
    let sha = git_output(&["rev-parse", "HEAD"], &repo.path());
    let source = repo.origin_path();
    git_output(&["update-ref", "-d", "refs/heads/main"], &source);
    let root = tempfile::tempdir().expect("temp dir");
    let workspace = root.path().join("workspace");
    std::fs::create_dir(&workspace).expect("empty workspace");
    materialize_object(&source, &sha, &workspace)
        .assert()
        .success();
    assert_checkout(&workspace, &sha);
    std::fs::remove_dir_all(&source).expect("remove source");
    assert_checkout(&workspace, &sha);
    assert_no_staging(root.path());
}

#[cfg(target_os = "macos")]
#[test]
fn new_destination_inherits_one_generation_native_deny_like_direct_clone() {
    let repo = fixtures::ClonedTestRepo::new();
    let sha = git_output(&["rev-parse", "HEAD"], &repo.path());
    let root = tempfile::tempdir().expect("temp dir");
    // The parent ACE is not effective on the parent, but denies traversal of
    // immediate children. limit_inherit prevents inheritance by grandchildren.
    assert!(StdCommand::new("chmod")
        .args([
            "+a",
            "nobody deny read,execute,directory_inherit,only_inherit,limit_inherit",
        ])
        .arg(root.path())
        .status()
        .expect("set one-generation native ACL")
        .success());
    let protocol = root.path().join("protocol");
    git_output(
        &[
            "clone",
            "--no-local",
            "--no-checkout",
            &repo.origin_path().display().to_string(),
            &protocol.display().to_string(),
        ],
        root.path(),
    );
    let entries = |path: &Path| {
        let output = StdCommand::new("ls")
            .arg("-lde")
            .arg(path)
            .output()
            .expect("inspect native ACL");
        assert!(output.status.success());
        String::from_utf8(output.stdout)
            .expect("native ACL text")
            .lines()
            .skip(1)
            .collect::<Vec<_>>()
            .join("\n")
    };
    let expected = entries(&protocol);
    assert!(
        expected.contains("user:nobody inherited deny list,search"),
        "{expected}"
    );
    // Demonstrate why nesting then renaming is incompatible: the second
    // generation lacks the deny, and moving it does not restore inheritance.
    let staging = root.path().join("staging");
    std::fs::create_dir(&staging).expect("staging");
    let nested = staging.join("workspace");
    std::fs::create_dir(&nested).expect("nested root");
    let moved = root.path().join("moved");
    std::fs::rename(&nested, &moved).expect("move nested root");
    assert!(!entries(&moved).contains("user:nobody"));
    let workspace = root.path().join("workspace");
    materialize_object(&repo.origin_path(), &sha, &workspace)
        .assert()
        .success();
    assert_eq!(entries(&workspace), expected);
    assert_checkout(&workspace, &sha);
    assert_no_staging(root.path());
}

#[cfg(unix)]
#[test]
fn local_copy_preserves_replaced_root_on_failure_and_rejects_replaced_success() {
    use std::os::unix::fs::PermissionsExt;
    let repo = fixtures::ClonedTestRepo::new();
    // Exercise root-identity protection with the concurrent status path too.
    for index in 0..1_000 {
        std::fs::write(
            repo.path().join(format!("file-{index}")),
            "root identity fixture",
        )
        .expect("write large fixture");
    }
    git_output(&["add", "."], &repo.path());
    git_output(
        &["commit", "-m", "large root identity fixture"],
        &repo.path(),
    );
    git_output(&["push", "origin", "main"], &repo.path());
    let sha = git_output(&["rev-parse", "HEAD"], &repo.path());
    let real_git = StdCommand::new("sh")
        .args(["-c", "command -v git"])
        .output()
        .expect("locate git");
    assert!(real_git.status.success());
    let real_git = String::from_utf8(real_git.stdout).expect("git path");
    for phase in [
        "symbolic-ref",
        "final-verification",
        "final-symlink-dot",
        "final-symlink-slash",
        "clone",
        "symlink",
        "missing",
    ] {
        let root = tempfile::tempdir().expect("temp dir");
        let workspace = root.path().join("workspace");
        let moved = root.path().join("moved");
        let bin = root.path().join("bin");
        std::fs::create_dir(&bin).expect("wrapper directory");
        let wrapper = bin.join("git");
        std::fs::write(
            &wrapper,
            r#"#!/bin/sh
if [ "$1" = symbolic-ref ] && [ ! -e "$WT_MOVED_ROOT.verified" ] && { [ "$WT_REPLACEMENT" = final-verification ] || [ "$WT_REPLACEMENT" = final-symlink-dot ] || [ "$WT_REPLACEMENT" = final-symlink-slash ]; }; then
    touch "$WT_MOVED_ROOT.verified" || exit 94
    exec "$WT_REAL_GIT" "$@"
fi
if [ "$1" = "$WT_REPLACE_PHASE" ] && [ ! -e "$WT_MOVED_ROOT" ]; then
    if [ "$WT_REPLACEMENT" = symlink ] || [ "$WT_REPLACEMENT" = missing ] || [ "$WT_REPLACEMENT" = final-symlink-dot ] || [ "$WT_REPLACEMENT" = final-symlink-slash ]; then
        mv "$WT_WORKSPACE" "$WT_MOVED_ROOT" || exit 90
        printf 'unrelated data' > "$WT_MOVED_ROOT/user-data"
        if [ "$WT_REPLACEMENT" != missing ]; then
            ln -s "$WT_MOVED_ROOT" "$WT_WORKSPACE" || exit 91
        fi
        exit 93
    fi
    if [ "$1" = symbolic-ref ]; then
        "$WT_REAL_GIT" "$@"
        result=$?
        mv "$WT_WORKSPACE" "$WT_MOVED_ROOT" || exit 90
        mkdir "$WT_WORKSPACE" || exit 91
        cp -R "$WT_MOVED_ROOT/." "$WT_WORKSPACE" || exit 92
    else
        mv "$WT_WORKSPACE" "$WT_MOVED_ROOT" || exit 90
        mkdir "$WT_WORKSPACE" || exit 91
        result=93
    fi
    printf 'unrelated data' > "$WT_WORKSPACE/user-data"
    # Keep a substituted successful checkout clean as well as exact/detached.
    if [ "$1" = symbolic-ref ]; then
        printf '\n/user-data\n' >> "$WT_WORKSPACE/.git/info/exclude"
    fi
    exit "$result"
fi
exec "$WT_REAL_GIT" "$@"
"#,
        )
        .expect("write wrapper");
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755))
            .expect("executable wrapper");
        let paths = std::env::join_paths(std::iter::once(bin).chain(std::env::split_paths(
            &std::env::var_os("PATH").expect("PATH"),
        )))
        .expect("wrapper PATH");
        let spelling = match phase {
            "final-symlink-dot" => PathBuf::from(format!("{}/.", workspace.display())),
            "final-symlink-slash" => PathBuf::from(format!("{}/", workspace.display())),
            _ => workspace.clone(),
        };
        materialize_object(&repo.origin_path(), &sha, &spelling)
            .env("PATH", paths)
            .env("WT_REAL_GIT", real_git.trim())
            .env(
                "WT_REPLACE_PHASE",
                if matches!(
                    phase,
                    "symbolic-ref"
                        | "final-verification"
                        | "final-symlink-dot"
                        | "final-symlink-slash"
                ) {
                    "symbolic-ref"
                } else {
                    "clone"
                },
            )
            .env("WT_REPLACEMENT", phase)
            .env("WT_WORKSPACE", &workspace)
            .env("WT_MOVED_ROOT", &moved)
            .assert()
            .failure();
        let preserved = if phase == "missing" {
            &moved
        } else {
            &workspace
        };
        assert_eq!(
            std::fs::read_to_string(preserved.join("user-data")).expect("preserved replacement"),
            "unrelated data"
        );
        if matches!(
            phase,
            "symlink" | "final-symlink-dot" | "final-symlink-slash"
        ) {
            assert!(std::fs::symlink_metadata(&workspace)
                .expect("preserved symlink")
                .file_type()
                .is_symlink());
        }
        if phase == "missing" {
            assert!(!workspace.exists());
        }
        assert!(moved.is_dir(), "do not chase and delete a moved task root");
    }
}

#[cfg(unix)]
#[test]
fn local_copy_reports_new_root_io_failure_as_git_error() {
    use std::os::unix::fs::PermissionsExt;
    let repo = fixtures::ClonedTestRepo::new();
    let sha = git_output(&["rev-parse", "HEAD"], &repo.path());
    let root = tempfile::tempdir().expect("temp dir");
    let permissions = std::fs::metadata(root.path())
        .expect("metadata")
        .permissions();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o500))
        .expect("read-only parent");
    let workspace = root.path().join("workspace");
    // Privileged users can bypass the fixture's write restriction.
    let probe = std::fs::create_dir(root.path().join("probe"));
    let output = if probe.is_err() {
        Some(materialize_object(&repo.origin_path(), &sha, &workspace).output())
    } else {
        None
    };
    std::fs::set_permissions(root.path(), permissions).expect("restore parent permissions");
    if let Some(output) = output {
        let output = output.expect("run materialize");
        assert_eq!(output.status.code(), Some(2));
        assert!(!workspace.exists());
    }
}

#[test]
fn materialize_copy_mode_rejects_invalid_and_conflicting_values() {
    let root = tempfile::tempdir().expect("temp directory");
    for flags in [
        vec!["--copy-mode", "reflink"],
        vec!["--copy-mode", "AUTO"],
        vec!["--copy-mode", "--copy"],
        vec!["--copy-mode", "auto", "--copy-mode", "copy"],
        vec!["--copy-mode=auto", "--copy-mode=auto"],
    ] {
        let workspace = root.path().join("workspace");
        materialize_object(
            root.path(),
            "0123456789abcdef0123456789abcdef01234567",
            &workspace,
        )
        .args(flags)
        .assert()
        .failure()
        .code(2);
        assert!(!workspace.exists());
    }
}

#[test]
fn both_copy_modes_preserve_unreachable_objects_in_new_workspaces() {
    let repo = fixtures::ClonedTestRepo::new();
    let sha = git_output(&["rev-parse", "HEAD"], &repo.path());
    let source = repo.origin_path();
    git_output(&["update-ref", "-d", "refs/heads/main"], &source);
    let root = tempfile::tempdir().expect("temp directory");
    for mode in ["auto", "copy"] {
        let workspace = root.path().join(mode);
        materialize_object(&source, &sha, &workspace)
            .args(["--copy-mode", mode])
            .assert()
            .success();
        assert_checkout(&workspace, &sha);
        #[cfg(unix)]
        assert_independent_files(&source, &workspace.join(".git"));
    }
    std::fs::remove_dir_all(source).expect("remove private source");
    for mode in ["auto", "copy"] {
        assert_checkout(&root.path().join(mode), &sha);
    }
}

#[cfg(unix)]
#[test]
fn both_copy_modes_reject_nested_object_symlinks_before_git_reads_them() {
    use std::os::unix::fs::symlink;
    let repo = fixtures::ClonedTestRepo::new();
    let sha = git_output(&["rev-parse", "HEAD"], &repo.path());
    let root = tempfile::tempdir().expect("temp directory");
    let source = repo.origin_path();
    let unsafe_path = source.join("objects/info/unsafe-link");
    symlink(root.path().join("missing"), &unsafe_path).expect("dangling symlink");
    for mode in ["auto", "copy"] {
        for existed in [false, true] {
            let workspace = root.path().join(format!("{mode}-{existed}"));
            if existed {
                std::fs::create_dir(&workspace).expect("empty workspace");
            }
            materialize_object(&source, &sha, &workspace)
                .args(["--copy-mode", mode])
                .assert()
                .failure()
                .stderr(predicate::str::contains("symlinks"));
            assert_eq!(workspace.exists(), existed);
            if existed {
                assert_eq!(std::fs::read_dir(workspace).expect("empty").count(), 0);
            }
            assert_no_staging(root.path());
        }
    }
}

#[test]
fn both_copy_modes_preserve_unrelated_unreachable_blobs_in_existing_workspaces() {
    let repo = fixtures::ClonedTestRepo::new();
    let sha = git_output(&["rev-parse", "HEAD"], &repo.path());
    let root = tempfile::tempdir().expect("temp directory");
    let blob_file = root.path().join("unreachable");
    std::fs::write(&blob_file, "unrelated unreachable content\n").expect("blob fixture");
    let source = repo.origin_path();
    let blob = git_output(
        &["hash-object", "-w", &blob_file.display().to_string()],
        &source,
    );
    for mode in ["auto", "copy"] {
        let workspace = root.path().join(mode);
        std::fs::create_dir(&workspace).expect("existing empty workspace");
        materialize_object(&source, &sha, &workspace)
            .args(["--copy-mode", mode])
            .assert()
            .success();
        assert_checkout(&workspace, &sha);
        assert_eq!(
            git_output(&["cat-file", "blob", &blob], &workspace),
            "unrelated unreachable content"
        );
    }
    std::fs::remove_dir_all(source).expect("remove source");
    for mode in ["auto", "copy"] {
        assert_eq!(
            git_output(&["cat-file", "blob", &blob], &root.path().join(mode)),
            "unrelated unreachable content"
        );
    }
}

#[cfg(unix)]
#[test]
fn cached_checkout_releases_lock_before_independent_checkout() {
    use std::os::unix::fs::PermissionsExt;
    use std::time::{Duration, Instant};
    let repo = fixtures::ClonedTestRepo::new();
    let sha = git_output(&["rev-parse", "HEAD"], &repo.path());
    let root = tempfile::tempdir().expect("fixture");
    let cache = root.path().join("cache");
    let workspace = root.path().join("workspace");
    let bin = root.path().join("bin");
    std::fs::create_dir(&bin).expect("wrapper directory");
    let real_git = StdCommand::new("sh")
        .args(["-c", "command -v git"])
        .output()
        .expect("locate git");
    assert!(real_git.status.success());
    let wrapper = bin.join("git");
    std::fs::write(
        &wrapper,
        r#"#!/bin/sh
for arg do
    if [ "$arg" = checkout ]; then
        printf ready > "$WT_CACHE_GATE/ready"
        while [ ! -f "$WT_CACHE_GATE/finish" ]; do sleep 0.05; done
        break
    fi
done
exec "$WT_REAL_GIT" "$@"
"#,
    )
    .expect("wrapper");
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755))
        .expect("executable wrapper");
    let paths = std::env::join_paths(std::iter::once(bin).chain(std::env::split_paths(
        &std::env::var_os("PATH").expect("PATH"),
    )))
    .expect("wrapper PATH");
    let mut owner = StdCommand::new(assert_cmd::cargo_bin!("wt-core"))
        .args(["materialize", "--repo-slug", "owner/repo", "--remote-url"])
        .arg(file_url(&repo.origin_path()))
        .args(["--sha", &sha, "--cache-root"])
        .arg(&cache)
        .arg("--workspace-root")
        .arg(&workspace)
        .env("PATH", paths)
        .env("WT_CACHE_GATE", root.path())
        .env(
            "WT_REAL_GIT",
            String::from_utf8(real_git.stdout).expect("git path").trim(),
        )
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn checkout");
    let started = Instant::now();
    while !root.path().join("ready").exists() {
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "checkout readiness timeout"
        );
        assert!(owner.try_wait().expect("owner status").is_none());
        std::thread::sleep(Duration::from_millis(10));
    }
    // The first checkout remains blocked, but another caller must finish.
    materialize_cached(&repo, &sha, &cache, &root.path().join("second"));
    assert!(owner.try_wait().expect("still blocked").is_none());
    std::fs::write(root.path().join("finish"), "").expect("release checkout");
    let result = owner.wait_with_output().expect("checkout result");
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_checkout(&workspace, &sha);
    assert_eq!(
        git_output(&["remote", "get-url", "origin"], &workspace),
        dunce::canonicalize(cache.join("owner__repo.git"))
            .expect("cache identity")
            .to_string_lossy()
    );
}
