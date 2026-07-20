//! Windows containment for lifecycle Git.
//!
//! The lifecycle owner only owns the repository lock.  A normally-created,
//! out-of-job guardian owns the child lease and the kill-on-close job.  That
//! distinction is the important lifetime invariant: killing the owner cannot
//! close the guardian's lease before the job has been terminated and waited.
//!
//! Git and hooks that Git synchronously waits for are supported.  A hook that
//! daemonizes and mutates the repository after Git returns is unsupported; the
//! guardian terminates and waits every remaining job member before releasing
//! the child lease.

use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Cursor, Read, Write};
use std::mem::size_of;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::io::FromRawHandle;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use windows_sys::Win32::Foundation::GENERIC_READ;
use windows_sys::Win32::Foundation::{
    CloseHandle, DuplicateHandle, GetLastError, LocalFree, DUPLICATE_SAME_ACCESS, HANDLE,
    INVALID_HANDLE_VALUE, WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::Security::Authorization::{
    GetNamedSecurityInfoW, SetNamedSecurityInfoW, SE_FILE_OBJECT,
};
use windows_sys::Win32::Security::{
    AclSizeInformation, AddAccessAllowedAce, EqualSid, GetAce, GetAclInformation,
    GetSecurityDescriptorDacl, GetTokenInformation, InitializeAcl, TokenUser, ACCESS_ALLOWED_ACE,
    ACL, ACL_REVISION, ACL_SIZE_INFORMATION, DACL_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION,
    SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ALL_ACCESS, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE,
    OPEN_EXISTING,
};
use windows_sys::Win32::System::JobObjects::{
    CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject,
    TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows_sys::Win32::System::Pipes::CreatePipe;
use windows_sys::Win32::System::Threading::{
    CreateEventW, CreateProcessW, DeleteProcThreadAttributeList, GetCurrentProcess,
    GetExitCodeProcess, InitializeProcThreadAttributeList, OpenProcessToken, ResumeThread,
    SetEvent, UpdateProcThreadAttribute, WaitForMultipleObjects, WaitForSingleObject,
    CREATE_BREAKAWAY_FROM_JOB, CREATE_NO_WINDOW, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT,
    EXTENDED_STARTUPINFO_PRESENT, INFINITE, PROCESS_INFORMATION, PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
    PROC_THREAD_ATTRIBUTE_JOB_LIST, STARTF_USESTDHANDLES, STARTUPINFOEXW,
};

const ERROR_INSUFFICIENT_BUFFER: u32 = 122;
const INTERNAL_GUARDIAN_ARG: &str = "--__wt-core-windows-lifecycle-guardian";
const GUARDIAN_MAGIC: &[u8] = b"wt-core-windows-guardian-v1\0";
const HANDOFF_MAGIC: &[u8] = b"wt-core-windows-guardian-handoff-v1\0";
const RESULT_MAGIC: &[u8] = b"wt-core-windows-guardian-result-v1\0";
const GUARDIAN_SETUP_TIMEOUT: Duration = Duration::from_secs(30);
const GUARDIAN_RETRY_DELAY: Duration = Duration::from_millis(25);
const TEST_FAIL_AFTER_JOB_ENV: &str = "WT_CORE_WINDOWS_LIFECYCLE_FAIL_AFTER_JOB";
const TEST_CLEANUP_GATE_ENV: &str = "WT_CORE_WINDOWS_LIFECYCLE_CLEANUP_GATE";
const TEST_HANDSHAKE_PHASE_ENV: &str = "WT_CORE_WINDOWS_LIFECYCLE_HANDSHAKE_PHASE";
const TEST_HANDSHAKE_GATE_ENV: &str = "WT_CORE_WINDOWS_LIFECYCLE_HANDSHAKE_GATE";
static PROTOCOL_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Run Git with captured stdout/stderr. The parent never owns the child lease;
/// the surviving guardian acquires it and publishes READY before the parent can
/// authorize the command handoff.
pub(crate) fn output_git(
    child_lock_path: &Path,
    args: &[&str],
    cwd: &Path,
    environment: &[(OsString, OsString)],
) -> io::Result<std::process::Output> {
    output_git_with_creation_flags(child_lock_path, args, cwd, environment, 0)
}

/// Run Git with caller-provided creation flags.
///
/// `CREATE_SUSPENDED` cannot be preserved because the guardian uses suspension
/// only for the atomically job-assigned Git process.  `CREATE_BREAKAWAY_FROM_JOB`
/// is rejected because lifecycle Git must not escape its job.
pub(crate) fn output_git_with_creation_flags(
    child_lock_path: &Path,
    args: &[&str],
    cwd: &Path,
    environment: &[(OsString, OsString)],
    creation_flags: u32,
) -> io::Result<std::process::Output> {
    validate_creation_flags(creation_flags)?;

    let current_exe = std::env::current_exe()?;
    let nonce = new_nonce();
    let paths = ProtocolPaths::new(child_lock_path)?;
    sweep_stale_protocol_files_for_lock(child_lock_path)?;
    let config = GuardianConfig {
        nonce: nonce.clone(),
        operation_id: nonce.clone(),
        bootstrap_path: paths.bootstrap.clone(),
        child_lock_path: child_lock_path.to_path_buf(),
        result_path: paths.result.clone(),
        status_path: paths.status.clone(),
        owner_handoff_path: paths.owner_handoff.clone(),
        command_handoff_path: paths.command_handoff.clone(),
        cwd: cwd.to_path_buf(),
        args: args.iter().map(OsString::from).collect(),
        environment: environment.to_vec(),
        creation_flags,
        // These are test-only values in the private bootstrap. They are never
        // read from a process-global switch by the guardian after startup.
        fail_after_job: cfg!(debug_assertions)
            && std::env::var_os(TEST_FAIL_AFTER_JOB_ENV).as_deref() == Some(OsStr::new("1")),
        cleanup_gate: cfg!(debug_assertions)
            .then(|| std::env::var_os(TEST_CLEANUP_GATE_ENV))
            .flatten()
            .map(PathBuf::from),
    };
    ProtocolFile::write_new(&paths.bootstrap, encode_config(&config)?)?;

    let mut stdio = StdioHandles::new()?;
    let start_event = OwnedHandle::new(create_event()?);
    let abort_event = OwnedHandle::new(create_event()?);
    let current_exe_wide = wide_path(current_exe.as_os_str())?;
    let guardian_args = vec![
        OsString::from(INTERNAL_GUARDIAN_ARG),
        paths.bootstrap.as_os_str().to_os_string(),
        paths.command_handoff.as_os_str().to_os_string(),
        OsString::from(&nonce),
    ];
    let mut command_line = command_line_os(current_exe.as_os_str(), &guardian_args)?;
    let mut startup = STARTUPINFOEXW::default();
    startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
    let mut process_info = PROCESS_INFORMATION::default();
    let guardian_flags = CREATE_UNICODE_ENVIRONMENT | (creation_flags & CREATE_NO_WINDOW);

    wait_for_test_handshake("parent-before-guardian");
    // The guardian is deliberately a normal process outside the Git job. No
    // lifecycle handle is inheritable and bInheritHandles is false.
    let created = unsafe {
        // SAFETY: all pointers reference live, NUL-terminated buffers for the
        // duration of CreateProcessW; no attribute list or inherited handles
        // are supplied to the guardian.
        CreateProcessW(
            current_exe_wide.as_ptr(),
            command_line.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            0,
            guardian_flags,
            std::ptr::null_mut(),
            std::ptr::null(),
            (&startup as *const STARTUPINFOEXW).cast(),
            &mut process_info,
        )
    } != 0;
    if !created {
        return Err(last_error());
    }

    let guardian = OwnedHandle::new(process_info.hProcess);
    close_raw(process_info.hThread);

    let owner_handoff = duplicate_guardian_parent_handle(guardian.raw())
        .and_then(|parent_process| encode_owner_handoff(&nonce, parent_process));
    let owner_handoff = match owner_handoff {
        Ok(contents) => contents,
        Err(error) => {
            let _ = wait_for_process(guardian.raw());
            return Err(error);
        }
    };
    if let Err(error) = ProtocolFile::write_new(&paths.owner_handoff, owner_handoff) {
        let _ = wait_for_process(guardian.raw());
        return Err(error);
    }

    let ready = match wait_for_ready_or_process(&paths.status, guardian.raw(), &nonce) {
        Ok(ready) => ready,
        Err(error) => {
            let _ = wait_for_process(guardian.raw());
            return Err(error);
        }
    };
    if !ready {
        let guardian_result = wait_for_process(guardian.raw());
        stdio.close_child_writes();
        let _ = read_pipe(stdio.stdout.take_read());
        let _ = read_pipe(stdio.stderr.take_read());
        guardian_result?;
        return Err(read_guardian_error(
            &paths.result,
            &nonce,
            "guardian exited before ready",
        ));
    }

    // READY is written only after the guardian owns the child lease and its
    // kill-on-close job. The parent is now allowed to publish authorization.
    wait_for_test_handshake("parent-after-ready-before-command");
    let command_handoff = duplicate_guardian_command_handles(
        guardian.raw(),
        stdio.stdout.write.raw(),
        stdio.stderr.write.raw(),
        start_event.raw(),
        abort_event.raw(),
    )
    .and_then(|handles| encode_command_handoff(&nonce, handles));
    let command_handoff = match command_handoff {
        Ok(contents) => contents,
        Err(error) => {
            let _ = unsafe { SetEvent(abort_event.raw()) };
            let _ = wait_for_process(guardian.raw());
            return Err(error);
        }
    };
    if let Err(error) = ProtocolFile::write_new(&paths.command_handoff, command_handoff) {
        let _ = unsafe { SetEvent(abort_event.raw()) };
        let _ = wait_for_process(guardian.raw());
        return Err(error);
    }

    stdio.close_child_writes();
    let stdout_reader = thread::spawn({
        let stdout = stdio.stdout.take_read();
        move || read_pipe(stdout)
    });
    let stderr_reader = thread::spawn({
        let stderr = stdio.stderr.take_read();
        move || read_pipe(stderr)
    });

    // The start event is the final authorization. A killed owner before this
    // point leaves the guardian with no permission to run Git.
    wait_for_test_handshake("parent-command-before-start");
    if unsafe { SetEvent(start_event.raw()) } == 0 {
        let error = last_error();
        let _ = unsafe { SetEvent(abort_event.raw()) };
        wait_for_process(guardian.raw())?;
        let _ = join_reader(stdout_reader);
        let _ = join_reader(stderr_reader);
        return Err(error);
    }

    let guardian_wait = wait_for_process(guardian.raw());
    let stdout = join_reader(stdout_reader)?;
    let stderr = join_reader(stderr_reader)?;
    guardian_wait?;

    let result = read_result(&paths.result, &nonce)?;
    if let Some(error) = result.error {
        return Err(io::Error::other(error));
    }
    Ok(std::process::Output {
        status: exit_status_from_raw(result.exit_code.unwrap_or(1)),
        stdout,
        stderr,
    })
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

fn new_nonce() -> String {
    let sequence = PROTOCOL_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    format!("{}-{nanos:x}-{sequence:x}", std::process::id())
}

struct ProtocolPaths {
    bootstrap: PathBuf,
    owner_handoff: PathBuf,
    command_handoff: PathBuf,
    status: PathBuf,
    result: PathBuf,
}

impl ProtocolPaths {
    fn new(child_lock_path: &Path) -> io::Result<Self> {
        let directory = child_lock_path.parent().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "managed merge lifecycle child lock has no parent",
            )
        })?;
        let stem = child_lock_path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "merge-operation-child.lock".to_string());
        Ok(Self {
            bootstrap: allocate_protocol_path(directory, &stem, "bootstrap")?,
            owner_handoff: allocate_protocol_path(directory, &stem, "owner-handoff")?,
            command_handoff: allocate_protocol_path(directory, &stem, "command-handoff")?,
            status: allocate_protocol_path(directory, &stem, "status")?,
            result: allocate_protocol_path(directory, &stem, "result")?,
        })
    }
}

