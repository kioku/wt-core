//! Durable merge-operation state primitives.
//!
//! The merge workflow owns the repository-specific state shape, while this
//! module owns its typed lifecycle/progress values and the filesystem rules
//! that make the journal safe to replace and recover.

use std::fs;
use std::io::{self, Seek, SeekFrom, Write};
use std::path::Path;
#[cfg(not(windows))]
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::{AppError, Result};

#[cfg(windows)]
#[path = "windows_lifecycle.rs"]
mod windows_lifecycle;

#[cfg(windows)]
pub(crate) fn run_windows_lifecycle_launcher() {
    windows_lifecycle::run_launcher_if_requested();
}

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
/// while Git and every hook Git synchronously waits for keep the lifecycle
/// lock. Background/daemonized hook repository mutation is unsupported.
/// Windows places a surviving guardian outside an atomic kill-on-close job; the
/// guardian acquires the separate child lease by path and retains it through
/// job quiescence. Unrelated subprocesses never receive lifecycle handles.
#[derive(Debug)]
pub(crate) struct MergeLifecycleLock {
    // The parent handle is intentionally retained for the entire lifecycle.
    // Its lock is explicitly released on normal Unix drop so a descriptor
    // inherited by an unrelated child cannot extend the owner lifetime.
    _file: fs::File,
    child_lock_path: std::path::PathBuf,
    operation_id: String,
}

impl MergeLifecycleLock {
    pub(crate) fn operation_id(&self) -> &str {
        &self.operation_id
    }

    /// Spawn the lifecycle child with a direct lease.
    ///
    /// Unix configures inheritance in the forked child, so the parent never
    /// exposes the descriptor to unrelated concurrent spawns. Windows uses an
    /// out-of-job guardian that acquires the child lease by path before
    /// starting atomically job-assigned Git; the guardian owns cleanup after
    /// parent death.
    #[cfg(not(windows))]
    pub(crate) fn spawn_child(&self, command: &mut Command) -> io::Result<std::process::Child> {
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;

            let child_lock_path = self.child_lock_path.clone();
            // SAFETY: the callback only opens the separate child lock in the
            // forked child, before it executes the requested program.
            unsafe {
                command.pre_exec(move || acquire_child_lock(&child_lock_path));
            }
            command.spawn()
        }

