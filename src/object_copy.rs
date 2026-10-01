//! Independent object copies. Git remains responsible for repository metadata.
use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::Path;

use crate::cli::MaterializeCopyMode;
use crate::error::{AppError, Result};

pub fn validate_source(source: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = fs::metadata(source).map_err(copy_error)?;
        // SAFETY: geteuid has no arguments, pointers, or preconditions.
        if metadata.uid() != unsafe { libc::geteuid() } {
            return Err(AppError::git(
                "local source must be owned by the current user",
            ));
        }
    }
    validate_tree(&source.join("objects"))
}

fn validate_tree(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path).map_err(copy_error)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(AppError::git(
            "object tree must contain only real directories and regular files",
        ));
    }
    for entry in fs::read_dir(path).map_err(copy_error)? {
        let entry = entry.map_err(copy_error)?;
        let kind = entry.file_type().map_err(copy_error)?;
        if kind.is_dir() {
            validate_tree(&entry.path())?;
        } else if !kind.is_file() {
            return Err(AppError::git(
                "object tree must not contain symlinks or special files",
            ));
        }
    }
    Ok(())
}

pub fn copy_tree(source: &Path, destination: &Path, mode: MaterializeCopyMode) -> Result<()> {
    // Validate again after Git metadata creation. Like Git's local clone, this
    // is not a lock against a same-privilege actor replacing parent directories.
    validate_tree(source)?;
    copy_directory(source, destination, mode).map_err(copy_error)
}

fn copy_directory(source: &Path, destination: &Path, mode: MaterializeCopyMode) -> io::Result<()> {
    fs::create_dir_all(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let target = destination.join(entry.file_name());
        if kind.is_dir() {
            copy_directory(&entry.path(), &target, mode)?;
        } else if kind.is_file() {
            copy_file(&entry.path(), &target, mode)?;
        } else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "unsafe object entry",
            ));
        }
    }
    Ok(())
}

fn copy_file(source: &Path, destination: &Path, mode: MaterializeCopyMode) -> io::Result<()> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // Do not follow a file replaced by a symlink between inspection/open.
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let mut input = options.open(source)?;
    let metadata = input.metadata()?;
    if !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "unsafe object file",
        ));
    }
    // create_new prevents overwriting Git metadata or following destination links.
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)?;
    if mode == MaterializeCopyMode::Copy || !try_reflink(&input, &output)? {
        // Unlike fs::copy, explicit stream copying never selects a reflink.
        io::copy(&mut input, &mut output)?;
    }
    output.set_permissions(metadata.permissions())
}

#[cfg(target_os = "linux")]
fn try_reflink(input: &File, output: &File) -> io::Result<bool> {
    use std::os::fd::AsRawFd;
    // FICLONE from Linux uapi/linux/fs.h; independent inode, shared COW extents.
    const FICLONE: libc::c_ulong = 0x4004_9409;
    // SAFETY: both descriptors remain open for this synchronous ioctl. FICLONE
    // takes a source fd as its integer argument, not a userspace pointer.
    let result = unsafe { libc::ioctl(output.as_raw_fd(), FICLONE, input.as_raw_fd()) };
    if result == 0 {
        return Ok(true);
    }
    let error = io::Error::last_os_error();
    if unsupported_reflink(&error) {
        return Ok(false);
    }
    Err(error)
}

#[cfg(target_os = "linux")]
fn unsupported_reflink(error: &io::Error) -> bool {
    matches!(
        error.raw_os_error(),
        Some(libc::EOPNOTSUPP | libc::ENOTTY | libc::EXDEV | libc::EINVAL | libc::ENOSYS)
    )
}

#[cfg(not(target_os = "linux"))]
fn try_reflink(_input: &File, _output: &File) -> io::Result<bool> {
    Ok(false)
}