impl Drop for ProtocolPaths {
    fn drop(&mut self) {
        for path in [
            &self.bootstrap,
            &self.owner_handoff,
            &self.command_handoff,
            &self.status,
            &self.result,
        ] {
            let _ = fs::remove_file(path);
        }
    }
}

fn sweep_all_stale_protocol_files(directory: &Path) -> io::Result<()> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        let Some(name) = path.file_name().and_then(OsStr::to_str) else {
            continue;
        };
        if name.starts_with('.') && name.contains("-guardian-") {
            validate_protocol_file(&path)?;
            fs::remove_file(path)?;
        }
    }
    Ok(())
}

pub(crate) fn sweep_stale_protocol_files_for_lock(child_lock_path: &Path) -> io::Result<()> {
    let directory = child_lock_path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "managed child lock has no parent",
        )
    })?;
    let stem = child_lock_path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "merge-operation-child.lock".to_string());
    sweep_stale_protocol_files(directory, &stem)
}

fn sweep_stale_protocol_files(directory: &Path, stem: &str) -> io::Result<()> {
    let prefix = format!(".{stem}-guardian-");
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        let Some(name) = path.file_name().and_then(OsStr::to_str) else {
            continue;
        };
        if !name.starts_with(&prefix) || (!name.ends_with(".tmp") && !name.ends_with(".write-tmp"))
        {
            continue;
        }
        validate_protocol_file(&path)?;
        fs::remove_file(path)?;
    }
    Ok(())
}

fn allocate_protocol_path(directory: &Path, stem: &str, kind: &str) -> io::Result<PathBuf> {
    let sequence = PROTOCOL_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    for attempt in 0..32u32 {
        let path = directory.join(format!(
            ".{stem}-guardian-{kind}-{}-{sequence}-{attempt}.tmp",
            std::process::id()
        ));
        if !path.exists() {
            return Ok(path);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not allocate a unique Windows lifecycle guardian file",
    ))
}

struct ProtocolFile;

impl ProtocolFile {
    fn write_new(path: &Path, contents: Vec<u8>) -> io::Result<()> {
        let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
        if let Err(error) = ensure_protocol_file_private(path) {
            let _ = fs::remove_file(path);
            return Err(error);
        }
        file.write_all(&contents)?;
        file.sync_all()
    }

    fn write_atomic(path: &Path, contents: Vec<u8>) -> io::Result<()> {
        let temporary = path.with_extension("write-tmp");
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        if let Err(error) = ensure_protocol_file_private(&temporary) {
            let _ = fs::remove_file(&temporary);
            return Err(error);
        }
        file.write_all(&contents)?;
        file.sync_all()?;
        if let Err(error) = replace_protocol_file(&temporary, path) {
            let _ = fs::remove_file(&temporary);
            return Err(error);
        }
        ensure_protocol_file_private(path)
    }
}

fn replace_protocol_file(source: &Path, destination: &Path) -> io::Result<()> {
    #[cfg(windows)]
    {
        let source = wide_path(source.as_os_str())?;
        let destination = wide_path(destination.as_os_str())?;
        if unsafe {
            windows_sys::Win32::Storage::FileSystem::MoveFileExW(
                source.as_ptr(),
                destination.as_ptr(),
                windows_sys::Win32::Storage::FileSystem::MOVEFILE_REPLACE_EXISTING
                    | windows_sys::Win32::Storage::FileSystem::MOVEFILE_WRITE_THROUGH,
            )
        } == 0
        {
            return Err(last_error());
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        fs::rename(source, destination)
    }
}

struct GuardianConfig {
    nonce: String,
    operation_id: String,
    bootstrap_path: PathBuf,
    child_lock_path: PathBuf,
    result_path: PathBuf,
    status_path: PathBuf,
    owner_handoff_path: PathBuf,
    command_handoff_path: PathBuf,
    cwd: PathBuf,
    args: Vec<OsString>,
    environment: Vec<(OsString, OsString)>,
    creation_flags: u32,
    fail_after_job: bool,
    cleanup_gate: Option<PathBuf>,
}

#[derive(Clone, Copy)]
struct GuardianOwnerHandle {
    parent_process: HANDLE,
}

#[derive(Clone, Copy)]
struct GuardianCommandHandles {
    stdout: HANDLE,
    stderr: HANDLE,
    start_event: HANDLE,
    abort_event: HANDLE,
}

struct GuardianResult {
    exit_code: Option<u32>,
    error: Option<String>,
}

#[derive(Clone, Copy)]
enum GuardianPhase {
    Starting = 0,
    LeaseHeld = 1,
    Ready = 2,
    AwaitingCommand = 3,
    Running = 4,
    Cleaning = 5,
}

struct GuardianStatus {
    nonce: String,
    operation_id: String,
    guardian_pid: u32,
    phase: GuardianPhase,
}

fn encode_config(config: &GuardianConfig) -> io::Result<Vec<u8>> {
    let mut output = Vec::new();
    output.extend_from_slice(GUARDIAN_MAGIC);
    write_bytes(&mut output, config.nonce.as_bytes())?;
    write_bytes(&mut output, config.operation_id.as_bytes())?;
    write_wide(&mut output, config.bootstrap_path.as_os_str())?;
    write_wide(&mut output, config.child_lock_path.as_os_str())?;
    write_wide(&mut output, config.result_path.as_os_str())?;
    write_wide(&mut output, config.status_path.as_os_str())?;
    write_wide(&mut output, config.owner_handoff_path.as_os_str())?;
    write_wide(&mut output, config.command_handoff_path.as_os_str())?;
    write_wide(&mut output, config.cwd.as_os_str())?;
    write_u32(&mut output, config.creation_flags)?;
    output.push(u8::from(config.fail_after_job));
    match &config.cleanup_gate {
        Some(path) => {
            output.push(1);
            write_wide(&mut output, path.as_os_str())?;
        }
        None => output.push(0),
    }
    write_u32(
        &mut output,
        config.args.len().try_into().map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "too many guardian arguments")
        })?,
    )?;
    for arg in &config.args {
        write_wide(&mut output, arg)?;
    }
    write_u32(
        &mut output,
        config.environment.len().try_into().map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "too many guardian environment entries",
            )
        })?,
    )?;
    for (key, value) in &config.environment {
        write_wide(&mut output, key)?;
        write_wide(&mut output, value)?;
    }
    Ok(output)
}

