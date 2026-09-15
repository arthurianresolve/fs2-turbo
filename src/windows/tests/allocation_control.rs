use std::cell::{Cell, RefCell};
use std::fs::{File, OpenOptions};
use std::io::{Error, ErrorKind};
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;

use windows_sys::Win32::Foundation::{
    ERROR_ACCESS_DENIED, ERROR_INVALID_FUNCTION, ERROR_INVALID_PARAMETER, ERROR_IO_PENDING,
    ERROR_MORE_DATA, ERROR_NOT_SUPPORTED, SetLastError,
};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_COMPRESSED, FILE_ATTRIBUTE_NORMAL, FILE_ATTRIBUTE_OFFLINE,
    FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS, FILE_ATTRIBUTE_RECALL_ON_OPEN, FILE_FLAG_OVERLAPPED,
};
use windows_sys::Win32::System::IO::DeviceIoControl;
use windows_sys::Win32::System::Ioctl::{
    FILE_ALLOCATED_RANGE_BUFFER, FSCTL_QUERY_ALLOCATED_RANGES,
};

use crate::AllocationState;
use crate::windows::allocation::{
    allocate, allocate_space, allocate_with_attributes, allocated_range_result, allocation_state,
    complete_device_control, device_control_result, extend_file_length_with,
    file_attributes_result, requested_range_is_allocated, reserve_sparse_range_with,
    sparse_clear_result, wait_for_device_control,
};
use crate::windows::overlapped::PrivateOverlapped;

fn state() -> AllocationState {
    AllocationState {
        allocated_size: 0,
        file_size: 0,
    }
}

fn denied() -> Error {
    Error::from_raw_os_error(ERROR_ACCESS_DENIED as i32)
}

#[test]
fn unsuitable_attributes_are_rejected_before_file_mutation() {
    let file = tempfile::tempfile().unwrap();
    for attributes in [
        FILE_ATTRIBUTE_COMPRESSED,
        FILE_ATTRIBUTE_OFFLINE,
        FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS,
        FILE_ATTRIBUTE_RECALL_ON_OPEN,
    ] {
        let error = allocate_with_attributes(&file, 4096, attributes, state()).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Unsupported);
        assert_eq!(file.metadata().unwrap().len(), 0);
    }
}

#[test]
fn attribute_queries_preserve_error_precedence_and_valid_snapshots() {
    for (result, attributes, state_fails, expected_calls, expected_error) in [
        (
            0,
            FILE_ATTRIBUTE_NORMAL,
            true,
            0,
            Some(ErrorKind::PermissionDenied),
        ),
        (0, 0, true, 0, Some(ErrorKind::PermissionDenied)),
        (1, 0, true, 0, Some(ErrorKind::InvalidData)),
        (
            1,
            FILE_ATTRIBUTE_NORMAL,
            true,
            1,
            Some(ErrorKind::PermissionDenied),
        ),
        (1, FILE_ATTRIBUTE_NORMAL, false, 1, None),
    ] {
        let calls = Cell::new(0);
        // SAFETY: the native error slot belongs to this test thread.
        unsafe { SetLastError(ERROR_ACCESS_DENIED) };
        let result = file_attributes_result(result, attributes, || {
            calls.set(calls.get() + 1);
            if state_fails {
                Err(denied())
            } else {
                Ok(AllocationState {
                    allocated_size: 8192,
                    file_size: 1234,
                })
            }
        });
        assert_eq!(calls.get(), expected_calls);
        if let Some(expected) = expected_error {
            let error = result.unwrap_err();
            assert_eq!(error.kind(), expected);
            if expected == ErrorKind::PermissionDenied {
                assert_eq!(error.raw_os_error(), Some(ERROR_ACCESS_DENIED as i32));
            }
        } else {
            let (actual_attributes, actual_state) = result.unwrap();
            assert_eq!(actual_attributes, attributes);
            assert_eq!(actual_state.allocated_size, 8192);
            assert_eq!(actual_state.file_size, 1234);
        }
    }
}

