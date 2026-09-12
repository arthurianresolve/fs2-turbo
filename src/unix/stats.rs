use std::ffi::{CStr, CString};
use std::io::{Error, Result};
use std::mem::MaybeUninit;
use std::path::Path;

use crate::stats::{FilesystemCounters, SpaceKind, invalid_stats};

use super::path::with_c_path;

#[cfg(all(target_os = "linux", target_pointer_width = "64"))]
const INVALID_FRAGMENT_SIZE: &str = "filesystem returned a negative fragment size";
#[cfg(not(all(target_os = "linux", target_pointer_width = "64")))]
const INVALID_FRAGMENT_SIZE: &str = "filesystem returned an invalid fragment size";
const INVALID_FREE_BLOCKS: &str = "filesystem returned an invalid free-block count";
const INVALID_AVAILABLE_BLOCKS: &str = "filesystem returned an invalid available-block count";
const INVALID_TOTAL_BLOCKS: &str = "filesystem returned an invalid block count";

#[derive(Debug)]
pub(crate) struct StatsQuery {
    path: CString,
}

impl StatsQuery {
    pub(crate) const fn new(path: CString) -> Self {
        Self { path }
    }

    pub(crate) fn counters(&self) -> Result<FilesystemCounters> {
        statvfs_cstr(&self.path)
    }
}

pub(crate) fn statvfs(path: &Path) -> Result<FilesystemCounters> {
    with_c_path(path, statvfs_cstr)
}

#[cfg(all(target_os = "linux", target_pointer_width = "64"))]
fn statvfs_cstr(path: &CStr) -> Result<FilesystemCounters> {
    let stat = query_stat(MaybeUninit::<libc::statfs>::uninit(), |stat| unsafe {
        // SAFETY: `path` is null-terminated and `stat` points to writable storage
        // large enough for `libc::statfs`.
        libc::statfs(path.as_ptr(), stat)
    })?;
    let fragment_size = filesystem_value(stat.f_frsize, INVALID_FRAGMENT_SIZE)?;
    let block_size = filesystem_value(stat.f_bsize, "filesystem returned a negative block size")?;
    Ok(FilesystemCounters::unix_blocks(
        linux_allocation_granularity(fragment_size, block_size),
        filesystem_value(stat.f_bfree, INVALID_FREE_BLOCKS)?,
        filesystem_value(stat.f_bavail, INVALID_AVAILABLE_BLOCKS)?,
        filesystem_value(stat.f_blocks, INVALID_TOTAL_BLOCKS)?,
    ))
}

#[cfg(all(target_os = "linux", target_pointer_width = "64"))]
fn linux_allocation_granularity(fragment_size: u64, block_size: u64) -> u64 {
    if fragment_size == 0 {
        block_size
    } else {
        fragment_size
    }
}

#[cfg(not(all(target_os = "linux", target_pointer_width = "64")))]
fn statvfs_cstr(path: &CStr) -> Result<FilesystemCounters> {
    let stat = query_stat(MaybeUninit::<libc::statvfs>::uninit(), |stat| unsafe {
        // SAFETY: `path` is null-terminated and `stat` points to writable storage.
        libc::statvfs(path.as_ptr() as *const _, stat)
    })?;
    Ok(FilesystemCounters::unix_blocks(
        filesystem_value(stat.f_frsize, INVALID_FRAGMENT_SIZE)?,
        filesystem_value(stat.f_bfree, INVALID_FREE_BLOCKS)?,
        filesystem_value(stat.f_bavail, INVALID_AVAILABLE_BLOCKS)?,
        filesystem_value(stat.f_blocks, INVALID_TOTAL_BLOCKS)?,
    ))
}

#[inline(always)]
fn query_stat<T>(mut stat: MaybeUninit<T>, query: impl FnOnce(*mut T) -> libc::c_int) -> Result<T> {
    let ret = query(stat.as_mut_ptr());
    if ret != 0 {
        Err(Error::last_os_error())
    } else {
        // SAFETY: a successful filesystem-stat syscall initialized the output.
        Ok(unsafe { stat.assume_init() })
    }
}

pub(crate) fn space(path: &Path, kind: SpaceKind) -> Result<u64> {
    statvfs(path)?.space(kind)
}

fn filesystem_value<T>(value: T, message: &'static str) -> Result<u64>
where
    T: TryInto<u64>,
{
    value.try_into().map_err(|_| invalid_stats(message))
}

#[cfg(test)]
mod test {
    #[cfg(all(target_os = "linux", target_pointer_width = "64"))]
    use super::linux_allocation_granularity;
    use super::{filesystem_value, statvfs};
    use std::io::ErrorKind;
    use tempfile::tempdir;

    #[test]
    fn missing_stats_path_reports_not_found() {
        let tempdir = tempdir().unwrap();
        let error = statvfs(&tempdir.path().join("missing")).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::NotFound);
    }

    #[test]
    fn rejects_invalid_filesystem_values() {
        assert_eq!(filesystem_value(0, "negative value").unwrap(), 0);
        assert_eq!(filesystem_value(4096i64, "negative value").unwrap(), 4096);
        assert!(filesystem_value(-1i64, "negative value").is_err());
        assert_eq!(
            filesystem_value(u64::MAX, "negative value").unwrap(),
            u64::MAX
        );
    }

    #[cfg(all(target_os = "linux", target_pointer_width = "64"))]
    #[test]
    fn uses_filesystem_block_size_when_fragment_size_is_zero() {
        assert_eq!(linux_allocation_granularity(0, 4096), 4096);
        assert_eq!(linux_allocation_granularity(1024, 4096), 1024);
    }
}
