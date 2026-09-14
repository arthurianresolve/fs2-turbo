use std::io::ErrorKind;

use tempfile::tempdir;

use super::{FilesystemCounters, FsStats, FsStatsQuery, statvfs};

fn counters(
    allocation_granularity: u64,
    free_space: u64,
    available_space: u64,
    total_space: u64,
) -> FilesystemCounters {
    #[cfg(unix)]
    return FilesystemCounters::unix_blocks(
        allocation_granularity,
        free_space,
        available_space,
        total_space,
    );

    #[cfg(windows)]
    return FilesystemCounters::windows_modern_bytes(
        allocation_granularity,
        free_space,
        available_space,
        total_space,
    );
}

#[cfg(windows)]
fn legacy_counters(
    allocation_granularity: u64,
    free_space: u64,
    available_space: u64,
    total_space: u64,
) -> FilesystemCounters {
    FilesystemCounters::windows_legacy_bytes(
        allocation_granularity,
        free_space,
        available_space,
        total_space,
    )
}

#[cfg(unix)]
#[test]
fn constructs_stats_from_block_counts() {
    let stats = FsStats::from_counters(counters(4096, 8, 6, 10)).unwrap();
    assert_eq!(stats.free_space(), 32_768);
    assert_eq!(stats.available_space(), 24_576);
    assert_eq!(stats.total_space(), 40_960);
    assert_eq!(stats.allocation_granularity(), 4096);
}

#[cfg(windows)]
#[test]
fn constructs_stats_from_bytes() {
    let stats = FsStats::from_counters(counters(4096, 32_768, 24_576, 40_960)).unwrap();
    assert_eq!(stats.free_space(), 32_768);
    assert_eq!(stats.available_space(), 24_576);
    assert_eq!(stats.total_space(), 40_960);
    assert_eq!(stats.allocation_granularity(), 4096);
}

#[test]
fn rejects_zero_granularity() {
    let error = FsStats::from_counters(counters(0, 1, 1, 1)).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::InvalidData);
}

#[cfg(unix)]
#[test]
fn rejects_space_overflow() {
    let error = FsStats::from_counters(counters(u64::MAX, 2, 1, 2)).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::InvalidData);
}

#[cfg(windows)]
#[test]
fn rejects_available_space_above_free_space() {
    let error = FsStats::from_counters(counters(4096, 32_768, 36_864, 40_960)).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::InvalidData);
}

#[cfg(unix)]
#[test]
fn rejects_available_space_above_free_space() {
    let error = FsStats::from_counters(counters(4096, 8, 9, 10)).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::InvalidData);
}

#[cfg(unix)]
#[test]
fn accepts_available_space_equal_to_free_space() {
    let stats = FsStats::from_counters(counters(4096, 8, 8, 10)).unwrap();
    assert_eq!(stats.available_space(), stats.free_space());
}

#[cfg(unix)]
#[test]
fn rejects_available_space_above_total_space() {
    let error = FsStats::from_counters(counters(4096, 8, 11, 10)).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::InvalidData);
}

#[test]
fn filesystem_space() {
    let tempdir = tempdir().unwrap();
    let stats = statvfs(tempdir.path()).unwrap();
    assert!(stats.total_space() > 0);
    #[cfg(unix)]
    {
        assert!(stats.free_space() <= stats.total_space());
        assert!(stats.available_space() <= stats.total_space());
    }
    #[cfg(windows)]
    assert!(stats.available_space() <= stats.free_space());
}

#[test]
fn prepared_query_returns_fresh_valid_snapshots() {
    let tempdir = tempdir().unwrap();
    let query = FsStatsQuery::new(tempdir.path()).unwrap();
    let pathbuf = tempdir.path().to_path_buf();
    let pathbuf_query = FsStatsQuery::new(pathbuf).unwrap();

    for query in [&query, &pathbuf_query] {
        for stats in [query.snapshot().unwrap(), query.snapshot().unwrap()] {
            assert!(stats.total_space() > 0);
            assert!(stats.allocation_granularity() > 0);
            #[cfg(unix)]
            {
                assert!(stats.free_space() <= stats.total_space());
                assert!(stats.available_space() <= stats.total_space());
            }
            #[cfg(windows)]
            assert!(stats.available_space() <= stats.free_space());
        }
    }
}

#[cfg(windows)]
#[test]
fn rejects_modern_free_space_above_physical_total() {
    let error = FsStats::from_counters(counters(4096, 50_000, 10_000, 40_000)).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::InvalidData);
}

#[cfg(windows)]
#[test]
fn accepts_quota_limited_legacy_total_space() {
    let stats = FsStats::from_counters(legacy_counters(4096, 50_000, 10_000, 40_000)).unwrap();
    assert_eq!(stats.free_space(), 50_000);
    assert_eq!(stats.available_space(), 10_000);
    assert_eq!(stats.total_space(), 40_000);
}

#[cfg(windows)]
#[test]
fn rejects_legacy_available_space_above_caller_total() {
    assert!(FsStats::from_counters(legacy_counters(4096, 50_000, 45_000, 40_000)).is_err());
}

#[cfg(windows)]
#[test]
fn rejects_invalid_legacy_available_space() {
    let error = FsStats::from_counters(legacy_counters(4096, 10_000, 20_000, 30_000)).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::InvalidData);
}