#[test]
fn existing_reservation_extends_the_logical_length() {
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ALLOCATION_INFO, FileAllocationInfo, SetFileInformationByHandle,
    };

    let file = tempfile::tempfile().unwrap();
    file.set_len(1).unwrap();
    let reservation = FILE_ALLOCATION_INFO {
        AllocationSize: 8192,
    };
    // SAFETY: the file handle and correctly sized input remain valid for this call.
    let result = unsafe {
        SetFileInformationByHandle(
            file.as_raw_handle(),
            FileAllocationInfo,
            std::ptr::from_ref(&reservation).cast(),
            std::mem::size_of::<FILE_ALLOCATION_INFO>() as u32,
        )
    };
    assert_ne!(result, 0, "{}", Error::last_os_error());
    let before = allocation_state(&file).unwrap();
    assert_eq!(before.file_size, 1);
    assert!(before.allocated_size >= 4096);

    allocate_with_attributes(&file, 4096, FILE_ATTRIBUTE_NORMAL, before).unwrap();

    let after = allocation_state(&file).unwrap();
    assert_eq!(after.file_size, 4096);
    assert!(after.allocated_size >= 4096);
}

#[test]
fn sparse_reservation_validates_before_and_after_restoring_the_attribute() {
    for (first, last, expected_calls) in [
        (true, true, vec!["query"]),
        (false, true, vec!["query", "clear", "restore", "query"]),
        (false, false, vec!["query", "clear", "restore", "query"]),
    ] {
        let calls = RefCell::new(Vec::new());
        let mut queries = [first, last].into_iter();
        let result = reserve_sparse_range_with(
            || {
                calls.borrow_mut().push("query");
                Ok(queries.next().unwrap())
            },
            || {
                calls.borrow_mut().push("clear");
                Ok(())
            },
            || {
                calls.borrow_mut().push("restore");
                Ok(())
            },
        );
        assert_eq!(calls.into_inner(), expected_calls);
        if first || last {
            result.unwrap();
        } else {
            assert_eq!(result.unwrap_err().kind(), ErrorKind::Unsupported);
        }
    }
}

#[test]
fn sparse_failures_stop_the_remaining_operations() {
    for failing_step in 0..4 {
        let calls = RefCell::new(Vec::new());
        let mut query_number = 0;
        let result = reserve_sparse_range_with(
            || {
                let step = if query_number == 0 { 0 } else { 3 };
                query_number += 1;
                calls.borrow_mut().push(step);
                if step == failing_step {
                    Err(denied())
                } else {
                    Ok(false)
                }
            },
            || {
                calls.borrow_mut().push(1);
                if failing_step == 1 {
                    Err(denied())
                } else {
                    Ok(())
                }
            },
            || {
                calls.borrow_mut().push(2);
                if failing_step == 2 {
                    Err(denied())
                } else {
                    Ok(())
                }
            },
        );
        assert_eq!(
            result.unwrap_err().raw_os_error(),
            Some(ERROR_ACCESS_DENIED as i32)
        );
        assert_eq!(calls.into_inner(), (0..=failing_step).collect::<Vec<_>>());
    }
}

#[test]
fn sparse_clear_maps_only_unsupported_native_errors() {
    for code in [
        ERROR_INVALID_FUNCTION,
        ERROR_INVALID_PARAMETER,
        ERROR_NOT_SUPPORTED,
    ] {
        let error = sparse_clear_result(Err(Error::from_raw_os_error(code as i32))).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Unsupported);
    }
    assert_eq!(
        sparse_clear_result(Err(denied()))
            .unwrap_err()
            .raw_os_error(),
        Some(ERROR_ACCESS_DENIED as i32)
    );
    sparse_clear_result(Ok(())).unwrap();
}

#[test]
fn device_control_captures_native_errors_and_waits_only_when_pending() {
    assert_eq!(device_control_result(1, 24).unwrap(), 24);
    // SAFETY: the native error slot belongs to this test thread.
    unsafe { SetLastError(ERROR_MORE_DATA) };
    let (error, returned) = device_control_result(0, 16).unwrap_err();
    assert_eq!(error.raw_os_error(), Some(ERROR_MORE_DATA as i32));
    assert_eq!(returned, 16);

    for submitted in [Ok(8), Err((denied(), 4))] {
        let expected = submitted.as_ref().copied().map_err(|(_, bytes)| *bytes);
        let waited = Cell::new(false);
        let result = complete_device_control(submitted, |_| {
            waited.set(true);
            Ok(99)
        });
        assert!(!waited.get());
        assert_eq!(result.map_err(|(_, bytes)| bytes), expected);
    }
    for completion in [Ok(32), Err((denied(), 7))] {
        let calls = Cell::new(0);
        let expected = completion.as_ref().copied().map_err(|(_, bytes)| *bytes);
        let result = complete_device_control(
            Err((Error::from_raw_os_error(ERROR_IO_PENDING as i32), 11)),
            |returned| {
                assert_eq!(returned, 11);
                calls.set(calls.get() + 1);
                completion
            },
        );
        assert_eq!(calls.get(), 1);
        assert_eq!(result.map_err(|(_, bytes)| bytes), expected);
    }
}

