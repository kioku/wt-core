//! Native helper tests use the actual CLI, never the unit-test executable as wt.
#![cfg(windows)]

use std::fs::{File, OpenOptions};
use std::mem::size_of;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, IntoRawHandle, OwnedHandle};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use windows_sys::Win32::Storage::FileSystem::{
    FileIdInfo, GetFileInformationByHandleEx, FILE_ID_INFO,
};
use windows_sys::Win32::System::JobObjects::{
    CreateJobObjectW, IsProcessInJob, JobObjectBasicAccountingInformation,
    QueryInformationJobObject, JOBOBJECT_BASIC_ACCOUNTING_INFORMATION,
};
use windows_sys::Win32::System::Threading::GetCurrentProcess;

fn job(file: &File) -> OwnedHandle {
    let mut id = FILE_ID_INFO::default();
    // SAFETY: live file and correctly sized writable identity buffer.
    assert_ne!(
        unsafe {
            GetFileInformationByHandleEx(
                file.as_raw_handle(),
                FileIdInfo,
                (&mut id as *mut FILE_ID_INFO).cast(),
                size_of::<FILE_ID_INFO>() as u32,
            )
        },
        0
    );
    let suffix: String = id
        .FileId
        .Identifier
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let name: Vec<u16> = format!(
        "Global\\wt-cache-children-v1-{:016x}-{suffix}",
        id.VolumeSerialNumber
    )
    .encode_utf16()
    .chain(Some(0))
    .collect();
    // SAFETY: name is terminated, null attributes request default security.
    let raw = unsafe { CreateJobObjectW(std::ptr::null(), name.as_ptr()) };
    assert!(!raw.is_null());
    // SAFETY: API returned a newly owned handle.
    unsafe { OwnedHandle::from_raw_handle(raw) }
}

fn active(job: &OwnedHandle) -> u32 {
    let mut info = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
    // SAFETY: handle and accounting buffer are live and correctly sized.
    assert_ne!(
        unsafe {
            QueryInformationJobObject(
                job.as_raw_handle(),
                JobObjectBasicAccountingInformation,
                (&mut info as *mut JOBOBJECT_BASIC_ACCOUNTING_INFORMATION).cast(),
                size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
                std::ptr::null_mut(),
            )
        },
        0
    );
    info.ActiveProcesses
}

