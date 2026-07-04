use std::path::PathBuf;

use crate::cli::MaterializeMode;
use crate::error::{AppError, Result};

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

pub fn materialize(options: MaterializeOptions) -> Result<MaterializeResult> {
    let MaterializeOptions {
        repo_slug,
        remote_url,
        ref_name,
        sha,
        cache_root,
        workspace_root,
        object_source,
        mode: _,
    } = options;
    let _ = (
        repo_slug,
        remote_url,
        ref_name,
        sha,
        cache_root,
        workspace_root,
        object_source,
    );

    Err(AppError::usage(
        "materialize is not implemented in this build".to_string(),
    ))
}
