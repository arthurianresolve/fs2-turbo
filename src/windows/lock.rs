use std::fs::File;
use std::io::{Error, Result};
use std::os::windows::io::AsRawHandle;

use windows_sys::Win32::Foundation::{ERROR_IO_PENDING, ERROR_LOCK_VIOLATION};
use windows_sys::Win32::Storage::FileSystem::{
    LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY, LockFile, LockFileEx, UnlockFile,
};
use windows_sys::Win32::System::IO::GetOverlappedResult;

use crate::windows::overlapped::PrivateOverlapped;
use crate::windows::path::win32_bool_result;

#[inline(always)]
pub(crate) fn lock_shared(file: &File, nonblocking: bool) -> Result<()> {
    let flags = if nonblocking {
        LOCKFILE_FAIL_IMMEDIATELY
    } else {
        0
    };
    lock_file(file, flags)
}

#[inline(always)]
pub(crate) fn lock_exclusive(file: &File, nonblocking: bool) -> Result<()> {
    if nonblocking {
        return try_lock_exclusive(file);
    }

    // LockFile completes synchronously and avoids creating OVERLAPPED state on
    // the common uncontended path. Any failure falls through to LockFileEx so
    // blocking, contention, and final error behavior remain unchanged.
    if try_lock_exclusive(file).is_ok() {
        return Ok(());
    }

    lock_file(file, LOCKFILE_EXCLUSIVE_LOCK)
}

fn try_lock_exclusive(file: &File) -> Result<()> {
    // LockFile is the synchronous, nonblocking exclusive API. It avoids allocating an
    // event and keeps OVERLAPPED state out of this immediate path.
    let ret = unsafe {
        // SAFETY: The file handle remains valid for the duration of the call, and the
        // offset and length describe the same byte range used by lock_file.
        LockFile(file.as_raw_handle(), 0, 0, u32::MAX, u32::MAX)
    };
    win32_bool_result(ret)
}

pub(crate) fn unlock(file: &File) -> Result<()> {
    let ret = unsafe {
        // SAFETY: `file` owns a valid handle for the duration of this call.
        UnlockFile(file.as_raw_handle(), 0, 0, u32::MAX, u32::MAX)
    };
    win32_bool_result(ret)
}

pub(crate) fn lock_error() -> Error {
    Error::from_raw_os_error(ERROR_LOCK_VIOLATION as i32)
}

fn lock_file(file: &File, flags: u32) -> Result<()> {
    let mut overlapped = PrivateOverlapped::new()?;
    let handle = file.as_raw_handle();
    let ret = unsafe {
        // SAFETY: `file` owns a valid handle and `overlapped` is a valid zeroed structure.
        LockFileEx(handle, flags, 0, u32::MAX, u32::MAX, overlapped.state_mut())
    };
    if ret != 0 {
        return Ok(());
    }

    let err = Error::last_os_error();
    if err.raw_os_error() != Some(ERROR_IO_PENDING as i32) {
        return Err(err);
    }

    let mut bytes_transferred = 0;
    let ret = unsafe {
        // SAFETY: `overlapped` stays alive until the pending lock completes.
        GetOverlappedResult(handle, overlapped.state(), &mut bytes_transferred, 1)
    };
    win32_bool_result(ret)
}
