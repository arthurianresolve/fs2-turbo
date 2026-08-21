use std::fs::File;
use std::io::{Error, ErrorKind, Result};
use std::os::windows::io::AsRawHandle;

use windows_sys::Win32::Storage::FileSystem::{
    FILE_ALLOCATION_INFO, FILE_STANDARD_INFO, FileAllocationInfo, FileStandardInfo,
    GetFileInformationByHandleEx, SetFileInformationByHandle,
};

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

pub(crate) const ALLOCATE_SPACE_EXTENDS_LENGTH: bool = false;

pub(crate) fn allocation_state_result(
    result: i32,
    info: FILE_STANDARD_INFO,
) -> Result<AllocationState> {
    win32_bool_result(result)?;
    Ok(AllocationState {
        allocated_size: u64::try_from(info.AllocationSize).map_err(|_| {
            Error::new(
                ErrorKind::InvalidData,
                "filesystem returned a negative allocation size",
            )
        })?,
        file_size: u64::try_from(info.EndOfFile).map_err(|_| {
            Error::new(
                ErrorKind::InvalidData,
                "filesystem returned a negative file size",
            )
        })?,
    })
}

pub(crate) fn allocate_space(file: &File, _state: AllocationState, len: u64) -> Result<()> {
    // FileAllocationInfo may reduce EOF when AllocationSize is below the
    // current logical length. Under FileExt::allocate's documented exclusive
    // size-ownership contract, refresh EOF at the destructive sink instead of
    // relying on the earlier allocation-policy snapshot. Windows exposes no
    // atomic max-allocation primitive, so this refresh is not concurrency
    // control for non-cooperating resizers.
    let live_state = allocation_state(file)?;
    let len = i64::try_from(allocation_target(live_state, len))
        .map_err(|_| Error::new(ErrorKind::InvalidInput, "allocation length is too large"))?;
    let info = FILE_ALLOCATION_INFO {
        AllocationSize: len,
    };
    let ret = unsafe {
        // SAFETY: `file` owns a valid handle and `info` is properly sized and aligned.
        SetFileInformationByHandle(
            file.as_raw_handle(),
            FileAllocationInfo,
            std::ptr::from_ref(&info).cast(),
            std::mem::size_of::<FILE_ALLOCATION_INFO>() as u32,
        )
    };
    win32_bool_result(ret)?;
    Ok(())
}

#[inline]
pub(crate) fn allocation_target(state: AllocationState, len: u64) -> u64 {
    len.max(state.file_size)
}
