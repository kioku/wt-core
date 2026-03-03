use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::domain::RepoRoot;

const CONFIG_DIR: &str = ".wt";
const CONFIG_FILE: &str = "symlinks";
const CONFIG_LOCAL_FILE: &str = "symlinks.local";

/// Read and merge symlink config from `.wt/symlinks` and `.wt/symlinks.local`.
///
/// Returns an empty vec when neither file exists.
pub fn load_config(repo: &RepoRoot) -> Vec<String> {
    let config_dir = repo.as_ref().join(CONFIG_DIR);
    let shared = config_dir.join(CONFIG_FILE);
    let local = config_dir.join(CONFIG_LOCAL_FILE);

    let mut seen = BTreeSet::new();
    let mut entries = Vec::new();

    for path in [&shared, &local] {
        for entry in parse_config_file(path) {
            if seen.insert(entry.clone()) {
                entries.push(entry);
            }
        }
    }

    entries
}

/// Parse a single config file into entries, skipping blanks and comments.
fn parse_config_file(path: &Path) -> Vec<String> {
    let content = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };

    content
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(String::from)
        .collect()
}

/// Expand a single config entry (possibly containing `*` globs) into concrete
/// paths relative to the repo root.
///
/// Only single-level globs are supported: `*` matches within one path segment.
/// Dispatch a single path segment: standalone wildcard, filename glob, or literal.
fn expand_segment(repo_root: &Path, candidates: Vec<PathBuf>, part: &str) -> Vec<PathBuf> {
    if part == "*" {
        return expand_wildcard_segment(repo_root, &candidates);
    }
    if part.contains('*') {
        return expand_glob_segment(repo_root, &candidates, part);
    }
    candidates
        .into_iter()
        .map(|mut c| {
            c.push(part);
            c
        })
        .collect()
}

fn expand_entry(repo_root: &Path, pattern: &str) -> Vec<PathBuf> {
    if !pattern.contains('*') {
        return vec![PathBuf::from(pattern)];
    }

    let parts: Vec<&str> = pattern.split('/').collect();
    let mut candidates = vec![PathBuf::new()];

    for part in &parts {
        candidates = expand_segment(repo_root, candidates, part);
    }

    candidates
}

/// Match a glob pattern against a filename.
///
/// Supports `*` as a wildcard that matches any sequence of characters.
/// Only single `*` within the pattern is supported (e.g. `.env.*`, `*.txt`).
fn glob_matches(pattern: &str, name: &str) -> bool {
    match pattern.split_once('*') {
        Some((prefix, suffix)) => {
            name.len() >= prefix.len() + suffix.len()
                && name.starts_with(prefix)
                && name.ends_with(suffix)
        }
        None => pattern == name,
    }
}

/// For each accumulated prefix, read the directory and keep entries matching
/// a filename glob pattern (e.g. `.env.*`).
fn expand_glob_segment(repo_root: &Path, prefixes: &[PathBuf], pattern: &str) -> Vec<PathBuf> {
    let mut expanded = Vec::new();

    for prefix in prefixes {
        let abs_dir = repo_root.join(prefix);
        let entries = match fs::read_dir(&abs_dir) {
            Ok(e) => e,
            Err(_) => continue,
        };

        for entry in entries.flatten() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if glob_matches(pattern, &name_str) {
                expanded.push(prefix.join(&*name_str));
            }
        }
    }

    expanded
}

/// For each accumulated prefix, read the corresponding directory in the main
/// worktree and replace with all child entries.
fn expand_wildcard_segment(repo_root: &Path, prefixes: &[PathBuf]) -> Vec<PathBuf> {
    let mut expanded = Vec::new();

    for prefix in prefixes {
        let abs_dir = repo_root.join(prefix);
        let entries = match fs::read_dir(&abs_dir) {
            Ok(e) => e,
            Err(_) => continue,
        };

        for entry in entries.flatten() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if name_str == ".git" {
                continue;
            }
            expanded.push(prefix.join(&*name_str));
        }
    }

    expanded
}

