use std::io::{Error, ErrorKind, Result};
use std::mem::MaybeUninit;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;

use windows_sys::Win32::Storage::FileSystem::GetVolumePathNameW;

pub(crate) const VOLUME_PATH_CAPACITY: usize = 261;

#[repr(align(16))]
struct InlineWidePath([MaybeUninit<u16>; VOLUME_PATH_CAPACITY]);

#[inline]
pub(crate) fn with_wide_path<T>(
    path: &Path,
    operation: impl FnOnce(&[u16]) -> Result<T>,
) -> Result<T> {
    let mut inline = InlineWidePath([MaybeUninit::<u16>::uninit(); VOLUME_PATH_CAPACITY]);
    let mut encoded = path.as_os_str().encode_wide();
    let mut length = 0;
    while let Some(code_unit) = encoded.next() {
        if code_unit == 0 {
            return Err(Error::new(ErrorKind::InvalidInput, "path contained a null"));
        }

        if length == VOLUME_PATH_CAPACITY - 1 {
            // SAFETY: every element before `length` was initialized in earlier iterations.
            let initialized =
                unsafe { std::slice::from_raw_parts(inline.0.as_ptr().cast::<u16>(), length) };
            let mut heap = Vec::with_capacity(path.as_os_str().len().saturating_add(1));
            heap.extend_from_slice(initialized);
            heap.push(code_unit);
            for code_unit in encoded {
                if code_unit == 0 {
                    return Err(Error::new(ErrorKind::InvalidInput, "path contained a null"));
                }
                heap.push(code_unit);
            }
            heap.push(0);
            return operation(&heap);
        }

        inline.0[length].write(code_unit);
        length += 1;
    }
    inline.0[length].write(0);
    // SAFETY: every element through `length` was initialized above.
    let path = unsafe { std::slice::from_raw_parts(inline.0.as_ptr().cast::<u16>(), length + 1) };
    operation(path)
}

#[cfg(test)]
pub(crate) fn wide_path(path: &Path) -> Result<Vec<u16>> {
    let path = path.as_os_str();
    let mut encoded = Vec::with_capacity(path.len().saturating_add(1));
    for code_unit in path.encode_wide() {
        if code_unit == 0 {
            return Err(Error::new(ErrorKind::InvalidInput, "path contained a null"));
        }
        encoded.push(code_unit);
    }
    encoded.push(0);
    Ok(encoded)
}

pub(crate) fn copy_exact_drive_root(
    path: &[u16],
    root_path: &mut [u16; VOLUME_PATH_CAPACITY],
) -> bool {
    let [drive, colon, separator, terminator] = path else {
        return false;
    };
    if !valid_drive_root_components(*drive, *colon, *separator, *terminator) {
        return false;
    }

    root_path[..path.len()].copy_from_slice(path);
    root_path[2] = u16::from(b'\\');
    true
}

pub(crate) fn valid_drive_root_components(
    drive: u16,
    colon: u16,
    separator: u16,
    terminator: u16,
) -> bool {
    let is_uppercase_drive = (u16::from(b'A')..=u16::from(b'Z')).contains(&drive);
    let is_lowercase_drive = (u16::from(b'a')..=u16::from(b'z')).contains(&drive);
    let is_drive_letter = is_uppercase_drive | is_lowercase_drive;
    let is_backslash = separator == u16::from(b'\\');
    let is_forward_slash = separator == u16::from(b'/');
    let is_separator = is_backslash | is_forward_slash;
    is_drive_letter && colon == u16::from(b':') && is_separator && terminator == 0
}

pub(crate) fn volume_path(path: &[u16], volume_path: &mut [u16]) -> Result<()> {
    let ret = unsafe {
        // SAFETY: `path` is null-terminated and `volume_path` is valid output storage.
        GetVolumePathNameW(
            path.as_ptr(),
            volume_path.as_mut_ptr(),
            volume_path.len() as u32,
        )
    };
    win32_bool_result(ret)
}

#[inline]
pub(crate) fn win32_bool_result(result: i32) -> Result<()> {
    if result == 0 {
        Err(Error::last_os_error())
    } else {
        Ok(())
    }
}