fn decode_config(contents: &[u8], expected_nonce: &str) -> io::Result<GuardianConfig> {
    let mut cursor = Cursor::new(contents);
    expect_magic(&mut cursor, GUARDIAN_MAGIC)?;
    let nonce = String::from_utf8(read_bytes(&mut cursor)?)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "guardian nonce is not UTF-8"))?;
    if nonce != expected_nonce {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "guardian bootstrap nonce mismatch",
        ));
    }
    let operation_id = String::from_utf8(read_bytes(&mut cursor)?).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "guardian operation id is not UTF-8",
        )
    })?;
    let bootstrap_path = PathBuf::from(read_wide(&mut cursor)?);
    let child_lock_path = PathBuf::from(read_wide(&mut cursor)?);
    let result_path = PathBuf::from(read_wide(&mut cursor)?);
    let status_path = PathBuf::from(read_wide(&mut cursor)?);
    let owner_handoff_path = PathBuf::from(read_wide(&mut cursor)?);
    let command_handoff_path = PathBuf::from(read_wide(&mut cursor)?);
    let cwd = PathBuf::from(read_wide(&mut cursor)?);
    let creation_flags = read_u32(&mut cursor)?;
    let fail_after_job = read_byte(&mut cursor)? != 0;
    let cleanup_gate = if read_byte(&mut cursor)? != 0 {
        Some(PathBuf::from(read_wide(&mut cursor)?))
    } else {
        None
    };
    let args = read_wide_vec(&mut cursor)?;
    let environment_count = read_u32(&mut cursor)? as usize;
    let mut environment = Vec::with_capacity(environment_count);
    for _ in 0..environment_count {
        environment.push((read_wide(&mut cursor)?, read_wide(&mut cursor)?));
    }
    ensure_cursor_exhausted(&cursor)?;
    for path in [
        &bootstrap_path,
        &child_lock_path,
        &result_path,
        &status_path,
        &owner_handoff_path,
        &command_handoff_path,
        &cwd,
    ] {
        validate_protocol_path(path)?;
    }
    if let Some(path) = &cleanup_gate {
        validate_protocol_path(path)?;
    }
    validate_creation_flags(creation_flags)?;
    Ok(GuardianConfig {
        nonce,
        operation_id,
        bootstrap_path,
        child_lock_path,
        result_path,
        status_path,
        owner_handoff_path,
        command_handoff_path,
        cwd,
        args,
        environment,
        creation_flags,
        fail_after_job,
        cleanup_gate,
    })
}

fn encode_owner_handoff(nonce: &str, parent_process: HANDLE) -> io::Result<Vec<u8>> {
    let mut output = Vec::new();
    output.extend_from_slice(HANDOFF_MAGIC);
    write_bytes(&mut output, nonce.as_bytes())?;
    write_u64(&mut output, parent_process as usize as u64);
    Ok(output)
}

fn decode_owner_handoff(contents: &[u8], expected_nonce: &str) -> io::Result<GuardianOwnerHandle> {
    let mut cursor = Cursor::new(contents);
    expect_magic(&mut cursor, HANDOFF_MAGIC)?;
    let nonce = String::from_utf8(read_bytes(&mut cursor)?)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "guardian owner nonce invalid"))?;
    if nonce != expected_nonce {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "guardian owner nonce mismatch",
        ));
    }
    let parent_process = read_u64(&mut cursor)? as usize as HANDLE;
    if parent_process.is_null() || parent_process == INVALID_HANDLE_VALUE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "guardian owner handoff contains an invalid process handle",
        ));
    }
    ensure_cursor_exhausted(&cursor)?;
    Ok(GuardianOwnerHandle { parent_process })
}

fn encode_command_handoff(nonce: &str, handles: GuardianCommandHandles) -> io::Result<Vec<u8>> {
    let mut output = Vec::new();
    output.extend_from_slice(HANDOFF_MAGIC);
    write_bytes(&mut output, nonce.as_bytes())?;
    for handle in [
        handles.stdout,
        handles.stderr,
        handles.start_event,
        handles.abort_event,
    ] {
        write_u64(&mut output, handle as usize as u64);
    }
    Ok(output)
}

fn decode_command_handoff(
    contents: &[u8],
    expected_nonce: &str,
) -> io::Result<GuardianCommandHandles> {
    let mut cursor = Cursor::new(contents);
    expect_magic(&mut cursor, HANDOFF_MAGIC)?;
    let nonce = String::from_utf8(read_bytes(&mut cursor)?).map_err(|_| {
        io::Error::new(io::ErrorKind::InvalidData, "guardian command nonce invalid")
    })?;
    if nonce != expected_nonce {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "guardian command nonce mismatch",
        ));
    }
    let mut handles = [std::ptr::null_mut(); 4];
    for handle in &mut handles {
        let raw = read_u64(&mut cursor)? as usize as HANDLE;
        if raw.is_null() || raw == INVALID_HANDLE_VALUE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "guardian command handoff contains an invalid handle",
            ));
        }
        *handle = raw;
    }
    ensure_cursor_exhausted(&cursor)?;
    Ok(GuardianCommandHandles {
        stdout: handles[0],
        stderr: handles[1],
        start_event: handles[2],
        abort_event: handles[3],
    })
}

fn encode_status(status: &GuardianStatus) -> io::Result<Vec<u8>> {
    let mut output = Vec::new();
    output.extend_from_slice(GUARDIAN_MAGIC);
    write_bytes(&mut output, status.nonce.as_bytes())?;
    write_bytes(&mut output, status.operation_id.as_bytes())?;
    write_u32(&mut output, status.guardian_pid)?;
    output.push(status.phase as u8);
    Ok(output)
}

fn decode_status(contents: &[u8], expected_nonce: &str) -> io::Result<GuardianStatus> {
    let mut cursor = Cursor::new(contents);
    expect_magic(&mut cursor, GUARDIAN_MAGIC)?;
    let nonce = String::from_utf8(read_bytes(&mut cursor)?)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "guardian status nonce invalid"))?;
    if nonce != expected_nonce {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "guardian status nonce mismatch",
        ));
    }
    let operation_id = String::from_utf8(read_bytes(&mut cursor)?).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "guardian status operation id invalid",
        )
    })?;
    let guardian_pid = read_u32(&mut cursor)?;
    let phase = match read_byte(&mut cursor)? {
        0 => GuardianPhase::Starting,
        1 => GuardianPhase::LeaseHeld,
        2 => GuardianPhase::Ready,
        3 => GuardianPhase::AwaitingCommand,
        4 => GuardianPhase::Running,
        5 => GuardianPhase::Cleaning,
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "guardian status phase invalid",
            ))
        }
    };
    ensure_cursor_exhausted(&cursor)?;
    Ok(GuardianStatus {
        nonce,
        operation_id,
        guardian_pid,
        phase,
    })
}