/// Resolve config entries into concrete paths that exist in the main worktree.
pub fn resolve_entries(repo: &RepoRoot, patterns: &[String]) -> Vec<PathBuf> {
    let repo_root = repo.as_ref();
    let mut seen = BTreeSet::new();
    let mut resolved = Vec::new();

    for pattern in patterns {
        for rel_path in expand_entry(repo_root, pattern) {
            let abs_path = repo_root.join(&rel_path);
            if abs_path.exists() && seen.insert(rel_path.clone()) {
                resolved.push(rel_path);
            }
        }
    }

    resolved
}

/// Outcome of a single symlink attempt.
#[derive(Debug)]
pub enum SymlinkOutcome {
    Created(PathBuf),
    Skipped(PathBuf, String),
}

/// Ensure parent directories exist for a symlink target.
fn ensure_parent_dirs(path: &Path) -> Result<(), String> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    if parent.exists() {
        return Ok(());
    }
    fs::create_dir_all(parent).map_err(|e| format!("failed to create parent directory: {e}"))
}

/// Create a single symlink, returning the outcome.
fn create_one_symlink(
    repo_root: &Path,
    worktree_path: &Path,
    rel_path: &PathBuf,
) -> SymlinkOutcome {
    let target_in_wt = worktree_path.join(rel_path);

    if target_in_wt.exists() || target_in_wt.symlink_metadata().is_ok() {
        return SymlinkOutcome::Skipped(rel_path.clone(), "already exists".to_string());
    }

    if let Err(reason) = ensure_parent_dirs(&target_in_wt) {
        return SymlinkOutcome::Skipped(rel_path.clone(), reason);
    }

    let source_in_main = repo_root.join(rel_path);
    let link_target = compute_relative_symlink(&target_in_wt, &source_in_main);

    match create_symlink(&link_target, &target_in_wt, &source_in_main) {
        Ok(()) => SymlinkOutcome::Created(rel_path.clone()),
        Err(e) => SymlinkOutcome::Skipped(rel_path.clone(), format!("symlink failed: {e}")),
    }
}

#[cfg(unix)]
fn create_symlink(
    link_target: &Path,
    target_in_wt: &Path,
    _source_in_main: &Path,
) -> io::Result<()> {
    std::os::unix::fs::symlink(link_target, target_in_wt)
}

#[cfg(windows)]
fn create_symlink(
    link_target: &Path,
    target_in_wt: &Path,
    source_in_main: &Path,
) -> io::Result<()> {
    use std::os::windows::fs::{symlink_dir, symlink_file};

    let metadata = fs::metadata(source_in_main)?;
    if metadata.is_dir() {
        symlink_dir(link_target, target_in_wt)
    } else {
        symlink_file(link_target, target_in_wt)
    }
}

/// Create symlinks in the new worktree for all resolved entries.
///
/// For each entry, computes a relative symlink from the target location
/// back to the corresponding path in the main worktree.
pub fn create_symlinks(
    repo: &RepoRoot,
    worktree_path: &Path,
    entries: &[PathBuf],
) -> Vec<SymlinkOutcome> {
    let repo_root = repo.as_ref();
    let mut outcomes = Vec::new();

    for rel_path in entries {
        outcomes.push(create_one_symlink(repo_root, worktree_path, rel_path));
    }

    outcomes
}

/// Compute the relative path for a symlink at `link_location` pointing to `target`.
fn compute_relative_symlink(link_location: &Path, target: &Path) -> PathBuf {
    let link_parent = link_location.parent().expect("link must have parent");
    relative_path(link_parent, target)
}

