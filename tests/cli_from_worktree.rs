mod fixtures;

use assert_cmd::Command;

fn wt_core() -> Command {
    Command::new(assert_cmd::cargo_bin!("wt-core"))
}

/// Helper: create a worktree and return its path.
fn add_worktree(repo_path: &str, branch: &str) -> String {
    let output = wt_core()
        .args(["add", branch, "--repo", repo_path, "--print-cd-path"])
        .output()
        .expect("failed to run wt-core add");
    assert!(output.status.success());
    String::from_utf8(output.stdout)
        .expect("invalid utf8")
        .trim()
        .to_string()
}

#[test]
fn list_from_inside_worktree_shows_correct_is_main() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();
    let wt_path = add_worktree(&repo_str, "feat-inside");

    // Run list from INSIDE the worktree (no --repo flag)
    let output = wt_core()
        .args(["list", "--json"])
        .current_dir(&wt_path)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let json: serde_json::Value = serde_json::from_slice(&output).expect("invalid json");
    let worktrees = json["worktrees"].as_array().expect("worktrees array");

    // The main worktree (repo root) must have is_main: true
    let main_wt = worktrees
        .iter()
        .find(|w| w["branch"] == "main")
        .expect("main worktree not found");
    assert_eq!(main_wt["is_main"], true, "main worktree should be is_main");

    // The linked worktree must have is_main: false
    let feat_wt = worktrees
        .iter()
        .find(|w| w["branch"] == "feat-inside")
        .expect("feat worktree not found");
    assert_eq!(
        feat_wt["is_main"], false,
        "linked worktree should not be is_main"
    );
}

#[test]
fn add_from_inside_worktree_creates_at_repo_root() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();
    let wt_path = add_worktree(&repo_str, "base-wt");

    // Run add from INSIDE a worktree (no --repo flag)
    let output = wt_core()
        .args(["add", "new-from-wt", "--print-cd-path"])
        .current_dir(&wt_path)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let new_path = String::from_utf8(output).expect("invalid utf8");
    let new_path = new_path.trim();

    // Must be under <repo-root>/.worktrees/, NOT under the current worktree
    let expected_prefix = format!("{}/.worktrees/", repo_str);
    assert!(
        new_path.starts_with(&expected_prefix),
        "new worktree should be under repo root .worktrees/: {new_path}"
    );
    assert!(
        !new_path.contains(".worktrees/base-wt"),
        "must not be nested under existing worktree: {new_path}"
    );
}

#[test]
fn remove_from_inside_worktree_succeeds() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();
    let _wt_path = add_worktree(&repo_str, "to-rm-inside");

    // Run remove from INSIDE the worktree being removed (no --repo flag)
    wt_core()
        .args(["remove", "to-rm-inside", "--repo", &repo_str])
        .assert()
        .success();
}

#[test]
fn remove_main_from_inside_worktree_blocked() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();
    let wt_path = add_worktree(&repo_str, "observer-wt");

    // From inside a linked worktree, removing main must be blocked
    wt_core()
        .args(["remove", "main"])
        .current_dir(&wt_path)
        .assert()
        .failure()
        .code(4); // Invariant violation
}

#[test]
fn list_from_subdirectory_of_main_worktree() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    // Create a subdirectory inside the main worktree
    let subdir = repo.path().join("subdir").join("deep");
    std::fs::create_dir_all(&subdir).expect("create subdir");

    // Add a worktree first (via --repo) so there's something to list
    add_worktree(&repo_str, "feat-sub");

    // Run list from a subdirectory of the main worktree (no --repo flag)
    let output = wt_core()
        .args(["list", "--json"])
        .current_dir(&subdir)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let json: serde_json::Value = serde_json::from_slice(&output).expect("invalid json");
    let worktrees = json["worktrees"].as_array().expect("worktrees array");
    assert_eq!(worktrees.len(), 2);

    let main_wt = worktrees
        .iter()
        .find(|w| w["branch"] == "main")
        .expect("main worktree not found");
    assert_eq!(main_wt["is_main"], true);
}

#[test]
fn add_from_subdirectory_of_main_worktree() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();

    let subdir = repo.path().join("subdir").join("deep");
    std::fs::create_dir_all(&subdir).expect("create subdir");

    // Run add from a subdirectory of the main worktree (no --repo flag)
    let output = wt_core()
        .args(["add", "from-deep-sub", "--print-cd-path"])
        .current_dir(&subdir)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let new_path = String::from_utf8(output).expect("invalid utf8");
    let new_path = new_path.trim();

    // Must be under <repo-root>/.worktrees/
    let expected_prefix = format!("{}/.worktrees/", repo_str);
    assert!(
        new_path.starts_with(&expected_prefix),
        "worktree should be under repo root: {new_path}"
    );
}

#[test]
fn list_from_subdirectory_of_linked_worktree() {
    let repo = fixtures::TestRepo::new();
    let repo_str = repo.path().display().to_string();
    let wt_path = add_worktree(&repo_str, "feat-linked-sub");

    // Create a subdirectory inside the linked worktree
    let subdir = std::path::PathBuf::from(&wt_path).join("nested");
    std::fs::create_dir_all(&subdir).expect("create subdir");

    // Run list from subdirectory of a linked worktree (no --repo flag)
    let output = wt_core()
        .args(["list", "--json"])
        .current_dir(&subdir)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let json: serde_json::Value = serde_json::from_slice(&output).expect("invalid json");
    let worktrees = json["worktrees"].as_array().expect("worktrees array");
    assert_eq!(worktrees.len(), 2);

    let main_wt = worktrees
        .iter()
        .find(|w| w["branch"] == "main")
        .expect("main worktree not found");
    assert_eq!(main_wt["is_main"], true, "main should be is_main");

    let linked_wt = worktrees
        .iter()
        .find(|w| w["branch"] == "feat-linked-sub")
        .expect("linked worktree not found");
    assert_eq!(linked_wt["is_main"], false, "linked should not be is_main");
}
