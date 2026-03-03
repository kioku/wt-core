mod fixtures;

use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;

fn wt_core() -> Command {
    Command::new(assert_cmd::cargo_bin!("wt-core"))
}

// ── wt add with symlinks ────────────────────────────────────────────

#[test]
fn add_symlinks_node_modules_when_configured() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    // Create gitignored content in main worktree
    fs::create_dir(repo.path().join("node_modules")).expect("mkdir");
    fs::write(repo.path().join("node_modules/pkg.js"), "module").expect("write");

    // Create symlinks config
    fs::create_dir(repo.path().join(".wt")).expect("mkdir .wt");
    fs::write(repo.path().join(".wt/symlinks"), "node_modules\n").expect("write config");

    // Create worktree
    let output = wt_core()
        .args([
            "add",
            "feat/sym-test",
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

    // Verify symlink exists and resolves
    let link = std::path::Path::new(&wt_path).join("node_modules");
    assert!(
        link.symlink_metadata()
            .expect("stat")
            .file_type()
            .is_symlink(),
        "node_modules should be a symlink"
    );
    assert!(
        link.join("pkg.js").exists(),
        "symlink should resolve to main worktree content"
    );
}

#[test]
fn add_symlinks_env_glob_pattern() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    fs::write(repo.path().join(".env"), "SECRET=1").expect("write");
    fs::write(repo.path().join(".env.local"), "SECRET=2").expect("write");
    fs::write(repo.path().join(".env.production"), "SECRET=3").expect("write");

    fs::create_dir(repo.path().join(".wt")).expect("mkdir .wt");
    fs::write(repo.path().join(".wt/symlinks"), ".env\n.env.*\n").expect("write config");

    let output = wt_core()
        .args([
            "add",
            "feat/env-glob",
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

    let wt = std::path::Path::new(&wt_path);
    assert!(wt.join(".env").exists(), ".env should be symlinked");
    assert!(
        wt.join(".env.local").exists(),
        ".env.local should be symlinked"
    );
    assert!(
        wt.join(".env.production").exists(),
        ".env.production should be symlinked"
    );
}

#[test]
fn add_symlinks_subdirectory_target() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    fs::create_dir_all(repo.path().join("apps/api")).expect("mkdir");
    fs::write(repo.path().join("apps/api/.env"), "DB_URL=x").expect("write");

    fs::create_dir(repo.path().join(".wt")).expect("mkdir .wt");
    fs::write(repo.path().join(".wt/symlinks"), "apps/api/.env\n").expect("write config");

    let output = wt_core()
        .args([
            "add",
            "feat/subdir-sym",
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

    let env_in_wt = std::path::Path::new(&wt_path).join("apps/api/.env");
    assert!(env_in_wt.exists(), "subdirectory .env should be symlinked");
    assert!(
        env_in_wt
            .symlink_metadata()
            .expect("stat")
            .file_type()
            .is_symlink(),
        "should be a symlink, not a copy"
    );
}

#[test]
fn add_symlinks_skips_missing_sources() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    // Config references node_modules but it doesn't exist
    fs::create_dir(repo.path().join(".wt")).expect("mkdir .wt");
    fs::write(repo.path().join(".wt/symlinks"), "node_modules\ntarget\n").expect("write config");

    // Should succeed without error
    wt_core()
        .args(["add", "feat/missing", "--repo", &repo_str])
        .assert()
        .success();
}

#[test]
fn add_no_config_skips_symlinks_silently() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    // No .wt/symlinks — should work exactly as before
    wt_core()
        .args(["add", "feat/no-config", "--repo", &repo_str])
        .assert()
        .success()
        .stdout(predicate::str::contains("feat/no-config"));
}

#[test]
fn add_json_includes_symlinks_array() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    fs::create_dir(repo.path().join("node_modules")).expect("mkdir");
    fs::create_dir(repo.path().join(".wt")).expect("mkdir .wt");
    fs::write(repo.path().join(".wt/symlinks"), "node_modules\n").expect("write config");

    let output = wt_core()
        .args(["add", "feat/json-sym", "--repo", &repo_str, "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let json: serde_json::Value = serde_json::from_slice(&output).expect("invalid json");
    assert_eq!(json["ok"], true);
    let symlinks = json["symlinks"]
        .as_array()
        .expect("symlinks should be array");
    assert_eq!(symlinks.len(), 1);
    assert_eq!(symlinks[0], "node_modules");
}

#[test]
fn add_human_reports_symlinks() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    fs::create_dir(repo.path().join("node_modules")).expect("mkdir");
    fs::create_dir(repo.path().join(".wt")).expect("mkdir .wt");
    fs::write(repo.path().join(".wt/symlinks"), "node_modules\n").expect("write config");

    wt_core()
        .args(["add", "feat/human-sym", "--repo", &repo_str])
        .assert()
        .success()
        .stdout(predicate::str::contains("Symlinked node_modules"));
}

#[test]
fn add_merges_shared_and_local_config() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    fs::create_dir(repo.path().join("node_modules")).expect("mkdir");
    fs::write(repo.path().join(".env"), "SECRET=x").expect("write");

    fs::create_dir(repo.path().join(".wt")).expect("mkdir .wt");
    fs::write(repo.path().join(".wt/symlinks"), "node_modules\n").expect("write shared");
    fs::write(repo.path().join(".wt/symlinks.local"), ".env\n").expect("write local");

    let output = wt_core()
        .args(["add", "feat/merged", "--repo", &repo_str, "--print-cd-path"])
        .output()
        .expect("add failed");
    let wt_path = String::from_utf8(output.stdout)
        .expect("invalid utf8")
        .trim()
        .to_string();

    let wt = std::path::Path::new(&wt_path);
    assert!(wt.join("node_modules").exists(), "shared entry symlinked");
    assert!(wt.join(".env").exists(), "local entry symlinked");
}

