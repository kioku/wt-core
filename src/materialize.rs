/// Private early dispatcher: only the first raw argument selects the helper.
#[cfg(windows)]
pub(crate) fn run_cache_helper_if_requested() -> Option<crate::error::Result<i32>> {
    let mut args = std::env::args_os().skip(1);
    if args.next().as_deref() != Some(std::ffi::OsStr::new("__wt-cache-git")) {
        return None;
    }
    Some(
        cache_windows::run_helper(args)
            .map_err(|e| crate::error::AppError::git(format!("cache Git helper failed: {e}"))),
    )
}

#[cfg(windows)]
#[path = "cache_windows.rs"]
mod cache_windows;

use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use crate::cli::{MaterializeCopyMode, MaterializeMode};
use crate::error::{AppError, Result};
use crate::git;

const CACHE_LOCK_TIMEOUT: Duration = Duration::from_secs(30);
const CACHE_LOCK_RETRY: Duration = Duration::from_millis(50);
const PARALLEL_STATUS_THRESHOLD: usize = 1_000;
// Match final path components, not repository-specific directory names. The
// exclusion-only shard is the complement, including punctuation and Unicode.
const STATUS_PATH_GROUPS: [&[&str]; 4] = [
    &[":(top,glob)**/[a-eA-E]*"],
    &[":(top,glob)**/[f-mF-M]*"],
    &[":(top,glob)**/[n-rN-R]*"],
    &[
        ":(top,glob,exclude)**/[a-eA-E]*",
        ":(top,glob,exclude)**/[f-mF-M]*",
        ":(top,glob,exclude)**/[n-rN-R]*",
    ],
];

#[derive(Debug)]
pub struct MaterializeOptions {
    pub repo_slug: String,
    pub remote_url: Option<String>,
    pub ref_name: Option<String>,
    pub sha: String,
    pub cache_root: Option<PathBuf>,
    pub workspace_root: PathBuf,
    pub object_source: Option<PathBuf>,
    pub mode: MaterializeMode,
    pub copy_mode: MaterializeCopyMode,
}

#[derive(Debug)]
pub struct MaterializeResult {
    pub repository: String,
    pub workspace_path: PathBuf,
    pub cache_path: Option<PathBuf>,
    pub requested_ref: Option<String>,
    pub requested_sha: String,
    pub resolved_commit: String,
    pub mode: &'static str,
    pub cache_status: &'static str,
    pub source: &'static str,
    pub timings: MaterializeTimings,
}

#[derive(Debug, Default)]
pub struct MaterializeTimings {
    pub cache_lock: u64,
    pub cache_refresh: u64,
    pub workspace_checkout: u64,
    pub total: u64,
}

struct ValidatedOptions {
    copy_mode: MaterializeCopyMode,
    repo_slug: String,
    remote_url: Option<String>,
    ref_name: Option<String>,
    sha: String,
    cache_root: Option<PathBuf>,
    workspace_root: PathBuf,
    object_source: Option<PathBuf>,
}

/// Persistent lock files are never unlinked: removing one could split ownership
/// between different inodes. The separate lease survives the owner through Git
/// and descendants, while the parent lock excludes new cooperating owners.
struct CacheLock {
    parent: fs::File,
    #[cfg(unix)]
    lease: fs::File,
    #[cfg(unix)]
    lease_path: PathBuf,
    #[cfg(windows)]
    descendants: cache_windows::Descendants,
}

impl Drop for CacheLock {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            // SAFETY: the parent descriptor remains open throughout this call.
            unsafe { libc::flock(self.parent.as_raw_fd(), libc::LOCK_UN) };
        }
        // Do not explicitly unlock the lease: children share its open-file
        // description and must retain it until their last descriptor closes.
    }
}

impl CacheLock {
    fn acquire(path: &Path) -> Result<Self> {
        Self::acquire_for(path, CACHE_LOCK_TIMEOUT)
    }

    fn acquire_for(path: &Path, timeout: Duration) -> Result<Self> {
        if !cfg!(any(unix, windows)) {
            return Err(AppError::conflict(
                "cache descendant ownership is unsupported on this platform; use --object-source or omit --cache-root",
            ));
        }
        #[cfg(windows)]
        return Self::acquire_windows(path, timeout);
        #[cfg(not(windows))]
        {
            let parent = open_cache_lock(path)?;
            #[cfg(unix)]
            let lease_path = path.with_extension("lock.children");
            #[cfg(unix)]
            let lease = open_cache_lock(&lease_path)?;
            let started = Instant::now();
            loop {
                let parent_owned = crate::operation_state::try_lock_exclusive(&parent)
                    .map_err(|e| AppError::git(format!("cannot lock cache: {e}")))?;
                #[cfg(unix)]
                let children_finished = parent_owned
                    && crate::operation_state::try_lock_exclusive(&lease).map_err(|e| {
                        AppError::git(format!("cannot lock cache child lease: {e}"))
                    })?;
                #[cfg(not(any(unix, windows)))]
                let children_finished = false;
                if children_finished {
                    #[cfg(unix)]
                    unlock_cache_lease(&lease)?;
                    return Ok(Self {
                        parent,
                        #[cfg(unix)]
                        lease,
                        #[cfg(unix)]
                        lease_path,
                    });
                }
                #[cfg(unix)]
                if parent_owned {
                    use std::os::fd::AsRawFd;
                    // SAFETY: parent is open and owned by this acquisition.
                    unsafe { libc::flock(parent.as_raw_fd(), libc::LOCK_UN) };
                }
                if started.elapsed() >= timeout {
                    return Err(AppError::conflict(format!(
                    "timed out waiting for cache lock {}; wait for the owner and its Git descendants to finish; do not remove lock files",
                    path.display()
                )));
                }
                thread::sleep(CACHE_LOCK_RETRY);
            }
        }
    }

    #[cfg(windows)]
    fn acquire_windows(path: &Path, timeout: Duration) -> Result<Self> {
        let started = Instant::now();
        loop {
            match cache_windows::open_file(path) {
                Ok(parent) => {
                    let descendants = cache_windows::Descendants::open(&parent).map_err(|e| {
                        AppError::git(format!("cannot open cache descendant job: {e}"))
                    })?;
                    if descendants.is_empty().map_err(|e| {
                        AppError::git(format!("cannot query cache descendant job: {e}"))
                    })? {
                        return Ok(Self {
                            parent,
                            descendants,
                        });
                    }
                    // Drop share denial before retrying; retain no byte-range lock.
                }
                Err(e) if e.raw_os_error() == Some(32) => {}
                Err(e) => {
                    return Err(AppError::conflict(format!(
                        "cannot open cache lock safely: {e}"
                    )))
                }
            }
            if started.elapsed() >= timeout {
                return Err(AppError::conflict(format!("timed out waiting for cache lock {}; wait for the owner and its Git descendants to finish; do not remove lock files", path.display())));
            }
            thread::sleep(CACHE_LOCK_RETRY);
        }
    }

