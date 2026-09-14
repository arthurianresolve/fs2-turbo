use std::fs::File;
use std::io::Result;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};

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
    win32_bool_result(result)?;
    let owned = unsafe { OwnedHandle::from_raw_handle(duplicate) };
    Ok(File::from(owned))
}
