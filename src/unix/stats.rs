use std::ffi::{CStr, CString};
use std::io::{Error, Result};
use std::mem::MaybeUninit;
use std::path::Path;

#[cfg(not(target_vendor = "apple"))]
use crate::stats::invalid_stats;
use crate::stats::{FilesystemCounters, SpaceKind};

use super::path::with_c_path;

#[cfg(all(target_os = "linux", target_pointer_width = "64", target_env = "gnu"))]
const INVALID_FRAGMENT_SIZE: &str = "filesystem returned a negative fragment size";
#[cfg(all(
    not(all(target_os = "linux", target_pointer_width = "64", target_env = "gnu")),
    not(target_vendor = "apple")
))]
const INVALID_FRAGMENT_SIZE: &str = "filesystem returned an invalid fragment size";
#[cfg(all(
    not(all(target_os = "linux", target_pointer_width = "64")),
    not(target_vendor = "apple")
))]
const INVALID_FREE_BLOCKS: &str = "filesystem returned an invalid free-block count";
#[cfg(all(
    not(all(target_os = "linux", target_pointer_width = "64")),
    not(target_vendor = "apple")
))]
const INVALID_AVAILABLE_BLOCKS: &str = "filesystem returned an invalid available-block count";
#[cfg(all(
    not(all(target_os = "linux", target_pointer_width = "64")),
    not(target_vendor = "apple")
))]
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
    filesystem_counters_from_statfs(&stat)
}

#[cfg(all(target_os = "linux", target_pointer_width = "64"))]
fn filesystem_counters_from_statfs(stat: &libc::statfs) -> Result<FilesystemCounters> {
    #[cfg(target_env = "gnu")]
    let fragment_size = signed_filesystem_value(stat.f_frsize, INVALID_FRAGMENT_SIZE)?;
    #[cfg(not(target_env = "gnu"))]
    let fragment_size = filesystem_value(stat.f_frsize, INVALID_FRAGMENT_SIZE)?;
    #[cfg(target_env = "gnu")]
    let block_size =
        signed_filesystem_value(stat.f_bsize, "filesystem returned a negative block size")?;
    #[cfg(not(target_env = "gnu"))]
    let block_size = filesystem_value(stat.f_bsize, "filesystem returned an invalid block size")?;
    Ok(FilesystemCounters::unix_blocks(
        linux_allocation_granularity(fragment_size, block_size),
        unsigned_filesystem_value(stat.f_bfree),
        unsigned_filesystem_value(stat.f_bavail),
        unsigned_filesystem_value(stat.f_blocks),
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
    filesystem_counters_from_statvfs(&stat)
}

#[cfg(all(
    not(all(target_os = "linux", target_pointer_width = "64")),
    target_vendor = "apple"
))]
fn filesystem_counters_from_statvfs(stat: &libc::statvfs) -> Result<FilesystemCounters> {
    Ok(FilesystemCounters::unix_blocks(
        unsigned_filesystem_value(stat.f_frsize),
        unsigned_filesystem_value(stat.f_bfree),
        unsigned_filesystem_value(stat.f_bavail),
        unsigned_filesystem_value(stat.f_blocks),
    ))
}

#[cfg(all(
    not(all(target_os = "linux", target_pointer_width = "64")),
    not(target_vendor = "apple")
))]
fn filesystem_counters_from_statvfs(stat: &libc::statvfs) -> Result<FilesystemCounters> {
    Ok(FilesystemCounters::unix_blocks(
        filesystem_value(stat.f_frsize, INVALID_FRAGMENT_SIZE)?,
        filesystem_value(stat.f_bfree, INVALID_FREE_BLOCKS)?,
        filesystem_value(stat.f_bavail, INVALID_AVAILABLE_BLOCKS)?,
        filesystem_value(stat.f_blocks, INVALID_TOTAL_BLOCKS)?,
    ))
}

#[cfg(any(
    all(target_os = "linux", target_pointer_width = "64"),
    target_vendor = "apple"
))]
#[inline(always)]
fn unsigned_filesystem_value<T>(value: T) -> u64
where
    u64: From<T>,
{
    u64::from(value)
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
    space_from_counters(statvfs(path), kind)
}

#[inline(always)]
fn space_from_counters(counters: Result<FilesystemCounters>, kind: SpaceKind) -> Result<u64> {
    counters?.space(kind)
}