/// Compute a relative path from `from` directory to `to` path.
fn relative_path(from: &Path, to: &Path) -> PathBuf {
    let from_parts: Vec<_> = from.components().collect();
    let to_parts: Vec<_> = to.components().collect();

    let common = from_parts
        .iter()
        .zip(to_parts.iter())
        .take_while(|(a, b)| a == b)
        .count();

    let mut result = PathBuf::new();
    for _ in common..from_parts.len() {
        result.push("..");
    }
    for part in &to_parts[common..] {
        result.push(part);
    }

    result
}

/// Result of the symlink phase during `wt add`.
#[derive(Debug)]
pub struct SymlinkReport {
    pub created: Vec<PathBuf>,
    pub skipped: Vec<(PathBuf, String)>,
}

/// Run the full symlink pipeline: load config, resolve, create.
///
/// Returns `None` when no config file exists (feature not configured).
pub fn apply_symlinks(repo: &RepoRoot, worktree_path: &Path) -> Option<SymlinkReport> {
    let patterns = load_config(repo);
    if patterns.is_empty() {
        return None;
    }

    let entries = resolve_entries(repo, &patterns);
    let outcomes = create_symlinks(repo, worktree_path, &entries);

    let mut created = Vec::new();
    let mut skipped = Vec::new();

    for outcome in outcomes {
        match outcome {
            SymlinkOutcome::Created(p) => created.push(p),
            SymlinkOutcome::Skipped(p, reason) => skipped.push((p, reason)),
        }
    }

    Some(SymlinkReport { created, skipped })
}

// ── Ecosystem detection and config generation (`wt setup`) ──────────

/// Marker-to-ecosystem mapping: (ecosystem_name, marker_file, entries).
const ECOSYSTEM_MARKERS: &[(&str, &str, &[&str])] = &[
    (
        "node",
        "package.json",
        &[
            "node_modules",
            ".next",
            ".nuxt",
            ".turbo",
            ".output",
            ".parcel-cache",
            ".svelte-kit",
            ".angular",
        ],
    ),
    ("rust", "Cargo.toml", &["target", ".cargo"]),
    (
        "python",
        "pyproject.toml",
        &[
            ".venv",
            "venv",
            ".mypy_cache",
            ".pytest_cache",
            ".ruff_cache",
            ".tox",
            ".nox",
        ],
    ),
    (
        "python",
        "setup.py",
        &[
            ".venv",
            "venv",
            ".mypy_cache",
            ".pytest_cache",
            ".ruff_cache",
            ".tox",
            ".nox",
        ],
    ),
    (
        "python",
        "setup.cfg",
        &[
            ".venv",
            "venv",
            ".mypy_cache",
            ".pytest_cache",
            ".ruff_cache",
            ".tox",
            ".nox",
        ],
    ),
    ("go", "go.mod", &["vendor"]),
    ("php", "composer.json", &["vendor"]),
    ("ruby", "Gemfile", &[".bundle"]),
    ("deno", "deno.json", &["node_modules"]),
    ("deno", "deno.jsonc", &["node_modules"]),
    ("jvm", "build.gradle", &[".gradle", "build"]),
    ("jvm", "build.gradle.kts", &[".gradle", "build"]),
];

const UNIVERSAL_ENTRIES: &[&str] = &[".env*"];