    fn output(&self, command: &mut Command) -> io::Result<std::process::Output> {
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            use std::os::unix::process::CommandExt;
            // A fresh open-file description per child avoids sharing the probe's
            // exclusive lock with descendants. Parent ownership excludes new owners.
            let child_lease =
                open_cache_lock(&self.lease_path).map_err(|e| io::Error::other(e.to_string()))?;
            // SAFETY: child_lease is open throughout flock and command execution.
            if unsafe { libc::flock(child_lease.as_raw_fd(), libc::LOCK_SH) } != 0 {
                return Err(io::Error::last_os_error());
            }
            let descriptor = child_lease.as_raw_fd();
            // SAFETY: only async-signal-safe fcntl runs after fork. The lease
            // stays open until output returns, and no path is reopened here.
            unsafe {
                command.pre_exec(move || {
                    if libc::fcntl(descriptor, libc::F_SETFD, 0) < 0 {
                        return Err(io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
            let output = command.output();
            // Close only: explicit unlock would release descendants' shared OFD.
            drop(child_lease);
            output
        }
        #[cfg(windows)]
        {
            cache_windows::output(&self.parent, command)
        }
        #[cfg(not(any(unix, windows)))]
        command.output()
    }

    /// Probe from an otherwise unlocked OFD, after the parent's child FD closes.
    fn wait_for_descendants(&self, timeout: Duration) -> Result<()> {
        let started = Instant::now();
        loop {
            #[cfg(unix)]
            if crate::operation_state::try_lock_exclusive(&self.lease)
                .map_err(|e| AppError::git(format!("cannot probe cache descendants: {e}")))?
            {
                return unlock_cache_lease(&self.lease);
            }
            #[cfg(windows)]
            if self
                .descendants
                .is_empty()
                .map_err(|e| AppError::git(format!("cannot probe cache descendants: {e}")))?
            {
                return Ok(());
            }
            if started.elapsed() >= timeout {
                return Err(AppError::conflict(
                    "timed out waiting for cache Git descendants; snapshot refused",
                ));
            }
            thread::sleep(CACHE_LOCK_RETRY);
        }
    }
}

#[cfg(unix)]
fn unlock_cache_lease(file: &fs::File) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        // SAFETY: the probe descriptor remains open; no child inherits this OFD.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) } != 0 {
            return Err(AppError::git(format!(
                "cannot unlock cache probe: {}",
                io::Error::last_os_error()
            )));
        }
    }
    #[cfg(not(unix))]
    let _ = file;
    Ok(())
}

#[cfg(not(windows))]
fn open_cache_lock(path: &Path) -> Result<fs::File> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => {
            return Err(AppError::conflict(format!(
                "legacy cache directory lock {} requires manual recovery: stop all users of this cache, confirm their Git descendants have exited, inspect and preserve unknown contents, then remove only the confirmed obsolete directory and retry",
                path.display()
            )));
        }
        Ok(metadata) if !metadata.is_file() || metadata.file_type().is_symlink() => {
            return Err(AppError::conflict(
                "cache lock must be a private regular file",
            ));
        }
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(AppError::git(format!("cannot inspect cache lock: {e}"))),
    }
    let mut options = fs::OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options
        .open(path)
        .map_err(|e| AppError::conflict(format!("cannot open cache lock safely: {e}")))?;
    let metadata = file
        .metadata()
        .map_err(|e| AppError::git(format!("cannot inspect opened cache lock: {e}")))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        // SAFETY: geteuid has no preconditions.
        let owner = unsafe { libc::geteuid() };
        if metadata.uid() != owner || metadata.mode() & 0o777 != 0o600 || metadata.nlink() != 1 {
            return Err(AppError::conflict(
                "cache lock has unsafe ownership, permissions, or links",
            ));
        }
    }
    if !metadata.is_file() || metadata.len() != 0 {
        return Err(AppError::conflict("cache lock has unknown native state; preserve it and inspect manually with all cache users stopped"));
    }
    let opened = same_file::Handle::from_file(
        file.try_clone()
            .map_err(|e| AppError::git(format!("cannot retain cache lock identity: {e}")))?,
    )
    .map_err(|e| AppError::git(format!("cannot inspect cache lock identity: {e}")))?;
    let current = same_file::Handle::from_path(path)
        .map_err(|e| AppError::git(format!("cannot inspect cache lock entry: {e}")))?;
    if opened != current {
        return Err(AppError::conflict(
            "cache lock entry changed during acquisition",
        ));
    }
    Ok(file)
}

pub fn materialize(options: MaterializeOptions) -> Result<MaterializeResult> {
    let started = Instant::now();
    let request = validate_options(options)?;
    ensure_workspace_available(&request.workspace_root)?;

    let mut timings = MaterializeTimings::default();
    let cache_path = request
        .cache_root
        .as_ref()
        .map(|root| root.join(format!("{}.git", safe_repo_key(&request.repo_slug))));

    let source_result = materialize_from_best_source(&request, &cache_path, &mut timings)?;
    let resolved_commit = verify_workspace(&request.workspace_root, &request.sha)?;
    if let Some(root) = &source_result.workspace_identity {
        verify_new_workspace_identity(&request.workspace_root, root)?;
    }
    timings.total = elapsed_ms(started);

    Ok(MaterializeResult {
        repository: request.repo_slug,
        workspace_path: request.workspace_root,
        cache_path,
        requested_ref: request.ref_name,
        requested_sha: request.sha,
        resolved_commit,
        mode: "detached",
        cache_status: source_result.cache_status,
        source: source_result.source,
        timings,
    })
}

struct SourceResult {
    cache_status: &'static str,
    source: &'static str,
    // Retain root identity through the public entry point's final Git
    // verification, not just through the local clone helper.
    workspace_identity: Option<same_file::Handle>,
}

