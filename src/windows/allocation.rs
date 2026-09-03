use std::fs::File;
use std::io::{Error, ErrorKind, Result};
use std::os::windows::io::AsRawHandle;

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_INVALID_FUNCTION, ERROR_INVALID_PARAMETER, ERROR_IO_PENDING,
    ERROR_MORE_DATA, ERROR_NOT_SUPPORTED, HANDLE,
};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_ALLOCATION_INFO, FILE_ATTRIBUTE_COMPRESSED, FILE_ATTRIBUTE_OFFLINE,
    FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS, FILE_ATTRIBUTE_RECALL_ON_OPEN,
    FILE_ATTRIBUTE_SPARSE_FILE, FILE_BASIC_INFO, FILE_STANDARD_INFO, FileAllocationInfo,
    FileBasicInfo, FileStandardInfo, GetFileInformationByHandleEx, SetFileInformationByHandle,
};
use windows_sys::Win32::System::IO::{DeviceIoControl, GetOverlappedResult, OVERLAPPED};
use windows_sys::Win32::System::Ioctl::{
    FILE_ALLOCATED_RANGE_BUFFER, FILE_SET_SPARSE_BUFFER, FSCTL_QUERY_ALLOCATED_RANGES,
    FSCTL_SET_SPARSE,
};
use windows_sys::Win32::System::Threading::CreateEventW;

use crate::AllocationState;

use crate::windows::path::win32_bool_result;

#[inline(always)]
pub(crate) fn allocation_state(file: &File) -> Result<AllocationState> {
    let handle = file.as_raw_handle();
    let mut info = FILE_STANDARD_INFO::default();
    let ret = unsafe {
        // SAFETY: `file` owns a valid handle and `info` is properly sized and aligned.
        GetFileInformationByHandleEx(
            handle,
            FileStandardInfo,
            std::ptr::from_mut(&mut info).cast(),
            std::mem::size_of::<FILE_STANDARD_INFO>() as u32,
        )
    };

    allocation_state_result(ret, info)
}

pub(crate) const ALLOCATE_SPACE_EXTENDS_LENGTH: bool = true;
pub(crate) const ALWAYS_RESERVE_RANGE: bool = true;

pub(crate) fn allocation_state_result(
    result: i32,
    info: FILE_STANDARD_INFO,
) -> Result<AllocationState> {
    win32_bool_result(result)?;
    allocation_state_from_values(info.AllocationSize, info.EndOfFile)
}

fn allocation_state_from_values(allocated_size: i64, file_size: i64) -> Result<AllocationState> {
    Ok(AllocationState {
        allocated_size: u64::try_from(allocated_size).map_err(|_| {
            Error::new(
                ErrorKind::InvalidData,
                "filesystem returned a negative allocation size",
            )
        })?,
        file_size: u64::try_from(file_size).map_err(|_| {
            Error::new(
                ErrorKind::InvalidData,
                "filesystem returned a negative file size",
            )
        })?,
    })
}

pub(crate) fn allocate(file: &File, len: u64) -> Result<()> {
    if len == 0 {
        return Ok(());
    }

    let (attributes, state) = file_attributes_and_state(file)?;
    if attributes
        & (FILE_ATTRIBUTE_OFFLINE
            | FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS
            | FILE_ATTRIBUTE_RECALL_ON_OPEN)
        != 0
    {
        return Err(Error::new(
            ErrorKind::Unsupported,
            "Windows cannot prove full reservation for an offline or recall-on-access file",
        ));
    }
    if attributes & FILE_ATTRIBUTE_COMPRESSED != 0 {
        return Err(Error::new(
            ErrorKind::Unsupported,
            "Windows cannot prove full reservation for a compressed file",
        ));
    }
    if attributes & FILE_ATTRIBUTE_SPARSE_FILE != 0 {
        return allocate_sparse_space(file, len);
    }

    // Only sparse or compressed files can contain unallocated ranges below
    // EOF. For an ordinary file, this snapshot is a complete prefix-coverage
    // proof and preserves the one-query already-satisfied fast path.
    if state.file_size >= len {
        return Ok(());
    }

    if state.allocated_size >= len {
        extend_file_length(file, len)
    } else {
        allocate_regular_space(file, state, len)
    }
}

