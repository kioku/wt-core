#![allow(dead_code)]

use std::path::PathBuf;
use std::process::Command;

use tempfile::TempDir;

/// Environment variables that can leak from parent git processes (e.g. hooks)
/// and interfere with subprocess calls in tests.
const GIT_ENV_OVERRIDES: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_PREFIX",
];

/// A temporary git repository for testing.
pub struct TestRepo {
    pub dir: TempDir,
}

impl TestRepo {
    /// Create a new temporary git repo with an initial commit.
    pub fn new() -> Self {
        let dir = TempDir::new().expect("failed to create temp dir");
        let path = dir.path();

        // Init repo with 'main' as default branch
        run_git(&["init", "-b", "main"], path);
        run_git(&["config", "user.email", "test@test.com"], path);
        run_git(&["config", "user.name", "Test"], path);

        // Create initial commit so HEAD exists
        let readme = path.join("README.md");
        std::fs::write(&readme, "# test repo\n").expect("failed to write README");
        run_git(&["add", "."], path);
        run_git(&["commit", "-m", "initial commit"], path);

        Self { dir }
    }

    pub fn path(&self) -> PathBuf {
        self.dir
            .path()
            .canonicalize()
            .expect("failed to canonicalize temp dir")
    }
}

/// Run a git command in the given directory (test helper).
///
/// Clears inherited GIT_* env vars so tests work correctly when invoked
/// from git hooks (e.g. pre-commit).
pub fn run_git(args: &[&str], cwd: &std::path::Path) {
    let mut cmd = Command::new("git");
    cmd.args(args).current_dir(cwd);
    for var in GIT_ENV_OVERRIDES {
        cmd.env_remove(var);
    }
    let output = cmd.output().expect("failed to run git");
    assert!(
        output.status.success(),
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Create a file, add, and commit in the given directory.
pub fn commit_file(cwd: &std::path::Path, filename: &str, content: &str, message: &str) {
    std::fs::write(cwd.join(filename), content).expect("write failed");
    run_git(&["add", "."], cwd);
    run_git(&["commit", "-m", message], cwd);
}

/// Find the worktree directory by slug prefix under .worktrees/.
pub fn find_worktree_dir(repo: &std::path::Path, slug_prefix: &str) -> std::path::PathBuf {
    let worktrees_dir = repo.join(".worktrees");
    for entry in std::fs::read_dir(&worktrees_dir)
        .expect("no .worktrees dir")
        .flatten()
    {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with(slug_prefix) {
            return entry.path();
        }
    }
    panic!(
        "worktree with slug prefix '{}' not found in {}",
        slug_prefix,
        worktrees_dir.display()
    );
}
