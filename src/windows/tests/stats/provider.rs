use super::*;
use crate::AllocationState;
use crate::windows::allocation::{allocation_target, requested_range_is_allocated};
use std::io::{Seek as _, SeekFrom, Write as _};
use std::os::windows::io::AsRawHandle as _;
use windows_sys::Win32::Foundation::{ERROR_INVALID_FUNCTION, ERROR_NOT_SUPPORTED};
use windows_sys::Win32::System::IO::DeviceIoControl;
use windows_sys::Win32::System::Ioctl::FSCTL_SET_SPARSE;

#[test]
fn maps_win32_boolean_results() {
    assert!(win32_bool_result(1).is_ok());
    assert!(win32_bool_result(-1).is_ok());
    assert!(win32_bool_result(0).is_err());
}

#[test]
fn maps_native_result_seams_without_faulting_the_os() {
    assert!(allocation_state_result(0, FILE_STANDARD_INFO::default()).is_err());
    let info = FILE_STANDARD_INFO {
        AllocationSize: 8,
        EndOfFile: 6,
        ..Default::default()
    };
    let state = allocation_state_result(1, info).unwrap();
    assert_eq!(state.allocated_size, 8);
    assert_eq!(state.file_size, 6);
    assert!(
        allocation_state_result(
            1,
            FILE_STANDARD_INFO {
                AllocationSize: -1,
                ..Default::default()
            },
        )
        .is_err()
    );
    assert!(
        allocation_state_result(
            1,
            FILE_STANDARD_INFO {
                EndOfFile: -1,
                ..Default::default()
            },
        )
        .is_err()
    );

    assert_eq!(cluster_geometry_result(1, 2, 512).unwrap(), 1024);
    assert_eq!(
        cluster_geometry_result(1, u32::MAX, u32::MAX).unwrap(),
        u64::from(u32::MAX) * u64::from(u32::MAX)
    );
    assert!(cluster_geometry_result(0, 2, 512).is_err());

    assert!(byte_space_result(0, 1, 2, 3).is_err());
    assert!(byte_space_result(1, 4, 2, 3).is_err());
    assert!(byte_space_result(1, 3, 2, 4).is_err());
    assert_eq!(
        crate::windows::stats::test_support::direct_space_result(
            1,
            3,
            2,
            4,
            crate::stats::SpaceKind::Available,
        ),
        crate::windows::stats::test_support::DirectSpace::Unavailable,
    );
    let bytes = byte_space_result(1, 1, 2, 3).unwrap();
    assert_eq!(bytes.actual_free, 3);
    assert_eq!(bytes.caller_available, 1);
    assert_eq!(bytes.caller_total, 2);
}

#[test]
fn allocation_target_never_drops_below_existing_eof() {
    let state = AllocationState {
        allocated_size: 8,
        file_size: 12,
    };

    assert_eq!(allocation_target(state, 10), 12);
    assert_eq!(allocation_target(state, 12), 12);
    assert_eq!(allocation_target(state, 14), 14);
}

#[test]
fn selects_modern_or_legacy_query_without_native_provider_state() {
    let counters = FilesystemCounters::windows_modern_bytes(4096, 8, 6, 10);
    let tempdir = tempdir().unwrap();
    let mut root_path = [0u16; VOLUME_PATH_CAPACITY];
    volume_path(&wide_path(tempdir.path()).unwrap(), &mut root_path).unwrap();

    assert_eq!(
        crate::FsStats::from_counters(
            statvfs_root_with(&root_path, ProviderOutcome::Value(counters)).unwrap(),
        )
        .unwrap()
        .total_space(),
        10
    );
    assert!(
        statvfs_root_with(
            &root_path,
            ProviderOutcome::Unavailable(FallbackReason::ProviderMissing),
        )
        .is_ok()
    );
    let legacy = crate::FsStats::from_counters(legacy_statvfs(&root_path).unwrap()).unwrap();
    assert_eq!(
        root_space_with(
            &root_path,
            SpaceKind::AllocationGranularity,
            Ok(ProviderOutcome::Unavailable(
                FallbackReason::ProviderMissing,
            )),
        )
        .unwrap(),
        legacy.allocation_granularity()
    );
}

#[test]
fn allocation_preserves_readonly_native_errors() {
    let tempdir = tempdir().unwrap();
    let path = tempdir.path().join("fs2");
    fs::write(&path, []).unwrap();

    let readonly = fs::OpenOptions::new().read(true).open(path).unwrap();
    assert!(readonly.allocate(4096).is_err());
}

#[test]
fn allocation_never_accepts_an_unallocated_sparse_prefix() {
    let tempdir = tempdir().unwrap();
    let mut file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(tempdir.path().join("fs2-sparse"))
        .unwrap();
    let mut returned = 0;
    let sparse = unsafe {
        // SAFETY: `file` owns the handle and this control has no input or output buffer.
        DeviceIoControl(
            file.as_raw_handle(),
            FSCTL_SET_SPARSE,
            std::ptr::null(),
            0,
            std::ptr::null_mut(),
            0,
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    if sparse == 0 {
        let error = std::io::Error::last_os_error();
        if matches!(
            error.raw_os_error(),
            Some(code) if code == ERROR_INVALID_FUNCTION as i32 || code == ERROR_NOT_SUPPORTED as i32
        ) {
            return;
        }
        panic!("unable to create sparse-file allocation fixture: {error}");
    }

    file.set_len(2 * 1024 * 1024).unwrap();
    file.seek(SeekFrom::Start(1024 * 1024)).unwrap();
    file.write_all(&[1; 4096]).unwrap();
    file.sync_all().unwrap();
    assert!(crate::FileExt::allocated_size(&file).unwrap() >= 4096);
    match requested_range_is_allocated(&file, 4096) {
        Ok(false) => {}
        Ok(true) => panic!("sparse fixture unexpectedly covered the requested prefix"),
        Err(error) if error.kind() == std::io::ErrorKind::Unsupported => return,
        Err(error) => panic!("unable to inspect sparse-file allocation: {error}"),
    }

    match file.allocate(4096) {
        Ok(()) => assert!(requested_range_is_allocated(&file, 4096).unwrap()),
        Err(error) => assert_eq!(error.kind(), std::io::ErrorKind::Unsupported),
    }
}

#[test]
fn resolves_module_symbols_only_when_the_module_is_available() {
    assert!(resolve_module_symbol(std::ptr::null_mut(), get_disk_space_information).is_none());
    let module = unsafe {
        // SAFETY: the module name is a null-terminated static string.
        windows_sys::Win32::System::LibraryLoader::GetModuleHandleA(windows_sys::core::s!(
            "kernel32.dll"
        ))
    };
    assert!(!module.is_null());
    assert!(resolve_module_symbol(module, get_disk_space_information).is_some());
}

#[test]
fn owns_only_valid_windows_handles() {
    assert_eq!(with_owned_handle(INVALID_HANDLE_VALUE, |_| 7_u8), None);

    let tempdir = tempdir().unwrap();
    let file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(tempdir.path().join("fs2"))
        .unwrap();
    let handle = file.into_raw_handle();
    assert_eq!(with_owned_handle(handle, |_| 7_u8), Some(7));
}
