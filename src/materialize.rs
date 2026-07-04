use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use crate::cli::MaterializeMode;
use crate::error::{AppError, Result};

const CACHE_LOCK_TIMEOUT: Duration = Duration::from_secs(30);
const CACHE_LOCK_RETRY: Duration = Duration::from_millis(50);
const GIT_ENV_OVERRIDES: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_PREFIX",
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
    repo_slug: String,
    remote_url: Option<String>,
    ref_name: Option<String>,
    sha: String,
    cache_root: Option<PathBuf>,
    workspace_root: PathBuf,
    object_source: Option<PathBuf>,
}

struct CacheLock {
    path: PathBuf,
}

impl Drop for CacheLock {
    fn drop(&mut self) {
        let _ = fs::remove_dir(&self.path);
    }
}

impl CacheLock {
    fn acquire(path: &Path) -> Result<Self> {
        let started = Instant::now();
        loop {
            match fs::create_dir(path) {
                Ok(()) => {
                    return Ok(Self {
                        path: path.to_path_buf(),
                    });
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                    if started.elapsed() >= CACHE_LOCK_TIMEOUT {
                        return Err(AppError::conflict(format!(
                            "timed out waiting for cache lock {}",
                            path.display()
                        )));
                    }
                    thread::sleep(CACHE_LOCK_RETRY);
                }
                Err(e) => {
                    return Err(AppError::git(format!(
                        "failed to acquire cache lock {}: {e}",
                        path.display()
                    )));
                }
            }
        }
    }
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
}

fn materialize_from_best_source(
    request: &ValidatedOptions,
    cache_path: &Option<PathBuf>,
    timings: &mut MaterializeTimings,
) -> Result<SourceResult> {
    if let Some(source) = &request.object_source {
        materialize_from_object_source(source, request, timings)?;
        return Ok(SourceResult {
            cache_status: "bypassed",
            source: "object_source",
        });
    }

    if let Some(cache_path) = cache_path {
        let cache_status = materialize_from_cache(cache_path, request, timings)?;
        return Ok(SourceResult {
            cache_status,
            source: "cache",
        });
    }

    materialize_from_remote(request, timings)?;
    Ok(SourceResult {
        cache_status: "bypassed",
        source: "remote",
    })
}

fn materialize_from_object_source(
    source: &Path,
    request: &ValidatedOptions,
    timings: &mut MaterializeTimings,
) -> Result<()> {
    verify_bare_repo(source)?;
    verify_commit_exists(source, &request.sha)?;
    let started = Instant::now();
    clone_local_bare(source, &request.workspace_root, &request.sha)?;
    timings.workspace_checkout = elapsed_ms(started);
    Ok(())
}

fn materialize_from_cache(
    cache_path: &Path,
    request: &ValidatedOptions,
    timings: &mut MaterializeTimings,
) -> Result<&'static str> {
    let remote_url = request
        .remote_url
        .as_deref()
        .ok_or_else(|| AppError::usage("--remote-url is required without --object-source"))?;
    let cache_root = request
        .cache_root
        .as_ref()
        .ok_or_else(|| AppError::invariant("cache path exists without cache root"))?;

    create_dir_all(cache_root, "cache root")?;
    let lock_path = cache_path.with_extension("git.lock");
    let lock_started = Instant::now();
    let _lock = CacheLock::acquire(&lock_path)?;
    timings.cache_lock = elapsed_ms(lock_started);

    let refresh_started = Instant::now();
    let cache_status = refresh_cache(cache_path, remote_url)?;
    timings.cache_refresh = elapsed_ms(refresh_started);

    verify_commit_exists(cache_path, &request.sha)?;
    let checkout_started = Instant::now();
    clone_local_bare(cache_path, &request.workspace_root, &request.sha)?;
    timings.workspace_checkout = elapsed_ms(checkout_started);
    Ok(cache_status)
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
    segment.is_empty() || segment == "." || segment == ".." || segment.ends_with(".lock")
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

fn refresh_cache(cache_path: &Path, remote_url: &str) -> Result<&'static str> {
    if cache_path.exists() {
        refresh_existing_cache(cache_path, remote_url)?;
        return Ok("refreshed");
    }

    let parent = cache_path
        .parent()
        .ok_or_else(|| AppError::invariant("cache path has no parent"))?;
    create_dir_all(parent, "cache parent")?;
    run_git_owned(
        vec![
            os("clone"),
            os("--mirror"),
            os(remote_url),
            cache_path.as_os_str().to_os_string(),
        ],
        None,
    )?;
    Ok("cold")
}

fn refresh_existing_cache(cache_path: &Path, remote_url: &str) -> Result<()> {
    if !cache_path.is_dir() {
        return Err(AppError::conflict(format!(
            "cache path exists and is not a directory: {}",
            cache_path.display()
        )));
    }

    verify_bare_repo(cache_path)?;
    run_git_owned(
        vec![
            os("--git-dir"),
            cache_path.as_os_str().to_os_string(),
            os("remote"),
            os("set-url"),
            os("origin"),
            os(remote_url),
        ],
        None,
    )?;
    run_git_owned(
        vec![
            os("--git-dir"),
            cache_path.as_os_str().to_os_string(),
            os("fetch"),
            os("--prune"),
            os("origin"),
            os("+refs/heads/*:refs/heads/*"),
            os("+refs/tags/*:refs/tags/*"),
        ],
        None,
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

fn clone_local_bare(source: &Path, workspace: &Path, sha: &str) -> Result<()> {
    create_workspace_parent(workspace)?;
    run_git_owned(
        vec![
            os("clone"),
            os("--no-local"),
            os("--no-checkout"),
            source.as_os_str().to_os_string(),
            workspace.as_os_str().to_os_string(),
        ],
        None,
    )?;
    checkout_detached(workspace, sha)
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
    run_git_owned(
        vec![os("checkout"), os("--detach"), os(sha)],
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

    let status = run_git_owned(vec![os("status"), os("--porcelain")], Some(workspace))?;
    if !status.is_empty() {
        return Err(AppError::conflict(
            "materialized workspace is not clean".to_string(),
        ));
    }

    if run_git_success(
        vec![os("symbolic-ref"), os("-q"), os("HEAD")],
        Some(workspace),
    ) {
        return Err(AppError::invariant(
            "materialized workspace is not detached".to_string(),
        ));
    }

    Ok(head)
}

fn run_git_owned(args: Vec<OsString>, cwd: Option<&Path>) -> Result<String> {
    let mut command = Command::new("git");
    command.args(args);
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    clear_git_env(&mut command);

    let output = command
        .output()
        .map_err(|e| AppError::git(format!("failed to run git: {e}")))?;
    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).trim().to_string());
    }

    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    Err(AppError::git(redact_credentials(&stderr)))
}

fn run_git_success(args: Vec<OsString>, cwd: Option<&Path>) -> bool {
    let mut command = Command::new("git");
    command.args(args);
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    clear_git_env(&mut command);
    command
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn clear_git_env(command: &mut Command) {
    for var in GIT_ENV_OVERRIDES {
        command.env_remove(var);
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