fn materialize_from_best_source(
    request: &ValidatedOptions,
    cache_path: &Option<PathBuf>,
    timings: &mut MaterializeTimings,
) -> Result<SourceResult> {
    if let Some(source) = &request.object_source {
        let workspace_identity = materialize_from_object_source(source, request, timings)?;
        return Ok(SourceResult {
            cache_status: "bypassed",
            source: "object_source",
            workspace_identity,
        });
    }

    if let Some(cache_path) = cache_path {
        let (cache_status, workspace_identity) =
            materialize_from_cache(cache_path, request, timings)?;
        return Ok(SourceResult {
            cache_status,
            source: "cache",
            workspace_identity,
        });
    }

    materialize_from_remote(request, timings)?;
    Ok(SourceResult {
        cache_status: "bypassed",
        source: "remote",
        workspace_identity: None,
    })
}

fn materialize_from_object_source(
    source: &Path,
    request: &ValidatedOptions,
    timings: &mut MaterializeTimings,
) -> Result<Option<same_file::Handle>> {
    verify_bare_repo(source)?;
    verify_commit_exists(source, &request.sha)?;
    let started = Instant::now();
    let identity = clone_local_bare(
        source,
        &request.workspace_root,
        &request.sha,
        request.copy_mode,
    )?;
    timings.workspace_checkout = elapsed_ms(started);
    Ok(identity)
}

fn materialize_from_cache(
    cache_path: &Path,
    request: &ValidatedOptions,
    timings: &mut MaterializeTimings,
) -> Result<(&'static str, Option<same_file::Handle>)> {
    let remote_url = request
        .remote_url
        .as_deref()
        .ok_or_else(|| AppError::usage("--remote-url is required without --object-source"))?;
    let cache_root = request
        .cache_root
        .as_ref()
        .ok_or_else(|| AppError::invariant("cache path exists without cache root"))?;

    let workspace: PathBuf = request.workspace_root.components().collect();
    let existing_root = if workspace.exists() {
        ensure_existing_workspace_available(&workspace)?;
        Some(same_file::Handle::from_path(&workspace).map_err(|e| {
            AppError::git(format!("cannot capture existing workspace identity: {e}"))
        })?)
    } else {
        None
    };
    create_dir_all(cache_root, "cache root")?;
    let lock_path = cache_path.with_extension("git.lock");
    let lock_started = Instant::now();
    let lock = CacheLock::acquire(&lock_path)?;
    timings.cache_lock = elapsed_ms(lock_started);

    let refresh_started = Instant::now();
    let cache_status = refresh_cache(cache_path, remote_url, &lock)?;
    timings.cache_refresh = elapsed_ms(refresh_started);

    run_cache_git(
        vec![
            os("--git-dir"),
            os(cache_path),
            os("cat-file"),
            os("-e"),
            os(format!("{}^{{commit}}", request.sha)),
        ],
        &lock,
    )?;
    let checkout_started = Instant::now();
    let snapshot = tempfile::Builder::new()
        .prefix(".wt-cache-snapshot-")
        .tempdir_in(cache_root)
        .map_err(|e| AppError::git(format!("cannot stage cache snapshot: {e}")))?;
    let staged = snapshot.path().join("repository");
    let source = independent_local_source(cache_path)?;
    clone_independent_snapshot(&source, &staged, request.copy_mode, Some(&lock))?;
    // All source reads are complete. Checkout, both verification passes, and
    // copying into an existing destination use only the independent snapshot.
    drop(lock);
    let identity = if let Some(root) = existing_root {
        checkout_detached(&staged, &request.sha)?;
        verify_workspace(&staged, &request.sha)?;
        populate_verified_checkout(&staged, &workspace, &request.sha, request.copy_mode, &root)?;
        Some(root)
    } else {
        clone_local_bare(
            &staged.join(".git"),
            &request.workspace_root,
            &request.sha,
            request.copy_mode,
        )?
    };
    run_git_owned(
        vec![os("remote"), os("set-url"), os("origin"), os(&source)],
        Some(&request.workspace_root),
    )?;
    timings.workspace_checkout = elapsed_ms(checkout_started);
    Ok((cache_status, identity))
}

fn materialize_from_remote(
    request: &ValidatedOptions,
    timings: &mut MaterializeTimings,
) -> Result<()> {
    let remote_url = request
        .remote_url
        .as_deref()
        .ok_or_else(|| AppError::usage("--remote-url is required without --object-source"))?;
    let started = Instant::now();
    checkout_from_remote(
        remote_url,
        request.ref_name.as_deref(),
        &request.workspace_root,
        &request.sha,
    )?;
    timings.workspace_checkout = elapsed_ms(started);
    Ok(())
}

fn validate_options(options: MaterializeOptions) -> Result<ValidatedOptions> {
    if options.mode != MaterializeMode::Detached {
        return Err(AppError::usage(
            "only detached materialize mode is supported".to_string(),
        ));
    }

    let repo_slug = validate_repo_slug(&options.repo_slug)?;
    validate_optional_remote(options.remote_url.as_deref())?;
    validate_optional_ref(options.ref_name.as_deref())?;
    validate_sha(&options.sha)?;
    validate_absolute_path(&options.workspace_root, "--workspace-root")?;
    validate_optional_absolute_path(options.cache_root.as_deref(), "--cache-root")?;
    validate_optional_absolute_path(options.object_source.as_deref(), "--object-source")?;

    if options.object_source.is_none() && options.remote_url.is_none() {
        return Err(AppError::usage(
            "--remote-url is required unless --object-source is provided".to_string(),
        ));
    }

    Ok(ValidatedOptions {
        repo_slug,
        copy_mode: options.copy_mode,
        remote_url: options.remote_url,
        ref_name: options.ref_name,
        sha: options.sha,
        cache_root: options.cache_root,
        workspace_root: options.workspace_root,
        object_source: options.object_source,
    })
}

fn validate_repo_slug(slug: &str) -> Result<String> {
    if slug.is_empty() || Path::new(slug).is_absolute() || slug_contains_control(slug) {
        return Err(AppError::usage("invalid --repo-slug".to_string()));
    }

    let segments: Vec<&str> = slug.split('/').collect();
    if segments.len() != 2 || segments.iter().any(invalid_slug_segment) {
        return Err(AppError::usage("invalid --repo-slug".to_string()));
    }

    Ok(slug.to_string())
}

fn invalid_slug_segment(segment: &&str) -> bool {
    segment.is_empty()
        || *segment == "."
        || *segment == ".."
        || !segment
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-'))
}

fn slug_contains_control(slug: &str) -> bool {
    slug.chars().any(char::is_control)
}

