//! The inherited exclusive-sharing file handle bridges helper startup. The
//! helper joins the named job before launching Git; ordinary descendants join
//! atomically at creation. Neither suspension nor crash reclamation is needed.
use std::fs::File;
use std::io;
use std::mem::size_of;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::process::{Command, Output, Stdio};
use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{
    FileIdInfo, GetFileInformationByHandle, GetFileInformationByHandleEx,
    BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT,
    FILE_FLAG_OPEN_REPARSE_POINT, FILE_ID_INFO,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectBasicAccountingInformation,
    JobObjectExtendedLimitInformation, QueryInformationJobObject,
    JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_BREAKAWAY_OK, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK,
};
use windows_sys::Win32::System::Threading::GetCurrentProcess;

pub(super) struct Descendants(OwnedHandle);

impl Descendants {
    pub(super) fn open(parent: &File) -> io::Result<Self> {
        let name = job_name(parent)?;
        // SAFETY: name is NUL terminated; null attributes make a noninheritable
        // handle with the user's default security descriptor.
        let handle = unsafe { CreateJobObjectW(std::ptr::null(), name.as_ptr()) };
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: CreateJobObjectW returned a new owned handle.
        let job = Self(unsafe { OwnedHandle::from_raw_handle(handle) });
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        // SAFETY: buffer and size match the requested information class.
        let ok = unsafe {
            QueryInformationJobObject(
                job.0.as_raw_handle(),
                JobObjectExtendedLimitInformation,
                (&mut limits as *mut JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        let forbidden = JOB_OBJECT_LIMIT_BREAKAWAY_OK
            | JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK
            | JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if limits.BasicLimitInformation.LimitFlags & forbidden != 0 {
            return Err(io::Error::other("cache job has unknown unsafe limits"));
        }
        Ok(job)
    }

    pub(super) fn is_empty(&self) -> io::Result<bool> {
        let mut accounting = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
        // SAFETY: buffer and size match the requested information class.
        let ok = unsafe {
            QueryInformationJobObject(
                self.0.as_raw_handle(),
                JobObjectBasicAccountingInformation,
                (&mut accounting as *mut JOBOBJECT_BASIC_ACCOUNTING_INFORMATION).cast(),
                size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(accounting.ActiveProcesses == 0)
    }

    fn join_current(&self) -> io::Result<()> {
        // SAFETY: the job handle is retained; the pseudo handle denotes this process.
        if unsafe { AssignProcessToJobObject(self.0.as_raw_handle(), GetCurrentProcess()) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

/// Preserve raw Git arguments, cwd and explicit environment removals/values.
/// The caller has already sanitized the Git command. No executable is forwarded.
pub(super) fn output(parent: &File, command: &Command) -> io::Result<Output> {
    if command.get_program() != "git" {
        return Err(io::Error::other("cache helper only accepts git"));
    }
    let mut helper = Command::new(std::env::current_exe()?);
    helper.arg("__wt-cache-git").args(command.get_args());
    if let Some(cwd) = command.get_current_dir() {
        helper.current_dir(cwd);
    }
    for (key, value) in command.get_envs() {
        match value {
            Some(value) => {
                helper.env(key, value);
            }
            None => {
                helper.env_remove(key);
            }
        }
    }
    helper.stdin(Stdio::from(parent.try_clone()?)).output()
}

pub(super) fn run_helper(args: impl Iterator<Item = std::ffi::OsString>) -> io::Result<i32> {
    use std::os::windows::io::BorrowedHandle;
    let stdin = std::io::stdin();
    // SAFETY: stdin stays alive while its borrowed native handle is duplicated.
    let borrowed = unsafe { BorrowedHandle::borrow_raw(stdin.as_raw_handle()) };
    let lock = File::from(borrowed.try_clone_to_owned()?);
    validate_file(&lock)?;
    validate_share_denial(&lock)?;
    let job = Descendants::open(&lock)?;
    #[cfg(debug_assertions)]
    test_barrier("before-join")?;
    job.join_current()?;
    #[cfg(debug_assertions)]
    test_barrier("after-join")?;
    // Keep lock and job alive through the wait, even when Git returns an error.
    let status = Command::new("git")
        .args(args)
        .stdin(Stdio::null())
        .status()?;
    drop(job);
    drop(lock);
    status
        .code()
        .ok_or_else(|| io::Error::other("Git exited without a native exit code"))
}

pub(super) fn open_file(path: &std::path::Path) -> io::Result<File> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .share_mode(0)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    validate_file(&file)?;
    // Share denial prevents replacement/deletion while this handle is alive.
    // Still reject path reparse points rather than following an unknown entry.
    let metadata = std::fs::symlink_metadata(path)?;
    use std::os::windows::fs::MetadataExt;
    if !metadata.is_file() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(io::Error::other(
            "cache lock entry is not a regular private file",
        ));
    }
    Ok(file)
}

fn validate_file(file: &File) -> io::Result<()> {
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: the buffer has the documented layout and the file remains open.
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0 {
        return Err(io::Error::last_os_error());
    }
    if info.dwFileAttributes & (FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_REPARSE_POINT) != 0
        || info.nNumberOfLinks != 1
        || info.nFileSizeHigh != 0
        || info.nFileSizeLow != 0
        || !file.metadata()?.is_file()
    {
        return Err(io::Error::other(
            "cache lock has unknown native state; preserve it for inspection",
        ));
    }
    Ok(())
}

fn job_name(parent: &File) -> io::Result<Vec<u16>> {
    let mut identity = FILE_ID_INFO::default();
    // SAFETY: identity buffer and size match FileIdInfo; parent stays open.
    if unsafe {
        GetFileInformationByHandleEx(
            parent.as_raw_handle(),
            FileIdInfo,
            (&mut identity as *mut FILE_ID_INFO).cast(),
            size_of::<FILE_ID_INFO>() as u32,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let mut name = format!(
        "Global\\wt-cache-children-v1-{:016x}-",
        identity.VolumeSerialNumber
    );
    for byte in identity.FileId.Identifier {
        use std::fmt::Write;
        write!(&mut name, "{byte:02x}").map_err(io::Error::other)?;
    }
    Ok(name.encode_utf16().chain(Some(0)).collect())
}

/// Reject ordinary stdin files: the helper requires inherited share denial,
/// including deletion, rather than accepting any empty regular file.
fn validate_share_denial(file: &File) -> io::Result<()> {
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, GetFinalPathNameByHandleW, DELETE, FILE_READ_DATA, FILE_SHARE_DELETE,
        FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_WRITE_DATA, OPEN_EXISTING,
    };
    let mut path = vec![0u16; 32768];
    // SAFETY: the live file and writable buffer remain valid for the query.
    let length = unsafe {
        GetFinalPathNameByHandleW(
            file.as_raw_handle(),
            path.as_mut_ptr(),
            path.len() as u32,
            0,
        )
    };
    if length == 0 {
        return Err(io::Error::last_os_error());
    }
    if length as usize >= path.len() {
        return Err(io::Error::other("cache lock path exceeds native limit"));
    }
    for access in [FILE_READ_DATA, FILE_WRITE_DATA, DELETE] {
        // SAFETY: the returned path is NUL terminated; no security/template pointers.
        let handle = unsafe {
            CreateFileW(
                path.as_ptr(),
                access,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_OPEN_REPARSE_POINT,
                std::ptr::null_mut(),
            )
        };
        if handle != INVALID_HANDLE_VALUE {
            // SAFETY: this probe returned a newly owned handle.
            unsafe { CloseHandle(handle) };
            return Err(io::Error::other(
                "cache helper stdin does not retain exclusive sharing",
            ));
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(32) {
            return Err(error);
        }
    }
    Ok(())
}

/// Debug-only integration seam. File barriers make startup crash tests
/// deterministic against the actual CLI. Release binaries have no test controls.
#[cfg(debug_assertions)]
fn test_barrier(phase: &str) -> io::Result<()> {
    if std::env::var("WT_CACHE_HELPER_TEST_PHASE").as_deref() != Ok(phase) {
        return Ok(());
    }
    let root = std::env::var_os("WT_CACHE_HELPER_TEST_ROOT")
        .ok_or_else(|| io::Error::other("missing native helper test barrier root"))?;
    let root = std::path::PathBuf::from(root);
    std::fs::write(root.join("helper-ready"), "")?;
    let started = std::time::Instant::now();
    while !root.join("helper-release").try_exists()? {
        if started.elapsed() >= std::time::Duration::from_secs(15) {
            return Err(io::Error::other("native helper test barrier timed out"));
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    Ok(())
}
