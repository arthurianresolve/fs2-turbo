#[cfg(any(unix, windows))]
use super::FilesystemCounters;
#[cfg(unix)]
use super::SpaceKind;
use super::validate_available_bounds;
#[cfg(unix)]
use super::validate_unix_counters;
#[cfg(windows)]
use super::validate_windows_counters;

#[test]
fn available_bounds_accept_equal_zero_and_separate_counter_domains() {
    for (available, free, total) in [
        (0, 0, 0),
        (4, 4, 4),
        (2, 3, 4),
        (2, 9, 4),
        (u64::MAX, u64::MAX, u64::MAX),
    ] {
        validate_available_bounds(available, free, total).unwrap();
    }
}

#[test]
fn available_bounds_preserve_error_messages_and_precedence() {
    for (available, free, total, expected) in [
        (4, 3, 9, "filesystem available space exceeds free space"),
        (10, 5, 8, "filesystem available space exceeds free space"),
        (5, 9, 4, "filesystem available space exceeds total space"),
    ] {
        let error = validate_available_bounds(available, free, total).unwrap_err();
        assert_eq!(error.to_string(), expected);
        assert_eq!(error.raw_os_error(), None);
    }
}

#[cfg(unix)]
#[test]
fn unix_counters_preserve_scaled_values() {
    let counters = FilesystemCounters::unix_blocks(4, 3, 2, 5);

    for (kind, expected) in [
        (SpaceKind::Free, 12),
        (SpaceKind::Available, 8),
        (SpaceKind::Total, 20),
        (SpaceKind::AllocationGranularity, 4),
    ] {
        assert_eq!(
            validate_unix_counters(counters).unwrap().space(kind),
            expected
        );
    }

    let stats = validate_unix_counters(counters).unwrap().into_stats();
    assert_eq!(stats.free_space(), 12);
    assert_eq!(stats.available_space(), 8);
    assert_eq!(stats.total_space(), 20);
    assert_eq!(stats.allocation_granularity(), 4);
}

#[cfg(unix)]
#[test]
fn unix_free_space_validation_preserves_strict_boundaries() {
    validate_unix_counters(FilesystemCounters::unix_blocks(1, 4, 4, 4)).unwrap();
    validate_unix_counters(FilesystemCounters::unix_blocks(1, 3, 3, 4)).unwrap();

    let error = validate_unix_counters(FilesystemCounters::unix_blocks(1, 5, 4, 4))
        .err()
        .unwrap();
    assert_eq!(
        error.to_string(),
        "filesystem free space exceeds total space"
    );
}

#[cfg(windows)]
#[test]
fn modern_windows_free_space_accepts_total_space_equality() {
    let counters = FilesystemCounters::windows_modern_bytes(4, 20, 20, 20);
    let stats = validate_windows_counters(counters).unwrap().into_stats();

    assert_eq!(stats.free_space(), 20);
    assert_eq!(stats.available_space(), 20);
    assert_eq!(stats.total_space(), 20);
    assert_eq!(stats.allocation_granularity(), 4);
}
