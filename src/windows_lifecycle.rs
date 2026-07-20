//! Windows containment for lifecycle Git.
//!
//! Windows 10 and Windows Server 2016 introduced `PROC_THREAD_ATTRIBUTE_JOB_LIST`.
//! The lifecycle path requires that API: it creates an internal launcher suspended
//! in a kill-on-close job, then duplicates non-inheritable handles into that
//! already-created process. No parent handle is made inheritable, so an unrelated
//! concurrent spawn cannot observe a lifecycle lease or a lifecycle pipe.
//!
//! The launcher keeps the child lease until the parent has terminated the job and
//! waited for every member. Cleanup failure retains the job handle and lease
//! instead of relying on an unproven kill-on-close. Git and hooks that Git
//! synchronously waits for are supported; daemonized hooks that mutate the
//! repository after Git returns are outside the contract.

use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::mem::size_of;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::FromRawHandle;
use std::path::{Path, PathBuf};
use std::process::{Child, ExitStatus, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;

use windows_sys::Win32::Foundation::{
    CloseHandle, DuplicateHandle, GetLastError, DUPLICATE_SAME_ACCESS, HANDLE,
    INVALID_HANDLE_VALUE, WAIT_FAILED, WAIT_OBJECT_0,
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
    CreateEventW, CreateProcessW, DeleteProcThreadAttributeList, GetCurrentProcess,
    GetExitCodeProcess, InitializeProcThreadAttributeList, ResumeThread, SetEvent,
    UpdateProcThreadAttribute, WaitForMultipleObjects, WaitForSingleObject,
    CREATE_BREAKAWAY_FROM_JOB, CREATE_NO_WINDOW, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT,
    EXTENDED_STARTUPINFO_PRESENT, INFINITE, PROCESS_INFORMATION, PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
    PROC_THREAD_ATTRIBUTE_JOB_LIST, STARTF_USESTDHANDLES, STARTUPINFOEXW,
};

const ERROR_INSUFFICIENT_BUFFER: u32 = 122;
const INTERNAL_LAUNCHER_ARG: &str = "--__wt-core-windows-lifecycle-launcher";
// This protocol is intentionally opt-in and bounded. It is a kernel-observable
// test of the no-inheritance invariant, not a production lifecycle mechanism.
const INTERNAL_LOCK_PROBE_ARG: &str = "--__wt-core-windows-lifecycle-lock-probe";
const LOCK_PROBE_ENV: &str = "WT_CORE_WINDOWS_LIFECYCLE_LOCK_PROBE";
const LOCK_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);
static BOOTSTRAP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Run Git with captured stdout/stderr while retaining the direct child lease.
///
/// Git and every hook Git synchronously waits for remain inside the supported
/// lifecycle boundary. Background/daemonized hook repository mutation is
/// unsupported; the job terminates leftover members before this function
/// releases either copy of the child lease. If termination or the job wait
/// fails, the job handle is retained and this call returns an error without
/// dropping the last lease.
pub(crate) fn output_git(
    child_lock_path: &Path,
    args: &[&str],
    cwd: &Path,
    environment: &[(OsString, OsString)],
) -> io::Result<Output> {
    output_git_with_creation_flags(child_lock_path, args, cwd, environment, 0)
}

