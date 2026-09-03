mod counters;
mod validation;

use std::io::{Error, ErrorKind, Result};
use std::path::Path;

pub(crate) use self::counters::FilesystemCounters;
pub(crate) use crate::FsStats;

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
pub(crate) fn statvfs<P: AsRef<Path>>(path: P) -> Result<FsStats> {
    crate::modular_sys::statvfs(path.as_ref()).and_then(FilesystemCounters::into_stats)
}
pub(crate) fn free_space<P: AsRef<Path>>(path: P) -> Result<u64> {
    crate::modular_sys::free_space(path.as_ref())
}
