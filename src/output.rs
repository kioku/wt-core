use serde::Serialize;

use crate::domain::Worktree;

/// The output format requested by the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    Human,
    Json,
    CdPath,
    /// For `remove --print-paths`: prints removed_path and repo_root, one per line.
    RemovePaths,
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