/// Run Git with caller-provided creation flags.
///
/// `CREATE_SUSPENDED` cannot be preserved: the internal launcher must resume
/// its own suspended bootstrap process, and passing that flag to Git would
/// leave Git suspended while the launcher waits. `CREATE_BREAKAWAY_FROM_JOB`
/// is also rejected because no lifecycle process may leave its job.
pub(crate) fn output_git_with_creation_flags(
    child_lock_path: &Path,
    args: &[&str],
    cwd: &Path,
    environment: &[(OsString, OsString)],
    creation_flags: u32,
) -> io::Result<Output> {
    validate_creation_flags(creation_flags)?;

    let child_lock = open_child_lock(child_lock_path)?;
    if !super::try_lock_exclusive(&child_lock)? {
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "managed merge lifecycle child lock is busy",
        ));
    }

    // The declaration order is deliberate. Setup failures before launcher
    // creation close an empty job; failures after creation retain the job and
    // lease rather than dropping them without a quiescence proof.
    let mut job = Job::new()?;
    let mut stdio = StdioHandles::new()?;
    let mut bootstrap = BootstrapFile::new(child_lock_path)?;
    let done_event = OwnedHandle::new(create_event()?);
    let gate_event = OwnedHandle::new(create_event()?);
    let current_exe = std::env::current_exe()?;
    let current_exe_wide = wide_path(current_exe.as_os_str())?;
    let current_directory = wide_path(cwd.as_os_str())?;
    let environment_block = environment_block(environment)?;

    // Only the job attribute is used for the atomic launcher creation. In
    // particular, there is no HANDLE_LIST and no inherited lifecycle handle.
    let mut attributes = AttributeList::new(1)?;
    let job_handle = job.raw();
    attributes.update(
        PROC_THREAD_ATTRIBUTE_JOB_LIST as usize,
        (&job_handle as *const HANDLE).cast(),
        size_of::<HANDLE>(),
    )?;

    let mut launcher_args = vec![
        OsString::from(INTERNAL_LAUNCHER_ARG),
        bootstrap.path().as_os_str().to_os_string(),
    ];
    launcher_args.extend(args.iter().map(OsString::from));
    let mut command_line = command_line_os(current_exe.as_os_str(), &launcher_args)?;
    let mut startup = STARTUPINFOEXW::default();
    startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
    startup.lpAttributeList = attributes.raw();

    // This opt-in helper is an ordinary spawn from the lifecycle owner at the
    // old inheritance-sensitive phase. It only probes whether the child lock
    // becomes acquirable after this function releases it. The helper has a
    // 60-second deadline and is reaped on every path; it cannot orphan a test
    // process when setup fails.
    let mut lock_probe = LockProbe::spawn(child_lock_path)?;

    let mut process_info = PROCESS_INFORMATION::default();
    // The launcher is an internal implementation detail. Only CREATE_NO_WINDOW
    // is copied to it; all other supported caller flags are applied unchanged
    // to the Git process by the launcher.
    let launcher_flags = CREATE_NO_WINDOW & creation_flags;
    // SAFETY: all pointers reference live, NUL-terminated buffers or valid
    // handles owned by this function for the duration of CreateProcessW.
    let created = unsafe {
        CreateProcessW(
            current_exe_wide.as_ptr(),
            command_line.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            0,
            launcher_flags
                | CREATE_SUSPENDED
                | EXTENDED_STARTUPINFO_PRESENT
                | CREATE_UNICODE_ENVIRONMENT,
            environment_block.as_ptr().cast(),
            current_directory.as_ptr(),
            (&startup as *const STARTUPINFOEXW).cast(),
            &mut process_info,
        )
    } != 0;
    if !created {
        return Err(last_error());
    }
    job.launcher_created = true;

    let process = OwnedHandle::new(process_info.hProcess);
    let launcher_thread = OwnedHandle::new(process_info.hThread);

    // DuplicateHandle writes directly into the suspended launcher's handle
    // table. Every target copy is explicitly non-inheritable. The source
    // handles in this parent were never inheritable either.
    let target_handles = match duplicate_launcher_handles(
        process.raw(),
        raw_handle(&child_lock),
        stdio.stdin.raw(),
        stdio.stdout.write.raw(),
        stdio.stderr.write.raw(),
        done_event.raw(),
        gate_event.raw(),
    ) {
        Ok(handles) => handles,
        Err(error) => {
            stdio.close_child_writes();
            let (error, retain_lease) = cleanup_setup_failure(&mut job, process.raw(), error);
            retain_or_release_child_lease(child_lock, retain_lease);
            return Err(error);
        }
    };

    // The target handle values are intentionally sent after the suspended
    // process exists. Before ResumeThread, parent death can only leave a
    // suspended job member, which kill-on-close terminates.
    if let Err(error) = bootstrap.write(&target_handles, creation_flags) {
        stdio.close_child_writes();
        let (error, retain_lease) = cleanup_setup_failure(&mut job, process.raw(), error);
        retain_or_release_child_lease(child_lock, retain_lease);
        return Err(error);
    }

    // The only possible setup window after handle duplication is the suspended
    // launcher. ResumeThread is the handoff point: the target now owns a lease,
    // and the parent continues to own the job until it proves quiescence.
    // SAFETY: launcher_thread was created suspended and remains valid until
    // after this call. The job already contains the target before it resumes.
    if unsafe { ResumeThread(launcher_thread.raw()) } == u32::MAX {
        stdio.close_child_writes();
        let resume_error = last_error();
        let (error, retain_lease) = cleanup_setup_failure(&mut job, process.raw(), resume_error);
        retain_or_release_child_lease(child_lock, retain_lease);
        return Err(error);
    }
    drop(launcher_thread);

    // Close the parent's copies of the child ends before reading. The target
    // launcher owns its duplicates, and Git owns the final explicit copies.
    stdio.close_child_writes();
    let stdout = stdio.stdout.take_read();
    let stderr = stdio.stderr.take_read();
    let stdout_reader = thread::spawn(move || read_pipe(stdout));
    let stderr_reader = thread::spawn(move || read_pipe(stderr));

    let completion = wait_for_done_or_process(done_event.raw(), process.raw());
    // Completion means Git has synchronously returned and the launcher is
    // waiting on its gate. Terminating the job then closes the launcher lease;
    // the lease is not released until the job wait proves every member gone.
    let cleanup_result = job.terminate_and_wait();
    if let Err(error) = cleanup_result {
        // The launcher may still own the only remaining child lease. Do not
        // join readers whose pipes can still be live: Job's Drop deliberately
        // leaks its handle on this path. Retain the parent's lease too, so a
        // wait failure cannot accidentally release the last lease if the
        // launcher happened to terminate concurrently.
        drop(stdout_reader);
        drop(stderr_reader);
        std::mem::forget(child_lock);
        return Err(error);
    }

    let completion = completion?;
    let process_wait_result = wait_for_process(process.raw());
    let stdout = join_reader(stdout_reader)?;
    let stderr = join_reader(stderr_reader)?;
    process_wait_result?;
    let exit_code = if completion {
        read_launcher_result(bootstrap.path())?
    } else {
        return Err(io::Error::other(
            "lifecycle launcher exited before Git completion",
        ));
    };
    drop(bootstrap);
    drop(child_lock);
    if let Some(probe) = lock_probe.as_mut() {
        probe.finish_after_lease_release()?;
    }
    Ok(Output {
        status: exit_status_from_raw(exit_code),
        stdout,
        stderr,
    })
}