fn validate_optional_remote(remote_url: Option<&str>) -> Result<()> {
    match remote_url {
        Some(url) => validate_remote_url(url),
        None => Ok(()),
    }
}

fn validate_remote_url(url: &str) -> Result<()> {
    if url.is_empty() || url.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err(AppError::usage("invalid --remote-url".to_string()));
    }

    if !has_supported_scheme(url) {
        return Err(AppError::usage(
            "unsupported --remote-url scheme; use https, http, ssh, git, or file".to_string(),
        ));
    }

    if has_credential_userinfo(url) || has_credential_query(url) {
        return Err(AppError::usage(
            "--remote-url must not include credentials".to_string(),
        ));
    }

    Ok(())
}

fn has_supported_scheme(url: &str) -> bool {
    ["https://", "http://", "ssh://", "git://", "file://"]
        .iter()
        .any(|scheme| url.starts_with(scheme))
}

fn has_credential_userinfo(url: &str) -> bool {
    let Some((_, rest)) = url.split_once("://") else {
        return false;
    };
    let authority = rest
        .split(['/', '?', '#'])
        .next()
        .map(str::to_string)
        .unwrap_or_default();
    !authority.is_empty() && authority.contains('@')
}

fn has_credential_query(url: &str) -> bool {
    let Some((_, after_query)) = url.split_once('?') else {
        return false;
    };
    after_query
        .split('#')
        .next()
        .unwrap_or_default()
        .split('&')
        .any(query_param_is_credential)
}

fn query_param_is_credential(param: &str) -> bool {
    let key = param
        .split('=')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    [
        "token",
        "password",
        "secret",
        "credential",
        "access_key",
        "private_key",
        "auth",
    ]
    .iter()
    .any(|needle| key.contains(needle))
}

fn validate_optional_ref(ref_name: Option<&str>) -> Result<()> {
    match ref_name {
        Some(name) => validate_ref(name),
        None => Ok(()),
    }
}

fn validate_ref(ref_name: &str) -> Result<()> {
    if ref_name.is_empty() || ref_name.starts_with('-') || ref_name.starts_with('/') {
        return Err(AppError::usage("invalid --ref".to_string()));
    }

    if ref_name.ends_with('/') || ref_name.ends_with(".lock") || ref_name.contains("//") {
        return Err(AppError::usage("invalid --ref".to_string()));
    }

    if ref_name.contains("..") || ref_name.contains("@{") || ref_name.contains('\\') {
        return Err(AppError::usage("invalid --ref".to_string()));
    }

    if ref_name.chars().any(invalid_ref_char) || ref_name.split('/').any(invalid_ref_segment) {
        return Err(AppError::usage("invalid --ref".to_string()));
    }

    Ok(())
}

fn invalid_ref_char(ch: char) -> bool {
    ch.is_control() || ch.is_whitespace() || matches!(ch, ':' | '?' | '*' | '[' | '^' | '~')
}

fn invalid_ref_segment(segment: &str) -> bool {
    segment.is_empty()
        || segment == "."
        || segment == ".."
        || segment == "@"
        || segment.starts_with('.')
        || segment.ends_with('.')
        || segment.ends_with(".lock")
}

fn validate_sha(sha: &str) -> Result<()> {
    let valid = sha.len() == 40 && sha.chars().all(|c| c.is_ascii_hexdigit());
    if valid {
        return Ok(());
    }
    Err(AppError::usage(
        "--sha must be a full 40-character hexadecimal commit SHA".to_string(),
    ))
}

fn validate_optional_absolute_path(path: Option<&Path>, flag: &str) -> Result<()> {
    match path {
        Some(path) => validate_absolute_path(path, flag),
        None => Ok(()),
    }
}

fn validate_absolute_path(path: &Path, flag: &str) -> Result<()> {
    if path.is_absolute() {
        return Ok(());
    }
    Err(AppError::usage(format!("{flag} must be an absolute path")))
}

fn ensure_workspace_available(path: &Path) -> Result<()> {
    if path.exists() {
        return ensure_existing_workspace_available(path);
    }

    let parent = path
        .parent()
        .ok_or_else(|| AppError::usage("--workspace-root has no parent directory"))?;
    create_dir_all(parent, "workspace parent")
}

fn ensure_existing_workspace_available(path: &Path) -> Result<()> {
    if !path.is_dir() {
        return Err(AppError::conflict(format!(
            "workspace path exists and is not a directory: {}",
            path.display()
        )));
    }

    let mut entries = fs::read_dir(path).map_err(|e| {
        AppError::conflict(format!(
            "cannot inspect workspace path {}: {e}",
            path.display()
        ))
    })?;
    match entries.next() {
        Some(Ok(_)) | Some(Err(_)) => Err(AppError::conflict(format!(
            "workspace path already exists and is not empty: {}",
            path.display()
        ))),
        None => Ok(()),
    }
}

fn refresh_cache(cache_path: &Path, remote_url: &str, lock: &CacheLock) -> Result<&'static str> {
    if cache_path.exists() {
        refresh_existing_cache(cache_path, remote_url, lock)?;
        return Ok("refreshed");
    }

    let parent = cache_path
        .parent()
        .ok_or_else(|| AppError::invariant("cache path has no parent"))?;
    create_dir_all(parent, "cache parent")?;
    run_cache_git(
        vec![
            os("clone"),
            os("--mirror"),
            os(remote_url),
            cache_path.as_os_str().to_os_string(),
        ],
        lock,
    )?;
    Ok("cold")
}

fn refresh_existing_cache(cache_path: &Path, remote_url: &str, lock: &CacheLock) -> Result<()> {
    if !cache_path.is_dir() {
        return Err(AppError::conflict(format!(
            "cache path exists and is not a directory: {}",
            cache_path.display()
        )));
    }

    let source = independent_local_source(cache_path)?;
    if run_cache_git(
        vec![
            os("--git-dir"),
            os(&source),
            os("rev-parse"),
            os("--is-bare-repository"),
        ],
        lock,
    )? != "true"
    {
        return Err(AppError::usage("cache is not a bare Git repository"));
    }
    run_cache_git(
        vec![
            os("--git-dir"),
            cache_path.as_os_str().to_os_string(),
            os("remote"),
            os("set-url"),
            os("origin"),
            os(remote_url),
        ],
        lock,
    )?;
    run_cache_git(
        vec![
            os("--git-dir"),
            cache_path.as_os_str().to_os_string(),
            os("fetch"),
            os("--prune"),
            os("origin"),
            os("+refs/heads/*:refs/heads/*"),
            os("+refs/tags/*:refs/tags/*"),
        ],
        lock,
    )?;
    Ok(())
}

