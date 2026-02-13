use std::fmt;
use std::process;

/// Stable exit codes as defined in the CLI contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitCode {
    /// 0 — success
    Success = 0,
    /// 1 — usage / argument error
    Usage = 1,
    /// 2 — git invocation error
    Git = 2,
    /// 3 — not a git repository / repo resolution failure
    NotARepo = 3,
    /// 4 — invariant violation (e.g. attempt to remove main worktree)
    Invariant = 4,
    /// 5 — state conflict (dirty worktree, existing path, branch conflict)
    Conflict = 5,
}

impl From<ExitCode> for process::ExitCode {
    fn from(code: ExitCode) -> Self {
        process::ExitCode::from(code as u8)
    }
}

/// Application-level error with a stable exit code.
#[derive(Debug)]
pub struct AppError {
    pub code: ExitCode,
    pub message: String,
}

impl AppError {
    pub fn usage(msg: impl Into<String>) -> Self {
        Self {
            code: ExitCode::Usage,
            message: msg.into(),
        }
    }

    pub fn git(msg: impl Into<String>) -> Self {
        Self {
            code: ExitCode::Git,
            message: msg.into(),
        }
    }

    pub fn not_a_repo(msg: impl Into<String>) -> Self {
        Self {
            code: ExitCode::NotARepo,
            message: msg.into(),
        }
    }

    pub fn invariant(msg: impl Into<String>) -> Self {
        Self {
            code: ExitCode::Invariant,
            message: msg.into(),
        }
    }

    pub fn conflict(msg: impl Into<String>) -> Self {
        Self {
            code: ExitCode::Conflict,
            message: msg.into(),
        }
    }
}

impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for AppError {}

pub type Result<T> = std::result::Result<T, AppError>;
