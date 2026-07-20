//! Atomic Windows containment for lifecycle Git.
//!
//! Lifecycle Git is created directly in a private kill-on-close Job Object.
//! Job-list and handle-list process attributes make containment and direct
//! lease transfer part of CreateProcessW; there is no suspended-process
//! handoff for a cleanup thread to lose.

use std::ffi::{OsStr, OsString};
use std::fs::{self, File};
use std::io::{self, Read};
use std::mem::size_of;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::FromRawHandle;
use std::path::Path;
use std::process::{ExitStatus, Output};
use std::thread;

use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, SetHandleInformation, GENERIC_READ, HANDLE, HANDLE_FLAG_INHERIT,
    INVALID_HANDLE_VALUE, WAIT_OBJECT_0,
};
use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows_sys::Win32::System::JobObjects::{
    CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject,
    TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows_sys::Win32::System::Pipes::CreatePipe;
use windows_sys::Win32::System::Threading::{
    CreateProcessW, DeleteProcThreadAttributeList, GetExitCodeProcess,
    InitializeProcThreadAttributeList, UpdateProcThreadAttribute, WaitForSingleObject,
    CREATE_UNICODE_ENVIRONMENT, EXTENDED_STARTUPINFO_PRESENT, INFINITE, PROCESS_INFORMATION,
    PROC_THREAD_ATTRIBUTE_HANDLE_LIST, PROC_THREAD_ATTRIBUTE_JOB_LIST, STARTF_USESTDHANDLES,
    STARTUPINFOEXW,
};

const ERROR_INSUFFICIENT_BUFFER: u32 = 122;

/// Run Git with captured stdout/stderr while retaining the direct child lease.
///
/// Git and every hook Git synchronously waits for remain inside the supported
/// lifecycle boundary. Background/daemonized hook repository mutation is
/// unsupported; the job nevertheless terminates leftover members before this
/// function releases the lease, so they cannot mutate during recovery.
pub(crate) fn output_git(
    child_lock_path: &Path,
    args: &[&str],
    cwd: &Path,
    environment: &[(OsString, OsString)],
) -> io::Result<Output> {
    output_git_with_creation_flags(child_lock_path, args, cwd, environment, 0)
}

/// Run Git with caller-provided creation flags without replacing them.
///
/// The lifecycle production path currently needs no extra flags. Keeping the
/// flags explicit prevents containment from silently discarding flags such as
/// CREATE_NO_WINDOW or CREATE_NEW_PROCESS_GROUP when another caller supplies
/// them.
pub(crate) fn output_git_with_creation_flags(
    child_lock_path: &Path,
    args: &[&str],
    cwd: &Path,
    environment: &[(OsString, OsString)],
    creation_flags: u32,
) -> io::Result<Output> {
    let child_lock = open_child_lock(child_lock_path)?;
    if !super::try_lock_exclusive(&child_lock)? {
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "managed merge lifecycle child lock is busy",
        ));
    }

    let job = Job::new()?;
    let mut stdio = StdioHandles::new()?;
    let mut command_line = command_line("git", args)?;
    let current_directory = wide_path(cwd.as_os_str())?;
    let environment_block = environment_block(environment)?;
    let mut attributes = AttributeList::new(2)?;
    let inherited_handles = [
        stdio.stdin.raw(),
        stdio.stdout.write.raw(),
        stdio.stderr.write.raw(),
        raw_handle(&child_lock),
    ];

    attributes.update(
        PROC_THREAD_ATTRIBUTE_JOB_LIST as usize,
        (&job.raw() as *const HANDLE).cast(),
        size_of::<HANDLE>(),
    )?;
    attributes.update(
        PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
        inherited_handles.as_ptr().cast(),
        size_of::<HANDLE>() * inherited_handles.len(),
    )?;

    // HANDLE_LIST requires inheritable handles. The list is atomically applied
    // by CreateProcessW, so no unrelated child receives this lease. The flags
    // are cleared immediately after this one OS call returns.
    for handle in &inherited_handles {
        if let Err(error) = set_inheritable(*handle, true) {
            for handle in &inherited_handles {
                let _ = set_inheritable(*handle, false);
            }
            return Err(error);
        }
    }

    let mut startup = STARTUPINFOEXW::default();
    startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    startup.StartupInfo.hStdInput = stdio.stdin.raw();
    startup.StartupInfo.hStdOutput = stdio.stdout.write.raw();
    startup.StartupInfo.hStdError = stdio.stderr.write.raw();
    startup.lpAttributeList = attributes.raw();

    let mut process_info = PROCESS_INFORMATION::default();
    // SAFETY: all pointers reference live, NUL-terminated buffers or valid
    // handles owned by this function for the duration of CreateProcessW.
    let created = unsafe {
        CreateProcessW(
            std::ptr::null(),
            command_line.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            1,
            creation_flags | EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT,
            environment_block.as_ptr().cast(),
            current_directory.as_ptr(),
            (&startup as *const STARTUPINFOEXW).cast(),
            &mut process_info,
        )
    } != 0;

    // The parent must never retain inheritable copies after CreateProcessW.
    let mut clear_error = None;
    for handle in &inherited_handles {
        if let Err(error) = set_inheritable(*handle, false) {
            clear_error = Some(error);
        }
    }
    if !created {
        return Err(clear_error.unwrap_or_else(last_error));
    }
    if let Some(error) = clear_error {
        // A process that was created but whose parent handles could not be
        // made private is not safe to return. Kill and reap the whole job
        // before closing the direct lease and reporting setup failure.
        // SAFETY: job is the live owned containment handle.
        let _ = unsafe { TerminateJobObject(job.raw(), 1) };
        let _ = wait_for_process(job.raw());
        close_raw(process_info.hProcess);
        close_raw(process_info.hThread);
        stdio.close_child_writes();
        drop(child_lock);
        return Err(error);
    }

    // The thread handle is not needed after creation. The process and job are
    // now owned by this function; every failure path below terminates and
    // waits the job before dropping the direct lease.
    close_raw(process_info.hThread);
    stdio.close_child_writes();
    drop(child_lock);

    let process = OwnedHandle::new(process_info.hProcess);
    let stdout = stdio.stdout.take_read();
    let stderr = stdio.stderr.take_read();
    let stdout_reader = thread::spawn(move || read_pipe(stdout));
    let stderr_reader = thread::spawn(move || read_pipe(stderr));

    let wait_result = wait_for_process(process.raw());
    let exit_code = wait_result.and_then(|()| get_exit_code(process.raw()));

    // Git normally waits for its hooks. If a daemonized hook survives Git,
    // terminate the job before joining readers and releasing the lease. A job
    // object becomes signaled only after all its members have exited.
    let cleanup_result = job.terminate_and_wait();
    if cleanup_result.is_err() {
        // KILL_ON_JOB_CLOSE is the last-resort cleanup if an explicit
        // termination or job wait failed; do it before joining pipe readers.
        drop(job);
    }
    let stdout = join_reader(stdout_reader);
    let stderr = join_reader(stderr_reader);

    let exit_code = exit_code?;
    cleanup_result?;
    Ok(Output {
        status: exit_status_from_raw(exit_code),
        stdout: stdout?,
        stderr: stderr?,
    })
}