fn verify_bare_repo(path: &Path) -> Result<()> {
    if !path.is_dir() {
        return Err(AppError::usage(format!(
            "bare repository path does not exist: {}",
            path.display()
        )));
    }

    let path = independent_local_source(path)?;
    let output = run_git_owned(
        vec![
            os("--git-dir"),
            path.as_os_str().to_os_string(),
            os("rev-parse"),
            os("--is-bare-repository"),
        ],
        None,
    )?;
    if output == "true" {
        return Ok(());
    }

    Err(AppError::usage(format!(
        "path is not a bare Git repository: {}",
        path.display()
    )))
}

fn verify_commit_exists(git_dir: &Path, sha: &str) -> Result<()> {
    run_git_owned(
        vec![
            os("--git-dir"),
            git_dir.as_os_str().to_os_string(),
            os("cat-file"),
            os("-e"),
            os(format!("{sha}^{{commit}}")),
        ],
        None,
    )?;
    Ok(())
}

/// Local clones must own their objects, including when the source uses alternates.
/// Git additionally rejects foreign-owned sources and symlinks in the object tree.
fn independent_local_source(source: &Path) -> Result<PathBuf> {
    // Strip trailing separators and `.` components before lstat: otherwise
    // the OS follows a final repository symlink despite symlink_metadata.
    let source: PathBuf = source.components().collect();
    let metadata = fs::symlink_metadata(&source)
        .map_err(|e| AppError::git(format!("cannot inspect local source: {e}")))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(AppError::usage(
            "local source must be a non-symlink directory",
        ));
    }
    let source = dunce::canonicalize(&source)
        .map_err(|e| AppError::git(format!("cannot resolve local source: {e}")))?;
    crate::object_copy::validate_source(&source)?;
    match fs::symlink_metadata(source.join("objects/info/alternates")) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(source),
        Err(e) => Err(AppError::git(format!(
            "cannot inspect source alternates: {e}"
        ))),
        Ok(_) => Err(AppError::usage(
            "local source must not use object alternates",
        )),
    }
}

fn clone_local_bare(
    source: &Path,
    workspace: &Path,
    sha: &str,
    copy_mode: MaterializeCopyMode,
) -> Result<Option<same_file::Handle>> {
    // Treat a final `/.` as the directory entry for creation and cloning.
    let normalized_workspace: PathBuf = workspace.components().collect();
    let workspace = normalized_workspace.as_path();
    ensure_workspace_available(workspace)?;
    let source = independent_local_source(source)?;
    create_workspace_parent(workspace)?;
    let parent = workspace
        .parent()
        .ok_or_else(|| AppError::usage("--workspace-root has no parent directory"))?;
    if !workspace.exists() {
        return clone_new_workspace(&source, workspace, sha, copy_mode).map(Some);
    }
    // Preverify existing-directory checkouts privately, and remove only this
    // task-owned staging tree on failure.
    let staging = tempfile::Builder::new()
        .prefix(".wt-materialize-")
        .tempdir_in(parent)
        .map_err(|e| AppError::git(format!("cannot stage local checkout: {e}")))?;
    let checkout = staging.path().join("workspace");
    clone_independent_checkout(&source, &checkout, sha, copy_mode)?;
    ensure_workspace_available(workspace)?;
    // Populate the original directory in place: replacing it would lose
    // native ACLs, security labels, and other filesystem-specific controls.
    // Copy the complete verified snapshot, including objects unrelated to HEAD.
    // A protocol clone would silently discard unreachable objects. Keep the
    // selected copy mode for this final transfer as well as private staging.
    // Do not recursively clean this user-owned destination on failure.
    clone_independent_checkout(&checkout.join(".git"), workspace, sha, copy_mode)?;
    run_git_owned(
        vec![
            os("remote"),
            os("set-url"),
            os("origin"),
            source.as_os_str().to_os_string(),
        ],
        Some(workspace),
    )?;
    independent_local_source(&workspace.join(".git"))?;
    checkout_detached(workspace, sha)?;
    verify_workspace(workspace, sha)?;
    Ok(None)
}

/// Populate in place from a privately verified independent checkout. Regenerate
/// files under the destination's native controls; never transplant staged files
/// or recursively clean a user-owned root after a failure.
fn populate_verified_checkout(
    checkout: &Path,
    workspace: &Path,
    sha: &str,
    copy_mode: MaterializeCopyMode,
    root: &same_file::Handle,
) -> Result<()> {
    verify_new_workspace_identity(workspace, root)?;
    ensure_existing_workspace_available(workspace)?;
    clone_independent_checkout(&checkout.join(".git"), workspace, sha, copy_mode)?;
    verify_new_workspace_identity(workspace, root)?;
    Ok(())
}

/// Claim a fresh root at its final path and clean only our own output on failure.
fn clone_new_workspace(
    source: &Path,
    workspace: &Path,
    sha: &str,
    copy_mode: MaterializeCopyMode,
) -> Result<same_file::Handle> {
    // Creating under the actual parent preserves one-generation native ACLs and
    // path-based labels; renaming a nested checkout would not re-inherit them.
    fs::create_dir(workspace).map_err(|e| match e.kind() {
        io::ErrorKind::AlreadyExists => {
            AppError::conflict(format!("cannot claim new workspace: {e}"))
        }
        _ => AppError::git(format!("cannot create new workspace: {e}")),
    })?;
    // Keep the handle open so directory IDs cannot be recycled during Git's
    // execution. If capture fails, preserve the root rather than guess ownership.
    let root = same_file::Handle::from_path(workspace)
        .map_err(|e| AppError::git(format!("cannot capture new workspace identity: {e}")))?;
    let result = clone_independent_checkout(source, workspace, sha, copy_mode);
    verify_new_workspace_identity(workspace, &root)?;
    if result.is_err() {
        fs::remove_dir_all(workspace)
            .map_err(|e| AppError::git(format!("cannot clean failed new workspace: {e}")))?;
    }
    result.map(|()| root)
}