fn retain_or_release_child_lease(child_lock: File, retain: bool) {
    if retain {
        std::mem::forget(child_lock);
    }
}

fn cleanup_setup_failure(job: &mut Job, process: HANDLE, original: io::Error) -> (io::Error, bool) {
    match job.terminate_and_wait() {
        Ok(()) => match wait_for_process(process) {
            Ok(()) => (original, false),
            Err(wait_error) => (
                io::Error::other(format!(
                    "{original}; lifecycle launcher cleanup completed but process reap failed: {wait_error}"
                )),
                false,
            ),
        },
        Err(cleanup_error) => (
            io::Error::other(format!(
                "{original}; lifecycle cleanup failed and the child lease was retained: {cleanup_error}"
            )),
            true,
        ),
    }
}

fn validate_creation_flags(creation_flags: u32) -> io::Result<()> {
    if creation_flags & CREATE_SUSPENDED != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "CREATE_SUSPENDED is incompatible with lifecycle Git containment",
        ));
    }
    if creation_flags & CREATE_BREAKAWAY_FROM_JOB != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "CREATE_BREAKAWAY_FROM_JOB is incompatible with lifecycle Git containment",
        ));
    }
    Ok(())
}

fn raw_handle(file: &File) -> HANDLE {
    use std::os::windows::io::AsRawHandle;
    file.as_raw_handle()
}

fn open_child_lock(path: &Path) -> io::Result<File> {
    let mut options = fs::OpenOptions::new();
    options.read(true).write(true);
    // Rust opens this handle non-inheritable. Do not toggle process-global
    // inheritability: that is exactly the race this launcher architecture
    // avoids.
    options.open(path)
}

fn create_event() -> io::Result<HANDLE> {
    // Null SECURITY_ATTRIBUTES makes the event handle non-inheritable.
    // SAFETY: null attributes/name request a private manual-reset event.
    let event = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
    if event.is_null() {
        Err(last_error())
    } else {
        Ok(event)
    }
}

fn duplicate_launcher_handles(
    target_process: HANDLE,
    child_lock: HANDLE,
    stdin: HANDLE,
    stdout: HANDLE,
    stderr: HANDLE,
    done_event: HANDLE,
    gate_event: HANDLE,
) -> io::Result<LauncherHandles> {
    Ok(LauncherHandles {
        lease: duplicate_into(target_process, child_lock)?,
        stdin: duplicate_into(target_process, stdin)?,
        stdout: duplicate_into(target_process, stdout)?,
        stderr: duplicate_into(target_process, stderr)?,
        done_event: duplicate_into(target_process, done_event)?,
        gate_event: duplicate_into(target_process, gate_event)?,
    })
}

fn duplicate_into(target_process: HANDLE, source: HANDLE) -> io::Result<HANDLE> {
    let mut target = std::ptr::null_mut();
    // SAFETY: source is a live handle owned by this process and target_process
    // is the live suspended launcher. bInheritHandle is explicitly false.
    if unsafe {
        DuplicateHandle(
            GetCurrentProcess(),
            source,
            target_process,
            &mut target,
            0,
            0,
            DUPLICATE_SAME_ACCESS,
        )
    } == 0
    {
        Err(last_error())
    } else {
        Ok(target)
    }
}

#[derive(Clone, Copy)]
struct LauncherHandles {
    lease: HANDLE,
    stdin: HANDLE,
    stdout: HANDLE,
    stderr: HANDLE,
    done_event: HANDLE,
    gate_event: HANDLE,
}

struct BootstrapFile {
    path: PathBuf,
    file: Option<File>,
}

