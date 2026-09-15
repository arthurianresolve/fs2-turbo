use std::cell::Cell;
use std::fs::File;
use std::io::{Error, ErrorKind};

use windows_sys::Win32::Foundation::{
    ERROR_ACCESS_DENIED, ERROR_NOT_ENOUGH_MEMORY, GetHandleInformation, HANDLE, SetLastError,
};

use crate::windows::overlapped::PrivateOverlapped;

use super::{allocate_sparse_space, allocate_with_attributes_result, with_device_control_event};

#[test]
fn allocation_propagates_attribute_snapshot_failure() {
    let file = tempfile::tempfile().unwrap();
    let error = Error::other("file attribute snapshot failed");

    assert!(allocate_with_attributes_result(&file, 1, Err(error)).is_err());
}

#[test]
fn sparse_allocation_propagates_length_extension_failure() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("read-only-sparse-allocation");
    File::create(&path).unwrap();
    let read_only = File::open(path).unwrap();

    assert!(allocate_sparse_space(&read_only, 1).is_err());
}

#[test]
fn sparse_allocation_rejects_oversized_lengths_before_file_mutation() {
    let file = File::open(std::env::current_exe().unwrap()).unwrap();
    let original_len = file.metadata().unwrap().len();

    for len in [1_u64 << 63, u64::MAX] {
        let error = allocate_sparse_space(&file, len).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::InvalidInput);
        assert_eq!(error.raw_os_error(), None);
        assert_eq!(error.to_string(), "allocation length is too large");
        assert_eq!(file.metadata().unwrap().len(), original_len);
    }
}

#[test]
fn event_initialization_failure_preserves_error_without_submitting() {
    for code in [
        None,
        Some(ERROR_NOT_ENOUGH_MEMORY),
        Some(ERROR_ACCESS_DENIED),
    ] {
        let event = match code {
            None => PrivateOverlapped::new(),
            Some(code) => unsafe {
                // SAFETY: last-error state is thread-local, and null transfers no handle.
                SetLastError(code);
                PrivateOverlapped::from_event(std::ptr::null_mut())
            },
        };
        let submitted = Cell::new(0);
        let result = with_device_control_event(event, |_| {
            submitted.set(submitted.get() + 1);
            Ok(99)
        });

        if let Some(code) = code {
            let (error, returned) = result.unwrap_err();
            assert_eq!(error.raw_os_error(), Some(code as i32));
            assert_eq!(returned, 0);
            assert_eq!(submitted.get(), 0);
        } else {
            assert_eq!(result.unwrap(), 99);
            assert_eq!(submitted.get(), 1);
        }
    }
}

#[test]
fn event_remains_valid_during_submission_and_preserves_its_result() {
    for success in [true, false] {
        let event = PrivateOverlapped::new().unwrap();
        let calls = Cell::new(0);
        let result = with_device_control_event(Ok(event), |overlapped| {
            calls.set(calls.get() + 1);
            let tagged = overlapped.state().hEvent as usize;
            assert_eq!(tagged & 1, 1);
            let handle = (tagged & !1) as HANDLE;
            let mut flags = 0;
            let valid = unsafe {
                // SAFETY: the owning event outlives this callback and flags is writable.
                GetHandleInformation(handle, &mut flags)
            };
            assert_ne!(valid, 0);

            if success {
                Ok(19)
            } else {
                Err((Error::from_raw_os_error(ERROR_ACCESS_DENIED as i32), 23))
            }
        });

        assert_eq!(calls.get(), 1);
        if success {
            assert_eq!(result.unwrap(), 19);
        } else {
            let (error, returned) = result.unwrap_err();
            assert_eq!(error.raw_os_error(), Some(ERROR_ACCESS_DENIED as i32));
            assert_eq!(returned, 23);
        }
    }
}
