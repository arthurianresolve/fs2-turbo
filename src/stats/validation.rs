use std::io::Result;

use super::counters::FilesystemCounters;
#[cfg(windows)]
use super::counters::WindowsCounterSource;
use super::{FsStats, SpaceKind, invalid_stats};

#[cfg(unix)]
pub(super) struct ValidatedUnixCounters {
    allocation_granularity: u64,
    free_blocks: u64,
    available_blocks: u64,
    total_blocks: u64,
    total_space: u64,
}

#[cfg(unix)]
impl ValidatedUnixCounters {
    pub(super) fn into_stats(self) -> FsStats {
        FsStats::from_parts(
            self.free_space(),
            self.available_space(),
            self.total_space,
            self.allocation_granularity,
        )
    }

    pub(super) fn space(self, kind: SpaceKind) -> u64 {
        match kind {
            SpaceKind::Free => self.free_space(),
            SpaceKind::Available => self.available_space(),
            SpaceKind::Total => self.total_space,
            SpaceKind::AllocationGranularity => self.allocation_granularity,
        }
    }

    #[inline]
    fn free_space(&self) -> u64 {
        debug_assert!(self.free_blocks <= self.total_blocks);
        self.allocation_granularity * self.free_blocks
    }

    #[inline]
    fn available_space(&self) -> u64 {
        debug_assert!(self.available_blocks <= self.total_blocks);
        self.allocation_granularity * self.available_blocks
    }
}

#[cfg(unix)]
pub(super) fn validate_unix_counters(
    counters: FilesystemCounters,
) -> Result<ValidatedUnixCounters> {
    let allocation_granularity = validate_granularity(counters.allocation_granularity)?;
    if counters.free_blocks > counters.total_blocks {
        return Err(invalid_stats("filesystem free space exceeds total space"));
    }
    if counters.available_blocks > counters.free_blocks {
        return Err(invalid_stats(
            "filesystem available space exceeds free space",
        ));
    }
    if counters.available_blocks > counters.total_blocks {
        return Err(invalid_stats(
            "filesystem available space exceeds total space",
        ));
    }
    let total_space = allocation_granularity
        .checked_mul(counters.total_blocks)
        .ok_or_else(|| invalid_stats("filesystem space calculation overflowed"))?;
    Ok(ValidatedUnixCounters {
        allocation_granularity,
        free_blocks: counters.free_blocks,
        available_blocks: counters.available_blocks,
        total_blocks: counters.total_blocks,
        total_space,
    })
}

#[cfg(windows)]
pub(super) struct ValidatedWindowsCounters(FilesystemCounters);

#[cfg(windows)]
impl ValidatedWindowsCounters {
    pub(super) fn into_stats(self) -> FsStats {
        FsStats::from_parts(
            self.0.free_space,
            self.0.available_space,
            self.0.total_space,
            self.0.allocation_granularity,
        )
    }

    pub(super) fn space(self, kind: SpaceKind) -> u64 {
        match kind {
            SpaceKind::Free => self.0.free_space,
            SpaceKind::Available => self.0.available_space,
            SpaceKind::Total => self.0.total_space,
            SpaceKind::AllocationGranularity => self.0.allocation_granularity,
        }
    }
}

#[cfg(windows)]
pub(super) fn validate_windows_counters(
    counters: FilesystemCounters,
) -> Result<ValidatedWindowsCounters> {
    validate_granularity(counters.allocation_granularity)?;
    if counters.available_space > counters.free_space {
        return Err(invalid_stats(
            "filesystem available space exceeds free space",
        ));
    }
    if counters.available_space > counters.total_space {
        return Err(invalid_stats(
            "filesystem available space exceeds total space",
        ));
    }
    if counters.source == WindowsCounterSource::Modern && counters.free_space > counters.total_space
    {
        return Err(invalid_stats("filesystem free space exceeds total space"));
    }
    Ok(ValidatedWindowsCounters(counters))
}

fn validate_granularity(allocation_granularity: u64) -> Result<u64> {
    if allocation_granularity == 0 {
        Err(invalid_stats("filesystem allocation granularity is zero"))
    } else {
        Ok(allocation_granularity)
    }
}