fn raw_handle(file: &File) -> HANDLE {
    use std::os::windows::io::AsRawHandle;
    file.as_raw_handle()
}

fn open_child_lock(path: &Path) -> io::Result<File> {
    let mut options = fs::OpenOptions::new();
    options.read(true).write(true);
    let file = options.open(path)?;
    set_inheritable(raw_handle(&file), false)?;
    Ok(file)
}

fn set_inheritable(handle: HANDLE, inheritable: bool) -> io::Result<()> {
    let flags = if inheritable { HANDLE_FLAG_INHERIT } else { 0 };
    // SAFETY: callers pass live handles owned by the surrounding RAII value.
    if unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT, flags) } == 0 {
        Err(last_error())
    } else {
        Ok(())
    }
}

fn close_raw(handle: HANDLE) {
    if !handle.is_null() && handle != INVALID_HANDLE_VALUE {
        // SAFETY: this function is called only for owned Win32 handles.
        unsafe { CloseHandle(handle) };
    }
}

fn last_error() -> io::Error {
    // SAFETY: GetLastError has no pointer or handle preconditions.
    io::Error::from_raw_os_error(unsafe { GetLastError() } as i32)
}

struct OwnedHandle(HANDLE);

impl OwnedHandle {
    fn new(handle: HANDLE) -> Self {
        Self(handle)
    }

    fn raw(&self) -> HANDLE {
        self.0
    }
}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        close_raw(self.0);
    }
}

struct Job(OwnedHandle);

impl Job {
    fn new() -> io::Result<Self> {
        // SAFETY: null attributes/name request a private unnamed job.
        let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if handle.is_null() {
            return Err(last_error());
        }

        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: handle is owned above and limits remains live for the call.
        if unsafe {
            SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        } == 0
        {
            close_raw(handle);
            return Err(last_error());
        }
        Ok(Self(OwnedHandle::new(handle)))
    }

    fn raw(&self) -> HANDLE {
        self.0.raw()
    }

