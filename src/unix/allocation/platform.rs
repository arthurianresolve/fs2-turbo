use cfg_if::cfg_if;

use super::{AllocationState, Error, ErrorKind, File, Result};

cfg_if! {
    if #[cfg(any(
        all(target_os = "linux", not(target_env = "uclibc")),
        target_os = "freebsd",
        target_os = "android",
        target_os = "emscripten",
    ))] {
        mod allocation_impl {
            use super::{AllocationState, Error, ErrorKind, File, Result};
            use std::os::unix::io::AsRawFd;

            pub(crate) const ALLOCATE_SPACE_EXTENDS_LENGTH: bool = true;
            // Aggregate st_blocks cannot prove that a particular sparse range
            // is covered, so every nonempty request reaches posix_fallocate.
            pub(crate) const ALWAYS_RESERVE_RANGE: bool = true;

            pub(crate) fn allocate_space(
                file: &File,
                _state: AllocationState,
                len: u64,
            ) -> Result<()> {
                let len = cast_allocate_length(len)?;
                let ret = allocate_with_fallocate(file, len);
                if ret == 0 {
                    Ok(())
                } else {
                    Err(Error::from_raw_os_error(ret))
                }
            }

            #[cfg(all(target_os = "linux", target_pointer_width = "32"))]
            type AllocateLength = libc::off64_t;

            #[cfg(not(all(target_os = "linux", target_pointer_width = "32")))]
            type AllocateLength = libc::off_t;

            fn cast_allocate_length(len: u64) -> Result<AllocateLength> {
                len.try_into().map_err(|_| {
                    Error::new(ErrorKind::InvalidInput, "allocation length is too large")
                })
            }

            #[cfg(all(target_os = "linux", target_pointer_width = "32"))]
            #[inline]
            fn allocate_with_fallocate(file: &File, len: AllocateLength) -> libc::c_int {
                unsafe {
                    // SAFETY: `file` owns a valid descriptor and `len` fits the platform ABI type.
                    libc::posix_fallocate64(file.as_raw_fd(), 0, len)
                }
            }

            #[cfg(not(all(target_os = "linux", target_pointer_width = "32")))]
            #[inline]
            fn allocate_with_fallocate(file: &File, len: AllocateLength) -> libc::c_int {
                unsafe {
                    // SAFETY: `file` owns a valid descriptor and `len` fits the platform ABI type.
                    libc::posix_fallocate(file.as_raw_fd(), 0, len)
                }
            }
        }
    } else if #[cfg(any(target_os = "macos", target_os = "ios"))] {
        mod allocation_impl {
            use super::{AllocationState, Error, ErrorKind, File, Result};
            use std::os::unix::io::AsRawFd;

            pub(crate) const ALLOCATE_SPACE_EXTENDS_LENGTH: bool = false;
            pub(crate) const ALWAYS_RESERVE_RANGE: bool = false;

            pub(crate) fn allocate_space(file: &File, state: AllocationState, len: u64) -> Result<()> {
                allocate_space_with_state(state, len, |fstore| unsafe {
                    preallocate(file, fstore)
                })
            }

            fn allocate_space_with_state(
                state: AllocationState,
                len: u64,
                mut preallocate: impl FnMut(&mut libc::fstore_t) -> libc::c_int,
            ) -> Result<()> {
                if len <= state.allocated_size {
                    return Ok(());
                }

                let len = libc::off_t::try_from(len).map_err(|_| {
                    Error::new(ErrorKind::InvalidInput, "allocation length is too large")
                })?;
                let mut fstore = libc::fstore_t {
                    fst_flags: libc::F_ALLOCATECONTIG,
                    fst_posmode: libc::F_PEOFPOSMODE,
                    fst_offset: 0,
                    fst_length: len,
                    fst_bytesalloc: 0,
                };

                let mut ret = preallocate(&mut fstore);
                if ret == -1 {
                    fstore.fst_flags = libc::F_ALLOCATEALL;
                    ret = preallocate(&mut fstore);
                }
                if ret == -1 {
                    Err(Error::last_os_error())
                } else {
                    Ok(())
                }
            }

            #[cfg(test)]
            pub(crate) fn allocate_space_with<F>(
                file: &File,
                len: u64,
                preallocate: &mut F,
            ) -> Result<()>
            where
                F: FnMut(&File, &mut libc::fstore_t) -> libc::c_int,
            {
                use std::os::unix::fs::MetadataExt;

                let metadata = file.metadata()?;
                let state = AllocationState {
                    allocated_size: super::super::blocks_to_bytes(metadata.blocks())?,
                    file_size: metadata.len(),
                };
                allocate_space_with_state(state, len, |fstore| preallocate(file, fstore))
            }

            #[inline(always)]
            unsafe fn preallocate(file: &File, fstore: &mut libc::fstore_t) -> libc::c_int {
                // SAFETY: `file` owns a valid descriptor and `fstore` is an exclusive,
                // correctly sized fstore structure for the duration of this call.
                unsafe { libc::fcntl(file.as_raw_fd(), libc::F_PREALLOCATE, fstore as *mut libc::fstore_t) }
            }
        }
    } else {
        mod allocation_impl {
            use super::{AllocationState, Error, ErrorKind, File, Result};

            pub(crate) const ALLOCATE_SPACE_EXTENDS_LENGTH: bool = false;
            // Aggregate st_blocks cannot prove requested-prefix coverage. Force
            // every nonempty request through the fail-closed provider below.
            pub(crate) const ALWAYS_RESERVE_RANGE: bool = true;

            pub(crate) fn allocate_space(
                _file: &File,
                _state: AllocationState,
                _len: u64,
            ) -> Result<()> {
                Err(Error::new(
                    ErrorKind::Unsupported,
                    "physical file allocation is unavailable on this platform",
                ))
            }
        }
    }
}

#[cfg(all(test, target_os = "macos"))]
pub(crate) use allocation_impl::allocate_space_with;
pub(crate) use allocation_impl::{
    ALLOCATE_SPACE_EXTENDS_LENGTH, ALWAYS_RESERVE_RANGE, allocate_space,
};