fn encode_result(nonce: &str, result: &GuardianResult) -> io::Result<Vec<u8>> {
    let mut output = Vec::new();
    output.extend_from_slice(RESULT_MAGIC);
    write_bytes(&mut output, nonce.as_bytes())?;
    match (&result.exit_code, &result.error) {
        (Some(code), None) => {
            output.push(0);
            write_u32(&mut output, *code)?;
            write_bytes(&mut output, &[])?;
        }
        (None, Some(error)) => {
            output.push(1);
            write_u32(&mut output, 1)?;
            write_bytes(&mut output, error.as_bytes())?;
        }
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid guardian result",
            ))
        }
    }
    Ok(output)
}

fn read_result(path: &Path, expected_nonce: &str) -> io::Result<GuardianResult> {
    validate_protocol_file(path)?;
    let contents = fs::read(path)?;
    let mut cursor = Cursor::new(contents.as_slice());
    expect_magic(&mut cursor, RESULT_MAGIC)?;
    let nonce = String::from_utf8(read_bytes(&mut cursor)?)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "guardian result nonce invalid"))?;
    if nonce != expected_nonce {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "guardian result nonce mismatch",
        ));
    }
    let kind = read_byte(&mut cursor)?;
    let code = read_u32(&mut cursor)?;
    let message = read_bytes(&mut cursor)?;
    ensure_cursor_exhausted(&cursor)?;
    match kind {
        0 => Ok(GuardianResult {
            exit_code: Some(code),
            error: None,
        }),
        1 => Ok(GuardianResult {
            exit_code: None,
            error: Some(String::from_utf8_lossy(&message).into_owned()),
        }),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "guardian result kind invalid",
        )),
    }
}

fn read_guardian_error(path: &Path, nonce: &str, fallback: &str) -> io::Error {
    match read_result(path, nonce) {
        Ok(result) => io::Error::other(result.error.unwrap_or_else(|| fallback.to_string())),
        Err(error) => io::Error::other(format!("{fallback}: {error}")),
    }
}

fn write_guardian_result(config: &GuardianConfig, result: GuardianResult) {
    if let Ok(contents) = encode_result(&config.nonce, &result) {
        let _ = ProtocolFile::write_atomic(&config.result_path, contents);
    }
}

fn write_bytes(output: &mut Vec<u8>, value: &[u8]) -> io::Result<()> {
    write_u32(
        output,
        value.len().try_into().map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "guardian record is too large")
        })?,
    )?;
    output.extend_from_slice(value);
    Ok(())
}

fn write_wide(output: &mut Vec<u8>, value: &OsStr) -> io::Result<()> {
    let units: Vec<u16> = value.encode_wide().collect();
    write_u32(
        output,
        units.len().try_into().map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "guardian path is too large")
        })?,
    )?;
    for unit in units {
        output.extend_from_slice(&unit.to_le_bytes());
    }
    Ok(())
}

fn write_u32(output: &mut Vec<u8>, value: u32) -> io::Result<()> {
    output.extend_from_slice(&value.to_le_bytes());
    Ok(())
}

fn write_u64(output: &mut Vec<u8>, value: u64) {
    output.extend_from_slice(&value.to_le_bytes());
}

fn read_byte(cursor: &mut Cursor<&[u8]>) -> io::Result<u8> {
    let mut value = [0; 1];
    cursor.read_exact(&mut value)?;
    Ok(value[0])
}

fn read_u32(cursor: &mut Cursor<&[u8]>) -> io::Result<u32> {
    let mut value = [0; 4];
    cursor.read_exact(&mut value)?;
    Ok(u32::from_le_bytes(value))
}

fn read_u64(cursor: &mut Cursor<&[u8]>) -> io::Result<u64> {
    let mut value = [0; 8];
    cursor.read_exact(&mut value)?;
    Ok(u64::from_le_bytes(value))
}

fn read_bytes(cursor: &mut Cursor<&[u8]>) -> io::Result<Vec<u8>> {
    let length = read_u32(cursor)? as usize;
    if length > 16 * 1024 * 1024 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "guardian record is too large",
        ));
    }
    let mut value = vec![0; length];
    cursor.read_exact(&mut value)?;
    Ok(value)
}

fn read_wide(cursor: &mut Cursor<&[u8]>) -> io::Result<OsString> {
    let count = read_u32(cursor)? as usize;
    if count > 4 * 1024 * 1024 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "guardian string is too large",
        ));
    }
    let mut units = Vec::with_capacity(count);
    for _ in 0..count {
        units.push(read_u16(cursor)?);
    }
    Ok(OsString::from_wide(&units))
}

fn read_u16(cursor: &mut Cursor<&[u8]>) -> io::Result<u16> {
    let mut value = [0; 2];
    cursor.read_exact(&mut value)?;
    Ok(u16::from_le_bytes(value))
}

fn read_wide_vec(cursor: &mut Cursor<&[u8]>) -> io::Result<Vec<OsString>> {
    let count = read_u32(cursor)? as usize;
    if count > 100_000 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "too many guardian arguments",
        ));
    }
    (0..count).map(|_| read_wide(cursor)).collect()
}

fn expect_magic(cursor: &mut Cursor<&[u8]>, magic: &[u8]) -> io::Result<()> {
    let mut actual = vec![0; magic.len()];
    cursor.read_exact(&mut actual)?;
    if actual == magic {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "guardian protocol magic mismatch",
        ))
    }
}

fn ensure_cursor_exhausted(cursor: &Cursor<&[u8]>) -> io::Result<()> {
    if cursor.position() == cursor.get_ref().len() as u64 {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "guardian protocol has trailing data",
        ))
    }
}

fn validate_protocol_path(path: &Path) -> io::Result<()> {
    if !path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "guardian protocol path must be absolute",
        ));
    }
    Ok(())
}

pub(crate) fn ensure_private_directory_windows(path: &Path) -> io::Result<()> {
    ensure_windows_owner_only(path)
}

pub(crate) fn ensure_private_file_windows(path: &Path) -> io::Result<()> {
    ensure_protocol_file_private(path)
}

fn ensure_protocol_file_private(path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "guardian protocol file is not a private regular file",
        ));
    }
    ensure_windows_owner_only(path)
}

fn ensure_windows_owner_only(path: &Path) -> io::Result<()> {
    use std::ptr::null_mut;
    use windows_sys::Win32::Security::ACE_HEADER;

    let sid = current_user_sid()?;
    let acl_size =
        size_of::<ACL>() + size_of::<ACCESS_ALLOWED_ACE>() - size_of::<u32>() + sid.len();
    let mut acl = vec![0u8; acl_size];
    if unsafe { InitializeAcl(acl.as_mut_ptr().cast(), acl.len() as u32, ACL_REVISION) } == 0 {
        return Err(last_error());
    }
    if unsafe {
        AddAccessAllowedAce(
            acl.as_mut_ptr().cast(),
            ACL_REVISION,
            FILE_ALL_ACCESS,
            sid.as_ptr().cast_mut().cast(),
        )
    } == 0
    {
        return Err(last_error());
    }
    let wide = wide_path(path.as_os_str())?;
    let error = unsafe {
        SetNamedSecurityInfoW(
            wide.as_ptr(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            sid.as_ptr().cast_mut().cast(),
            null_mut(),
            acl.as_ptr().cast(),
            null_mut(),
        )
    };
    if error != 0 {
        return Err(io::Error::from_raw_os_error(error as i32));
    }

    // Re-read the descriptor rather than trusting SetNamedSecurityInfoW. This
    // validates both the owner and the effective default DACL used by every
    // protocol file. SYSTEM can still administer the machine, but no broad or
    // unowned trustee may read/replace bootstrap or handoff contents.
    let mut owner: windows_sys::Win32::Security::PSID = null_mut();
    let mut dacl: *mut ACL = null_mut();
    let mut descriptor: windows_sys::Win32::Security::PSECURITY_DESCRIPTOR = null_mut();
    let error = unsafe {
        GetNamedSecurityInfoW(
            wide.as_ptr(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut owner,
            null_mut(),
            &mut dacl,
            null_mut(),
            &mut descriptor,
        )
    };
    if error != 0 {
        return Err(io::Error::from_raw_os_error(error as i32));
    }
    let result = (|| {
        if owner.is_null() || unsafe { EqualSid(owner, sid.as_ptr().cast_mut().cast()) } == 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "guardian protocol owner does not match the current user",
            ));
        }
        let mut present = 0;
        let mut defaulted = 0;
        if dacl.is_null()
            || unsafe {
                GetSecurityDescriptorDacl(descriptor, &mut present, &mut dacl, &mut defaulted)
            } == 0
            || present == 0
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "guardian protocol has no explicit DACL",
            ));
        }
        let mut size = ACL_SIZE_INFORMATION::default();
        if unsafe {
            GetAclInformation(
                dacl,
                (&mut size as *mut ACL_SIZE_INFORMATION).cast(),
                size_of::<ACL_SIZE_INFORMATION>() as u32,
                AclSizeInformation,
            )
        } == 0
        {
            return Err(last_error());
        }
        if size.AceCount != 1 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "guardian protocol DACL is not owner-only",
            ));
        }
        let mut ace = null_mut();
        if unsafe { GetAce(dacl, 0, &mut ace) } == 0 || ace.is_null() {
            return Err(last_error());
        }
        let header = unsafe { &*(ace as *const ACE_HEADER) };
        let allowed = unsafe { &*(ace as *const ACCESS_ALLOWED_ACE) };
        if header.AceType != 0
            || header.AceFlags != 0
            || unsafe {
                EqualSid(
                    (&allowed.SidStart as *const u32).cast_mut().cast(),
                    sid.as_ptr().cast_mut().cast(),
                )
            } == 0
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "guardian protocol DACL contains a non-owner ACE",
            ));
        }
        Ok(())
    })();
    unsafe {
        LocalFree(descriptor);
    }
    result
}

