//! Durable merge-operation state primitives.
//!
//! The merge workflow owns the repository-specific state shape, while this
//! module owns its typed lifecycle/progress values and the filesystem rules
//! that make the journal safe to replace and recover.

use std::fs;
use std::io::{self, Seek, SeekFrom, Write};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

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

/// An OS-backed owner for the repository's mutating merge lifecycle.
///
/// The file remains after the process exits, but the OS lock does not. This is
/// intentional: process death therefore recovers without a PID-based timeout,
/// while a paused Git hook keeps the lock until Git and its hook descendants
/// finish. The handle is made inheritable so an orphaned Git subprocess cannot
/// finalize a merge after its `wt-core` parent has died and the lock has been
/// reclaimed.
#[derive(Debug)]
pub(crate) struct MergeLifecycleLock {
    // The handle is intentionally retained for the entire lifecycle. Its OS
    // lock is released automatically on normal drop or process death.
    _file: fs::File,
    operation_id: String,
}

impl MergeLifecycleLock {
    pub(crate) fn operation_id(&self) -> &str {
        &self.operation_id
    }
}

/// Report whether an already-created lifecycle lock is currently owned by
/// another process, without creating the state directory or lock file.
///
/// Read-only status uses this probe so a live pre-merge hook is reported as
/// `busy` rather than as an interrupted/conflicted operation. A lock that is
/// absent is not initialized as a side effect of observation.
pub(crate) fn lock_is_held(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(AppError::conflict(format!(
                    "managed merge lifecycle lock '{}' is not a private regular file",
                    path.display()
                )));
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(AppError::git(format!(
                "cannot inspect managed merge lifecycle lock '{}': {error}",
                path.display()
            )))
        }
    }

    let file = match fs::OpenOptions::new().read(true).write(true).open(path) {
        Ok(file) => file,
        // On platforms that deny opening a locked file, the existing file is
        // still authoritative evidence of a live owner.
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => return Ok(true),
        Err(error) => {
            return Err(AppError::git(format!(
                "cannot open managed merge lifecycle lock '{}': {error}",
                path.display()
            )))
        }
    };
    let locked = !try_lock_exclusive(&file).map_err(|error| {
        AppError::git(format!(
            "cannot inspect managed merge lifecycle lock '{}': {error}",
            path.display()
        ))
    })?;
    if locked {
        // Internal status inspection occurs while the owning command holds the
        // same lock. Its owner record lets that code continue to inspect its
        // own operation, while a status subprocess has a different PID and
        // truthfully reports `busy`.
        let owner_is_current = fs::read_to_string(path)
            .ok()
            .and_then(|contents| {
                contents
                    .lines()
                    .find_map(|line| line.strip_prefix("pid=")?.parse::<u32>().ok())
            })
            .is_some_and(|pid| pid == std::process::id());
        return Ok(!owner_is_current);
    }
    Ok(false)
}

/// Acquire the repository-wide mutating merge lifecycle lock.
///
/// Status deliberately does not call this function. Every command that can
/// run Git or change the journal must hold the returned handle until its last
/// durable action has completed.
pub(crate) fn acquire_merge_lifecycle_lock(path: &Path) -> Result<MergeLifecycleLock> {
    let parent = path.parent().ok_or_else(|| {
        AppError::invariant(format!(
            "managed merge lock path '{}' has no parent",
            path.display()
        ))
    })?;
    ensure_private_directory(parent)?;

    let mut options = fs::OpenOptions::new();
    options.create(true).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).map_err(|error| {
        AppError::git(format!(
            "cannot open managed merge lifecycle lock '{}': {error}",
            path.display()
        ))
    })?;
    ensure_private_file(path)?;

    if !try_lock_exclusive(&file).map_err(|error| {
        AppError::git(format!(
            "cannot acquire managed merge lifecycle lock '{}': {error}",
            path.display()
        ))
    })? {
        let owner = fs::read_to_string(path)
            .ok()
            .map(|contents| contents.trim().replace('\n', "; "))
            .filter(|contents| !contents.is_empty())
            .unwrap_or_else(|| "owner details are not yet available".to_string());
        return Err(AppError::conflict(format!(
            "managed merge lifecycle is busy ({owner}); wait for the live owner to finish, inspect with `wt merge --status`, then retry `wt merge`, `wt merge --continue`, or `wt merge --abort` as appropriate"
        )));
    }

    let operation_id = new_operation_id();
    let owner = format!(
        "pid={}\noperation_id={}\n",
        std::process::id(),
        operation_id
    );
    file.set_len(0)
        .and_then(|_| file.seek(SeekFrom::Start(0)))
        .and_then(|_| file.write_all(owner.as_bytes()))
        .and_then(|_| file.sync_all())
        .map_err(|error| {
            AppError::git(format!(
                "cannot record managed merge lifecycle owner '{}': {error}",
                path.display()
            ))
        })?;
    make_inheritable(&file).map_err(|error| {
        AppError::git(format!(
            "cannot make managed merge lifecycle lock inheritable: {error}"
        ))
    })?;

    Ok(MergeLifecycleLock {
        _file: file,
        operation_id,
    })
}