impl BootstrapFile {
    fn new(child_lock_path: &Path) -> io::Result<Self> {
        let directory = child_lock_path.parent().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "managed merge lifecycle child lock has no parent",
            )
        })?;
        let stem = child_lock_path
            .file_name()
            .map(|name| name.to_string_lossy())
            .unwrap_or_else(|| std::borrow::Cow::Borrowed("merge-operation-child.lock"));
        let sequence = BOOTSTRAP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        for attempt in 0..32u32 {
            let path = directory.join(format!(
                ".{stem}-launcher-{}-{sequence}-{attempt}.tmp",
                std::process::id()
            ));
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(file) => {
                    return Ok(Self {
                        path,
                        file: Some(file),
                    })
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not allocate a unique lifecycle launcher bootstrap file",
        ))
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn write(&mut self, handles: &LauncherHandles, creation_flags: u32) -> io::Result<()> {
        let file = self.file.as_mut().expect("bootstrap file is open");
        file.set_len(0)?;
        file.seek(SeekFrom::Start(0))?;
        writeln!(file, "{:x}", handles.lease as usize)?;
        writeln!(file, "{:x}", handles.stdin as usize)?;
        writeln!(file, "{:x}", handles.stdout as usize)?;
        writeln!(file, "{:x}", handles.stderr as usize)?;
        writeln!(file, "{:x}", handles.done_event as usize)?;
        writeln!(file, "{:x}", handles.gate_event as usize)?;
        writeln!(file, "{creation_flags:x}")?;
        file.sync_all()?;
        drop(self.file.take());
        Ok(())
    }
}

impl Drop for BootstrapFile {
    fn drop(&mut self) {
        drop(self.file.take());
        let _ = fs::remove_file(&self.path);
    }
}

fn read_bootstrap(path: &Path) -> io::Result<(LauncherHandles, u32)> {
    let contents = fs::read_to_string(path)?;
    let mut lines = contents.lines();
    let handles = LauncherHandles {
        lease: parse_handle(lines.next())?,
        stdin: parse_handle(lines.next())?,
        stdout: parse_handle(lines.next())?,
        stderr: parse_handle(lines.next())?,
        done_event: parse_handle(lines.next())?,
        gate_event: parse_handle(lines.next())?,
    };
    let flags = lines
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "launcher flags missing"))?
        .trim()
        .parse::<u32>()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "launcher flags invalid"))?;
    if lines.next().is_some() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "launcher bootstrap has trailing data",
        ));
    }
    Ok((handles, flags))
}

fn parse_handle(value: Option<&str>) -> io::Result<HANDLE> {
    let value = value
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "launcher handle missing"))?
        .trim();
    let raw = usize::from_str_radix(value, 16)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "launcher handle invalid"))?;
    let handle = raw as HANDLE;
    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "launcher handle is null",
        ));
    }
    Ok(handle)
}

/// Handle the private launcher command before Clap parses normal CLI input.
/// The launcher is the same executable so no extra binary or inherited control
/// channel is required. Test binaries do not invoke this entry point; their
/// Windows coverage exercises the public CLI integration path instead.
pub(crate) fn run_launcher_if_requested() {
    let mut args = std::env::args_os();
    let _argv0 = args.next();
    match args.next().as_deref() {
        Some(value) if value == OsStr::new(INTERNAL_LAUNCHER_ARG) => {
            let code = match args.next() {
                Some(path) => launcher_main(PathBuf::from(path), args.collect()),
                None => 1,
            };
            std::process::exit(code as i32);
        }
        Some(value) if value == OsStr::new(INTERNAL_LOCK_PROBE_ARG) => {
            let code = match (args.next(), args.next(), args.next()) {
                (Some(lock), Some(started), Some(acquired)) => lock_probe_main(
                    PathBuf::from(lock),
                    PathBuf::from(started),
                    PathBuf::from(acquired),
                ),
                _ => 1,
            };
            std::process::exit(code);
        }
        _ => {}
    }
}

/// A bounded helper used only by Windows integration tests. It is spawned by
/// the lifecycle owner, not by the test runner, and attempts to acquire the
/// child lock after the owner drops it. If the owner accidentally lends the
/// lock handle to this unrelated process, acquisition never succeeds; the
/// helper times out and is reaped instead of waiting on an unbounded release
/// file.
struct LockProbe {
    child: Option<Child>,
    acquired: PathBuf,
}

impl LockProbe {
    fn spawn(child_lock_path: &Path) -> io::Result<Option<Self>> {
        // Integration tests run the debug binary. Keep this internal protocol
        // out of release builds even if an inherited environment is hostile.
        if !cfg!(debug_assertions) {
            return Ok(None);
        }
        let Some(root) = std::env::var_os(LOCK_PROBE_ENV) else {
            return Ok(None);
        };
        let root = PathBuf::from(root);
        let started = root.join("owner-lock-probe-started");
        let acquired = root.join("owner-lock-probe-acquired");
        let executable = std::env::current_exe()?;
        let child = std::process::Command::new(executable)
            .arg(INTERNAL_LOCK_PROBE_ARG)
            .arg(child_lock_path)
            .arg(&started)
            .arg(&acquired)
            .env_remove(LOCK_PROBE_ENV)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()?;
        Ok(Some(Self {
            child: Some(child),
            acquired,
        }))
    }