fn await_condition(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while !condition() {
        assert!(
            Instant::now() < deadline,
            "native fixture barrier timed out"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

// Cleanup also runs during assertion unwinding: no blocked fixture survives.
struct Running {
    child: Child,
    root: std::path::PathBuf,
}
impl Drop for Running {
    fn drop(&mut self) {
        let _ = std::fs::write(self.root.join("finish"), "");
        let _ = std::fs::write(self.root.join("helper-release"), "");
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn native_git_fixture() {
    let Some(root) = std::env::var_os("WT_NATIVE_CACHE_FIXTURE") else {
        return;
    };
    let root = std::path::PathBuf::from(root);
    let name: Vec<u16> = std::fs::read_to_string(root.join("job-name"))
        .expect("job name")
        .encode_utf16()
        .chain(Some(0))
        .collect();
    // SAFETY: terminated fixture name and null security attributes.
    let raw = unsafe { CreateJobObjectW(std::ptr::null(), name.as_ptr()) };
    assert!(!raw.is_null());
    // SAFETY: newly returned owned handle.
    let job = unsafe { OwnedHandle::from_raw_handle(raw) };
    let mut member = 0;
    // SAFETY: valid job, current-process pseudo handle and writable boolean.
    assert_ne!(
        unsafe { IsProcessInJob(GetCurrentProcess(), job.as_raw_handle(), &mut member) },
        0
    );
    assert_ne!(
        member, 0,
        "Git must already belong to the cache job before writing"
    );
    drop(job); // Only process membership, not fixture handles, retains the job.

    if std::env::var_os("WT_NATIVE_CACHE_LEAF").is_none() {
        let mut leaf = Command::new(std::env::current_exe().expect("fixture exe"));
        leaf.args(["--exact", "native_git_fixture", "--nocapture"])
            .env("WT_NATIVE_CACHE_LEAF", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let child = leaf.spawn().expect("ordinary Git descendant");
        // Windows has no waitpid zombies. Close the process handle explicitly
        // while the ordinary descendant continues; the coordinator releases it.
        let raw = child.into_raw_handle();
        // SAFETY: ownership was transferred out of Child just above.
        drop(unsafe { OwnedHandle::from_raw_handle(raw) });
        if std::env::var("WT_NATIVE_CACHE_HOLD_GIT").as_deref() == Ok("1") {
            await_condition(|| root.join("finish").exists());
        }
        return;
    }
    std::fs::write(root.join("ready"), "").expect("ready barrier");
    await_condition(|| root.join("finish").exists());
}

fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_wt-core"))
}

#[test]
fn helper_rejects_unleased_stdin_and_only_dispatches_first_argument() {
    let root = tempfile::tempdir().expect("fixture");
    let path = root.path().join("lock");
    std::fs::write(&path, "").expect("empty file");
    let output = cli()
        .args(["__wt-cache-git", "--version"])
        .stdin(File::open(&path).expect("ordinary stdin"))
        .output()
        .expect("helper");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("exclusive sharing"));
    let output = cli()
        .args(["--version", "__wt-cache-git"])
        .output()
        .expect("normal CLI");
    assert!(!String::from_utf8_lossy(&output.stderr).contains("cache Git helper"));
    assert!(cli()
        .arg("--version")
        .output()
        .expect("version")
        .status
        .success());
}

#[test]
fn helper_accepts_raw_git_arguments_and_returns_native_code() {
    let root = tempfile::tempdir().expect("fixture");
    let lock = exclusive(root.path());
    let output = cli()
        .args(["__wt-cache-git", "--version"])
        .stdin(lock.try_clone().expect("clone"))
        .output()
        .expect("helper");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).starts_with("git version"));
    let raw = [
        "rev-parse",
        "--sq-quote",
        "",
        "a b",
        "a\"b",
        "trailing\\",
        "--version",
        "__wt-cache-git",
    ];
    let expected = Command::new("git").args(raw).output().expect("direct Git");
    let actual = cli()
        .arg("__wt-cache-git")
        .args(raw)
        .stdin(lock.try_clone().expect("clone"))
        .output()
        .expect("raw helper");
    assert_eq!(actual.status.code(), expected.status.code());
    assert_eq!(actual.stdout, expected.stdout);
    let output = cli()
        .args([
            "__wt-cache-git",
            "--definitely-not-a-git-option",
            "",
            "a b",
            "a\"b",
            "trailing\\",
        ])
        .stdin(lock.try_clone().expect("clone"))
        .output()
        .expect("helper");
    assert_eq!(output.status.code(), Some(129));
}

fn exclusive(root: &Path) -> File {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .share_mode(0)
        .open(root.join("lock"))
        .expect("exclusive lock")
}

#[test]
fn ordinary_grandchild_keeps_job_after_helper_and_caller_handles_close() {
    descendant_scenario(false);
    descendant_scenario(true);
}

fn descendant_scenario(kill_helper: bool) {
    let root = tempfile::tempdir().expect("fixture");
    let lock = exclusive(root.path());
    let named_job = job(&lock);
    let mut id = FILE_ID_INFO::default();
    // SAFETY: same retained file and sized buffer as job identity query above.
    assert_ne!(
        unsafe {
            GetFileInformationByHandleEx(
                lock.as_raw_handle(),
                FileIdInfo,
                (&mut id as *mut FILE_ID_INFO).cast(),
                size_of::<FILE_ID_INFO>() as u32,
            )
        },
        0
    );
    let suffix: String = id
        .FileId
        .Identifier
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    std::fs::write(
        root.path().join("job-name"),
        format!(
            "Global\\wt-cache-children-v1-{:016x}-{suffix}",
            id.VolumeSerialNumber
        ),
    )
    .expect("name");
    std::fs::copy(
        std::env::current_exe().expect("test exe"),
        root.path().join("git.exe"),
    )
    .expect("native Git fixture");
    let child = cli()
        .args([
            "__wt-cache-git",
            "--exact",
            "native_git_fixture",
            "--nocapture",
        ])
        .env("PATH", root.path())
        .env(
            "WT_NATIVE_CACHE_HOLD_GIT",
            if kill_helper { "1" } else { "" },
        )
        .env("WT_NATIVE_CACHE_FIXTURE", root.path())
        .stdin(lock.try_clone().expect("clone"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("actual CLI helper");
    let mut running = Running {
        child,
        root: root.path().to_path_buf(),
    };
    drop(lock); // The caller no longer owns any file handle.
    await_condition(|| root.path().join("ready").exists());
    if kill_helper {
        running
            .child
            .kill()
            .expect("crash helper while Git and grandchild live");
    }
    running
        .child
        .wait()
        .expect("helper completes after direct Git exit");
    assert!(
        active(&named_job) > 0,
        "ordinary grandchild preserves lease"
    );
    drop(named_job); // Test retention with no coordinator job handle, too.
    let file = OpenOptions::new()
        .read(true)
        .share_mode(7)
        .open(root.path().join("lock"));
    // If an inherited handle survives in the fixture, sharing still denies entry.
    // Otherwise membership alone must protect admission (as production checks).
    if let Ok(file) = file {
        let reopened = job(&file);
        assert!(active(&reopened) > 0);
        std::fs::write(root.path().join("finish"), "").expect("release leaf");
        await_condition(|| active(&reopened) == 0);
    } else {
        std::fs::write(root.path().join("finish"), "").expect("release leaf");
        await_condition(|| {
            OpenOptions::new()
                .read(true)
                .write(true)
                .share_mode(0)
                .open(root.path().join("lock"))
                .is_ok()
        });
    }
}

/// Caller mode runs in a separate native process, so killing it closes every
/// caller-side clone while the actual CLI is stopped at a startup barrier.
#[test]
#[cfg(debug_assertions)]
fn native_caller_fixture() {
    let Some(root) = std::env::var_os("WT_NATIVE_CALLER_ROOT") else {
        return;
    };
    let root = std::path::PathBuf::from(root);
    let lock = exclusive(&root);
    let mut helper = cli()
        .args(["__wt-cache-git", "--version"])
        .stdin(lock.try_clone().expect("clone"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("actual helper");
    std::fs::write(root.join("helper-pid"), helper.id().to_string()).expect("helper pid");
    await_condition(|| root.join("finish").exists());
    helper.wait().expect("reap helper if caller survives");
}

#[cfg(debug_assertions)]
struct NativeHelper(OwnedHandle);
#[cfg(debug_assertions)]
impl Drop for NativeHelper {
    fn drop(&mut self) {
        use windows_sys::Win32::System::Threading::{TerminateProcess, WaitForSingleObject};
        // SAFETY: retained fixture process handle; cleanup is bounded and does
        // not target processes outside the fixture, even after PID reuse.
        unsafe {
            TerminateProcess(self.0.as_raw_handle(), 1);
            WaitForSingleObject(self.0.as_raw_handle(), 5000);
        }
    }
}

#[test]
#[cfg(debug_assertions)]
fn caller_death_before_join_and_helper_death_before_git_spawn() {
    startup_crash_scenario("before-join");
    startup_crash_scenario("after-join");
}

#[cfg(debug_assertions)]
fn startup_crash_scenario(phase: &str) {
    use windows_sys::Win32::System::Threading::{
        OpenProcess, TerminateProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
    };
    let root = tempfile::tempdir().expect("fixture");
    let child = Command::new(std::env::current_exe().expect("test exe"))
        .args(["--exact", "native_caller_fixture", "--nocapture"])
        .env("WT_NATIVE_CALLER_ROOT", root.path())
        .env("WT_CACHE_HELPER_TEST_ROOT", root.path())
        .env("WT_CACHE_HELPER_TEST_PHASE", phase)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("caller");
    let mut caller = Running {
        child,
        root: root.path().to_path_buf(),
    };
    await_condition(|| root.path().join("helper-pid").exists());
    let pid: u32 = std::fs::read_to_string(root.path().join("helper-pid"))
        .expect("pid")
        .parse()
        .expect("native pid");
    // SAFETY: open the fixture process and immediately retain its identity.
    let raw = unsafe { OpenProcess(PROCESS_TERMINATE | PROCESS_SYNCHRONIZE, 0, pid) };
    assert!(!raw.is_null());
    // SAFETY: newly returned owned process handle.
    let helper = NativeHelper(unsafe { OwnedHandle::from_raw_handle(raw) });
    await_condition(|| root.path().join("helper-ready").exists());
    caller.child.kill().expect("crash caller");
    caller.child.wait().expect("reap caller");
    // Before join, the inherited handle alone excludes another caller. After
    // join it still excludes one until the helper exits; no Git has been spawned.
    let attempt = OpenOptions::new()
        .read(true)
        .write(true)
        .share_mode(0)
        .open(root.path().join("lock"));
    assert_eq!(
        attempt
            .expect_err("inherited share denial survives caller death")
            .raw_os_error(),
        Some(32)
    );
    if phase == "before-join" {
        std::fs::write(root.path().join("helper-release"), "").expect("release startup");
    } else {
        // SAFETY: retained handle targets only the blocked fixture helper.
        assert_ne!(unsafe { TerminateProcess(helper.0.as_raw_handle(), 1) }, 0);
    }
    // SAFETY: process handle remains live and the wait has a finite deadline.
    assert_eq!(
        unsafe { WaitForSingleObject(helper.0.as_raw_handle(), 15000) },
        0
    );
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .share_mode(0)
        .open(root.path().join("lock"))
        .expect("startup crash releases file");
    assert_eq!(
        active(&job(&file)),
        0,
        "no permanent member remains after startup crash"
    );
}
