//! Durable merge-operation state primitives.
//!
//! The merge workflow owns the repository-specific state shape, while this
//! module owns its typed lifecycle/progress values and the filesystem rules
//! that make the journal safe to replace and recover.

use std::fs;
use std::io;
use std::path::Path;

use crate::error::{AppError, Result};

/// Lifecycle phase persisted in the managed merge journal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MergePhase {
    Starting,
    Conflicted,
    Committed,
}

impl MergePhase {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Conflicted => "conflicted",
            Self::Committed => "committed",
        }
    }
}

/// Completed post-commit actions. These are deliberately grouped so a journal
/// update cannot accidentally confuse the cleanup policy with action progress.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct MergeProgress {
    pub(crate) worktree_removed: bool,
    pub(crate) branch_deleted: bool,
    pub(crate) push_done: bool,
}

/// Ensure the journal directory is private before creating or trusting a
/// managed operation record. Refusing an existing insecure directory is safer
/// than silently changing permissions on a directory another local user may
/// intentionally share.
pub(crate) fn ensure_private_directory(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(AppError::conflict(format!(
                    "managed merge state directory '{}' is not a private directory",
                    path.display()
                )));
            }
            ensure_directory_mode(path)?;
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            fs::create_dir_all(path).map_err(|error| {
                AppError::git(format!(
                    "cannot create managed merge state directory '{}': {error}",
                    path.display()
                ))
            })?;
            // create_dir_all obeys umask, so enforce the mode after creation
            // and check again for a concurrent replacement.
            set_private_directory_mode(path)?;
            ensure_directory_mode(path)?;
        }
        Err(error) => {
            return Err(AppError::git(format!(
                "cannot inspect managed merge state directory '{}': {error}",
                path.display()
            )))
        }
    }
    Ok(())
}

/// Verify an existing journal is a regular private file. Missing files are
/// handled by the caller because a status query may legitimately have none.
pub(crate) fn ensure_private_file(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        AppError::git(format!(
            "cannot inspect managed merge state '{}': {error}",
            path.display()
        ))
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(AppError::conflict(format!(
            "managed merge state '{}' is not a private regular file",
            path.display()
        )));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o777 != 0o600 {
            return Err(AppError::conflict(format!(
                "managed merge state '{}' has insecure permissions (expected 0600)",
                path.display()
            )));
        }
    }
    Ok(())
}

#[cfg(unix)]
fn ensure_directory_mode(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = fs::symlink_metadata(path)
        .map_err(|error| AppError::git(format!("cannot inspect state directory: {error}")))?
        .permissions()
        .mode();
    if mode & 0o077 != 0 {
        return Err(AppError::conflict(format!(
            "managed merge state directory '{}' has insecure permissions (expected 0700)",
            path.display()
        )));
    }
    Ok(())
}

#[cfg(not(unix))]
fn ensure_directory_mode(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn set_private_directory_mode(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|error| {
        AppError::git(format!(
            "cannot make managed merge state directory '{}' private: {error}",
            path.display()
        ))
    })
}

#[cfg(not(unix))]
fn set_private_directory_mode(_path: &Path) -> Result<()> {
    Ok(())
}

/// Install a fully-written temporary file over the journal path.
///
/// `rename` replaces atomically on POSIX but fails when the destination exists
/// on Windows. MoveFileEx(REPLACE_EXISTING | WRITE_THROUGH) provides the same
/// replacement semantics on Windows without a delete-then-rename gap.
pub(crate) fn replace_existing(temp: &Path, destination: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        fs::rename(temp, destination).map_err(|error| {
            AppError::git(format!(
                "cannot install managed merge state '{}': {error}",
                destination.display()
            ))
        })
    }

    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::{
            MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
        };

        let source: Vec<u16> = temp.as_os_str().encode_wide().chain(Some(0)).collect();
        let target: Vec<u16> = destination
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect();
        // SAFETY: both vectors are NUL-terminated and remain alive for the
        // duration of the OS call; MoveFileExW does not retain the pointers.
        let result = unsafe {
            MoveFileExW(
                source.as_ptr(),
                target.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        };
        if result == 0 {
            return Err(AppError::git(format!(
                "cannot install managed merge state '{}': {}",
                destination.display(),
                io::Error::last_os_error()
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replacement_replaces_an_existing_record() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let destination = directory.path().join("merge-operation.json");
        let temporary = directory.path().join("merge-operation.json.tmp");
        fs::write(&destination, b"old").expect("old record");
        fs::write(&temporary, b"new").expect("new record");

        replace_existing(&temporary, &destination).expect("atomic replacement");
        assert_eq!(fs::read(&destination).expect("replacement"), b"new");
        assert!(!temporary.exists());
    }
}
