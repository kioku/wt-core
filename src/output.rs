use serde::Serialize;

use crate::domain::Worktree;

/// Output format for commands that produce a navigable path (add, go).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavigationFormat {
    Human,
    Json,
    CdPath,
}

/// Output format for commands that produce status/list output (list, doctor).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusFormat {
    Human,
    Json,
}

/// Output format for the remove command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoveFormat {
    Human,
    Json,
    /// `--print-paths`: prints removed_path, repo_root, and branch (one per line).
    PrintPaths,
}

/// JSON envelope for single-operation responses.
#[derive(Debug, Serialize)]
pub struct JsonResponse {
    pub ok: bool,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo_root: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worktree_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cd_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub removed_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
}

impl JsonResponse {
    pub fn success(message: impl Into<String>) -> Self {
        Self {
            ok: true,
            message: message.into(),
            repo_root: None,
            worktree_path: None,
            cd_path: None,
            removed_path: None,
            branch: None,
        }
    }

    pub fn with_repo_root(mut self, root: impl Into<String>) -> Self {
        self.repo_root = Some(root.into());
        self
    }

    pub fn with_worktree_path(mut self, path: impl Into<String>) -> Self {
        self.worktree_path = Some(path.into());
        self
    }

    pub fn with_cd_path(mut self, path: impl Into<String>) -> Self {
        self.cd_path = Some(path.into());
        self
    }

    pub fn with_removed_path(mut self, path: impl Into<String>) -> Self {
        self.removed_path = Some(path.into());
        self
    }

    pub fn with_branch(mut self, branch: impl Into<String>) -> Self {
        self.branch = Some(branch.into());
        self
    }
}

/// JSON envelope for list responses.
#[derive(Debug, Serialize)]
pub struct JsonListResponse {
    pub ok: bool,
    pub worktrees: Vec<JsonWorktreeEntry>,
}

#[derive(Debug, Serialize)]
pub struct JsonWorktreeEntry {
    pub path: String,
    pub branch: Option<String>,
    pub commit: String,
    pub is_main: bool,
}

impl From<&Worktree> for JsonWorktreeEntry {
    fn from(wt: &Worktree) -> Self {
        Self {
            path: wt.path.display().to_string(),
            branch: wt.branch.clone(),
            commit: wt.commit.clone(),
            is_main: wt.is_main,
        }
    }
}

impl JsonListResponse {
    pub fn from_worktrees(worktrees: &[Worktree]) -> Self {
        Self {
            ok: true,
            worktrees: worktrees.iter().map(JsonWorktreeEntry::from).collect(),
        }
    }
}

/// JSON envelope for doctor responses.
#[derive(Debug, Serialize)]
pub struct JsonDoctorResponse {
    pub ok: bool,
    pub diagnostics: Vec<JsonDiagEntry>,
}

#[derive(Debug, Serialize)]
pub struct JsonDiagEntry {
    pub level: crate::worktree::DiagLevel,
    pub message: String,
}

impl JsonDoctorResponse {
    pub fn from_diagnostics(diags: &[crate::worktree::Diagnostic]) -> Self {
        let has_errors = diags
            .iter()
            .any(|d| d.level == crate::worktree::DiagLevel::Error);
        Self {
            ok: !has_errors,
            diagnostics: diags
                .iter()
                .map(|d| JsonDiagEntry {
                    level: d.level,
                    message: d.message.clone(),
                })
                .collect(),
        }
    }
}

/// Serialize a value as pretty-printed JSON to stdout.
pub fn print_json(value: &impl Serialize) -> crate::error::Result<()> {
    println!(
        "{}",
        serde_json::to_string_pretty(value)
            .map_err(|e| crate::error::AppError::invariant(format!("json error: {e}")))?
    );
    Ok(())
}
