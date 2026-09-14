mod counters;
mod query;
mod snapshot;
mod validation;

#[cfg(test)]
mod tests;

use std::io::{Error, ErrorKind, Result};
use std::path::Path;

use crate::sys;

pub(crate) use counters::FilesystemCounters;
pub use query::FsStatsQuery;
pub use snapshot::FsStats;

#[cold]
#[inline(never)]
pub(crate) fn invalid_stats(message: &'static str) -> Error {
    Error::new(ErrorKind::InvalidData, message)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SpaceKind {
    Free,
    Available,
    Total,
    AllocationGranularity,
}

/// Gets one statistics snapshot for the filesystem containing `path`.
pub fn statvfs<P: AsRef<Path>>(path: P) -> Result<FsStats> {
    sys::statvfs(path.as_ref()).and_then(FilesystemCounters::into_stats)
}

/// Returns free space from a newly acquired filesystem snapshot.
///
/// Call [`statvfs`] once and use [`FsStats::free_space`] when multiple counters
/// are needed.
pub fn free_space<P: AsRef<Path>>(path: P) -> Result<u64> {
    sys::space(path.as_ref(), SpaceKind::Free)
}

/// Returns available space from a newly acquired filesystem snapshot.
///
/// Call [`statvfs`] once and use [`FsStats::available_space`] when multiple
/// counters are needed.
pub fn available_space<P: AsRef<Path>>(path: P) -> Result<u64> {
    sys::space(path.as_ref(), SpaceKind::Available)
}

/// Returns total space from a newly acquired filesystem snapshot.
///
/// Call [`statvfs`] once and use [`FsStats::total_space`] when multiple counters
/// are needed.
pub fn total_space<P: AsRef<Path>>(path: P) -> Result<u64> {
    sys::space(path.as_ref(), SpaceKind::Total)
}

/// Returns allocation granularity from a newly acquired filesystem snapshot.
///
/// Call [`statvfs`] once and use [`FsStats::allocation_granularity`] when
/// multiple counters are needed.
pub fn allocation_granularity<P: AsRef<Path>>(path: P) -> Result<u64> {
    sys::space(path.as_ref(), SpaceKind::AllocationGranularity)
}