pub(crate) fn allocate_space(file: &File, _state: AllocationState, len: u64) -> Result<()> {
    allocate(file, len)
}

fn allocate_regular_space(file: &File, state: AllocationState, len: u64) -> Result<()> {
    // FileAllocationInfo may reduce EOF when AllocationSize is below the
    // current logical length. Under the documented exclusive size-ownership
    // contract, the allocation snapshot immediately preceding this call owns
    // the size transition. Windows exposes no atomic max-allocation primitive,
    // so this is not concurrency control for non-cooperating resizers.
    let target = allocation_target(state, len);
    let allocation_size = i64::try_from(target)
        .map_err(|_| Error::new(ErrorKind::InvalidInput, "allocation length is too large"))?;
    let info = FILE_ALLOCATION_INFO {
        AllocationSize: allocation_size,
    };
    let ret = unsafe {
        // SAFETY: the handle and input structure are valid for this call.
        SetFileInformationByHandle(
            file.as_raw_handle(),
            FileAllocationInfo,
            std::ptr::from_ref(&info).cast(),
            std::mem::size_of::<FILE_ALLOCATION_INFO>() as u32,
        )
    };
    win32_bool_result(ret)?;
    extend_file_length(file, target)
}

fn allocate_sparse_space(file: &File, len: u64) -> Result<()> {
    i64::try_from(len)
        .map_err(|_| Error::new(ErrorKind::InvalidInput, "allocation length is too large"))?;
    extend_file_length(file, len)?;
    if requested_range_is_allocated(file, len)? {
        return Ok(());
    }

    clear_sparse_file(file)?;
    set_sparse_file(file, true)?;
    if requested_range_is_allocated(file, len)? {
        Ok(())
    } else {
        Err(Error::new(
            ErrorKind::Unsupported,
            "Windows could not establish physical coverage of the requested sparse range",
        ))
    }
}

fn clear_sparse_file(file: &File) -> Result<()> {
    match set_sparse_file(file, false) {
        Err(error)
            if matches!(
                error.raw_os_error(),
                Some(code)
                    if code == ERROR_INVALID_FUNCTION as i32
                        || code == ERROR_INVALID_PARAMETER as i32
                        || code == ERROR_NOT_SUPPORTED as i32
            ) =>
        {
            Err(Error::new(
                ErrorKind::Unsupported,
                format!("Windows sparse-range allocation is unavailable: {error}"),
            ))
        }
        result => result,
    }
}

fn set_sparse_file(file: &File, sparse: bool) -> Result<()> {
    let input = FILE_SET_SPARSE_BUFFER { SetSparse: sparse };
    let result = unsafe {
        // SAFETY: `input` remains valid until the helper has observed native
        // completion, and this control request has no output buffer.
        overlapped_device_io_control(
            file,
            FSCTL_SET_SPARSE,
            std::ptr::from_ref(&input).cast(),
            std::mem::size_of::<FILE_SET_SPARSE_BUFFER>() as u32,
            std::ptr::null_mut(),
            0,
        )
    };
    result.map(|_| ()).map_err(|(error, _)| error)
}

#[inline]
pub(crate) fn allocation_target(state: AllocationState, len: u64) -> u64 {
    len.max(state.file_size)
}

fn extend_file_length(file: &File, len: u64) -> Result<()> {
    if file.metadata()?.len() < len {
        file.set_len(len)
    } else {
        Ok(())
    }
}

fn file_attributes_and_state(file: &File) -> Result<(u32, AllocationState)> {
    let mut info = FILE_BASIC_INFO::default();
    let result = unsafe {
        // SAFETY: `file` owns a valid handle and `info` is properly sized and aligned.
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FileBasicInfo,
            std::ptr::from_mut(&mut info).cast(),
            std::mem::size_of::<FILE_BASIC_INFO>() as u32,
        )
    };
    win32_bool_result(result)?;
    if info.FileAttributes == 0 {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "filesystem returned no file attributes",
        ));
    }
    Ok((info.FileAttributes, allocation_state(file)?))
}