/// Detect ecosystems present at the repo root and generate `.wt/symlinks` content.
pub fn generate_config(repo: &RepoRoot) -> String {
    let root = repo.as_ref();
    let mut output = String::new();

    output.push_str("# Generated by: wt setup\n");
    output.push_str("# Review this file and remove entries that don't apply to your workflow.\n");
    output.push_str("# Entries that don't exist in the main worktree are silently skipped\n");
    output.push_str("# during `wt add`, so false positives are harmless — they just add noise.\n");
    output.push_str("#\n");
    output.push_str("# Docs: https://github.com/kioku/wt-core#worktree-symlinks\n");
    output.push('\n');

    output.push_str("# universal\n");
    for entry in UNIVERSAL_ENTRIES {
        output.push_str(entry);
        output.push('\n');
    }

    let mut seen_ecosystems = BTreeSet::new();

    for (name, marker, entries) in ECOSYSTEM_MARKERS {
        if !root.join(marker).exists() {
            continue;
        }

        if !seen_ecosystems.insert(*name) {
            continue;
        }

        if entries.is_empty() {
            continue;
        }

        output.push('\n');
        output.push_str(&format!("# {name} (detected: {marker})\n"));
        for entry in *entries {
            output.push_str(entry);
            output.push('\n');
        }
    }

    // Terraform: check for *.tf files in root or infra/
    if !seen_ecosystems.contains("terraform") && has_tf_files(root) {
        output.push('\n');
        output.push_str("# terraform (detected: *.tf)\n");
        output.push_str(".terraform\n");
    }

    output
}

fn has_tf_files(root: &Path) -> bool {
    let check_dir = |dir: &Path| -> bool {
        fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .any(|e| e.file_name().to_string_lossy().ends_with(".tf"))
    };

    check_dir(root) || check_dir(&root.join("infra"))
}

/// Return the list of detected ecosystem names (for summary output).
pub fn detect_ecosystems(repo: &RepoRoot) -> Vec<String> {
    let root = repo.as_ref();
    let mut seen = BTreeSet::new();

    for (name, marker, _) in ECOSYSTEM_MARKERS {
        if root.join(marker).exists() {
            seen.insert(name.to_string());
        }
    }

    if has_tf_files(root) {
        seen.insert("terraform".to_string());
    }

    seen.into_iter().collect()
}

/// Ensure `.wt/symlinks.local` is listed in `.gitignore`.
///
/// Returns `true` if the entry was added, `false` if already present.
pub fn ensure_gitignore_entry(repo: &RepoRoot) -> io::Result<bool> {
    let gitignore = repo.as_ref().join(".gitignore");
    let entry = ".wt/symlinks.local";

    let content = fs::read_to_string(&gitignore).unwrap_or_default();

    if content.lines().any(|line| line.trim() == entry) {
        return Ok(false);
    }

    let mut new_content = content;
    if !new_content.is_empty() && !new_content.ends_with('\n') {
        new_content.push('\n');
    }
    new_content.push_str(entry);
    new_content.push('\n');

    fs::write(&gitignore, new_content)?;
    Ok(true)
}

/// Path to the shared config file.
pub fn config_path(repo: &RepoRoot) -> PathBuf {
    repo.as_ref().join(CONFIG_DIR).join(CONFIG_FILE)
}