    fn terminate_and_wait(&self) -> io::Result<()> {
        // SAFETY: the job handle is owned by self and remains live here.
        let termination_error = if unsafe { TerminateJobObject(self.raw(), 1) } == 0 {
            Some(last_error())
        } else {
            None
        };
        match wait_for_process(self.raw()) {
            Ok(()) => Ok(()),
            Err(wait_error) => Err(termination_error.unwrap_or(wait_error)),
        }
    }
}

struct AttributeList {
    storage: Vec<usize>,
    raw: *mut core::ffi::c_void,
}

impl AttributeList {
    fn new(attribute_count: u32) -> io::Result<Self> {
        let mut bytes = 0usize;
        // SAFETY: the null probe only writes the required size.
        let first = unsafe {
            InitializeProcThreadAttributeList(std::ptr::null_mut(), attribute_count, 0, &mut bytes)
        };
        // SAFETY: GetLastError has no preconditions.
        if first == 0 && unsafe { GetLastError() } != ERROR_INSUFFICIENT_BUFFER {
            return Err(last_error());
        }
        let words = bytes.div_ceil(size_of::<usize>());
        let mut storage = vec![0usize; words];
        let raw = storage.as_mut_ptr().cast();
        // SAFETY: raw points to aligned storage of the requested size.
        if unsafe { InitializeProcThreadAttributeList(raw, attribute_count, 0, &mut bytes) } == 0 {
            return Err(last_error());
        }
        Ok(Self { storage, raw })
    }

    fn raw(&self) -> *mut core::ffi::c_void {
        self.raw
    }

    fn update(
        &mut self,
        attribute: usize,
        value: *const core::ffi::c_void,
        size: usize,
    ) -> io::Result<()> {
        // SAFETY: raw and value remain valid for this attribute update.
        if unsafe {
            UpdateProcThreadAttribute(
                self.raw,
                0,
                attribute,
                value,
                size,
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        } == 0
        {
            Err(last_error())
        } else {
            Ok(())
        }
    }
}

impl Drop for AttributeList {
    fn drop(&mut self) {
        // SAFETY: self.raw was successfully initialized and is not reused.
        unsafe { DeleteProcThreadAttributeList(self.raw) };
        let _ = self.storage.as_ptr();
    }
}

struct Pipe {
    read: Option<OwnedHandle>,
    write: OwnedHandle,
}

impl Pipe {
    fn new() -> io::Result<Self> {
        let attributes = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: std::ptr::null_mut(),
            bInheritHandle: 1,
        };
        let mut read = std::ptr::null_mut();
        let mut write = std::ptr::null_mut();
        // SAFETY: output pointers and security attributes remain live for the call.
        if unsafe { CreatePipe(&mut read, &mut write, &attributes, 0) } == 0 {
            return Err(last_error());
        }
        if let Err(error) = set_inheritable(read, false) {
            close_raw(read);
            close_raw(write);
            return Err(error);
        }
        Ok(Self {
            read: Some(OwnedHandle::new(read)),
            write: OwnedHandle::new(write),
        })
    }

    fn take_read(&mut self) -> File {
        let read = self.read.take().expect("pipe read handle is available");
        let raw = read.0;
        std::mem::forget(read);
        // SAFETY: raw is the one owned read end removed from OwnedHandle.
        unsafe { File::from_raw_handle(raw) }
    }
}

struct StdioHandles {
    stdin: OwnedHandle,
    stdout: Pipe,
    stderr: Pipe,
}

impl StdioHandles {
    fn new() -> io::Result<Self> {
        let nul = wide_path(OsStr::new("NUL"))?;
        // SAFETY: nul is NUL-terminated and all output pointers are null by
        // design; the returned handle is adopted immediately below.
        let stdin = unsafe {
            CreateFileW(
                nul.as_ptr(),
                GENERIC_READ,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                std::ptr::null_mut(),
            )
        };
        if stdin == INVALID_HANDLE_VALUE || stdin.is_null() {
            return Err(last_error());
        }
        if let Err(error) = set_inheritable(stdin, true) {
            close_raw(stdin);
            return Err(error);
        }
        Ok(Self {
            stdin: OwnedHandle::new(stdin),
            stdout: Pipe::new()?,
            stderr: Pipe::new()?,
        })
    }

    fn close_child_writes(&mut self) {
        let stdout = std::mem::replace(&mut self.stdout.write, OwnedHandle(std::ptr::null_mut()));
        let stderr = std::mem::replace(&mut self.stderr.write, OwnedHandle(std::ptr::null_mut()));
        drop(stdout);
        drop(stderr);
    }
}

fn wait_for_process(handle: HANDLE) -> io::Result<()> {
    // SAFETY: handle is an owned process or job synchronization handle.
    if unsafe { WaitForSingleObject(handle, INFINITE) } != WAIT_OBJECT_0 {
        Err(last_error())
    } else {
        Ok(())
    }
}

