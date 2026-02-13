use std::path::PathBuf;
use std::process::Command;

use tempfile::TempDir;

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

fn run_git(args: &[&str], cwd: &std::path::Path) {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("failed to run git");
    assert!(
        output.status.success(),
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
}
