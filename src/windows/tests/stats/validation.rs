use super::*;

#[test]
fn maps_legacy_space_kinds_without_native_queries() {
    for (kind, expected) in [
        (SpaceKind::Free, 3),
        (SpaceKind::Available, 1),
        (SpaceKind::Total, 2),
        (SpaceKind::AllocationGranularity, 4096),
    ] {
        let actual = legacy_space_with(
            kind,
            || {
                Ok(ByteSpace {
                    actual_free: 3,
                    caller_available: 1,
                    caller_total: 2,
                })
            },
            || Ok(4096),
        )
        .unwrap();
        assert_eq!(actual, expected, "{kind:?}");
    }
}

#[test]
fn legacy_space_queries_cover_native_fallback_kinds() {
    let tempdir = tempdir().unwrap();
    let mut root_path = [0u16; VOLUME_PATH_CAPACITY];
    volume_path(&wide_path(tempdir.path()).unwrap(), &mut root_path).unwrap();

    for kind in [
        SpaceKind::Free,
        SpaceKind::Available,
        SpaceKind::Total,
        SpaceKind::AllocationGranularity,
    ] {
        assert!(
            legacy_space(&root_path, kind).is_ok(),
            "legacy native query failed for {kind:?}"
        );
    }
}

#[test]
fn propagates_legacy_provider_errors_without_cross_querying() {
    let geometry_error = Error::other("cluster geometry failed");
    assert!(legacy_statvfs_after_geometry(&[0], Err(geometry_error)).is_err());
    assert!(legacy_statvfs_after_geometry(&[0], Ok(1)).is_err());
}

#[test]
fn filesystem_counters_retain_compact_layout() {
    assert_eq!(
        std::mem::size_of::<FilesystemCounters>(),
        std::mem::size_of::<[u64; 5]>()
    );
}

#[test]
fn recognizes_only_volume_resolution_errors() {
    assert!(!is_volume_resolution_error(&Error::other(
        "provider failure"
    )));
    assert!(!is_volume_resolution_error(&Error::from_raw_os_error(
        ERROR_ACCESS_DENIED as i32
    )));
}
