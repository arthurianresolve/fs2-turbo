use std::fs::File;
use std::io::{Error, ErrorKind, Result};
#[cfg(all(target_os = "linux", target_pointer_width = "64"))]
use std::mem::MaybeUninit;
#[cfg(not(all(target_os = "linux", target_pointer_width = "64")))]
use std::os::unix::fs::MetadataExt;
#[cfg(all(target_os = "linux", target_pointer_width = "64"))]
use std::os::unix::io::AsRawFd;

use crate::AllocationState;

#[inline(always)]
#[cfg(all(target_os = "linux", target_pointer_width = "64"))]
pub(crate) fn allocation_state(file: &File) -> Result<AllocationState> {
    let mut stat = MaybeUninit::<libc::stat>::uninit();
    let result = unsafe {
        // SAFETY: `stat` points to writable storage and `file` owns a valid
        // descriptor for the duration of this call.
        libc::fstat(file.as_raw_fd(), stat.as_mut_ptr())
    };
    if result < 0 {
        return Err(Error::last_os_error());
    }

    // SAFETY: a successful `fstat` initialized the complete `stat` value.
    let stat = unsafe { stat.assume_init() };
    Ok(AllocationState {
        allocated_size: blocks_to_bytes(i64_to_u64(
            stat.st_blocks,
            "filesystem returned a negative allocated block count",
        )?)?,
        file_size: i64_to_u64(stat.st_size, "filesystem returned a negative file size")?,
    })
}

#[inline(always)]
#[cfg(not(all(target_os = "linux", target_pointer_width = "64")))]
pub(crate) fn allocation_state(file: &File) -> Result<AllocationState> {
    let metadata = file.metadata()?;
    Ok(AllocationState {
        allocated_size: blocks_to_bytes(metadata.blocks())?,
        file_size: metadata.len(),
    })
}

#[inline(always)]
fn blocks_to_bytes(blocks: u64) -> Result<u64> {
    if blocks <= u64::MAX / 512 {
        Ok(blocks << 9)
    } else {
        Err(allocated_size_overflow())
    }
}

#[cfg(all(target_os = "linux", target_pointer_width = "64"))]
#[cold]
#[inline(never)]
fn i64_to_u64(value: i64, message: &'static str) -> Result<u64> {
    value
        .try_into()
        .map_err(|_| Error::new(ErrorKind::InvalidData, message))
}

#[cold]
#[inline(never)]
fn allocated_size_overflow() -> Error {
    Error::new(ErrorKind::InvalidData, "allocated size overflowed")
}

mod platform;

#[cfg(all(test, target_os = "macos"))]
pub(crate) use platform::allocate_space_with;
pub(crate) use platform::{ALLOCATE_SPACE_EXTENDS_LENGTH, ALWAYS_RESERVE_RANGE, allocate_space};

#[cfg(test)]
mod tests;
