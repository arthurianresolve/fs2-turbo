#[cfg(target_os = "macos")]
use std::cell::RefCell;
#[cfg(target_os = "macos")]
use std::fs;
#[cfg(target_os = "macos")]
use std::fs::File;
use std::io::ErrorKind;
#[cfg(target_os = "macos")]
use std::os::unix::io::AsRawFd;

#[cfg(not(all(target_os = "linux", target_pointer_width = "64")))]
use super::allocation_state_from_metadata;
#[cfg(all(target_os = "linux", target_pointer_width = "64"))]
use super::i64_to_u64;
use super::{allocation_state_from_blocks, blocks_to_bytes};
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

#[test]
fn validates_allocation_state_block_conversion() {
    let state = allocation_state_from_blocks(1, 7).unwrap();
    assert_eq!((state.allocated_size, state.file_size), (512, 7));
    assert_eq!(
        allocation_state_from_blocks(u64::MAX, 7)
            .unwrap_err()
            .kind(),
        ErrorKind::InvalidData
    );
}

#[cfg(not(all(target_os = "linux", target_pointer_width = "64")))]
#[test]
fn propagates_metadata_query_failure() {
    assert!(allocation_state_from_metadata(Err(std::io::Error::other("metadata failed"))).is_err());
}

#[cfg(all(target_os = "linux", target_pointer_width = "64"))]
#[test]
fn rejects_negative_native_sizes() {
    assert_eq!(i64_to_u64(0, "negative value").unwrap(), 0);
    assert_eq!(i64_to_u64(4096i64, "negative value").unwrap(), 4096);
    assert!(i64_to_u64(-1i64, "negative value").is_err());

    let file = tempfile::tempfile().unwrap();
    let state = super::AllocationState {
        allocated_size: 0,
        file_size: 0,
    };
    assert_eq!(
        super::allocate_space(&file, state, u64::MAX)
            .unwrap_err()
            .kind(),
        ErrorKind::InvalidInput
    );
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

    let empty_state = super::AllocationState {
        allocated_size: 0,
        file_size: 0,
    };

    super::allocate_space_with(&file, empty_state, 4096, &mut preallocate).unwrap();
    assert_eq!(
        flags.borrow().as_slice(),
        &[libc::F_ALLOCATECONTIG, libc::F_ALLOCATEALL]
    );
    assert_eq!(bytesalloc.borrow().as_slice(), &[0, 4096]);

    let error = super::allocate_space_with(&file, empty_state, 4096, &mut preallocate).unwrap_err();
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

    super::allocate_space_with(&file, empty_state, 0, &mut preallocate).unwrap();
    assert_eq!(
        super::allocate_space_with(&file, empty_state, u64::MAX, &mut preallocate)
            .unwrap_err()
            .kind(),
        ErrorKind::InvalidInput
    );

    super::allocate_space(
        &file,
        super::AllocationState {
            allocated_size: 4096,
            file_size: 0,
        },
        4096,
    )
    .unwrap();
    assert_eq!(
        super::allocate_space(
            &file,
            super::AllocationState {
                allocated_size: 0,
                file_size: 0,
            },
            u64::MAX,
        )
        .unwrap_err()
        .kind(),
        ErrorKind::InvalidInput
    );

    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "unix::allocation::tests::macos_invalid_descriptor_fixture",
            "--nocapture",
        ])
        .env("FS2_MACOS_INVALID_DESCRIPTOR_FIXTURE", "1")
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(0));
}

#[cfg(target_os = "macos")]
#[test]
fn macos_invalid_descriptor_fixture() {
    if std::env::var_os("FS2_MACOS_INVALID_DESCRIPTOR_FIXTURE").is_none() {
        return;
    }

    let file = std::mem::ManuallyDrop::new(tempfile::tempfile().unwrap());
    let invalid_fd = file.as_raw_fd();
    // SAFETY: the wrapper is manually dropped, so this descriptor is closed
    // exactly once and cannot be reused by another test in this child process.
    assert_eq!(unsafe { libc::close(invalid_fd) }, 0);
    let state = super::AllocationState {
        allocated_size: 0,
        file_size: 0,
    };

    // Exercise every branch through one child-local closure type so its
    // compiler-generated instantiations remain complete.
    let mut results = [0, -1, 0, -1, -1].into_iter();
    let mut preallocate = |_: &File, _: &mut libc::fstore_t| results.next().unwrap();
    super::allocate_space_with(&file, state, 1, &mut preallocate).unwrap();
    super::allocate_space_with(&file, state, 1, &mut preallocate).unwrap();
    assert!(super::allocate_space_with(&file, state, 1, &mut preallocate).is_err());
    super::allocate_space_with(&file, state, 0, &mut preallocate).unwrap();
    assert_eq!(
        super::allocate_space_with(&file, state, u64::MAX, &mut preallocate)
            .unwrap_err()
            .kind(),
        ErrorKind::InvalidInput
    );

    assert!(
        super::allocate_space(&file, state, 1)
            .unwrap_err()
            .raw_os_error()
            .is_some()
    );
}

#[cfg(all(target_os = "linux", target_pointer_width = "64"))]
#[test]
fn native_stat_failure_does_not_read_uninitialized_output() {
    // SAFETY: close(-1) cannot close an owned descriptor and sets errno to EBADF.
    assert_eq!(unsafe { libc::close(-1) }, -1);
    // SAFETY: the failure result does not require an initialized output value.
    let error =
        unsafe { super::allocation_state_result(-1, std::mem::MaybeUninit::uninit()) }.unwrap_err();
    assert_eq!(error.raw_os_error(), Some(libc::EBADF));
}

#[cfg(all(target_os = "linux", target_pointer_width = "64"))]
#[test]
fn native_stat_validation_checks_both_signed_fields() {
    for (blocks, length, expected) in [(1, 7, Some((512, 7))), (-1, 7, None), (1, -1, None)] {
        // SAFETY: Linux stat contains integer fields and accepts all-zero storage.
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        stat.st_blocks = blocks;
        stat.st_size = length;
        // SAFETY: every field was initialized before supplying a success result.
        let result = unsafe { super::allocation_state_result(0, std::mem::MaybeUninit::new(stat)) };
        match expected {
            Some((allocated, size)) => {
                let state = result.unwrap();
                assert_eq!((state.allocated_size, state.file_size), (allocated, size));
            }
            None => assert_eq!(result.unwrap_err().kind(), ErrorKind::InvalidData),
        }
    }
}
