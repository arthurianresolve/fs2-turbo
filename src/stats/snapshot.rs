use std::io::Result;

use super::FilesystemCounters;

/// A consistent filesystem statistics snapshot.
///
/// Obtain one snapshot with [`crate::statvfs`] when more than one counter is
/// needed. The individual convenience functions each acquire their own
/// snapshot. On Windows, `total_space` reports physical volume capacity when
/// the modern provider is available; the legacy fallback may report a
/// quota-limited total for the calling user.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FsStats {
    free_space: u64,
    available_space: u64,
    total_space: u64,
    allocation_granularity: u64,
}

impl FsStats {
    pub(crate) fn from_counters(counters: FilesystemCounters) -> Result<Self> {
        counters.into_stats()
    }

    #[inline]
    pub(super) const fn from_parts(
        free_space: u64,
        available_space: u64,
        total_space: u64,
        allocation_granularity: u64,
    ) -> Self {
        Self {
            free_space,
            available_space,
            total_space,
            allocation_granularity,
        }
    }

    /// Returns the number of free bytes in the file system containing the provided path.
    #[inline]
    pub fn free_space(&self) -> u64 {
        self.free_space
    }

    /// Returns the available space in bytes to non-privileged users in the file system containing the provided path.
    #[inline]
    pub fn available_space(&self) -> u64 {
        self.available_space
    }

    /// Returns the total space in bytes in the file system containing the provided path.
    ///
    /// On Windows, this is the physical volume capacity when the modern
    /// provider is available; the legacy fallback may be quota-limited.
    #[inline]
    pub fn total_space(&self) -> u64 {
        self.total_space
    }

    /// Returns the filesystem's disk space allocation granularity in bytes.
    /// The provided path may be for any file in the filesystem.
    ///
    /// On Posix, this is equivalent to the filesystem's block size.
    /// On Windows, this is equivalent to the filesystem's cluster size.
    #[inline]
    pub fn allocation_granularity(&self) -> u64 {
        self.allocation_granularity
    }
}