        #[cfg(not(unix))]
        {
            command.spawn()
        }
    }

    /// Capture a lifecycle child using the same stdio behavior as
    /// `Command::output`, while retaining the lock in Git and its hooks.
    #[cfg(not(windows))]
    pub(crate) fn output(&self, command: &mut Command) -> io::Result<std::process::Output> {
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        self.spawn_child(command)?.wait_with_output()
    }

    /// Capture a lifecycle child whose stdin must receive a Git transaction.
    #[cfg(not(windows))]
    pub(crate) fn output_with_stdin(
        &self,
        command: &mut Command,
        input: &[u8],
    ) -> io::Result<std::process::Output> {
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = self.spawn_child(command)?;
        let stdin_result = child.stdin.take().map(|mut stdin| stdin.write_all(input));
        if let Some(Err(error)) = stdin_result {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
        child.wait_with_output()
    }

    #[cfg(windows)]
    pub(crate) fn output_git(
        &self,
        args: &[&str],
        cwd: &Path,
        environment: &[(std::ffi::OsString, std::ffi::OsString)],
    ) -> io::Result<std::process::Output> {
        windows_lifecycle::output_git(&self.child_lock_path, args, cwd, environment)
    }

    #[cfg(windows)]
    pub(crate) fn output_git_with_stdin(
        &self,
        args: &[&str],
        cwd: &Path,
        environment: &[(std::ffi::OsString, std::ffi::OsString)],
        input: &[u8],
    ) -> io::Result<std::process::Output> {
        windows_lifecycle::output_git_with_stdin(
            &self.child_lock_path,
            args,
            cwd,
            environment,
            input,
        )
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
    child_lock_is_held(path)
}

fn child_lock_is_held(parent_path: &Path) -> Result<bool> {
    let path = child_lock_path(parent_path);
    match fs::symlink_metadata(&path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(AppError::conflict(format!(
                    "managed merge lifecycle child lock '{}' is not a private regular file",
                    path.display()
                )));
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(AppError::git(format!(
                "cannot inspect managed merge lifecycle child lock '{}': {error}",
                path.display()
            )))
        }
    }

    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .map_err(|error| {
            AppError::git(format!(
                "cannot open managed merge lifecycle child lock '{}': {error}",
                path.display()
            ))
        })?;
    Ok(!try_lock_exclusive(&file).map_err(|error| {
        AppError::git(format!(
            "cannot inspect managed merge lifecycle child lock '{}': {error}",
            path.display()
        ))
    })?)
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

    let child_lock_path = child_lock_path(path);
    let child_file = open_child_lock(&child_lock_path).map_err(|error| {
        release_parent_lock(&file);
        AppError::git(format!(
            "cannot open managed merge lifecycle child lock '{}': {error}",
            child_lock_path.display()
        ))
    })?;
    ensure_private_file(&child_lock_path).map_err(|error| {
        release_parent_lock(&file);
        error
    })?;
    let child_available = try_lock_exclusive(&child_file).map_err(|error| {
        release_parent_lock(&file);
        AppError::git(format!(
            "cannot inspect managed merge lifecycle child lock '{}': {error}",
            child_lock_path.display()
        ))
    })?;
    drop(child_file);
    if !child_available {
        let owner = fs::read_to_string(path)
            .ok()
            .map(|contents| contents.trim().replace('\n', "; "))
            .filter(|contents| !contents.is_empty())
            .unwrap_or_else(|| "owner details are not yet available".to_string());
        release_parent_lock(&file);
        return Err(AppError::conflict(format!(
            "managed merge lifecycle is busy ({owner}); wait for the live owner to finish, inspect with `wt merge --status`, then retry `wt merge`, `wt merge --continue`, or `wt merge --abort` as appropriate"
        )));
    }

    #[cfg(windows)]
    windows_lifecycle::sweep_stale_protocol_files_for_lock(&child_lock_path).map_err(|error| {
        release_parent_lock(&file);
        AppError::git(format!(
            "cannot sweep managed merge lifecycle guardian protocol: {error}"
        ))
    })?;

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
            release_parent_lock(&file);
            AppError::git(format!(
                "cannot record managed merge lifecycle owner '{}': {error}",
                path.display()
            ))
        })?;

    Ok(MergeLifecycleLock {
        _file: file,
        child_lock_path,
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

impl Drop for MergeLifecycleLock {
    fn drop(&mut self) {
        release_parent_lock(&self._file);
    }
}

fn child_lock_path(path: &Path) -> std::path::PathBuf {
    let name = path
        .file_name()
        .map(|name| format!(".{}-child", name.to_string_lossy()))
        .unwrap_or_else(|| ".merge-operation-child.lock".to_string());
    path.with_file_name(name)
}

fn open_child_lock(path: &Path) -> io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.create(true).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    // Windows opens ordinary file handles non-inheritable by default. Keep
    // this invariant instead of toggling process-global inheritability during
    // lifecycle setup.
    options.open(path)
}

fn release_parent_lock(file: &fs::File) {
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;

        // SAFETY: the descriptor belongs to `file` and remains open for the
        // unlock. Explicitly releasing the parent OFD prevents an unrelated
        // forked child from extending the lock until its exec.
        let _ = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
    }
    #[cfg(not(unix))]
    let _ = file;
}

#[cfg(unix)]
fn acquire_child_lock(path: &Path) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::io::RawFd;

    let path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "lock path contains NUL"))?;
    // SAFETY: the path is NUL-terminated and the child lock file was created
    // privately by the parent before this callback runs.
    let descriptor: RawFd = unsafe { libc::open(path.as_ptr(), libc::O_RDWR) };
    if descriptor < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the descriptor belongs to this callback's child process.
    if unsafe { libc::flock(descriptor, libc::LOCK_EX | libc::LOCK_NB) } < 0 {
        let error = io::Error::last_os_error();
        // SAFETY: descriptor was opened successfully above.
        unsafe { libc::close(descriptor) };
        return Err(error);
    }
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
    #[cfg(windows)]
    windows_lifecycle::ensure_private_file_windows(path).map_err(|error| {
        AppError::conflict(format!(
            "managed merge state '{}' has insecure Windows ACL: {error}",
            path.display()
        ))
    })?;
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