fn current_user_sid() -> io::Result<Vec<u8>> {
    use std::ptr::null_mut;
    let mut token = null_mut();
    if unsafe {
        OpenProcessToken(
            windows_sys::Win32::System::Threading::GetCurrentProcess(),
            TOKEN_QUERY,
            &mut token,
        )
    } == 0
    {
        return Err(last_error());
    }
    let mut needed = 0;
    unsafe {
        GetTokenInformation(token, TokenUser, null_mut(), 0, &mut needed);
    }
    if needed == 0 {
        unsafe {
            CloseHandle(token);
        }
        return Err(last_error());
    }
    let mut buffer = vec![0u8; needed as usize];
    if unsafe {
        GetTokenInformation(
            token,
            TokenUser,
            buffer.as_mut_ptr().cast(),
            needed,
            &mut needed,
        )
    } == 0
    {
        unsafe {
            CloseHandle(token);
        }
        return Err(last_error());
    }
    unsafe {
        CloseHandle(token);
    }
    let user = unsafe { &*(buffer.as_ptr() as *const TOKEN_USER) };
    let sid = user.User.Sid;
    if sid.is_null() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "current user has no SID",
        ));
    }
    let length = unsafe { windows_sys::Win32::Security::GetLengthSid(sid) } as usize;
    let bytes = unsafe { std::slice::from_raw_parts(sid.cast::<u8>(), length) };
    Ok(bytes.to_vec())
}

fn validate_protocol_file(path: &Path) -> io::Result<()> {
    ensure_protocol_file_private(path)
}

fn open_child_lock(path: &Path) -> io::Result<File> {
    let mut options = fs::OpenOptions::new();
    options.read(true).write(true);
    // Windows opens ordinary Rust file handles non-inheritable. The guardian
    // never changes that process-wide property.
    let file = options.open(path)?;
    ensure_private_file_windows(path)?;
    Ok(file)
}

fn duplicate_guardian_parent_handle(target_process: HANDLE) -> io::Result<HANDLE> {
    duplicate_into(
        target_process,
        // SAFETY: GetCurrentProcess returns the current process pseudo-handle.
        unsafe { GetCurrentProcess() },
    )
}

fn duplicate_guardian_command_handles(
    target_process: HANDLE,
    stdout: HANDLE,
    stderr: HANDLE,
    start_event: HANDLE,
    abort_event: HANDLE,
) -> io::Result<GuardianCommandHandles> {
    Ok(GuardianCommandHandles {
        stdout: duplicate_into(target_process, stdout)?,
        stderr: duplicate_into(target_process, stderr)?,
        start_event: duplicate_into(target_process, start_event)?,
        abort_event: duplicate_into(target_process, abort_event)?,
    })
}

fn duplicate_into(target_process: HANDLE, source: HANDLE) -> io::Result<HANDLE> {
    let mut target = std::ptr::null_mut();
    // SAFETY: source is a live handle in this process and target_process is the
    // live guardian.  bInheritHandle is false for every guardian copy.
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

/// Handle the private guardian command before Clap parses normal CLI input.
/// The mode is usable only with a matching private bootstrap, handoff, and
/// nonce; malformed or spoofed invocations never run Git.
pub(crate) fn run_launcher_if_requested() {
    let mut args = std::env::args_os();
    let _argv0 = args.next();
    if args.next().as_deref() != Some(OsStr::new(INTERNAL_GUARDIAN_ARG)) {
        return;
    }
    let values: Vec<OsString> = args.collect();
    if values.len() != 3 {
        std::process::exit(1);
    }
    let code = guardian_main(
        PathBuf::from(&values[0]),
        PathBuf::from(&values[1]),
        values[2].to_string_lossy().into_owned(),
    );
    std::process::exit(code as i32);
}

fn guardian_main(bootstrap_path: PathBuf, handoff_path: PathBuf, expected_nonce: String) -> u32 {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        guardian_main_inner(&bootstrap_path, &handoff_path, &expected_nonce)
    }));
    match result {
        Ok(code) => code,
        Err(_) => {
            // A panic before the config is decoded cannot safely run Git. The
            // next owner also performs a stale sweep, while these paths are
            // removed immediately on the normal panic-unwind path.
            let _ = fs::remove_file(&bootstrap_path);
            let _ = fs::remove_file(&handoff_path);
            if let Some(directory) = bootstrap_path.parent() {
                let _ = sweep_all_stale_protocol_files(directory);
            }
            1
        }
    }
}