/// Fail closed if the requested entry disappeared, became a symlink, or changed
/// identity. This bounds ordinary root-replacement cleanup; it is not a lock
/// against a same-privilege actor racing every filesystem operation.
fn verify_new_workspace_identity(workspace: &Path, root: &same_file::Handle) -> Result<()> {
    // Inspect the root entry itself even when the public request ends in `/`
    // or `/.`; those spellings otherwise make lstat follow a replacement link.
    let workspace: PathBuf = workspace.components().collect();
    let workspace = workspace.as_path();
    let metadata = fs::symlink_metadata(workspace)
        .map_err(|e| AppError::git(format!("cannot inspect new workspace identity: {e}")))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(AppError::conflict(
            "new workspace root was replaced; preserving unexpected destination",
        ));
    }
    let current = same_file::Handle::from_path(workspace)
        .map_err(|e| AppError::git(format!("cannot inspect new workspace identity: {e}")))?;
    if &current != root {
        return Err(AppError::conflict(
            "new workspace root was replaced; preserving unexpected destination",
        ));
    }
    Ok(())
}

/// Copy objects and mutable metadata independently, then verify before success.
fn clone_independent_checkout(
    source: &Path,
    workspace: &Path,
    sha: &str,
    copy_mode: MaterializeCopyMode,
) -> Result<()> {
    clone_independent_snapshot(source, workspace, copy_mode, None)?;
    checkout_detached(workspace, sha)?;
    verify_workspace(workspace, sha)?;
    Ok(())
}

/// Finish every source read before releasing cache ownership.
fn clone_independent_snapshot(
    source: &Path,
    workspace: &Path,
    copy_mode: MaterializeCopyMode,
    lock: Option<&CacheLock>,
) -> Result<()> {
    if let Some(lock) = lock {
        lock.wait_for_descendants(CACHE_LOCK_TIMEOUT)?;
    }
    run_snapshot_git(
        vec![
            os("clone"),
            os("--local"),
            os("--shared"),
            os("--no-checkout"),
            source.as_os_str().to_os_string(),
            workspace.as_os_str().to_os_string(),
        ],
        lock,
    )?;
    // Git creates metadata, but the temporary alternate is replaced with all
    // source objects (including unreachable ones), without repacking.
    crate::object_copy::copy_tree(
        &source.join("objects"),
        &workspace.join(".git/objects"),
        copy_mode,
    )?;
    fs::remove_file(workspace.join(".git/objects/info/alternates"))
        .map_err(|e| AppError::git(format!("cannot remove temporary alternate: {e}")))?;
    // Defense in depth: never publish a clone that depends on source objects.
    independent_local_source(&workspace.join(".git"))?;
    Ok(())
}

fn checkout_from_remote(
    remote_url: &str,
    ref_name: Option<&str>,
    workspace: &Path,
    sha: &str,
) -> Result<()> {
    create_dir_all(workspace, "workspace")?;
    run_git_owned(vec![os("init")], Some(workspace))?;
    run_git_owned(
        vec![os("remote"), os("add"), os("origin"), os(remote_url)],
        Some(workspace),
    )?;
    let fetch_target = ref_name.unwrap_or(sha);
    run_git_owned(
        vec![os("fetch"), os("origin"), os(fetch_target)],
        Some(workspace),
    )?;
    checkout_detached(workspace, sha)
}

fn checkout_detached(workspace: &Path, sha: &str) -> Result<()> {
    // Bound parallelism instead of using every available CPU. Git retains its
    // small-checkout threshold and handles ineligible entries serially. Keep
    // this command-local so workspace and caller configuration are unchanged.
    run_git_owned(
        vec![
            os("-c"),
            os("checkout.workers=4"),
            // Never create assume-unchanged entries through caller preferences:
            // both verification passes must retain Git's content validation.
            os("-c"),
            os("core.ignoreStat=false"),
            os("checkout"),
            os("--detach"),
            os(sha),
        ],
        Some(workspace),
    )?;
    Ok(())
}

fn verify_workspace(workspace: &Path, sha: &str) -> Result<String> {
    let head = run_git_owned(vec![os("rev-parse"), os("HEAD")], Some(workspace))?;
    if head != sha {
        return Err(AppError::invariant(format!(
            "materialized HEAD {} did not match requested SHA {}",
            head, sha
        )));
    }

    if workspace_is_dirty(workspace)? {
        return Err(AppError::conflict(
            "materialized workspace is not clean".to_string(),
        ));
    }

    if run_git_success(
        vec![os("symbolic-ref"), os("-q"), os("HEAD")],
        Some(workspace),
    )? {
        return Err(AppError::invariant(
            "materialized workspace is not detached".to_string(),
        ));
    }

    Ok(head)
}

/// Keep Git's racy-index/content validation, but split large checks across four
/// read-only processes. Optional index writes must be disabled: concurrent
/// writers contend on index.lock and can revalidate other shards while writing.
/// Both public and prepublication verification still run; no timestamps or
/// assume-unchanged flags are modified. Small checkouts retain normal status.
fn workspace_is_dirty(workspace: &Path) -> Result<bool> {
    let entries = run_git_owned(vec![os("ls-files"), os("-z")], Some(workspace))?;
    if entries.bytes().filter(|byte| *byte == 0).count() < PARALLEL_STATUS_THRESHOLD {
        let status = run_git_owned(
            vec![
                os("-c"),
                os("core.fsmonitor=false"),
                os("status"),
                os("--porcelain"),
                os("--untracked-files=all"),
            ],
            Some(workspace),
        )?;
        return Ok(!status.is_empty());
    }

    let results = thread::scope(|scope| {
        let workers: Vec<_> = STATUS_PATH_GROUPS
            .iter()
            .map(|paths| scope.spawn(move || status_for_paths(workspace, paths)))
            .collect();
        // Join every worker even if another reports dirt or a Git failure.
        workers
            .into_iter()
            .map(|worker| {
                worker.join().unwrap_or_else(|_| {
                    Err(AppError::invariant("workspace status worker panicked"))
                })
            })
            .collect::<Vec<_>>()
    });
    let statuses = results.into_iter().collect::<Result<Vec<_>>>()?;
    Ok(statuses.iter().any(|status| !status.is_empty()))
}

fn status_for_paths(workspace: &Path, paths: &[&str]) -> Result<String> {
    let mut args = vec![
        os("--no-optional-locks"),
        // Verify contents without trusting a caller's potentially stale monitor.
        os("-c"),
        os("core.fsmonitor=false"),
        os("status"),
        os("--porcelain"),
        // Cleanliness is an invariant, independent of status display preferences.
        os("--untracked-files=all"),
        os("--"),
    ];
    args.extend(paths.iter().map(os));
    let mut command = Command::new("git");
    command.args(args);
    // These are internal magic pathspecs. In particular, inherited literal
    // mode would treat every shard as a nonexistent literal path and falsely
    // report a dirty workspace clean.
    for name in [
        "GIT_LITERAL_PATHSPECS",
        "GIT_GLOB_PATHSPECS",
        "GIT_NOGLOB_PATHSPECS",
        "GIT_ICASE_PATHSPECS",
    ] {
        command.env_remove(name);
    }
    run_git_command(command, Some(workspace))
}

