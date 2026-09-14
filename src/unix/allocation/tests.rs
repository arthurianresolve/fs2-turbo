#[cfg(target_os = "macos")]
use std::cell::RefCell;
#[cfg(target_os = "macos")]
use std::fs;
#[cfg(target_os = "macos")]
use std::fs::File;
use std::io::ErrorKind;
#[cfg(target_os = "macos")]
use std::os::unix::io::AsRawFd;

use super::blocks_to_bytes;
#[cfg(all(target_os = "linux", target_pointer_width = "64"))]
use super::i64_to_u64;
#[cfg(target_os = "macos")]
use tempfile::tempdir;

#[test]
fn checks_block_to_byte_conversion() {
    let largest = u64::MAX / 512;

    assert_eq!(blocks_to_bytes(largest).unwrap(), largest * 512);
    assert_eq!(
        blocks_to_bytes(largest + 1).unwrap_err().kind(),
        ErrorKind::InvalidData
    );
}

#[cfg(all(target_os = "linux", target_pointer_width = "64"))]
#[test]
fn rejects_negative_native_sizes() {
    assert_eq!(i64_to_u64(0, "negative value").unwrap(), 0);
    assert_eq!(i64_to_u64(4096i64, "negative value").unwrap(), 4096);
    assert!(i64_to_u64(-1i64, "negative value").is_err());
}

#[cfg(target_os = "macos")]
#[test]
fn macos_allocate_space_covers_native_control_flow() {
    let tempdir = tempdir().unwrap();
    let path = tempdir.path().join("fs2-macos-allocation");
    let file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&path)
        .unwrap();

    let flags = RefCell::new(Vec::new());
    let bytesalloc = RefCell::new(Vec::new());
    let mut results = [-1, 0, -1, -1].into_iter();
    let mut preallocate = |_: &File, fstore: &mut libc::fstore_t| -> libc::c_int {
        flags.borrow_mut().push(fstore.fst_flags);
        bytesalloc.borrow_mut().push(fstore.fst_bytesalloc);
        fstore.fst_bytesalloc = fstore.fst_length as _;
        results.next().unwrap()
    };

    super::allocate_space_with(&file, 4096, &mut preallocate).unwrap();
    assert_eq!(
        flags.borrow().as_slice(),
        &[libc::F_ALLOCATECONTIG, libc::F_ALLOCATEALL]
    );
    assert_eq!(bytesalloc.borrow().as_slice(), &[0, 4096]);

    let error = super::allocate_space_with(&file, 4096, &mut preallocate).unwrap_err();
    assert!(error.raw_os_error().is_some());
    assert_eq!(
        flags.borrow().as_slice(),
        &[
            libc::F_ALLOCATECONTIG,
            libc::F_ALLOCATEALL,
            libc::F_ALLOCATECONTIG,
            libc::F_ALLOCATEALL,
        ]
    );
    assert_eq!(bytesalloc.borrow().as_slice(), &[0, 4096, 0, 4096]);

    super::allocate_space_with(&file, 0, &mut preallocate).unwrap();

    let invalid = File::open(&path).unwrap();
    let invalid_fd = invalid.as_raw_fd();
    assert_eq!(unsafe { libc::close(invalid_fd) }, 0);
    assert!(
        super::allocate_space_with(&invalid, 1, &mut preallocate)
            .unwrap_err()
            .raw_os_error()
            .is_some()
    );
    std::mem::forget(invalid);
}
