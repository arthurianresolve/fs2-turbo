use std::fs::File;
use std::io::Error;
use std::os::windows::io::AsRawHandle;

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, WAIT_TIMEOUT};
use windows_sys::Win32::System::IO::{
    CreateIoCompletionPort, GetQueuedCompletionStatus, OVERLAPPED,
};

#[cfg(test)]
#[path = "tests/stats/mod.rs"]
mod stats;

#[cfg(test)]
#[path = "tests/lock.rs"]
mod lock;

#[cfg(test)]
#[path = "tests/allocation.rs"]
mod allocation;

struct CompletionPort(HANDLE);

impl CompletionPort {
    fn associate(file: &File) -> Self {
        let handle = unsafe {
            // SAFETY: `file` owns an overlapped-capable handle, and null asks
            // Windows to create a new completion port for that handle.
            CreateIoCompletionPort(file.as_raw_handle(), std::ptr::null_mut(), 0, 1)
        };
        assert!(!handle.is_null(), "{}", Error::last_os_error());
        Self(handle)
    }

    fn assert_empty(&self) {
        let mut bytes_transferred = 0;
        let mut completion_key = 0;
        let mut overlapped: *mut OVERLAPPED = std::ptr::null_mut();
        let result = unsafe {
            // SAFETY: all output pointers are valid for the call, and this
            // value owns a live completion-port handle.
            GetQueuedCompletionStatus(
                self.0,
                &mut bytes_transferred,
                &mut completion_key,
                &mut overlapped,
                100,
            )
        };
        let error = Error::last_os_error();
        assert_eq!(result, 0, "unexpected private completion packet");
        assert!(overlapped.is_null(), "unexpected private completion packet");
        assert_eq!(error.raw_os_error(), Some(WAIT_TIMEOUT as i32));
    }
}

impl Drop for CompletionPort {
    fn drop(&mut self) {
        unsafe {
            // SAFETY: this value exclusively owns the completion-port handle.
            CloseHandle(self.0);
        }
    }
}