#[test]
fn allocated_range_results_require_a_complete_prefix() {
    let buffer_size = std::mem::size_of::<FILE_ALLOCATED_RANGE_BUFFER>() as u32;
    for more_data in [false, true] {
        for (returned, offset, length, expected) in [
            (buffer_size, 0, 4096, true),
            (buffer_size - 1, 0, 4096, false),
            (buffer_size, 1, 4096, false),
            (buffer_size, 0, 4095, false),
            (0, 0, 0, false),
        ] {
            let result = if more_data {
                Err((Error::from_raw_os_error(ERROR_MORE_DATA as i32), returned))
            } else {
                Ok(returned)
            };
            let range = FILE_ALLOCATED_RANGE_BUFFER {
                FileOffset: offset,
                Length: length,
            };
            assert_eq!(
                allocated_range_result(result, range, 4096).unwrap(),
                expected
            );
        }
    }
    let error = allocated_range_result(
        Err((denied(), buffer_size)),
        FILE_ALLOCATED_RANGE_BUFFER::default(),
        4096,
    )
    .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Unsupported);
}

#[test]
fn allocation_length_errors_and_zero_requests_preserve_the_file() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("length");
    let file = File::create(&path).unwrap();
    file.set_len(8).unwrap();
    let error = extend_file_length_with(&file, 16, Err(denied())).unwrap_err();
    assert_eq!(error.raw_os_error(), Some(ERROR_ACCESS_DENIED as i32));
    assert_eq!(file.metadata().unwrap().len(), 8);
    extend_file_length_with(&file, 4, Ok(8)).unwrap();
    assert_eq!(file.metadata().unwrap().len(), 8);
    drop(file);

    let readonly = File::open(path).unwrap();
    assert!(extend_file_length_with(&readonly, 16, Ok(8)).is_err());
    assert!(allocate(&readonly, 4096).is_err());
    allocate_space(&readonly, state(), 0).unwrap();
    assert!(requested_range_is_allocated(&readonly, 0).unwrap());
    assert_eq!(readonly.metadata().unwrap().len(), 8);
}

#[test]
fn completed_native_range_query_can_be_observed_through_private_event() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("overlapped-range");
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .custom_flags(FILE_FLAG_OVERLAPPED)
        .open(path)
        .unwrap();
    allocate(&file, 4096).unwrap();
    let query = FILE_ALLOCATED_RANGE_BUFFER {
        FileOffset: 0,
        Length: 4096,
    };
    let mut range = FILE_ALLOCATED_RANGE_BUFFER::default();
    let mut overlapped = PrivateOverlapped::new().unwrap();
    let mut returned = 0;
    let buffer_size = std::mem::size_of::<FILE_ALLOCATED_RANGE_BUFFER>() as u32;
    // SAFETY: all buffers, the handle and OVERLAPPED remain alive until completion below.
    let result = unsafe {
        DeviceIoControl(
            file.as_raw_handle(),
            FSCTL_QUERY_ALLOCATED_RANGES,
            std::ptr::from_ref(&query).cast(),
            buffer_size,
            std::ptr::from_mut(&mut range).cast(),
            buffer_size,
            &mut returned,
            overlapped.state_mut(),
        )
    };
    if result == 0 {
        assert_eq!(
            Error::last_os_error().raw_os_error(),
            Some(ERROR_IO_PENDING as i32)
        );
    }
    let completed = wait_for_device_control(&file, &overlapped, returned).unwrap();
    assert!(completed >= buffer_size);
    assert_eq!(range.FileOffset, 0);
    assert!(range.Length >= 4096);
}