struct Event(HANDLE);

impl Event {
    fn new() -> Result<Self> {
        let handle = unsafe {
            // SAFETY: null security attributes make the unnamed event non-inheritable.
            CreateEventW(std::ptr::null(), 1, 0, std::ptr::null())
        };
        if handle.is_null() {
            Err(Error::last_os_error())
        } else {
            Ok(Self(handle))
        }
    }
}

impl Drop for Event {
    fn drop(&mut self) {
        unsafe {
            // SAFETY: this object exclusively owns the event handle.
            CloseHandle(self.0);
        }
    }
}

unsafe fn overlapped_device_io_control(
    file: &File,
    control_code: u32,
    input: *const std::ffi::c_void,
    input_len: u32,
    output: *mut std::ffi::c_void,
    output_len: u32,
) -> std::result::Result<u32, (Error, u32)> {
    let event = Event::new().map_err(|error| (error, 0))?;
    let mut overlapped = OVERLAPPED {
        hEvent: event.0,
        ..OVERLAPPED::default()
    };
    let mut returned = 0;
    let result = unsafe {
        // SAFETY: the caller keeps both buffers valid until this helper returns,
        // and `overlapped` and its event remain alive through native completion.
        DeviceIoControl(
            file.as_raw_handle(),
            control_code,
            input,
            input_len,
            output,
            output_len,
            &mut returned,
            &mut overlapped,
        )
    };
    if result != 0 {
        return Ok(returned);
    }

    let error = Error::last_os_error();
    if error.raw_os_error() != Some(ERROR_IO_PENDING as i32) {
        return Err((error, returned));
    }

    let result = unsafe {
        // SAFETY: the file, OVERLAPPED state, event, and caller-owned buffers
        // remain valid while this waits for the pending operation to complete.
        GetOverlappedResult(file.as_raw_handle(), &overlapped, &mut returned, 1)
    };
    if result == 0 {
        Err((Error::last_os_error(), returned))
    } else {
        Ok(returned)
    }
}

pub(crate) fn requested_range_is_allocated(file: &File, len: u64) -> Result<bool> {
    if len == 0 {
        return Ok(true);
    }
    let len = i64::try_from(len)
        .map_err(|_| Error::new(ErrorKind::InvalidInput, "allocation length is too large"))?;
    let query = FILE_ALLOCATED_RANGE_BUFFER {
        FileOffset: 0,
        Length: len,
    };
    let mut range = FILE_ALLOCATED_RANGE_BUFFER::default();
    let result = unsafe {
        // SAFETY: both structures remain valid until the helper has observed
        // native completion, and `file` owns the queried handle.
        overlapped_device_io_control(
            file,
            FSCTL_QUERY_ALLOCATED_RANGES,
            std::ptr::from_ref(&query).cast(),
            std::mem::size_of::<FILE_ALLOCATED_RANGE_BUFFER>() as u32,
            std::ptr::from_mut(&mut range).cast(),
            std::mem::size_of::<FILE_ALLOCATED_RANGE_BUFFER>() as u32,
        )
    };
    let returned = match result {
        Ok(returned) => returned,
        Err((error, returned)) if error.raw_os_error() == Some(ERROR_MORE_DATA as i32) => returned,
        Err((error, _)) => {
            return Err(Error::new(
                ErrorKind::Unsupported,
                format!("Windows allocated-range verification is unavailable: {error}"),
            ));
        }
    };
    Ok(
        returned as usize >= std::mem::size_of::<FILE_ALLOCATED_RANGE_BUFFER>()
            && allocated_range_covers_prefix(range, len),
    )
}

#[inline]
pub(crate) fn allocated_range_covers_prefix(range: FILE_ALLOCATED_RANGE_BUFFER, len: i64) -> bool {
    range.FileOffset == 0 && range.Length >= len
}
