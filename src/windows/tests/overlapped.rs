use std::io::Error;

use windows_sys::Win32::Foundation::{
    ERROR_NOT_ENOUGH_MEMORY, GetHandleInformation, HANDLE, HANDLE_FLAG_INHERIT, SetLastError,
};

use crate::windows::overlapped::PrivateOverlapped;

#[test]
fn event_creation_failure_preserves_native_error() {
    // SAFETY: this only sets the calling thread's native error slot.
    unsafe { SetLastError(ERROR_NOT_ENOUGH_MEMORY) };
    // SAFETY: null transfers no event ownership and represents creation failure.
    let result = unsafe { PrivateOverlapped::from_event(std::ptr::null_mut()) };
    let error = match result {
        Err(error) => error,
        Ok(_) => panic!("null event accepted"),
    };
    assert_eq!(error.raw_os_error(), Some(ERROR_NOT_ENOUGH_MEMORY as i32));
}

#[test]
fn private_event_is_tagged_and_not_inheritable() {
    let state = PrivateOverlapped::new().unwrap();
    let tagged = state.state().hEvent as usize;
    assert_eq!(tagged & 1, 1);
    let event = (tagged & !1) as HANDLE;
    let mut flags = 0;
    // SAFETY: state owns the untagged event for this entire query.
    let result = unsafe { GetHandleInformation(event, &mut flags) };
    assert_ne!(result, 0, "{}", Error::last_os_error());
    assert_eq!(flags & HANDLE_FLAG_INHERIT, 0);
}