#[cfg(all(
    not(all(target_os = "linux", target_pointer_width = "64", target_env = "gnu")),
    not(target_vendor = "apple")
))]
fn filesystem_value<T>(value: T, message: &'static str) -> Result<u64>
where
    T: TryInto<u64>,
{
    value.try_into().map_err(|_| invalid_stats(message))
}

#[cfg(all(target_os = "linux", target_pointer_width = "64", target_env = "gnu"))]
fn signed_filesystem_value(value: i64, message: &'static str) -> Result<u64> {
    match value.try_into() {
        Ok(value) => Ok(value),
        Err(_) => Err(invalid_stats(message)),
    }
}

#[cfg(test)]
mod test {
    use super::super::path::SMALL_PATH_BUFFER_SIZE;
    #[cfg(all(
        not(all(target_os = "linux", target_pointer_width = "64", target_env = "gnu")),
        not(target_vendor = "apple")
    ))]
    use super::filesystem_value;
    #[cfg(all(target_os = "linux", target_pointer_width = "64"))]
    use super::linux_allocation_granularity;
    #[cfg(all(target_os = "linux", target_pointer_width = "64", target_env = "gnu"))]
    use super::{filesystem_counters_from_statfs, signed_filesystem_value};
    use super::{space, space_from_counters, statvfs};
    use crate::stats::SpaceKind;
    use std::ffi::OsStr;
    use std::io::ErrorKind;
    use std::os::unix::ffi::OsStrExt;
    use tempfile::tempdir;

    #[test]
    fn missing_stats_path_reports_not_found() {
        let tempdir = tempdir().unwrap();
        let missing = tempdir.path().join("missing");
        let error = statvfs(&missing).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::NotFound);

        for kind in [
            SpaceKind::Free,
            SpaceKind::Available,
            SpaceKind::Total,
            SpaceKind::AllocationGranularity,
        ] {
            assert_eq!(
                space(&missing, kind).unwrap_err().kind(),
                ErrorKind::NotFound
            );
            assert!(space_from_counters(statvfs(tempdir.path()), kind).is_ok());
        }

        let mut bytes = vec![b'a'; SMALL_PATH_BUFFER_SIZE];
        bytes[SMALL_PATH_BUFFER_SIZE / 2] = 0;
        let invalid = std::path::Path::new(OsStr::from_bytes(&bytes));
        assert_eq!(
            statvfs(invalid).unwrap_err().kind(),
            ErrorKind::InvalidInput
        );
    }

    #[test]
    #[cfg(all(
        not(all(target_os = "linux", target_pointer_width = "64", target_env = "gnu")),
        not(target_vendor = "apple")
    ))]
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

    #[cfg(all(target_os = "linux", target_pointer_width = "64", target_env = "gnu"))]
    #[test]
    fn rejects_invalid_signed_filesystem_values() {
        assert_eq!(signed_filesystem_value(0, "negative value").unwrap(), 0);
        assert_eq!(
            signed_filesystem_value(4096, "negative value").unwrap(),
            4096
        );
        assert!(signed_filesystem_value(-1, "negative value").is_err());
    }

    #[cfg(all(target_os = "linux", target_pointer_width = "64", target_env = "gnu"))]
    #[test]
    fn rejects_each_invalid_signed_native_counter() {
        // SAFETY: Linux statfs contains integer fields and accepts all-zero storage.
        let mut stat: libc::statfs = unsafe { std::mem::zeroed() };
        stat.f_frsize = 4096;
        stat.f_bsize = 4096;
        stat.f_bfree = 1;
        stat.f_bavail = 1;
        stat.f_blocks = 1;
        assert!(filesystem_counters_from_statfs(&stat).is_ok());

        stat.f_frsize = -1;
        assert_eq!(
            filesystem_counters_from_statfs(&stat).unwrap_err().kind(),
            ErrorKind::InvalidData
        );
        stat.f_frsize = 4096;

        stat.f_bsize = -1;
        assert_eq!(
            filesystem_counters_from_statfs(&stat).unwrap_err().kind(),
            ErrorKind::InvalidData
        );
        stat.f_bsize = 4096;

        stat.f_bfree = u64::MAX;
        stat.f_bavail = u64::MAX;
        stat.f_blocks = u64::MAX;
        assert!(filesystem_counters_from_statfs(&stat).is_ok());
    }
}