fn guardian_main_inner(bootstrap_path: &Path, handoff_path: &Path, expected_nonce: &str) -> u32 {
    let config = match read_config_file(bootstrap_path, expected_nonce) {
        Ok(config) => config,
        Err(_) => return 1,
    };
    let _ = fs::remove_file(bootstrap_path);

    let directory = match bootstrap_path.parent() {
        Some(directory) => directory,
        None => return 1,
    };
    if handoff_path.parent() != Some(directory)
        || config.owner_handoff_path.parent() != Some(directory)
        || config.command_handoff_path.parent() != Some(directory)
        || config.status_path.parent() != Some(directory)
        || config.result_path.parent() != Some(directory)
        || config.command_handoff_path != handoff_path
    {
        cleanup_protocol_paths(&config);
        return 1;
    }
    if write_guardian_status(&config, GuardianPhase::Starting).is_err() {
        cleanup_protocol_paths(&config);
        return 1;
    }

    wait_for_test_handshake("guardian-before-lease");
    let owner = match wait_for_owner_handoff(&config.owner_handoff_path, expected_nonce) {
        Ok(owner) => owner,
        Err(_) => {
            cleanup_protocol_paths(&config);
            return 1;
        }
    };
    let handles = GuardianOwnedHandles::new(owner);
    if !parent_is_alive(handles.parent.raw()) {
        cleanup_protocol_paths(&config);
        return 0;
    }

    let child_lock = match open_child_lock(&config.child_lock_path) {
        Ok(lock) => lock,
        Err(error) => {
            write_guardian_result(&config, guardian_error(error));
            return 0;
        }
    };
    if !parent_is_alive(handles.parent.raw()) {
        drop(child_lock);
        cleanup_protocol_paths(&config);
        return 0;
    }
    if !super::try_lock_exclusive(&child_lock).unwrap_or(false) {
        write_guardian_result(
            &config,
            guardian_error(io::Error::new(
                io::ErrorKind::WouldBlock,
                "managed merge lifecycle child lock is busy",
            )),
        );
        return 0;
    }

    let job = match Job::new() {
        Ok(job) => job,
        Err(error) => {
            drop(child_lock);
            write_guardian_result(&config, guardian_error(error));
            return 0;
        }
    };
    let mut resources = GuardianResources::new(job, child_lock);
    if write_guardian_status(&config, GuardianPhase::LeaseHeld).is_err() {
        return finish_guardian(
            &config,
            resources,
            Some(guardian_error(io::Error::other(
                "cannot publish lifecycle guardian lease status",
            ))),
            handles.parent.raw(),
        );
    }

    if config.fail_after_job {
        return finish_guardian(
            &config,
            resources,
            Some(guardian_error(io::Error::other(
                "injected setup failure after guardian job creation",
            ))),
            handles.parent.raw(),
        );
    }
    if !parent_is_alive(handles.parent.raw()) {
        return finish_guardian(&config, resources, None, handles.parent.raw());
    }

    wait_for_test_handshake("guardian-after-lease-before-ready");
    if !parent_is_alive(handles.parent.raw()) {
        return finish_guardian(&config, resources, None, handles.parent.raw());
    }
    if write_guardian_status(&config, GuardianPhase::Ready).is_err() {
        return finish_guardian(
            &config,
            resources,
            Some(guardian_error(io::Error::other(
                "cannot publish lifecycle guardian READY status",
            ))),
            handles.parent.raw(),
        );
    }
    wait_for_test_handshake("guardian-ready-before-command");
    if write_guardian_status(&config, GuardianPhase::AwaitingCommand).is_err() {
        return finish_guardian(
            &config,
            resources,
            Some(guardian_error(io::Error::other(
                "cannot publish lifecycle guardian command status",
            ))),
            handles.parent.raw(),
        );
    }

    let command = match wait_for_command_handoff(
        &config.command_handoff_path,
        expected_nonce,
        handles.parent.raw(),
    ) {
        Ok(Some(command)) => command,
        Ok(None) | Err(_) => {
            return finish_guardian(&config, resources, None, handles.parent.raw());
        }
    };
    let command = GuardianOwnedCommandHandles::new(command);
    if !parent_is_alive(handles.parent.raw()) {
        return finish_guardian(&config, resources, None, handles.parent.raw());
    }
    if write_guardian_status(&config, GuardianPhase::AwaitingCommand).is_err() {
        return finish_guardian(
            &config,
            resources,
            Some(guardian_error(io::Error::other(
                "cannot refresh lifecycle guardian command status",
            ))),
            handles.parent.raw(),
        );
    }

    let start = wait_for_start_abort_or_parent(
        command.start.raw(),
        command.abort.raw(),
        handles.parent.raw(),
    );
    if !start {
        return finish_guardian(&config, resources, None, handles.parent.raw());
    }
    if write_guardian_status(&config, GuardianPhase::Running).is_err() {
        return finish_guardian(
            &config,
            resources,
            Some(guardian_error(io::Error::other(
                "cannot publish lifecycle guardian running status",
            ))),
            handles.parent.raw(),
        );
    }

    let git_result = launch_git_inside_job(
        &mut resources.job,
        &config,
        command.stdout.raw(),
        command.stderr.raw(),
        handles.parent.raw(),
    );
    if !parent_is_alive(handles.parent.raw()) {
        wait_for_cleanup_gate(config.cleanup_gate.as_deref());
    }
    let result = match git_result {
        Ok(exit_code) => GuardianResult {
            exit_code: Some(exit_code),
            error: None,
        },
        Err(error) => guardian_error(error),
    };
    finish_guardian(&config, resources, Some(result), handles.parent.raw())
}

fn finish_guardian(
    config: &GuardianConfig,
    mut resources: GuardianResources,
    result: Option<GuardianResult>,
    parent: HANDLE,
) -> u32 {
    let _ = write_guardian_status(config, GuardianPhase::Cleaning);
    // This is deliberately fail-closed: the helper retries while retaining
    // both the job handle and the child lease until the job is quiescent.
    let _ = resources.cleanup();
    drop(resources);
    if !parent_is_alive(parent) {
        cleanup_protocol_paths(config);
        return 0;
    }
    if let Some(result) = result {
        write_guardian_result(config, result);
    }
    0
}

struct GuardianResources {
    job: Job,
    child_lock: Option<File>,
}

impl GuardianResources {
    fn new(job: Job, child_lock: File) -> Self {
        Self {
            job,
            child_lock: Some(child_lock),
        }
    }

    fn cleanup(&mut self) -> io::Result<()> {
        cleanup_job_until_quiesced(&mut self.job)?;
        drop(self.child_lock.take());
        Ok(())
    }
}

impl Drop for GuardianResources {
    fn drop(&mut self) {
        // Catching panics around guardian setup is not enough by itself: a
        // panic after job creation must still quiesce the job before releasing
        // the child lease. This Drop path is only reached for such cleanup
        // paths; deliberate process termination remains covered by job close.
        if !self.job.quiesced {
            let _ = self.cleanup();
        }
        drop(self.child_lock.take());
    }
}

fn read_config_file(path: &Path, nonce: &str) -> io::Result<GuardianConfig> {
    validate_protocol_path(path)?;
    validate_protocol_file(path)?;
    let contents = fs::read(path)?;
    let config = decode_config(&contents, nonce)?;
    let directory = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::PermissionDenied,
            "guardian bootstrap has no parent",
        )
    })?;
    if [
        &config.bootstrap_path,
        &config.child_lock_path,
        &config.result_path,
        &config.status_path,
        &config.owner_handoff_path,
        &config.command_handoff_path,
    ]
    .iter()
    .any(|path| path.parent() != Some(directory))
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "guardian bootstrap escapes its private directory",
        ));
    }
    Ok(config)
}

fn cleanup_protocol_paths(config: &GuardianConfig) {
    for path in [
        &config.bootstrap_path,
        &config.owner_handoff_path,
        &config.command_handoff_path,
        &config.status_path,
        &config.result_path,
    ] {
        let _ = fs::remove_file(path);
    }
}

fn write_guardian_status(config: &GuardianConfig, phase: GuardianPhase) -> io::Result<()> {
    let status = GuardianStatus {
        nonce: config.nonce.clone(),
        operation_id: config.operation_id.clone(),
        guardian_pid: unsafe { windows_sys::Win32::System::Threading::GetCurrentProcessId() },
        phase,
    };
    ProtocolFile::write_atomic(&config.status_path, encode_status(&status)?)
}

fn wait_for_owner_handoff(path: &Path, nonce: &str) -> io::Result<GuardianOwnerHandle> {
    validate_protocol_path(path)?;
    let deadline = Instant::now() + GUARDIAN_SETUP_TIMEOUT;
    loop {
        match validate_protocol_file(path).and_then(|()| fs::read(path)) {
            Ok(contents) => match decode_owner_handoff(&contents, nonce) {
                Ok(owner) => {
                    let _ = fs::remove_file(path);
                    return Ok(owner);
                }
                Err(error) if Instant::now() >= deadline => return Err(error),
                Err(_) => {}
            },
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) if Instant::now() >= deadline => return Err(error),
            Err(_) => {}
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "lifecycle guardian owner handoff timed out",
            ));
        }
        thread::sleep(GUARDIAN_RETRY_DELAY);
    }
}

fn wait_for_command_handoff(
    path: &Path,
    nonce: &str,
    parent: HANDLE,
) -> io::Result<Option<GuardianCommandHandles>> {
    validate_protocol_path(path)?;
    let deadline = Instant::now() + GUARDIAN_SETUP_TIMEOUT;
    loop {
        if !parent_is_alive(parent) {
            return Ok(None);
        }
        match validate_protocol_file(path).and_then(|()| fs::read(path)) {
            Ok(contents) => match decode_command_handoff(&contents, nonce) {
                Ok(handles) => {
                    let _ = fs::remove_file(path);
                    return Ok(Some(handles));
                }
                Err(error) if Instant::now() >= deadline => return Err(error),
                Err(_) => {}
            },
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) if Instant::now() >= deadline => return Err(error),
            Err(_) => {}
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "lifecycle guardian command handoff timed out",
            ));
        }
        thread::sleep(GUARDIAN_RETRY_DELAY);
    }
}