fn get_exit_code(handle: HANDLE) -> io::Result<u32> {
    let mut code = 0;
    // SAFETY: handle is an owned process handle and code is writable storage.
    if unsafe { GetExitCodeProcess(handle, &mut code) } == 0 {
        Err(last_error())
    } else {
        Ok(code)
    }
}

fn read_pipe(mut file: File) -> io::Result<Vec<u8>> {
    let mut output = Vec::new();
    file.read_to_end(&mut output)?;
    Ok(output)
}

fn join_reader(reader: thread::JoinHandle<io::Result<Vec<u8>>>) -> io::Result<Vec<u8>> {
    reader
        .join()
        .map_err(|_| io::Error::other("lifecycle Git output reader panicked"))?
}

fn wide_path(path: &OsStr) -> io::Result<Vec<u16>> {
    if path.encode_wide().any(|unit| unit == 0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Windows process path contains NUL",
        ));
    }
    Ok(path.encode_wide().chain(Some(0)).collect())
}

fn command_line(program: &str, args: &[&str]) -> io::Result<Vec<u16>> {
    let mut values = Vec::with_capacity(args.len() + 1);
    values.push(OsString::from(program));
    values.extend(args.iter().map(OsString::from));
    let mut line = String::new();
    for (index, value) in values.iter().enumerate() {
        if index != 0 {
            line.push(' ');
        }
        quote_windows_arg(&mut line, &value.to_string_lossy());
    }
    wide_path(OsStr::new(&line))
}

fn quote_windows_arg(output: &mut String, value: &str) {
    output.push('"');
    let mut backslashes = 0;
    for character in value.chars() {
        match character {
            '\\' => backslashes += 1,
            '"' => {
                output.push_str(&"\\".repeat(backslashes * 2 + 1));
                output.push('"');
                backslashes = 0;
            }
            _ => {
                output.push_str(&"\\".repeat(backslashes));
                output.push(character);
                backslashes = 0;
            }
        }
    }
    output.push_str(&"\\".repeat(backslashes * 2));
    output.push('"');
}

fn environment_block(environment: &[(OsString, OsString)]) -> io::Result<Vec<u16>> {
    let mut values = environment.to_vec();
    values.sort_by(|left, right| {
        left.0
            .to_string_lossy()
            .to_ascii_lowercase()
            .cmp(&right.0.to_string_lossy().to_ascii_lowercase())
    });

    let mut block = Vec::new();
    for (key, value) in values {
        let mut entry = key;
        entry.push("=");
        entry.push(value);
        let wide = wide_path(&entry)?;
        block.extend_from_slice(&wide[..wide.len() - 1]);
        block.push(0);
    }
    block.push(0);
    Ok(block)
}

fn exit_status_from_raw(code: u32) -> ExitStatus {
    use std::os::windows::process::ExitStatusExt;
    ExitStatusExt::from_raw(code)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operation_state::acquire_merge_lifecycle_lock;

    fn environment() -> Vec<(OsString, OsString)> {
        crate::git::sanitized_git_environment()
    }

    #[test]
    fn lifecycle_git_preserves_stdio_and_creation_flags() {
        let repository = tempfile::tempdir().expect("temporary repository");
        let lock_path = repository.path().join("merge-operation.lock");
        fs::File::create(&lock_path).expect("child lease file");
        let output = output_git_with_creation_flags(
            &lock_path,
            &["--version"],
            repository.path(),
            &environment(),
            windows_sys::Win32::System::Threading::CREATE_NO_WINDOW
                | windows_sys::Win32::System::Threading::CREATE_NEW_PROCESS_GROUP,
        )
        .expect("Git should start in the contained process");

        assert!(output.status.success());
        assert!(!output.stdout.is_empty(), "stdout was not captured");

        let output = output_git(
            &lock_path,
            &["--not-a-real-option"],
            repository.path(),
            &environment(),
        )
        .expect("Git should report an argument error through stderr");
        assert!(!output.status.success());
        assert!(!output.stderr.is_empty(), "stderr was not captured");
    }

    #[test]
    fn setup_failure_does_not_leave_a_child_lease_or_process() {
        let repository = tempfile::tempdir().expect("temporary repository");
        let missing_lock = repository.path().join("missing/merge-operation.lock");
        assert!(output_git(
            &missing_lock,
            &["--version"],
            repository.path(),
            &environment()
        )
        .is_err());

        let lock_path = repository.path().join("merge-operation.lock");
        let lifecycle = acquire_merge_lifecycle_lock(&lock_path).expect("lock should recover");
        drop(lifecycle);
        let recovered = acquire_merge_lifecycle_lock(&lock_path).expect("lock should be reusable");
        drop(recovered);
    }
}
