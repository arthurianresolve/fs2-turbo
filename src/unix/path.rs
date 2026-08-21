use std::ffi::{CStr, CString};
use std::io::{Error, ErrorKind, Result};
use std::mem::MaybeUninit;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::slice;

// Keep common paths on the stack; longer paths use a checked CString.
pub(crate) const SMALL_PATH_BUFFER_SIZE: usize = 3584;

pub(crate) fn with_c_path<T>(path: &Path, query: impl FnOnce(&CStr) -> Result<T>) -> Result<T> {
    let bytes = path.as_os_str().as_bytes();
    if bytes.len() >= SMALL_PATH_BUFFER_SIZE {
        let path = CString::new(bytes)
            .map_err(|_| Error::new(ErrorKind::InvalidInput, "path contained a null"))?;
        return query(&path);
    }
    let mut buffer = MaybeUninit::<[u8; SMALL_PATH_BUFFER_SIZE]>::uninit();
    let buffer_ptr = buffer.as_mut_ptr().cast::<u8>();

    // SAFETY: `bytes.len() < SMALL_PATH_BUFFER_SIZE`, so the buffer has room
    // for the bytes and their trailing null. The initialized prefix remains
    // valid for the duration of `query`.
    unsafe {
        for (i, b) in bytes.iter().copied().enumerate() {
            if b == 0 {
                return Err(Error::new(ErrorKind::InvalidInput, "path contained a null"));
            }
            buffer_ptr.add(i).write(b);
        }
        buffer_ptr.add(bytes.len()).write(0);
    }
    // SAFETY: the preceding writes initialized exactly `bytes.len() + 1`
    // bytes, including the trailing null.
    let bytes_with_null = unsafe { slice::from_raw_parts(buffer_ptr, bytes.len() + 1) };
    // SAFETY: bytes were explicitly checked for embedded nul and this function wrote
    // exactly one trailing nul byte.
    let path = unsafe { CStr::from_bytes_with_nul_unchecked(bytes_with_null) };
    query(path)
}

#[cfg(test)]
mod test {
    use std::ffi::OsStr;
    use std::io::ErrorKind;
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;

    use super::{SMALL_PATH_BUFFER_SIZE, with_c_path};

    #[test]
    fn converts_paths_at_the_stack_buffer_boundary() {
        for length in [
            0,
            SMALL_PATH_BUFFER_SIZE - 1,
            SMALL_PATH_BUFFER_SIZE,
            SMALL_PATH_BUFFER_SIZE + 1,
        ] {
            let bytes = vec![b'a'; length];
            let path = Path::new(OsStr::from_bytes(&bytes));

            with_c_path(path, |path| {
                assert_eq!(path.to_bytes(), bytes);
                Ok(())
            })
            .unwrap();
        }
    }

    #[test]
    fn rejects_nulls_on_both_path_conversion_branches() {
        for length in [SMALL_PATH_BUFFER_SIZE - 1, SMALL_PATH_BUFFER_SIZE] {
            let mut bytes = vec![b'a'; length];
            bytes[length / 2] = 0;
            let path = Path::new(OsStr::from_bytes(&bytes));

            let error = with_c_path(path, |_| -> Result<(), std::io::Error> {
                panic!("query called with an invalid path")
            })
            .unwrap_err();
            assert_eq!(error.kind(), ErrorKind::InvalidInput);
        }
    }
}
