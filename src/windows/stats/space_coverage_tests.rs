use super::{
    DirectSpace, appears_to_be_drive_root, exact_root_space_from_path, project_handle_space,
};
use crate::stats::SpaceKind;

#[test]
fn exact_root_fast_path_rejects_data_after_the_terminator() {
    let path = [u16::from(b'C'), u16::from(b':'), u16::from(b'\\'), 0, 1];
    assert!(appears_to_be_drive_root(&path));

    for kind in [
        SpaceKind::Free,
        SpaceKind::Available,
        SpaceKind::Total,
        SpaceKind::AllocationGranularity,
    ] {
        assert_eq!(exact_root_space_from_path(&path, kind, None).unwrap(), None);
    }
}

#[test]
fn handle_projection_rejects_overflow_and_inconsistent_byte_counters() {
    assert_eq!(
        project_handle_space(u64::MAX, 2, u64::MAX, SpaceKind::Available),
        DirectSpace::Unavailable
    );
    assert_eq!(
        project_handle_space(4, 5, 16, SpaceKind::Available),
        DirectSpace::Unavailable
    );
}

#[test]
fn handle_projection_preserves_each_scalar_domain() {
    for (kind, expected) in [
        (SpaceKind::Free, DirectSpace::Hit(16)),
        (SpaceKind::Available, DirectSpace::Hit(12)),
        (SpaceKind::AllocationGranularity, DirectSpace::Hit(4)),
        (SpaceKind::Total, DirectSpace::Unavailable),
    ] {
        assert_eq!(project_handle_space(4, 3, 16, kind), expected);
    }
}

#[test]
fn handle_projection_accepts_zero_equal_and_maximum_available_space() {
    for (granularity, caller_units, actual_free, expected) in [
        (4, 0, 16, 0),
        (4, 4, 16, 16),
        (1, u64::MAX, u64::MAX, u64::MAX),
    ] {
        assert_eq!(
            project_handle_space(granularity, caller_units, actual_free, SpaceKind::Available),
            DirectSpace::Hit(expected)
        );
    }
}