fn copy_error(error: io::Error) -> AppError {
    AppError::git(format!("cannot copy independent object tree: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forced_copy_and_auto_have_independent_writes() {
        let root = tempfile::tempdir().expect("temp directory");
        let source = root.path().join("source");
        fs::write(&source, b"original bytes").expect("source");
        for mode in [MaterializeCopyMode::Auto, MaterializeCopyMode::Copy] {
            let destination = root.path().join(format!("{mode:?}"));
            copy_file(&source, &destination, mode).expect("copy");
            fs::write(&destination, b"changed bytes").expect("independent write");
            assert_eq!(fs::read(&source).expect("read source"), b"original bytes");
        }
    }

    #[test]
    fn copies_many_loose_files_without_links_or_alternates() {
        let root = tempfile::tempdir().expect("temp directory");
        let source = root.path().join("objects");
        fs::create_dir_all(source.join("ab")).expect("object directory");
        for index in 0_i32..1_200 {
            fs::write(
                source.join("ab").join(index.to_string()),
                index.to_le_bytes(),
            )
            .expect("loose file");
        }
        for mode in [MaterializeCopyMode::Auto, MaterializeCopyMode::Copy] {
            let destination = root.path().join(format!("{mode:?}"));
            copy_tree(&source, &destination, mode).expect("copy tree");
            for index in 0_i32..1_200 {
                let path = destination.join("ab").join(index.to_string());
                assert_eq!(fs::read(&path).expect("copied file"), index.to_le_bytes());
                #[cfg(unix)]
                {
                    use std::os::unix::fs::MetadataExt;
                    assert_eq!(fs::metadata(path).expect("metadata").nlink(), 1);
                }
            }
            assert!(!destination.join("info/alternates").exists());
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn supported_reflinks_have_independent_inodes_and_writes() {
        use std::os::unix::fs::MetadataExt;
        let root = tempfile::tempdir().expect("temp directory");
        let source = root.path().join("source");
        let destination = root.path().join("destination");
        fs::write(&source, "original").expect("source");
        let input = File::open(&source).expect("input");
        let output = File::create(&destination).expect("output");
        if !try_reflink(&input, &output).expect("reflink attempt") {
            // Portable suite: unsupported filesystems are covered separately.
            return;
        }
        let original = input.metadata().expect("source metadata");
        let cloned = output.metadata().expect("clone metadata");
        assert_ne!(
            (original.dev(), original.ino()),
            (cloned.dev(), cloned.ino())
        );
        assert_eq!(cloned.nlink(), 1);
        fs::write(&destination, "changed").expect("independent write");
        assert_eq!(fs::read(&source).expect("source content"), b"original");
        fs::remove_file(&source).expect("source removal");
        assert_eq!(fs::read(&destination).expect("copy survives"), b"changed");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn invalid_reflink_descriptor_is_reported() {
        let root = tempfile::tempdir().expect("temp directory");
        let source = root.path().join("source");
        fs::write(&source, "content").expect("source");
        let input = File::open(&source).expect("input");
        let output = File::open(&source).expect("read-only output");
        assert!(try_reflink(&input, &output).is_err());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn fallback_errors_exclude_genuine_failures() {
        for code in [
            libc::EOPNOTSUPP,
            libc::ENOTTY,
            libc::EXDEV,
            libc::EINVAL,
            libc::ENOSYS,
        ] {
            assert!(unsupported_reflink(&io::Error::from_raw_os_error(code)));
        }
        for code in [libc::EIO, libc::ENOSPC, libc::EACCES, libc::EPERM] {
            assert!(!unsupported_reflink(&io::Error::from_raw_os_error(code)));
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn tmpfs_unsupported_reflink_falls_back() {
        let Ok(root) = tempfile::tempdir_in("/dev/shm") else {
            return;
        };
        let source = root.path().join("source");
        let destination = root.path().join("destination");
        fs::write(&source, b"fallback bytes").expect("source");
        let input = File::open(&source).expect("input");
        let output = File::create(root.path().join("probe")).expect("probe");
        assert!(!try_reflink(&input, &output).expect("unsupported tmpfs"));
        copy_file(&source, &destination, MaterializeCopyMode::Auto).expect("fallback");
        assert_eq!(fs::read(destination).expect("read copy"), b"fallback bytes");
    }
}
