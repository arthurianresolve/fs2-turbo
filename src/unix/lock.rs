use std::fs::File;
use std::io::{Error, Result};
use std::os::unix::io::AsRawFd;

#[cfg(target_os = "solaris")]
use super::solaris::flock as flock_solaris;

#[inline(always)]
pub(crate) fn lock_shared(file: &File, nonblocking: bool) -> Result<()> {
    let flag = libc::LOCK_SH | if nonblocking { libc::LOCK_NB } else { 0 };
    flock(file, flag)
}

#[inline(always)]
pub(crate) fn lock_exclusive(file: &File, nonblocking: bool) -> Result<()> {
    let flag = libc::LOCK_EX | if nonblocking { libc::LOCK_NB } else { 0 };
    flock(file, flag)
}

pub(crate) fn unlock(file: &File) -> Result<()> {
    flock(file, libc::LOCK_UN)
}

pub(crate) fn lock_error() -> Error {
    Error::from_raw_os_error(libc::EWOULDBLOCK)
}

#[cfg(not(target_os = "solaris"))]
fn flock(file: &File, flag: libc::c_int) -> Result<()> {
    let ret = unsafe {
        // SAFETY: `file` owns a valid descriptor for the duration of this call.
        libc::flock(file.as_raw_fd(), flag)
    };
    if ret < 0 {
        Err(Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(target_os = "solaris")]
fn flock(file: &File, flag: libc::c_int) -> Result<()> {
    flock_solaris(file, flag)
}
