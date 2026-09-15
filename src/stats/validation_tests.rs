use super::validate_available_bounds;

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