#[cfg(windows)]
fn ensure_directory_mode(path: &Path) -> Result<()> {
    windows_lifecycle::ensure_private_directory_windows(path).map_err(|error| {
        AppError::conflict(format!(
            "managed merge state directory '{}' has insecure Windows ACL: {error}",
            path.display()
        ))
    })
}

#[cfg(not(any(unix, windows)))]
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

    #[cfg(not(windows))]
    #[test]
    fn failed_child_spawn_releases_the_child_lock() {
        let repository = tempfile::tempdir().expect("temporary repository");
        let lock_path = repository.path().join("wt-core/merge-operation.lock");
        let lifecycle_lock =
            acquire_merge_lifecycle_lock(&lock_path).expect("lifecycle lock should be available");
        let missing_program = repository.path().join("missing-lifecycle-child");
        let mut command = Command::new(missing_program);

        assert!(lifecycle_lock.spawn_child(&mut command).is_err());
        drop(lifecycle_lock);
        let recovered = acquire_merge_lifecycle_lock(&lock_path)
            .expect("a failed child spawn must release its child lock");
        drop(recovered);
    }

    #[cfg(unix)]
    fn blocking_child() -> Command {
        let mut command = {
            #[cfg(unix)]
            {
                let mut command = Command::new("sh");
                command.args(["-c", "printf 'ready\n'; cat >/dev/null"]);
                command
            }
            #[cfg(windows)]
            {
                let mut command = Command::new("cmd.exe");
                command.args(["/C", "echo ready&more"]);
                command
            }
        };
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        command
    }

    #[cfg(unix)]
    fn wait_for_child_ready(child: &mut std::process::Child) {
        use std::io::Read;

        #[cfg(unix)]
        let mut ready = [0; 6];
        #[cfg(windows)]
        let mut ready = [0; 7];
        child
            .stdout
            .take()
            .expect("blocking child stdout")
            .read_exact(&mut ready)
            .expect("blocking child should report readiness");
        #[cfg(unix)]
        assert_eq!(&ready, b"ready\n");
        #[cfg(windows)]
        assert_eq!(&ready, b"ready\r\n");
    }

    #[cfg(unix)]
    fn finish_blocking_child(mut child: std::process::Child) {
        drop(child.stdin.take());
        child.wait().expect("blocking child should exit");
    }

    #[cfg(unix)]
    #[test]
    fn lifecycle_lock_is_retained_only_by_the_intended_child() {
        let unrelated_repo = tempfile::tempdir().expect("unrelated repository");
        let unrelated_path = unrelated_repo.path().join("wt-core/merge-operation.lock");
        let unrelated_lock = acquire_merge_lifecycle_lock(&unrelated_path).expect("unrelated lock");
        let mut unrelated_command = blocking_child();
        let mut unrelated_child = unrelated_command
            .spawn()
            .expect("unrelated child should start");
        wait_for_child_ready(&mut unrelated_child);

        drop(unrelated_lock);
        let recovered = acquire_merge_lifecycle_lock(&unrelated_path)
            .expect("unrelated child must not retain the released lock");
        drop(recovered);
        finish_blocking_child(unrelated_child);

        let intended_repo = tempfile::tempdir().expect("intended repository");
        let intended_path = intended_repo.path().join("wt-core/merge-operation.lock");
        let intended_lock = acquire_merge_lifecycle_lock(&intended_path).expect("intended lock");
        let mut intended_command = blocking_child();
        let mut intended_child = intended_lock
            .spawn_child(&mut intended_command)
            .expect("intended child should start");
        wait_for_child_ready(&mut intended_child);

        drop(intended_lock);
        let busy = acquire_merge_lifecycle_lock(&intended_path)
            .expect_err("intended child must retain the lifecycle lock");
        assert!(busy.message.contains("managed merge lifecycle is busy"));
        finish_blocking_child(intended_child);

        let recovered = acquire_merge_lifecycle_lock(&intended_path)
            .expect("lock must be immediately recoverable after intended child exit");
        drop(recovered);
    }
}
