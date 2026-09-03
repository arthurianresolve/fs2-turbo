use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};

use tempfile::tempdir;

use fs2::{FileExt, allocation_granularity};

/// Tests file duplication.
#[allow(deprecated)]
#[test]
fn duplicate() {
    let tempdir = tempdir().unwrap();
    let path = tempdir.path().join("fs2");
    let mut file1 = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .unwrap();
    let mut file2 = file1.duplicate().unwrap();

    // Write into the first file and then drop it.
    file1.write_all(b"foo").unwrap();
    drop(file1);

    let mut buf = vec![];

    // Read from the second file; since the position is shared it will already be at EOF.
    file2.read_to_end(&mut buf).unwrap();
    assert_eq!(0, buf.len());

    // Rewind and read.
    file2.seek(SeekFrom::Start(0)).unwrap();
    file2.read_to_end(&mut buf).unwrap();
    assert_eq!(&buf, &b"foo");
}

/// Tests file allocation.
#[test]
fn allocate() {
    let tempdir = tempdir().unwrap();
    let path = tempdir.path().join("fs2");
    let file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .unwrap();
    let blksize = allocation_granularity(&path).unwrap();

    assert_eq!(0, file.metadata().unwrap().len());

    // Allocate space for the file, checking that the allocated size steps
    // up by block size, and the file length matches the allocated size.

    file.allocate(2 * blksize - 1).unwrap();
    assert!(file.allocated_size().unwrap() >= 2 * blksize - 1);
    assert_eq!(2 * blksize - 1, file.metadata().unwrap().len());

    // Truncate the file, checking that the allocated size steps down by
    // block size.

    file.set_len(blksize + 1).unwrap();
    assert!(file.allocated_size().unwrap() > blksize);
    assert_eq!(blksize + 1, file.metadata().unwrap().len());

    // Allocation also restores the logical length when physical space is
    // already reserved. This protects the Windows metadata/set-length
    // path and the equivalent Unix fast path.
    file.allocate(2 * blksize - 1).unwrap();
    assert!(file.allocated_size().unwrap() >= 2 * blksize - 1);
    assert_eq!(2 * blksize - 1, file.metadata().unwrap().len());

    // An allocation request that is already satisfied leaves both the
    // allocated space and the file length unchanged.
    file.allocate(2 * blksize - 1).unwrap();
    assert!(file.allocated_size().unwrap() >= 2 * blksize - 1);
    assert_eq!(2 * blksize - 1, file.metadata().unwrap().len());
}

#[cfg(target_os = "linux")]
#[test]
fn allocate_reserves_sparse_file_blocks() {
    use std::os::unix::fs::MetadataExt;

    let tempdir = tempdir().unwrap();
    let path = tempdir.path().join("fs2-sparse");
    let file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&path)
        .unwrap();
    let len = 4 * allocation_granularity(&path).unwrap();

    file.set_len(len).unwrap();
    assert_eq!(file.metadata().unwrap().len(), len);
    let allocated = file.metadata().unwrap().blocks().checked_mul(512).unwrap();
    if allocated >= len {
        eprintln!("filesystem does not expose sparse allocation; skipping reservation assertion");
        return;
    }

    file.allocate(len).unwrap();

    assert!(file.allocated_size().unwrap() >= len);
    assert_eq!(file.metadata().unwrap().len(), len);
}

#[cfg(any(
    target_os = "windows",
    target_os = "freebsd",
    target_os = "android",
    target_os = "emscripten",
    target_os = "macos",
    target_os = "ios",
    all(target_os = "linux", not(target_env = "uclibc")),
))]
#[test]
fn allocate_is_idempotent() {
    let tempdir = tempdir().unwrap();
    let path = tempdir.path().join("fs2-idempotent");
    let file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&path)
        .unwrap();
    let block_size = allocation_granularity(&path).unwrap();
    let len = 2 * block_size;

    file.allocate(len).unwrap();
    file.allocate(len).unwrap();
    file.allocate(block_size).unwrap();

    assert!(file.allocated_size().unwrap() >= len);
    assert_eq!(file.metadata().unwrap().len(), len);
}

#[cfg(any(
    target_os = "freebsd",
    target_os = "android",
    target_os = "emscripten",
    target_os = "macos",
    target_os = "ios",
    all(target_os = "linux", not(target_env = "uclibc")),
))]
#[test]
fn allocate_propagates_read_only_file_error() {
    let tempdir = tempdir().unwrap();
    let path = tempdir.path().join("fs2-read-only");
    drop(
        fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)
            .unwrap(),
    );
    let file = fs::OpenOptions::new().read(true).open(path).unwrap();

    let error = file.allocate(4096).unwrap_err();

    assert!(error.raw_os_error().is_some());
}

#[test]
fn allocate_rejects_unrepresentable_length() {
    let tempdir = tempdir().unwrap();
    let file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(tempdir.path().join("fs2"))
        .unwrap();

    assert_eq!(
        file.allocate(i64::MAX as u64 + 1).unwrap_err().kind(),
        std::io::ErrorKind::InvalidInput
    );
}

#[cfg(unix)]
#[test]
fn unix_lock_is_replaced_only_when_expected() {
    let tempdir = tempdir().unwrap();
    let path = tempdir.path().join("fs2");
    let file1 = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .unwrap();
    let file2 = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .unwrap();

    // Creating a shared lock will drop an exclusive lock.
    file1.fs2_lock_exclusive().unwrap();
    file1.fs2_lock_shared().unwrap();
    file2.fs2_lock_shared().unwrap();

    // Attempting to replace a shared lock with an exclusive lock must fail.
    assert_eq!(
        file2.fs2_try_lock_exclusive().unwrap_err().raw_os_error(),
        fs2::lock_contended_error().raw_os_error()
    );
    file1.fs2_lock_shared().unwrap();
}

#[cfg(unix)]
#[allow(deprecated)]
#[test]
fn unix_lock_duplicate_descriptor_contract() {
    let tempdir = tempdir().unwrap();
    let path = tempdir.path().join("fs2");
    let file1 = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .unwrap();
    let file2 = file1.duplicate().unwrap();
    let file3 = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .unwrap();

    file1.fs2_lock_shared().unwrap();
    file2.fs2_lock_exclusive().unwrap();
    assert_eq!(
        file3.fs2_try_lock_shared().unwrap_err().raw_os_error(),
        fs2::lock_contended_error().raw_os_error()
    );

    // Either of the file descriptors should be able to unlock.
    file1.fs2_unlock().unwrap();
    file3.fs2_lock_shared().unwrap();
}

#[cfg(unix)]
#[test]
fn unix_lock_acquired_from_clone_is_not_inheritable() {
    use std::os::unix::io::AsRawFd;

    let tempdir = tempdir().unwrap();
    let path = tempdir.path().join("fs2");
    let file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .unwrap()
        .try_clone()
        .unwrap();

    let flags = unsafe {
        // SAFETY: `file` owns a valid descriptor for the duration of this call.
        libc::fcntl(file.as_raw_fd(), libc::F_GETFD)
    };
    assert_ne!(flags, -1);
    assert_ne!(flags & libc::FD_CLOEXEC, 0);
}