fn launch_git_inside_job(
    job: &mut Job,
    config: &GuardianConfig,
    stdout: HANDLE,
    stderr: HANDLE,
    parent: HANDLE,
) -> io::Result<u32> {
    let stdin = create_nul()?;
    let stdin_handle = duplicate_in_process(stdin.raw(), true)?;
    let stdout_handle = match duplicate_in_process(stdout, true) {
        Ok(handle) => handle,
        Err(error) => {
            close_raw(stdin_handle);
            return Err(error);
        }
    };
    let stderr_handle = match duplicate_in_process(stderr, true) {
        Ok(handle) => handle,
        Err(error) => {
            close_raw(stdin_handle);
            close_raw(stdout_handle);
            return Err(error);
        }
    };
    let inherited = [stdin_handle, stdout_handle, stderr_handle];
    let mut attributes = match AttributeList::new(2) {
        Ok(attributes) => attributes,
        Err(error) => {
            close_handles(&inherited);
            return Err(error);
        }
    };
    let job_handle = job.raw();
    if let Err(error) = attributes
        .update(
            PROC_THREAD_ATTRIBUTE_JOB_LIST as usize,
            (&job_handle as *const HANDLE).cast(),
            size_of::<HANDLE>(),
        )
        .and_then(|()| {
            attributes.update(
                PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                inherited.as_ptr().cast(),
                size_of::<HANDLE>() * inherited.len(),
            )
        })
    {
        close_handles(&inherited);
        return Err(error);
    }

    let mut command_line = match command_line_os(OsStr::new("git"), &config.args) {
        Ok(command_line) => command_line,
        Err(error) => {
            close_handles(&inherited);
            return Err(error);
        }
    };
    let current_directory = wide_path(config.cwd.as_os_str())?;
    let environment = environment_block(&config.environment)?;
    let mut startup = STARTUPINFOEXW::default();
    startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    startup.StartupInfo.hStdInput = inherited[0];
    startup.StartupInfo.hStdOutput = inherited[1];
    startup.StartupInfo.hStdError = inherited[2];
    startup.lpAttributeList = attributes.raw();

    let mut process_info = PROCESS_INFORMATION::default();
    let created = unsafe {
        // SAFETY: the command line, environment, current directory, startup
        // structure, and attribute storage remain live through this call.
        CreateProcessW(
            std::ptr::null(),
            command_line.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            1,
            config.creation_flags
                | CREATE_SUSPENDED
                | EXTENDED_STARTUPINFO_PRESENT
                | CREATE_UNICODE_ENVIRONMENT,
            environment.as_ptr().cast(),
            current_directory.as_ptr(),
            (&startup as *const STARTUPINFOEXW).cast(),
            &mut process_info,
        )
    } != 0;
    close_handles(&inherited);
    drop(stdin);
    if !created {
        return Err(last_error());
    }

    let process = OwnedHandle::new(process_info.hProcess);
    let thread = OwnedHandle::new(process_info.hThread);
    job.member_created = true;
    if unsafe { ResumeThread(thread.raw()) } == u32::MAX {
        return Err(last_error());
    }

    let wait = wait_for_git_or_parent(process.raw(), parent)?;
    if !wait {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "lifecycle owner exited while Git was running",
        ));
    }
    let exit_code = get_exit_code(process.raw())?;
    Ok(exit_code)
}

fn guardian_error(error: io::Error) -> GuardianResult {
    GuardianResult {
        exit_code: None,
        error: Some(error.to_string()),
    }
}

fn wait_for_test_handshake(phase: &str) {
    if !cfg!(debug_assertions)
        || std::env::var_os(TEST_HANDSHAKE_PHASE_ENV).as_deref() != Some(OsStr::new(phase))
    {
        return;
    }
    let Some(gate) = std::env::var_os(TEST_HANDSHAKE_GATE_ENV).map(PathBuf::from) else {
        return;
    };
    let started = gate.with_extension("started");
    let release = gate.with_extension("release");
    let _ = fs::write(&started, format!("{phase}\n"));
    let deadline = Instant::now() + GUARDIAN_SETUP_TIMEOUT;
    while !release.is_file() && Instant::now() < deadline {
        thread::sleep(GUARDIAN_RETRY_DELAY);
    }
}

fn wait_for_cleanup_gate(path: Option<&Path>) {
    let Some(path) = path else {
        return;
    };
    let started = path.with_extension("started");
    let release = path.with_extension("release");
    let _ = fs::write(&started, "started\n");
    let deadline = Instant::now() + GUARDIAN_SETUP_TIMEOUT;
    while !release.is_file() && Instant::now() < deadline {
        thread::sleep(GUARDIAN_RETRY_DELAY);
    }
}

fn cleanup_job_until_quiesced(job: &mut Job) -> io::Result<()> {
    // A guardian must not exit after a cleanup error: process exit closes the
    // kill-on-close handle and can release the lease before quiescence.  Retry
    // while retaining both the job and child-lock handles instead.
    loop {
        match job.terminate_and_wait() {
            Ok(()) => return Ok(()),
            Err(_error) => thread::sleep(GUARDIAN_RETRY_DELAY),
        }
    }
}

struct GuardianOwnedHandles {
    parent: OwnedHandle,
}

impl GuardianOwnedHandles {
    fn new(handles: GuardianOwnerHandle) -> Self {
        Self {
            parent: OwnedHandle::new(handles.parent_process),
        }
    }
}

struct GuardianOwnedCommandHandles {
    stdout: OwnedHandle,
    stderr: OwnedHandle,
    start: OwnedHandle,
    abort: OwnedHandle,
}

impl GuardianOwnedCommandHandles {
    fn new(handles: GuardianCommandHandles) -> Self {
        Self {
            stdout: OwnedHandle::new(handles.stdout),
            stderr: OwnedHandle::new(handles.stderr),
            start: OwnedHandle::new(handles.start_event),
            abort: OwnedHandle::new(handles.abort_event),
        }
    }
}

fn create_event() -> io::Result<HANDLE> {
    // Null SECURITY_ATTRIBUTES makes the event handle non-inheritable.
    let handle = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
    if handle.is_null() {
        Err(last_error())
    } else {
        Ok(handle)
    }
}

fn create_nul() -> io::Result<OwnedHandle> {
    let nul = wide_path(OsStr::new("NUL"))?;
    let handle = unsafe {
        // SAFETY: nul is NUL-terminated and the returned handle is adopted
        // immediately by OwnedHandle.
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
    if handle == INVALID_HANDLE_VALUE || handle.is_null() {
        Err(last_error())
    } else {
        Ok(OwnedHandle::new(handle))
    }
}

fn duplicate_in_process(source: HANDLE, inheritable: bool) -> io::Result<HANDLE> {
    let mut target = std::ptr::null_mut();
    if unsafe {
        // SAFETY: source is a live handle in the guardian process.  This is a
        // private duplicate used only by the immediately following Git spawn.
        DuplicateHandle(
            GetCurrentProcess(),
            source,
            GetCurrentProcess(),
            &mut target,
            0,
            i32::from(inheritable),
            DUPLICATE_SAME_ACCESS,
        )
    } == 0
    {
        Err(last_error())
    } else {
        Ok(target)
    }
}

fn parent_is_alive(parent: HANDLE) -> bool {
    unsafe { WaitForSingleObject(parent, 0) == WAIT_TIMEOUT }
}

fn wait_for_start_abort_or_parent(start: HANDLE, abort: HANDLE, parent: HANDLE) -> bool {
    let handles = [start, abort, parent];
    matches!(
        unsafe { WaitForMultipleObjects(3, handles.as_ptr(), 0, INFINITE) },
        WAIT_OBJECT_0
    )
}

// The owner process handle, rather than EOF on a capture pipe, is the
// authoritative death signal. A broken pipe therefore cannot release the
// guardian lease while Git or a hook is still a job member.
fn wait_for_git_or_parent(git: HANDLE, parent: HANDLE) -> io::Result<bool> {
    let handles = [git, parent];
    match unsafe { WaitForMultipleObjects(2, handles.as_ptr(), 0, INFINITE) } {
        WAIT_OBJECT_0 => Ok(true),
        value if value == WAIT_OBJECT_0 + 1 => Ok(false),
        WAIT_FAILED => Err(last_error()),
        _ => Err(io::Error::other("unexpected lifecycle Git wait result")),
    }
}

fn guardian_status_phase(path: &Path, nonce: &str) -> Option<GuardianPhase> {
    let Ok(contents) = validate_protocol_file(path).and_then(|()| fs::read(path)) else {
        return None;
    };
    decode_status(&contents, nonce)
        .ok()
        .map(|status| status.phase)
}

fn wait_for_ready_or_process(path: &Path, process: HANDLE, nonce: &str) -> io::Result<bool> {
    validate_protocol_path(path)?;
    let deadline = Instant::now() + GUARDIAN_SETUP_TIMEOUT;
    loop {
        match guardian_status_phase(path, nonce) {
            Some(
                GuardianPhase::Ready | GuardianPhase::AwaitingCommand | GuardianPhase::Running,
            ) => return Ok(true),
            // Cleaning is a terminal setup failure, not authorization. Return
            // through the normal result-reporting path so injected and real
            // setup errors remain deterministic.
            Some(GuardianPhase::Cleaning) => return Ok(false),
            Some(GuardianPhase::Starting | GuardianPhase::LeaseHeld) | None => {}
        }
        if unsafe { WaitForSingleObject(process, 0) } == WAIT_OBJECT_0 {
            return Ok(false);
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "lifecycle guardian READY status timed out",
            ));
        }
        thread::sleep(GUARDIAN_RETRY_DELAY);
    }
}