fn new_operation_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    format!("{}-{nanos}", std::process::id())
}

#[cfg(unix)]
pub(crate) fn try_lock_exclusive(file: &fs::File) -> io::Result<bool> {
    use std::os::fd::AsRawFd;

    // SAFETY: the descriptor belongs to `file` and remains open for the call.
    let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if result == 0 {
        return Ok(true);
    }
    let error = io::Error::last_os_error();
    if error
        .raw_os_error()
        .is_some_and(|code| code == libc::EWOULDBLOCK || code == libc::EAGAIN)
    {
        Ok(false)
    } else {
        Err(error)
    }
}

#[cfg(windows)]
pub(crate) fn try_lock_exclusive(file: &fs::File) -> io::Result<bool> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::{GetLastError, ERROR_LOCK_VIOLATION};
    use windows_sys::Win32::Storage::FileSystem::{
        LockFileEx, LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY,
    };
    use windows_sys::Win32::System::IO::OVERLAPPED;

    let mut overlapped = OVERLAPPED::default();
    // SAFETY: the file handle is owned by `file`; the overlapped structure is
    // initialized and remains alive for the duration of the nonblocking call.
    let result = unsafe {
        LockFileEx(
            file.as_raw_handle(),
            LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
            0,
            u32::MAX,
            u32::MAX,
            &mut overlapped,
        )
    };
    if result != 0 {
        return Ok(true);
    }
    let error = unsafe { GetLastError() };
    if error == ERROR_LOCK_VIOLATION {
        Ok(false)
    } else {
        Err(io::Error::from_raw_os_error(error as i32))
    }
}

#[cfg(not(any(unix, windows)))]
pub(crate) fn try_lock_exclusive(_file: &fs::File) -> io::Result<bool> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "managed merge lifecycle locking is unsupported on this platform",
    ))
}

#[cfg(unix)]
fn make_inheritable(file: &fs::File) -> io::Result<()> {
    use std::os::fd::AsRawFd;

    let descriptor = file.as_raw_fd();
    // SAFETY: the descriptor belongs to `file` and remains open for both calls.
    let flags = unsafe { libc::fcntl(descriptor, libc::F_GETFD) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: as above; clearing close-on-exec lets Git and its hooks retain
    // the lifecycle lock until the mutating subprocess has fully exited.
    if unsafe { libc::fcntl(descriptor, libc::F_SETFD, flags & !libc::FD_CLOEXEC) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(windows)]
fn make_inheritable(file: &fs::File) -> io::Result<()> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::{SetHandleInformation, HANDLE_FLAG_INHERIT};

    // SAFETY: the handle belongs to `file` and remains valid for the call.
    if unsafe {
        SetHandleInformation(
            file.as_raw_handle(),
            HANDLE_FLAG_INHERIT,
            HANDLE_FLAG_INHERIT,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn make_inheritable(_file: &fs::File) -> io::Result<()> {
    Ok(())
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

    #[test]
    fn lifecycle_locks_are_repository_local_and_released_by_drop() {
        let first_repo = tempfile::tempdir().expect("first repository");
        let second_repo = tempfile::tempdir().expect("second repository");
        let first_path = first_repo.path().join("wt-core/merge-operation.lock");
        let second_path = second_repo.path().join("wt-core/merge-operation.lock");

        let first = acquire_merge_lifecycle_lock(&first_path).expect("first lock");
        let busy = acquire_merge_lifecycle_lock(&first_path).expect_err("same repo is busy");
        assert!(busy.message.contains("managed merge lifecycle is busy"));
        let second = acquire_merge_lifecycle_lock(&second_path)
            .expect("a distinct repository has an independent lifecycle");

        drop(first);
        let _recovered =
            acquire_merge_lifecycle_lock(&first_path).expect("released lock is recoverable");
        drop(second);
    }
}
