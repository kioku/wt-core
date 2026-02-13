use std::fmt;
use std::path::{Path, PathBuf};

/// Root path of a git repository (the directory containing `.git`).
#[derive(Debug, Clone)]
pub struct RepoRoot(pub PathBuf);

impl RepoRoot {
    /// The `.worktrees/` directory under the repo root.
    pub fn worktrees_dir(&self) -> PathBuf {
        self.0.join(".worktrees")
    }
}

impl AsRef<Path> for RepoRoot {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl std::ops::Deref for RepoRoot {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.0
    }
}

impl fmt::Display for RepoRoot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0.display())
    }
}

/// A sanitized branch name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchName(pub String);

impl BranchName {
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    /// Return the branch name as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Convert the branch name to a collision-safe directory slug.
    ///
    /// Format: `<slug>--<8hex>`
    /// Example: `feature/auth` → `feature-auth--a1b2c3d4`
    pub fn to_dir_name(&self) -> String {
        let slug = slugify(&self.0);
        let hash = hash8(&self.0);
        format!("{slug}--{hash}")
    }
}

impl fmt::Display for BranchName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A worktree entry as reported by `git worktree list`.
#[derive(Debug, Clone)]
pub struct Worktree {
    pub path: PathBuf,
    pub branch: Option<String>,
    pub commit: String,
    pub is_main: bool,
}

/// Slugify a branch name: replace non-alphanumeric chars with hyphens,
/// collapse runs, and trim leading/trailing hyphens.
fn slugify(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let mut prev_hyphen = true; // suppress leading hyphen

    for ch in input.chars() {
        if ch.is_ascii_alphanumeric() {
            result.push(ch.to_ascii_lowercase());
            prev_hyphen = false;
        } else if !prev_hyphen {
            result.push('-');
            prev_hyphen = true;
        }
    }

    // trim trailing hyphen
    if result.ends_with('-') {
        result.pop();
    }
    result
}

/// Produce an 8-character hex hash of the input (FNV-1a based).
fn hash8(input: &str) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for byte in input.as_bytes() {
        h ^= u64::from(*byte);
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{:08x}", h as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_simple_branch() {
        assert_eq!(slugify("main"), "main");
    }

    #[test]
    fn slugify_slashed_branch() {
        assert_eq!(slugify("feature/auth"), "feature-auth");
    }

    #[test]
    fn slugify_collapses_runs() {
        assert_eq!(slugify("a//b--c"), "a-b-c");
    }

    #[test]
    fn slugify_trims_edges() {
        assert_eq!(slugify("/leading/"), "leading");
    }

    #[test]
    fn collision_safe_dir_names_differ() {
        let a = BranchName::new("feature/a-b");
        let b = BranchName::new("feature-a/b");
        // slugs are the same, but hashes differ
        assert_ne!(a.to_dir_name(), b.to_dir_name());
    }

    #[test]
    fn dir_name_format() {
        let name = BranchName::new("feature/auth");
        let dir = name.to_dir_name();
        assert!(dir.starts_with("feature-auth--"));
        assert_eq!(dir.len(), "feature-auth--".len() + 8);
    }

    #[test]
    fn hash8_is_deterministic() {
        assert_eq!(hash8("hello"), hash8("hello"));
        assert_ne!(hash8("hello"), hash8("world"));
    }
}