    fn finish_after_lease_release(&mut self) -> io::Result<()> {
        let mut child = self
            .child
            .take()
            .ok_or_else(|| io::Error::other("lifecycle lock probe was already reaped"))?;
        let deadline = std::time::Instant::now() + LOCK_PROBE_TIMEOUT;
        loop {
            if child.try_wait()?.is_some() {
                let output = child.wait_with_output()?;
                return validate_lock_probe_output(output, &self.acquired);
            }
            if std::time::Instant::now() >= deadline {
                kill_and_reap_probe(&mut child);
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "lifecycle lock probe exceeded its 60-second deadline",
                ));
            }
            thread::sleep(std::time::Duration::from_millis(25));
        }
    }
}

fn kill_and_reap_probe(child: &mut Child) {
    if let Err(error) = child.kill() {
        eprintln!("failed to terminate Windows lifecycle test helper: {error}");
    }
    if let Err(error) = child.wait() {
        eprintln!("failed to reap Windows lifecycle test helper: {error}");
    }
}

fn validate_lock_probe_output(output: Output, acquired: &Path) -> io::Result<()> {
    if !output.status.success()
        || !fs::read_to_string(acquired)
            .map(|contents| contents == "acquired\n")
            .unwrap_or(false)
    {
        return Err(io::Error::other(format!(
            "lifecycle lock probe failed: stdout={:?}, stderr={:?}",
            output.stdout, output.stderr
        )));
    }
    Ok(())
}

impl Drop for LockProbe {
    fn drop(&mut self) {
        let Some(mut child) = self.child.take() else {
            return;
        };
        let deadline = std::time::Instant::now() + LOCK_PROBE_TIMEOUT;
        loop {
            match child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) if std::time::Instant::now() < deadline => {
                    thread::sleep(std::time::Duration::from_millis(25));
                }
                Ok(None) => {
                    kill_and_reap_probe(&mut child);
                    return;
                }
                Err(error) => {
                    eprintln!("failed to poll Windows lifecycle test helper: {error}");
                    kill_and_reap_probe(&mut child);
                    return;
                }
            }
        }
    }
}

fn lock_probe_main(child_lock_path: PathBuf, started: PathBuf, acquired: PathBuf) -> i32 {
    if fs::write(&started, "started\n").is_err() {
        return 1;
    }
    let lock = match OpenOptions::new()
        .read(true)
        .write(true)
        .open(child_lock_path)
    {
        Ok(lock) => lock,
        Err(_) => return 1,
    };
    let deadline = std::time::Instant::now() + LOCK_PROBE_TIMEOUT;
    loop {
        match super::try_lock_exclusive(&lock) {
            Ok(true) => {
                // Publish only after the successful lock handle is dropped;
                // the marker is therefore an explicit lease-release event.
                drop(lock);
                return i32::from(fs::write(acquired, "acquired\n").is_err());
            }
            Ok(false) if std::time::Instant::now() < deadline => {
                thread::sleep(std::time::Duration::from_millis(25));
            }
            Ok(false) => return 2,
            Err(_) => return 1,
        }
    }
}

fn read_launcher_result(path: &Path) -> io::Result<u32> {
    let result = fs::read_to_string(path)?;
    let line = result
        .lines()
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "launcher result missing"))?;
    let code = line
        .strip_prefix("exit=")
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "launcher result invalid"))?
        .parse::<u32>()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "launcher exit code invalid"))?;
    Ok(code)
}

fn launcher_main(bootstrap_path: PathBuf, git_args: Vec<OsString>) -> u32 {
    let (handles, creation_flags) = match read_bootstrap(&bootstrap_path) {
        Ok(value) => value,
        Err(_) => return 1,
    };
    // Remove the setup record as soon as the suspended-target handoff has been
    // consumed. The same path is recreated only for the completion result.
    let _ = fs::remove_file(&bootstrap_path);

    let exit_code = launch_git_from_launcher(handles, &git_args, creation_flags).unwrap_or(1);

    // The result is written and closed before the parent is notified. The
    // launcher then waits on an unsignaled gate; the parent terminates the job
    // and waits for quiescence, so the lease cannot be released early.
    let _ = fs::write(&bootstrap_path, format!("exit={exit_code}\n"));
    if unsafe { SetEvent(handles.done_event) } == 0 {
        return 1;
    }
    // The parent deliberately never signals this event. Terminating the job
    // closes the launcher and its lease after all remaining members are gone.
    let _ = wait_for_process(handles.gate_event);
    1
}