/// Path to the config directory.
pub fn config_dir(repo: &RepoRoot) -> PathBuf {
    repo.as_ref().join(CONFIG_DIR)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::fs::symlink;

    fn make_temp_dir() -> tempfile::TempDir {
        tempfile::TempDir::new().expect("failed to create temp dir")
    }

    #[test]
    fn parse_config_skips_blanks_and_comments() {
        let dir = make_temp_dir();
        let path = dir.path().join("symlinks");
        fs::write(
            &path,
            "# comment\nnode_modules\n\n  .env  \n# another\ntarget\n",
        )
        .expect("write failed");

        let entries = parse_config_file(&path);
        assert_eq!(entries, vec!["node_modules", ".env", "target"]);
    }

    #[test]
    fn parse_config_missing_file_returns_empty() {
        let entries = parse_config_file(Path::new("/nonexistent/path"));
        assert!(entries.is_empty());
    }

    #[test]
    fn glob_matches_rejects_overlapping_prefix_suffix() {
        assert!(glob_matches(".env.*", ".env.local"));
        assert!(glob_matches(".env.*", ".env."));
        assert!(!glob_matches(".env.loc*.local", ".env.local"));
        assert!(glob_matches("a*", "abc"));
        assert!(glob_matches("*c", "abc"));
        assert!(!glob_matches("abc*def", "abcde"));
    }

    #[test]
    fn expand_entry_literal_returns_as_is() {
        let dir = make_temp_dir();
        let result = expand_entry(dir.path(), "node_modules");
        assert_eq!(result, vec![PathBuf::from("node_modules")]);
    }

    #[test]
    fn expand_entry_glob_enumerates_directory() {
        let dir = make_temp_dir();
        fs::create_dir_all(dir.path().join("apps/api")).expect("mkdir");
        fs::create_dir_all(dir.path().join("apps/web")).expect("mkdir");
        fs::write(dir.path().join("apps/api/.env"), "SECRET=1").expect("write");
        fs::write(dir.path().join("apps/web/.env"), "SECRET=2").expect("write");

        let mut result = expand_entry(dir.path(), "apps/*/.env");
        result.sort();
        assert_eq!(
            result,
            vec![
                PathBuf::from("apps/api/.env"),
                PathBuf::from("apps/web/.env"),
            ]
        );
    }

    #[test]
    fn expand_entry_glob_missing_dir_returns_empty() {
        let dir = make_temp_dir();
        let result = expand_entry(dir.path(), "nonexistent/*/.env");
        assert!(result.is_empty());
    }

    #[test]
    fn resolve_entries_filters_nonexistent() {
        let dir = make_temp_dir();
        let repo = RepoRoot(dir.path().to_path_buf());
        fs::create_dir(dir.path().join("node_modules")).expect("mkdir");

        let patterns = vec!["node_modules".to_string(), "target".to_string()];
        let resolved = resolve_entries(&repo, &patterns);
        assert_eq!(resolved, vec![PathBuf::from("node_modules")]);
    }

    #[test]
    fn resolve_entries_deduplicates() {
        let dir = make_temp_dir();
        let repo = RepoRoot(dir.path().to_path_buf());
        fs::create_dir(dir.path().join("node_modules")).expect("mkdir");

        let patterns = vec!["node_modules".to_string(), "node_modules".to_string()];
        let resolved = resolve_entries(&repo, &patterns);
        assert_eq!(resolved.len(), 1);
    }

    #[test]
    fn relative_path_sibling() {
        let result = relative_path(Path::new("/a/b"), Path::new("/a/c"));
        assert_eq!(result, PathBuf::from("../c"));
    }

    #[test]
    fn relative_path_nested() {
        let result = relative_path(
            Path::new("/repo/.worktrees/slug"),
            Path::new("/repo/node_modules"),
        );
        assert_eq!(result, PathBuf::from("../../node_modules"));
    }

    #[test]
    fn relative_path_deep_target() {
        let result = relative_path(
            Path::new("/repo/.worktrees/slug/apps/api"),
            Path::new("/repo/apps/api/.env"),
        );
        assert_eq!(result, PathBuf::from("../../../../apps/api/.env"));
    }

    #[test]
    fn create_symlinks_creates_link() {
        let dir = make_temp_dir();
        let repo_root = dir.path().join("repo");
        let wt_path = repo_root.join(".worktrees/feat--aaaa1111");

        fs::create_dir_all(&wt_path).expect("mkdir");
        fs::create_dir(repo_root.join("node_modules")).expect("mkdir");

        let repo = RepoRoot(repo_root);
        let entries = vec![PathBuf::from("node_modules")];
        let outcomes = create_symlinks(&repo, &wt_path, &entries);

        assert_eq!(outcomes.len(), 1);
        assert!(matches!(outcomes[0], SymlinkOutcome::Created(_)));

        let link = wt_path.join("node_modules");
        assert!(link
            .symlink_metadata()
            .expect("stat")
            .file_type()
            .is_symlink());
        assert!(link.exists(), "symlink should resolve to existing target");
    }

    #[test]
    fn create_symlinks_creates_file_link() {
        let dir = make_temp_dir();
        let repo_root = dir.path().join("repo");
        let wt_path = repo_root.join(".worktrees/feat--file1111");

        fs::create_dir_all(&wt_path).expect("mkdir");
        fs::write(repo_root.join(".env"), "SECRET=1").expect("write");

        let repo = RepoRoot(repo_root);
        let entries = vec![PathBuf::from(".env")];
        let outcomes = create_symlinks(&repo, &wt_path, &entries);

        assert_eq!(outcomes.len(), 1);
        assert!(matches!(outcomes[0], SymlinkOutcome::Created(_)));

        let link = wt_path.join(".env");
        assert!(link
            .symlink_metadata()
            .expect("stat")
            .file_type()
            .is_symlink());
        assert!(link.exists(), "symlink should resolve to existing target");
    }

    #[test]
    fn create_symlinks_skips_existing() {
        let dir = make_temp_dir();
        let repo_root = dir.path().join("repo");
        let wt_path = repo_root.join(".worktrees/feat--bbbb2222");

        fs::create_dir_all(&wt_path).expect("mkdir");
        fs::create_dir(repo_root.join("node_modules")).expect("mkdir");
        fs::create_dir(wt_path.join("node_modules")).expect("mkdir");

        let repo = RepoRoot(repo_root);
        let entries = vec![PathBuf::from("node_modules")];
        let outcomes = create_symlinks(&repo, &wt_path, &entries);

        assert!(matches!(outcomes[0], SymlinkOutcome::Skipped(_, _)));
    }

    #[test]
    fn create_symlinks_creates_intermediate_dirs() {
        let dir = make_temp_dir();
        let repo_root = dir.path().join("repo");
        let wt_path = repo_root.join(".worktrees/feat--cccc3333");

        fs::create_dir_all(&wt_path).expect("mkdir");
        fs::create_dir_all(repo_root.join("apps/api")).expect("mkdir");
        fs::write(repo_root.join("apps/api/.env"), "SECRET=x").expect("write");

        let repo = RepoRoot(repo_root);
        let entries = vec![PathBuf::from("apps/api/.env")];
        let outcomes = create_symlinks(&repo, &wt_path, &entries);

        assert!(matches!(outcomes[0], SymlinkOutcome::Created(_)));
        assert!(wt_path.join("apps/api/.env").exists());
    }

    #[cfg(unix)]
    #[test]
    fn create_symlinks_skips_dangling_symlink() {
        let dir = make_temp_dir();
        let repo_root = dir.path().join("repo");
        let wt_path = repo_root.join(".worktrees/feat--dddd4444");

        fs::create_dir_all(&wt_path).expect("mkdir");
        fs::create_dir(repo_root.join("node_modules")).expect("mkdir");
        symlink("/nonexistent", wt_path.join("node_modules")).expect("symlink");

        let repo = RepoRoot(repo_root);
        let entries = vec![PathBuf::from("node_modules")];
        let outcomes = create_symlinks(&repo, &wt_path, &entries);

        assert!(matches!(outcomes[0], SymlinkOutcome::Skipped(_, _)));
    }

    #[test]
    fn ensure_gitignore_creates_file_when_missing() {
        let dir = make_temp_dir();
        let repo = RepoRoot(dir.path().to_path_buf());

        let added = ensure_gitignore_entry(&repo).expect("io error");
        assert!(added);

        let content = fs::read_to_string(dir.path().join(".gitignore")).expect("read");
        assert_eq!(content, ".wt/symlinks.local\n");
    }

    #[test]
    fn ensure_gitignore_appends_when_not_present() {
        let dir = make_temp_dir();
        let repo = RepoRoot(dir.path().to_path_buf());
        fs::write(dir.path().join(".gitignore"), "node_modules/\n").expect("write");

        let added = ensure_gitignore_entry(&repo).expect("io error");
        assert!(added);

        let content = fs::read_to_string(dir.path().join(".gitignore")).expect("read");
        assert_eq!(content, "node_modules/\n.wt/symlinks.local\n");
    }

    #[test]
    fn ensure_gitignore_appends_newline_when_missing() {
        let dir = make_temp_dir();
        let repo = RepoRoot(dir.path().to_path_buf());
        fs::write(dir.path().join(".gitignore"), "node_modules/").expect("write");

        let added = ensure_gitignore_entry(&repo).expect("io error");
        assert!(added);

        let content = fs::read_to_string(dir.path().join(".gitignore")).expect("read");
        assert_eq!(content, "node_modules/\n.wt/symlinks.local\n");
    }

    #[test]
    fn ensure_gitignore_noop_when_present() {
        let dir = make_temp_dir();
        let repo = RepoRoot(dir.path().to_path_buf());
        fs::write(
            dir.path().join(".gitignore"),
            "node_modules/\n.wt/symlinks.local\n",
        )
        .expect("write");

        let added = ensure_gitignore_entry(&repo).expect("io error");
        assert!(!added);
    }

    #[test]
    fn generate_config_detects_node() {
        let dir = make_temp_dir();
        let repo = RepoRoot(dir.path().to_path_buf());
        fs::write(dir.path().join("package.json"), "{}").expect("write");

        let config = generate_config(&repo);
        assert!(config.contains("node_modules"));
        assert!(config.contains("# node (detected: package.json)"));
    }

    #[test]
    fn generate_config_detects_rust() {
        let dir = make_temp_dir();
        let repo = RepoRoot(dir.path().to_path_buf());
        fs::write(dir.path().join("Cargo.toml"), "[package]").expect("write");

        let config = generate_config(&repo);
        assert!(config.contains("target"));
        assert!(config.contains("# rust (detected: Cargo.toml)"));
    }

    #[test]
    fn generate_config_universal_always_present() {
        let dir = make_temp_dir();
        let repo = RepoRoot(dir.path().to_path_buf());

        let config = generate_config(&repo);
        assert!(config.contains(".env*"));
        assert!(config.contains("# universal"));
    }

    #[test]
    fn generate_config_multiple_ecosystems() {
        let dir = make_temp_dir();
        let repo = RepoRoot(dir.path().to_path_buf());
        fs::write(dir.path().join("package.json"), "{}").expect("write");
        fs::write(dir.path().join("Cargo.toml"), "[package]").expect("write");

        let config = generate_config(&repo);
        assert!(config.contains("node_modules"));
        assert!(config.contains("target"));
    }

    #[test]
    fn generate_config_python_deduplicates_markers() {
        let dir = make_temp_dir();
        let repo = RepoRoot(dir.path().to_path_buf());
        fs::write(dir.path().join("pyproject.toml"), "").expect("write");
        fs::write(dir.path().join("setup.py"), "").expect("write");

        let config = generate_config(&repo);
        let python_count = config.matches("# python").count();
        assert_eq!(python_count, 1);
    }

    #[test]
    fn generate_config_python_setup_py_emits_entries() {
        let dir = make_temp_dir();
        let repo = RepoRoot(dir.path().to_path_buf());
        fs::write(dir.path().join("setup.py"), "").expect("write");

        let config = generate_config(&repo);
        assert!(config.contains(".venv"), "setup.py should emit .venv");
        assert!(
            config.contains("# python (detected: setup.py)"),
            "should credit setup.py as the marker"
        );
    }

    #[test]
    fn detect_ecosystems_returns_names() {
        let dir = make_temp_dir();
        let repo = RepoRoot(dir.path().to_path_buf());
        fs::write(dir.path().join("package.json"), "{}").expect("write");
        fs::write(dir.path().join("Cargo.toml"), "[package]").expect("write");

        let ecosystems = detect_ecosystems(&repo);
        assert!(ecosystems.contains(&"node".to_string()));
        assert!(ecosystems.contains(&"rust".to_string()));
    }
}
