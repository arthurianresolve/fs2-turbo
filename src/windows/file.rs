use std::fs::File;
use std::io::Result;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};

use windows_sys::Win32::Foundation::{DUPLICATE_SAME_ACCESS, DuplicateHandle};
use windows_sys::Win32::System::Threading::GetCurrentProcess;

use crate::windows::path::win32_bool_result;

#[inline]
pub(crate) fn duplicate(file: &File) -> Result<File> {
    let mut duplicate = std::ptr::null_mut();
    let process = unsafe {
        // SAFETY: `GetCurrentProcess` returns the calling process's pseudo-handle.
        GetCurrentProcess()
    };
    let result = unsafe {
        // SAFETY: `file` owns a valid handle, `process` identifies the calling
        // process, and `duplicate` is writable output storage. On success the
        // returned handle is newly owned by the caller.
        DuplicateHandle(
            process,
            file.as_raw_handle(),
            process,
            &mut duplicate,
            0,
            windows_sys::Win32::Foundation::TRUE,
            DUPLICATE_SAME_ACCESS,
        )
    };
    duplicate_result(result, duplicate)
}

#[inline(always)]
fn duplicate_result(result: i32, duplicate: RawHandle) -> Result<File> {
    win32_bool_result(result)?;
    let owned = unsafe { OwnedHandle::from_raw_handle(duplicate) };
    Ok(File::from(owned))
}

#[cfg(test)]
mod tests {
    use windows_sys::Win32::Foundation::{ERROR_INVALID_HANDLE, SetLastError};

    #[test]
    fn duplication_propagates_native_failure() {
        unsafe {
            // SAFETY: last-error state is thread-local and no handle is transferred.
            SetLastError(ERROR_INVALID_HANDLE);
        }
        let error = super::duplicate_result(0, std::ptr::null_mut()).unwrap_err();
        assert_eq!(error.raw_os_error(), Some(ERROR_INVALID_HANDLE as i32));
    }
}