// ── wt setup ────────────────────────────────────────────────────────

#[test]
fn setup_generates_config_for_rust_project() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    // Cargo.toml already exists (it's wt-core), simulate with a marker
    fs::write(repo.path().join("Cargo.toml"), "[package]").expect("write");

    wt_core()
        .args(["setup", "--repo", &repo_str])
        .assert()
        .success()
        .stderr(predicate::str::contains("rust"));

    let config = fs::read_to_string(repo.path().join(".wt/symlinks")).expect("read config");
    assert!(config.contains("target"));
    assert!(config.contains(".env*"));
    assert!(config.contains("# rust"));
}

#[test]
fn setup_refuses_if_config_exists() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    fs::create_dir(repo.path().join(".wt")).expect("mkdir .wt");
    fs::write(repo.path().join(".wt/symlinks"), "existing\n").expect("write");

    wt_core()
        .args(["setup", "--repo", &repo_str])
        .assert()
        .failure()
        .code(5)
        .stderr(predicate::str::contains("already exists"));
}

#[test]
fn setup_adds_symlinks_local_to_gitignore() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    wt_core()
        .args(["setup", "--repo", &repo_str])
        .assert()
        .success()
        .stderr(predicate::str::contains(".gitignore"));

    let gitignore = fs::read_to_string(repo.path().join(".gitignore")).expect("read");
    assert!(gitignore.contains(".wt/symlinks.local"));
}

#[test]
fn setup_detects_multiple_ecosystems() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    fs::write(repo.path().join("package.json"), "{}").expect("write");
    fs::write(repo.path().join("Cargo.toml"), "[package]").expect("write");

    wt_core()
        .args(["setup", "--repo", &repo_str])
        .assert()
        .success()
        .stderr(predicate::str::contains("node"))
        .stderr(predicate::str::contains("rust"));

    let config = fs::read_to_string(repo.path().join(".wt/symlinks")).expect("read config");
    assert!(config.contains("node_modules"));
    assert!(config.contains("target"));
}

// ── wt remove with symlinks ─────────────────────────────────────────

#[test]
fn remove_worktree_with_symlinks_preserves_originals() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    fs::create_dir(repo.path().join("node_modules")).expect("mkdir");
    fs::write(repo.path().join("node_modules/pkg.js"), "keep").expect("write");

    fs::create_dir(repo.path().join(".wt")).expect("mkdir .wt");
    fs::write(repo.path().join(".wt/symlinks"), "node_modules\n").expect("write config");

    wt_core()
        .args(["add", "feat/remove-sym", "--repo", &repo_str])
        .assert()
        .success();

    wt_core()
        .args(["remove", "feat/remove-sym", "--force", "--repo", &repo_str])
        .assert()
        .success();

    // Original content must still exist
    assert!(
        repo.path().join("node_modules/pkg.js").exists(),
        "original node_modules must survive worktree removal"
    );
}