fn run_cache_git(args: Vec<OsString>, lock: &CacheLock) -> Result<String> {
    run_snapshot_git(args, Some(lock))
}

fn run_snapshot_git(args: Vec<OsString>, lock: Option<&CacheLock>) -> Result<String> {
    let mut command = Command::new("git");
    command.args(args);
    git::sanitize_git_environment(&mut command);
    let output = match lock {
        Some(lock) => lock.output(&mut command),
        None => command.output(),
    }
    .map_err(|e| AppError::git(format!("failed to run git: {e}")))?;
    if let Some(lock) = lock {
        lock.wait_for_descendants(CACHE_LOCK_TIMEOUT)?;
    }
    if !output.status.success() {
        return Err(AppError::git(redact_credentials(
            String::from_utf8_lossy(&output.stderr).trim(),
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn run_git_owned(args: Vec<OsString>, cwd: Option<&Path>) -> Result<String> {
    let mut command = Command::new("git");
    command.args(args);
    run_git_command(command, cwd)
}

fn run_git_command(mut command: Command, cwd: Option<&Path>) -> Result<String> {
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    git::sanitize_git_environment(&mut command);

    let output = command
        .output()
        .map_err(|e| AppError::git(format!("failed to run git: {e}")))?;
    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).trim().to_string());
    }

    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    Err(AppError::git(redact_credentials(&stderr)))
}

/// `symbolic-ref -q` uses exit 1 for a detached HEAD; other failures are errors.
fn run_git_success(args: Vec<OsString>, cwd: Option<&Path>) -> Result<bool> {
    let mut command = Command::new("git");
    command.args(args);
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    git::sanitize_git_environment(&mut command);
    let output = command
        .output()
        .map_err(|e| AppError::git(format!("failed to run git: {e}")))?;
    match output.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(AppError::git(redact_credentials(
            String::from_utf8_lossy(&output.stderr).trim(),
        ))),
    }
}

fn redact_credentials(message: &str) -> String {
    message
        .replace("gho_", "<redacted>_")
        .replace("token=", "token=<redacted>")
        .replace("access_token=", "access_token=<redacted>")
        .replace("password=", "password=<redacted>")
}

fn create_workspace_parent(workspace: &Path) -> Result<()> {
    let parent = workspace
        .parent()
        .ok_or_else(|| AppError::usage("--workspace-root has no parent directory"))?;
    create_dir_all(parent, "workspace parent")
}

fn create_dir_all(path: &Path, label: &str) -> Result<()> {
    fs::create_dir_all(path)
        .map_err(|e| AppError::git(format!("failed to create {label} {}: {e}", path.display())))
}

fn safe_repo_key(slug: &str) -> String {
    slug.split('/')
        .map(safe_repo_segment)
        .collect::<Vec<String>>()
        .join("__")
}

fn safe_repo_segment(segment: &str) -> String {
    segment
        .chars()
        .map(|ch| match ch {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '.' | '_' | '-' => ch,
            _ => '_',
        })
        .collect()
}

fn elapsed_ms(started: Instant) -> u64 {
    started.elapsed().as_millis().try_into().unwrap_or(u64::MAX)
}

fn os(value: impl Into<OsString>) -> OsString {
    value.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn cache_lock_refuses_unknown_entries_and_times_out() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let root = tempfile::tempdir().expect("fixture");
        let path = root.path().join("cache.lock");
        fs::create_dir(&path).expect("legacy lock");
        let error = CacheLock::acquire(&path).err().expect("refuse legacy");
        assert!(error.to_string().contains("manual recovery"));
        fs::remove_dir(&path).expect("remove fixture");
        let target = root.path().join("target");
        fs::write(&target, "preserve").expect("target");
        symlink(&target, &path).expect("link");
        assert!(CacheLock::acquire(&path).is_err());
        assert_eq!(fs::read_to_string(&target).expect("target"), "preserve");
        fs::remove_file(&path).expect("remove link");
        fs::write(&path, "unknown").expect("native state");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("mode");
        assert!(CacheLock::acquire(&path).is_err());
        fs::remove_file(&path).expect("remove fixture");
        let lock = CacheLock::acquire(&path).expect("owner");
        assert!(CacheLock::acquire_for(&path, Duration::ZERO).is_err());
        drop(lock);
        CacheLock::acquire_for(&path, Duration::ZERO).expect("immediate recovery");
        let lease = path.with_extension("lock.children");
        fs::remove_file(&lease).expect("remove fixture lease");
        symlink(&target, &lease).expect("malicious lease");
        assert!(CacheLock::acquire(&path).is_err());
    }

    // A separate harness process lets the test kill the owner, rather than
    // relying on Drop to simulate a crash. Its shell grandchild retains the
    // lease after both the owner and the initial shell have exited.
    #[cfg(unix)]
    #[test]
    fn cache_crash_owner_helper() {
        let Some(root) = std::env::var_os("WT_CACHE_CRASH_FIXTURE") else {
            return;
        };
        let root = PathBuf::from(root);
        let lock = CacheLock::acquire(&root.join("cache.lock")).expect("owner");
        run_git_owned(vec![os("init"), os(&root)], None).expect("fixture repository");
        let mut command = Command::new("git");
        command.current_dir(&root).args([
            "-c",
            "alias.cache-lease=! (printf ready > ready; while test ! -f finish; do sleep 0.05; done) &",
            "cache-lease",
        ]);
        git::sanitize_git_environment(&mut command);
        lock.output(&mut command).expect("leased child");
    }

    #[cfg(unix)]
    #[test]
    fn cache_crash_retains_descendant_lease_until_exit() {
        let root = tempfile::tempdir().expect("fixture");
        let mut owner = Command::new(std::env::current_exe().expect("test binary"))
            .args([
                "--exact",
                "materialize::tests::cache_crash_owner_helper",
                "--nocapture",
            ])
            .env("WT_CACHE_CRASH_FIXTURE", root.path())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn owner");
        let started = Instant::now();
        while !root.path().join("ready").exists() {
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "child readiness timeout"
            );
            thread::sleep(Duration::from_millis(10));
        }
        owner.kill().expect("crash owner");
        owner.wait().expect("reap owner");
        let path = root.path().join("cache.lock");
        assert!(CacheLock::acquire_for(&path, Duration::ZERO).is_err());
        fs::write(root.path().join("finish"), "").expect("release descendants");
        CacheLock::acquire_for(&path, Duration::from_secs(10)).expect("recover after descendants");
    }

    #[cfg(unix)]
    #[test]
    fn cache_snapshot_waits_for_background_mutator() {
        let root = tempfile::tempdir().expect("fixture");
        let source = root.path().join("source.git");
        run_git_owned(vec![os("init"), os("--bare"), os(&source)], None).expect("repository");
        let lock = CacheLock::acquire(&root.path().join("cache.lock")).expect("owner");
        let mut command = Command::new("git");
        command.current_dir(&source).args([
            "-c",
            "alias.background=! (printf ready > ready; while test ! -f finish; do sleep 0.05; done) >/dev/null 2>&1 &",
            "background",
        ]);
        lock.output(&mut command).expect("initial Git exits");
        assert!(lock.wait_for_descendants(Duration::ZERO).is_err());
        let snapshot = root.path().join("snapshot");
        thread::scope(|scope| {
            let (sent, received) = std::sync::mpsc::channel();
            let source = &source;
            let snapshot = &snapshot;
            let lock = &lock;
            scope.spawn(move || {
                let result = clone_independent_snapshot(
                    source,
                    snapshot,
                    MaterializeCopyMode::Auto,
                    Some(lock),
                );
                sent.send(result).expect("report snapshot");
            });
            assert!(matches!(
                received.recv_timeout(Duration::from_millis(100)),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout)
            ));
            // No source-dependent snapshot work can start before quiescence.
            assert!(!snapshot.exists());
            fs::write(source.join("finish"), "").expect("finish mutator");
            received
                .recv_timeout(Duration::from_secs(10))
                .expect("snapshot resumes")
                .expect("stable snapshot");
        });
        assert!(!snapshot.join(".git/objects/info/alternates").exists());
        lock.wait_for_descendants(Duration::ZERO)
            .expect("probe stays unlocked");
    }

    #[cfg(unix)]
    #[test]
    fn cache_snapshot_finishes_reads_before_checkout() {
        let fixture = tempfile::tempdir().expect("fixture");
        let source = fixture.path().join("source.git");
        run_git_owned(vec![os("init"), os("--bare"), os(&source)], None).expect("source");
        let blob = Command::new("git")
            .args(["hash-object", "-w", "--stdin"])
            .env("GIT_DIR", &source)
            .stdin(std::process::Stdio::null())
            .output()
            .expect("unreachable blob");
        assert!(blob.status.success());
        let object = String::from_utf8_lossy(&blob.stdout).trim().to_string();
        let lock_path = fixture.path().join("cache.lock");
        let lock = CacheLock::acquire(&lock_path).expect("lock");
        let snapshot = fixture.path().join("snapshot");
        clone_independent_snapshot(&source, &snapshot, MaterializeCopyMode::Auto, Some(&lock))
            .expect("snapshot");
        drop(lock);
        let _next =
            CacheLock::acquire_for(&lock_path, Duration::ZERO).expect("released before validation");
        fs::remove_dir_all(&source).expect("remove original source");
        run_git_owned(
            vec![
                os("--git-dir"),
                os(snapshot.join(".git")),
                os("cat-file"),
                os("-e"),
                os(object),
            ],
            None,
        )
        .expect("independent unreachable object");
        assert!(!snapshot.join(".git/objects/info/alternates").exists());
    }

    #[test]
    fn cleanliness_ignores_status_and_stat_preferences() {
        for count in [1, PARALLEL_STATUS_THRESHOLD] {
            let workspace = tempfile::tempdir().expect("workspace");
            let path = workspace.path();
            run_git_owned(vec![os("init")], Some(path)).expect("init");
            run_git_owned(
                vec![os("config"), os("status.showUntrackedFiles"), os("no")],
                Some(path),
            )
            .expect("configure status preference");
            for index in 0..count {
                fs::write(path.join(format!("file-{index}")), "clean\n").expect("write file");
            }
            run_git_owned(vec![os("add"), os(".")], Some(path)).expect("stage");
            run_git_owned(
                vec![
                    os("-c"),
                    os("user.name=Test"),
                    os("-c"),
                    os("user.email=test@example.invalid"),
                    os("commit"),
                    os("-m"),
                    os("fixture"),
                ],
                Some(path),
            )
            .expect("commit");
            run_git_owned(
                vec![os("config"), os("core.ignoreStat"), os("true")],
                Some(path),
            )
            .expect("configure stat preference");
            let sha =
                run_git_owned(vec![os("rev-parse"), os("HEAD")], Some(path)).expect("fixture SHA");
            // Recreate the no-checkout clone state: no index and no tracked files.
            fs::remove_file(path.join(".git/index")).expect("remove index");
            for index in 0..count {
                fs::remove_file(path.join(format!("file-{index}"))).expect("remove file");
            }
            checkout_detached(path, &sha).expect("checkout despite stat preference");
            #[cfg(unix)]
            let _monitor = {
                use std::os::unix::fs::PermissionsExt;
                let monitor = tempfile::tempdir().expect("monitor directory");
                let hook = monitor.path().join("hook");
                fs::write(&hook, "#!/bin/sh\nprintf 'token\\0'\n").expect("write monitor hook");
                fs::set_permissions(&hook, fs::Permissions::from_mode(0o755))
                    .expect("executable monitor hook");
                run_git_owned(
                    vec![os("config"), os("core.fsmonitor"), os(hook)],
                    Some(path),
                )
                .expect("configure monitor");
                // Prime fsmonitor-valid index bits as an ordinary Git client can.
                run_git_owned(vec![os("status"), os("--porcelain")], Some(path))
                    .expect("prime monitor");
                monitor
            };
            assert!(!workspace_is_dirty(path).expect("clean status"));
            fs::write(path.join("file-0"), "dirty\n").expect("tracked edit");
            assert!(workspace_is_dirty(path).expect("tracked dirty status"));
            fs::write(path.join("file-0"), "clean\n").expect("restore tracked file");
            fs::create_dir(path.join("untracked-directory")).expect("directory");
            fs::write(path.join("untracked-directory/.hidden"), "untracked").expect("untracked");
            assert!(workspace_is_dirty(path).expect("dirty status"));
        }
    }
}