fn launch_git_from_launcher(
    handles: LauncherHandles,
    args: &[OsString],
    creation_flags: u32,
) -> io::Result<u32> {
    validate_creation_flags(creation_flags)?;

    // The copies in the launcher are non-inheritable. Create only private,
    // inheritable duplicates immediately for Git's explicit HANDLE_LIST; no
    // parent process can race this local launcher with an unrelated spawn.
    let stdin = duplicate_in_launcher(handles.stdin, true)?;
    let stdout = match duplicate_in_launcher(handles.stdout, true) {
        Ok(handle) => handle,
        Err(error) => {
            close_raw(stdin);
            return Err(error);
        }
    };
    let stderr = match duplicate_in_launcher(handles.stderr, true) {
        Ok(handle) => handle,
        Err(error) => {
            close_raw(stdin);
            close_raw(stdout);
            return Err(error);
        }
    };
    let inherited_handles = [stdin, stdout, stderr];
    let mut attributes = match AttributeList::new(1) {
        Ok(attributes) => attributes,
        Err(error) => {
            close_handles(&inherited_handles);
            return Err(error);
        }
    };
    if let Err(error) = attributes.update(
        PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
        inherited_handles.as_ptr().cast(),
        size_of::<HANDLE>() * inherited_handles.len(),
    ) {
        close_handles(&inherited_handles);
        return Err(error);
    }

    let mut command_line = match command_line_os(OsStr::new("git"), args) {
        Ok(command_line) => command_line,
        Err(error) => {
            close_handles(&inherited_handles);
            return Err(error);
        }
    };
    let mut startup = STARTUPINFOEXW::default();
    startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    startup.StartupInfo.hStdInput = stdin;
    startup.StartupInfo.hStdOutput = stdout;
    startup.StartupInfo.hStdError = stderr;
    startup.lpAttributeList = attributes.raw();

    let mut process_info = PROCESS_INFORMATION::default();
    // Null environment and current directory intentionally inherit the
    // launcher's exact environment/cwd, supplied by the parent at creation.
    // The launcher itself does not mutate either value.
    let created = unsafe {
        CreateProcessW(
            std::ptr::null(),
            command_line.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            1,
            creation_flags | EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT,
            std::ptr::null_mut(),
            std::ptr::null(),
            (&startup as *const STARTUPINFOEXW).cast(),
            &mut process_info,
        )
    } != 0;

    close_handles(&inherited_handles);
    if !created {
        return Err(last_error());
    }

    close_raw(process_info.hThread);
    let process = OwnedHandle::new(process_info.hProcess);
    wait_for_process(process.raw())?;
    get_exit_code(process.raw())
}

fn duplicate_in_launcher(source: HANDLE, inheritable: bool) -> io::Result<HANDLE> {
    let mut target = std::ptr::null_mut();
    // SAFETY: source is a valid handle duplicated into this launcher. The
    // inheritable copy is restricted to the one explicit Git CreateProcessW.
    if unsafe {
        DuplicateHandle(
            GetCurrentProcess(),
            source,
            GetCurrentProcess(),
            &mut target,
            0,
            if inheritable { 1 } else { 0 },
            DUPLICATE_SAME_ACCESS,
        )
    } == 0
    {
        Err(last_error())
    } else {
        Ok(target)
    }
}

fn close_raw(handle: HANDLE) {
    if !handle.is_null() && handle != INVALID_HANDLE_VALUE {
        // SAFETY: this function is called only for owned Win32 handles.
        unsafe { CloseHandle(handle) };
    }
}