fn wait_for_process(handle: HANDLE) -> io::Result<()> {
    if unsafe { WaitForSingleObject(handle, INFINITE) } == WAIT_OBJECT_0 {
        Ok(())
    } else {
        Err(last_error())
    }
}

fn get_exit_code(handle: HANDLE) -> io::Result<u32> {
    let mut code = 0;
    if unsafe { GetExitCodeProcess(handle, &mut code) } == 0 {
        Err(last_error())
    } else {
        Ok(code)
    }
}

fn close_raw(handle: HANDLE) {
    if !handle.is_null() && handle != INVALID_HANDLE_VALUE {
        unsafe {
            // SAFETY: callers pass only owned Win32 handles.
            CloseHandle(handle);
        }
    }
}

fn close_handles(handles: &[HANDLE]) {
    for &handle in handles {
        close_raw(handle);
    }
}

fn last_error() -> io::Error {
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
    member_created: bool,
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
        let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if handle.is_null() {
            return Err(last_error());
        }
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        // Neither BREAKAWAY_OK nor SILENT_BREAKAWAY_OK is set.
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
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
            member_created: false,
            #[cfg(test)]
            cleanup_failure: None,
        })
    }

    #[cfg(test)]
    fn new_with_cleanup_failure(failure: CleanupFailure) -> io::Result<Self> {
        let mut job = Self::new()?;
        job.member_created = true;
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
                if unsafe { TerminateJobObject(self.raw(), 1) } == 0 {
                    return Err(last_error());
                }
                return Err(io::Error::other("injected WaitForSingleObject failure"));
            }
            None => {}
        }
        if unsafe { TerminateJobObject(self.raw(), 1) } == 0 {
            return Err(last_error());
        }
        if unsafe { WaitForSingleObject(self.raw(), INFINITE) } != WAIT_OBJECT_0 {
            return Err(last_error());
        }
        self.quiesced = true;
        Ok(())
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        if self.quiesced || !self.member_created {
            return;
        }
        // A Job cannot report an OS error from Drop.  Never close an
        // unquiesced kill-on-close handle here: guardian_main keeps retrying
        // until the job is quiescent, and this fallback intentionally leaks the
        // handle in tests or unexpected paths rather than releasing the lease.
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
        let first = unsafe {
            // SAFETY: null is the documented size-only probe.
            InitializeProcThreadAttributeList(std::ptr::null_mut(), attribute_count, 0, &mut bytes)
        };
        if first == 0 && unsafe { GetLastError() } != ERROR_INSUFFICIENT_BUFFER {
            return Err(last_error());
        }
        let mut storage = vec![0usize; bytes.div_ceil(size_of::<usize>())];
        let raw = storage.as_mut_ptr().cast();
        if unsafe {
            // SAFETY: raw points to aligned storage of the requested size.
            InitializeProcThreadAttributeList(raw, attribute_count, 0, &mut bytes)
        } == 0
        {
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
        if unsafe {
            // SAFETY: the attribute list and value remain valid for this call.
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
        unsafe {
            // SAFETY: raw was successfully initialized by new.
            DeleteProcThreadAttributeList(self.raw);
        }
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
            bInheritHandle: 0,
        };
        let mut read = std::ptr::null_mut();
        let mut write = std::ptr::null_mut();
        if unsafe { CreatePipe(&mut read, &mut write, &attributes, 0) } == 0 {
            return Err(last_error());
        }
        Ok(Self {
            read: Some(OwnedHandle::new(read)),
            write: OwnedHandle::new(write),
        })
    }

    fn take_read(&mut self) -> File {
        let read = self
            .read
            .take()
            .expect("lifecycle pipe read handle is available");
        let raw = read.raw();
        std::mem::forget(read);
        unsafe {
            // SAFETY: raw is the one read handle removed from OwnedHandle.
            File::from_raw_handle(raw)
        }
    }
}

struct StdioHandles {
    stdout: Pipe,
    stderr: Pipe,
}

impl StdioHandles {
    fn new() -> io::Result<Self> {
        Ok(Self {
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
        block.extend_from_slice(&[0, 0]);
    } else {
        block.push(0);
    }
    Ok(block)
}

fn exit_status_from_raw(code: u32) -> std::process::ExitStatus {
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
    fn guardian_bootstrap_round_trips_exact_unicode_arguments_and_environment() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let config = GuardianConfig {
            nonce: "nonce-значение".to_string(),
            operation_id: "operation-значение".to_string(),
            bootstrap_path: directory.path().join("bootstrap"),
            child_lock_path: directory.path().join("child.lock"),
            result_path: directory.path().join("result"),
            status_path: directory.path().join("status"),
            owner_handoff_path: directory.path().join("owner-handoff"),
            command_handoff_path: directory.path().join("command-handoff"),
            cwd: directory.path().join("рабочая папка"),
            args: vec![OsString::from("a b\\"), OsString::from("значение")],
            environment: vec![(OsString::from("A"), OsString::from("значение \\"))],
            creation_flags: CREATE_NEW_PROCESS_GROUP,
            fail_after_job: false,
            cleanup_gate: None,
        };
        let encoded = encode_config(&config).expect("encode config");
        let decoded = decode_config(&encoded, &config.nonce).expect("decode config");
        assert_eq!(decoded.nonce, config.nonce);
        assert_eq!(decoded.operation_id, config.operation_id);
        assert_eq!(decoded.bootstrap_path, config.bootstrap_path);
        assert_eq!(decoded.child_lock_path, config.child_lock_path);
        assert_eq!(decoded.cwd, config.cwd);
        assert_eq!(decoded.args, config.args);
        assert_eq!(decoded.environment, config.environment);
        assert_eq!(decoded.creation_flags, config.creation_flags);
    }

    #[test]
    fn setup_failure_before_guardian_spawn_does_not_leave_a_child_lease() {
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
    fn lifecycle_git_captures_exact_output_with_unicode_cwd_and_environment() {
        let repository = tempfile::tempdir().expect("temporary repository");
        let cwd = repository.path().join("рабочая папка");
        fs::create_dir(&cwd).expect("unicode cwd");
        assert!(std::process::Command::new("git")
            .args(["init", "--initial-branch=main"])
            .current_dir(&cwd)
            .status()
            .expect("git init")
            .success());

        let hook = cwd.join(".git/hooks/pre-commit");
        fs::write(
            &hook,
            "#!/bin/sh\nprintf hook-stdout\nprintf hook-stderr >&2\n",
        )
        .expect("write exact-output hook");
        fs::write(cwd.join("output.txt"), "output\n").expect("write output fixture");

        let lock_path = repository.path().join("merge-operation.lock");
        fs::File::create(&lock_path).expect("child lease file");
        let exact_value = r#"значение with spaces \"and\" \\ trailing"#;
        let config = format!("wt.lifecycle={exact_value}");
        let mut environment = crate::git::sanitized_git_environment();
        let staged = output_git(&lock_path, &["add", "output.txt"], &cwd, &environment)
            .expect("stage exact-output fixture");
        assert!(staged.status.success());
        let hooked = output_git(
            &lock_path,
            &[
                "-c",
                "user.name=Windows Test",
                "-c",
                "user.email=windows@example.test",
                "commit",
                "--quiet",
                "-m",
                "exact output",
            ],
            &cwd,
            &environment,
        )
        .expect("hooked commit should run");
        assert_eq!(hooked.stdout, b"hook-stdout\n");
        assert_eq!(hooked.stderr, b"hook-stderr\n");
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

        let actual = output_git(
            &lock_path,
            &["rev-parse", "--show-toplevel"],
            &cwd,
            &environment,
        )
        .expect("contained Git invocation");
        assert_eq!(actual.stdout, format!("{}\n", cwd.display()).as_bytes());
        assert_eq!(actual.stderr, b"");

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
