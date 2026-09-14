use std::io::{Error, Result};

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::System::IO::OVERLAPPED;
use windows_sys::Win32::System::Threading::CreateEventW;

pub(crate) struct PrivateOverlapped {
    state: OVERLAPPED,
    event: HANDLE,
}

impl PrivateOverlapped {
    pub(crate) fn new() -> Result<Self> {
        let event = unsafe {
            // SAFETY: null security attributes and name request an unnamed,
            // non-inheritable manual-reset event owned by the returned handle.
            CreateEventW(std::ptr::null(), 1, 0, std::ptr::null())
        };
        if event.is_null() {
            return Err(Error::last_os_error());
        }

        // Windows treats the low-order hEvent bit as a request not to enqueue
        // this private operation on a completion port associated with the file.
        // Retain the untagged handle separately because CloseHandle requires it.
        let tagged_event = ((event as usize) | 1) as HANDLE;
        Ok(Self {
            state: OVERLAPPED {
                hEvent: tagged_event,
                ..OVERLAPPED::default()
            },
            event,
        })
    }

    pub(crate) fn state(&self) -> &OVERLAPPED {
        &self.state
    }

    pub(crate) fn state_mut(&mut self) -> &mut OVERLAPPED {
        &mut self.state
    }
}

impl Drop for PrivateOverlapped {
    fn drop(&mut self) {
        unsafe {
            // SAFETY: this value exclusively owns the untagged event handle.
            CloseHandle(self.event);
        }
    }
}