fn close_handles(handles: &[HANDLE]) {
    for &handle in handles {
        close_raw(handle);
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

struct Job {
    handle: OwnedHandle,
    quiesced: bool,
    launcher_created: bool,
    #[cfg(test)]
    cleanup_failure: Option<CleanupFailure>,
}

#[cfg(test)]
#[derive(Clone, Copy)]
enum CleanupFailure {
    Terminate,
    Wait,
}

#[cfg(test)]
static RETAINED_JOB_HANDLES: AtomicU64 = AtomicU64::new(0);

impl Job {
    fn new() -> io::Result<Self> {
        // SAFETY: null attributes/name request a private unnamed job.
        let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if handle.is_null() {
            return Err(last_error());
        }

        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        // BREAKAWAY_OK and SILENT_BREAKAWAY_OK are intentionally absent. A
        // launcher or Git descendant therefore cannot escape this job.
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
        Ok(Self {
            handle: OwnedHandle::new(handle),
            quiesced: false,
            launcher_created: false,
            #[cfg(test)]
            cleanup_failure: None,
        })
    }

    #[cfg(test)]
    fn new_with_cleanup_failure(failure: CleanupFailure) -> io::Result<Self> {
        let mut job = Self::new()?;
        job.cleanup_failure = Some(failure);
        Ok(job)
    }

    fn raw(&self) -> HANDLE {
        self.handle.raw()
    }

    fn terminate_and_wait(&mut self) -> io::Result<()> {
        #[cfg(test)]
        match self.cleanup_failure.take() {
            Some(CleanupFailure::Terminate) => {
                return Err(io::Error::other("injected TerminateJobObject failure"));
            }
            Some(CleanupFailure::Wait) => {
                // Exercise the same fail-closed state as a real WAIT_FAILED
                // without relying on a race or an invalid kernel handle.
                // Termination still happens below, but quiescence is withheld.
                // The injected error is returned after the call.
                if unsafe { TerminateJobObject(self.raw(), 1) } == 0 {
                    return Err(last_error());
                }
                return Err(io::Error::other("injected WaitForSingleObject failure"));
            }
            None => {}
        }

        // SAFETY: the job handle is owned by self and remains live here.
        if unsafe { TerminateJobObject(self.raw(), 1) } == 0 {
            return Err(last_error());
        }
        // A successful wait is the proof that no job member can retain the
        // duplicated lease. Neither a TerminateJobObject failure nor a wait
        // failure may mark this job quiesced.
        wait_for_process(self.raw())?;
        self.quiesced = true;
        Ok(())
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        if self.quiesced || !self.launcher_created {
            return;
        }

        // Drop cannot report an OS error. Closing this handle would activate
        // kill-on-close without proving that the launcher and Git are gone,
        // allowing the last child lease to disappear before quiescence. Leave
        // the job handle open instead: a live launcher retains the child lease
        // and blocks recovery; process exit closes the handle and lets the job
        // kill its remaining members. This intentional leak is the fail-closed
        // fallback for every unexpected cleanup failure.
        let leaked = std::mem::replace(&mut self.handle, OwnedHandle(std::ptr::null_mut()));
        std::mem::forget(leaked);
        #[cfg(test)]
        RETAINED_JOB_HANDLES.fetch_add(1, Ordering::Relaxed);
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
        // bInheritHandle=0 is essential: the parent never creates a temporary
        // inheritable pipe end, even while another thread may spawn a child.
        let attributes = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: std::ptr::null_mut(),
            bInheritHandle: 0,
        };
        let mut read = std::ptr::null_mut();
        let mut write = std::ptr::null_mut();
        // SAFETY: output pointers and security attributes remain live for the call.
        if unsafe { CreatePipe(&mut read, &mut write, &attributes, 0) } == 0 {
            return Err(last_error());
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
                windows_sys::Win32::Foundation::GENERIC_READ,
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
    // SAFETY: handle is an owned process, event, or job synchronization handle.
    if unsafe { WaitForSingleObject(handle, INFINITE) } != WAIT_OBJECT_0 {
        Err(last_error())
    } else {
        Ok(())
    }
}

fn wait_for_done_or_process(done: HANDLE, process: HANDLE) -> io::Result<bool> {
    let handles = [done, process];
    // SAFETY: both handles remain live for the duration of this wait.
    let result = unsafe { WaitForMultipleObjects(2, handles.as_ptr(), 0, INFINITE) };
    if result == WAIT_OBJECT_0 {
        Ok(true)
    } else if result == WAIT_OBJECT_0 + 1 {
        Ok(false)
    } else if result == WAIT_FAILED {
        Err(last_error())
    } else {
        Err(io::Error::other(
            "unexpected lifecycle launcher wait result",
        ))
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

fn command_line_os(program: &OsStr, args: &[OsString]) -> io::Result<Vec<u16>> {
    let mut line = Vec::new();
    quote_windows_arg(&mut line, program)?;
    for value in args {
        line.push(' ' as u16);
        quote_windows_arg(&mut line, value)?;
    }
    line.push(0);
    Ok(line)
}

fn quote_windows_arg(output: &mut Vec<u16>, value: &OsStr) -> io::Result<()> {
    let units: Vec<u16> = value.encode_wide().collect();
    if units.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Windows process argument contains NUL",
        ));
    }
    output.push('"' as u16);
    let mut backslashes = 0usize;
    for unit in units {
        match unit {
            92 => backslashes += 1,
            34 => {
                output.extend(std::iter::repeat(b'\\' as u16).take(backslashes * 2 + 1));
                output.push(b'"' as u16);
                backslashes = 0;
            }
            unit => {
                output.extend(std::iter::repeat(b'\\' as u16).take(backslashes));
                output.push(unit);
                backslashes = 0;
            }
        }
    }
    output.extend(std::iter::repeat(b'\\' as u16).take(backslashes * 2));
    output.push('"' as u16);
    Ok(())
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
    if block.is_empty() {
        // CreateProcessW requires two NUL code units even for an empty block.
        block.extend_from_slice(&[0, 0]);
    } else {
        block.push(0);
    }
    Ok(block)
}

fn exit_status_from_raw(code: u32) -> ExitStatus {
    use std::os::windows::process::ExitStatusExt;
    ExitStatusExt::from_raw(code)
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::System::Threading::CREATE_NEW_PROCESS_GROUP;

    #[test]
    fn empty_environment_is_double_nul_terminated() {
        assert_eq!(
            environment_block(&[]).expect("empty environment"),
            vec![0, 0]
        );
    }

    #[test]
    fn nonempty_environment_is_sorted_and_double_terminated() {
        let block = environment_block(&[
            (OsString::from("z-key"), OsString::from("z")),
            (OsString::from("A-key"), OsString::from("a")),
        ])
        .expect("environment block");
        assert!(block.ends_with(&[0, 0]));
        assert!(block
            .windows(2)
            .any(|entry| entry == [b'A' as u16, b'-' as u16]));
    }

    #[test]
    fn command_line_quotes_backslashes_quotes_and_unicode() {
        let args = vec![
            OsString::from(r#"path with spaces\"#),
            OsString::from("значение"),
            OsString::from(""),
        ];
        let line = command_line_os(OsStr::new("git"), &args).expect("command line");
        assert!(line
            .windows(4)
            .any(|window| window == [0x0437, 0x043d, 0x0430, 0x0447]));
        assert_eq!(line.last(), Some(&0));
    }

    #[test]
    fn incompatible_creation_flags_are_rejected() {
        assert!(validate_creation_flags(CREATE_SUSPENDED).is_err());
        assert!(validate_creation_flags(CREATE_BREAKAWAY_FROM_JOB).is_err());
        assert!(validate_creation_flags(0).is_ok());
    }

    #[test]
    fn cleanup_failures_retain_an_unquiesced_job_handle() {
        let before = RETAINED_JOB_HANDLES.load(Ordering::Relaxed);
        for failure in [CleanupFailure::Terminate, CleanupFailure::Wait] {
            let mut job = Job::new_with_cleanup_failure(failure).expect("test job");
            // Model the post-CreateProcess state in which the job owns the
            // launcher and therefore owns the only safe cleanup boundary.
            job.launcher_created = true;
            assert!(job.terminate_and_wait().is_err());
            assert!(!job.quiesced);
            drop(job);
        }
        assert_eq!(
            RETAINED_JOB_HANDLES.load(Ordering::Relaxed),
            before + 2,
            "cleanup failures must use the retaining Drop path"
        );
    }

    #[test]
    fn setup_failure_does_not_leave_a_child_lease() {
        let repository = tempfile::tempdir().expect("temporary repository");
        let environment = crate::git::sanitized_git_environment();
        let missing = repository.path().join("missing/merge-operation.lock");
        assert!(output_git(&missing, &["--version"], repository.path(), &environment).is_err());

        let lock_path = repository.path().join("merge-operation.lock");
        fs::File::create(&lock_path).expect("child lease file");
        let lock = open_child_lock(&lock_path).expect("child lock remains usable");
        assert!(crate::operation_state::try_lock_exclusive(&lock).expect("child lock probe"));
    }

    #[test]
    fn lifecycle_git_captures_exact_output_with_unicode_args_cwd_and_env() {
        let repository = tempfile::tempdir().expect("temporary repository");
        let cwd = repository.path().join("рабочая папка");
        fs::create_dir(&cwd).expect("unicode cwd");
        assert!(std::process::Command::new("git")
            .args(["init", "--initial-branch=main"])
            .current_dir(&cwd)
            .status()
            .expect("git init")
            .success());

        let lock_path = repository.path().join("merge-operation.lock");
        fs::File::create(&lock_path).expect("child lease file");
        let exact_value = r#"значение with spaces "and" \ trailing"#;
        let config = format!("wt.lifecycle={exact_value}");
        let mut environment = crate::git::sanitized_git_environment();
        environment.push((
            OsString::from("WT_WINDOWS_EXACT_ENV"),
            OsString::from(exact_value),
        ));
        let output = output_git_with_creation_flags(
            &lock_path,
            &["-c", &config, "config", "--get", "wt.lifecycle"],
            &cwd,
            &environment,
            CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP,
        )
        .expect("Git config should run");
        assert!(output.status.success());
        assert_eq!(output.stdout, format!("{exact_value}\n").as_bytes());
        assert_eq!(output.stderr, b"");

        let expected = std::process::Command::new("git")
            .args(["rev-parse", "--show-toplevel"])
            .current_dir(&cwd)
            .env_clear()
            .envs(environment.iter().map(|(key, value)| (key, value)))
            .output()
            .expect("reference Git invocation");
        let actual = output_git(
            &lock_path,
            &["rev-parse", "--show-toplevel"],
            &cwd,
            &environment,
        )
        .expect("contained Git invocation");
        assert_eq!(actual.stdout, expected.stdout);
        assert_eq!(actual.stderr, expected.stderr);

        let failed = output_git(
            &lock_path,
            &["config", "--get", "wt.missing"],
            &cwd,
            &environment,
        )
        .expect("Git failures are returned as Output");
        assert!(!failed.status.success());
        assert_eq!(failed.stdout, b"");
        assert_eq!(failed.stderr, b"");
        let lock = open_child_lock(&lock_path).expect("child lock remains");
        assert!(crate::operation_state::try_lock_exclusive(&lock).expect("child lock probe"));
    }
}
