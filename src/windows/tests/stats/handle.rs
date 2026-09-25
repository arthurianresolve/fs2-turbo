use super::*;

#[test]
fn maps_handle_query_results_before_projecting_counters() {
    let info = FILE_FS_FULL_SIZE_INFORMATION {
        TotalAllocationUnits: 10,
        ActualAvailableAllocationUnits: 8,
        CallerAvailableAllocationUnits: 6,
        SectorsPerAllocationUnit: 2,
        BytesPerSector: 512,
    };

    assert_eq!(
        handle_space_query_result(1, info, SpaceKind::Free),
        DirectSpace::Unavailable
    );
    assert_eq!(
        handle_space_query_result(0, info, SpaceKind::Free),
        DirectSpace::Hit(8192)
    );
}

#[test]
fn evaluates_handle_attribute_decision_independently() {
    assert!(handle_space_attributes_decision(true, true));
    assert!(!handle_space_attributes_decision(false, true));
    assert!(!handle_space_attributes_decision(true, false));
    assert!(!handle_space_attributes_decision(false, false));
}

#[test]
fn direct_directory_space_uses_narrow_queries() {
    let tempdir = tempdir().unwrap();
    let path = wide_path(tempdir.path()).unwrap();

    let DirectSpace::Hit(_) = direct_space(&path, SpaceKind::Free) else {
        panic!("direct free-space query unexpectedly required fallback");
    };
    let DirectSpace::Hit(_) = direct_space(&path, SpaceKind::Available) else {
        panic!("direct available-space query unexpectedly required fallback");
    };

    assert!(matches!(
        direct_space(&path, SpaceKind::Total),
        DirectSpace::Unavailable
    ));
}

#[test]
fn direct_file_space_defers_to_an_alternate_provider() {
    let tempdir = tempdir().unwrap();
    let path = tempdir.path().join("fs2");
    fs::write(&path, []).unwrap();

    assert_eq!(
        direct_space(&wide_path(&path).unwrap(), SpaceKind::Free),
        DirectSpace::Unavailable
    );
    assert!(space(&path, SpaceKind::Free).is_ok());
}

#[test]
fn handle_space_resolves_online_files_and_absolute_directories() {
    let tempdir = tempdir().unwrap();
    let file = tempdir.path().join("fs2");
    fs::write(&file, []).unwrap();

    let path = wide_path(&file).unwrap();
    let os_path = file.as_os_str();
    assert!(matches!(
        handle_space(&path, os_path, SpaceKind::Free),
        DirectSpace::Hit(_)
    ));
    assert!(matches!(
        handle_space(&path, os_path, SpaceKind::Available),
        DirectSpace::Hit(_)
    ));
    assert_eq!(
        handle_space(&path, os_path, SpaceKind::Total),
        DirectSpace::Unavailable
    );
    assert!(matches!(
        handle_space(&path, os_path, SpaceKind::AllocationGranularity),
        DirectSpace::Hit(_)
    ));
    let tempdir_path = tempdir.path().to_path_buf();
    let tempdir_path_wide = wide_path(&tempdir_path).unwrap();
    assert_eq!(
        handle_space(
            &tempdir_path_wide,
            tempdir_path.as_os_str(),
            SpaceKind::Free
        ),
        DirectSpace::Unavailable
    );
    let DirectSpace::Hit(handle_granularity) = handle_space(
        &tempdir_path_wide,
        tempdir_path.as_os_str(),
        SpaceKind::AllocationGranularity,
    ) else {
        panic!("absolute directory allocation granularity unexpectedly required fallback");
    };
    assert_eq!(
        space(&tempdir_path, SpaceKind::AllocationGranularity).unwrap(),
        handle_granularity
    );

    let canonical_tempdir = tempdir_path.canonicalize().unwrap();
    let DirectSpace::Hit(canonical_granularity) = handle_space(
        &wide_path(&canonical_tempdir).unwrap(),
        canonical_tempdir.as_os_str(),
        SpaceKind::AllocationGranularity,
    ) else {
        panic!("verbatim local directory unexpectedly required fallback");
    };
    assert_eq!(canonical_granularity, handle_granularity);

    let drive_root = tempdir_path.ancestors().last().unwrap().to_path_buf();
    let canonical_drive_root = drive_root.canonicalize().unwrap();
    for path in [
        Path::new("."),
        drive_root.as_path(),
        canonical_drive_root.as_path(),
    ] {
        assert_eq!(
            handle_space(
                &wide_path(path).unwrap(),
                path.as_os_str(),
                SpaceKind::AllocationGranularity,
            ),
            DirectSpace::Unavailable
        );
    }
}

#[test]
fn handle_space_attributes_preserve_existing_routes() {
    assert!(handle_space_attributes_eligible(
        FILE_ATTRIBUTE_ARCHIVE,
        SpaceKind::Free
    ));
    assert!(handle_space_attributes_eligible(
        FILE_ATTRIBUTE_ARCHIVE,
        SpaceKind::AllocationGranularity
    ));
    assert!(!handle_space_attributes_eligible(
        FILE_ATTRIBUTE_DIRECTORY,
        SpaceKind::Free
    ));
    assert!(handle_space_attributes_eligible(
        FILE_ATTRIBUTE_DIRECTORY,
        SpaceKind::AllocationGranularity
    ));
    assert!(handle_space_attributes_eligible(
        FILE_ATTRIBUTE_REPARSE_POINT,
        SpaceKind::Free
    ));
    assert!(!handle_space_attributes_eligible(
        FILE_ATTRIBUTE_REPARSE_POINT,
        SpaceKind::AllocationGranularity
    ));
    for attributes in [
        INVALID_FILE_ATTRIBUTES,
        FILE_ATTRIBUTE_DEVICE,
        FILE_ATTRIBUTE_OFFLINE,
        FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS,
        FILE_ATTRIBUTE_RECALL_ON_OPEN,
    ] {
        for kind in [SpaceKind::Free, SpaceKind::AllocationGranularity] {
            assert!(!handle_space_attributes_eligible(attributes, kind));
        }
    }
}

#[test]
fn handle_space_projects_valid_file_counters() {
    let info = FILE_FS_FULL_SIZE_INFORMATION {
        TotalAllocationUnits: i64::MAX,
        ActualAvailableAllocationUnits: 8,
        CallerAvailableAllocationUnits: 6,
        SectorsPerAllocationUnit: 2,
        BytesPerSector: 512,
    };

    assert_eq!(
        handle_space_from_info(info, SpaceKind::Free),
        DirectSpace::Hit(8192)
    );
    assert_eq!(
        handle_space_from_info(info, SpaceKind::Available),
        DirectSpace::Hit(6144)
    );
    assert_eq!(
        handle_space_from_info(info, SpaceKind::Total),
        DirectSpace::Unavailable
    );
    assert_eq!(
        handle_space_from_info(info, SpaceKind::AllocationGranularity),
        DirectSpace::Hit(1024)
    );
}

#[test]
fn handle_space_rejects_invalid_file_counters() {
    let valid = FILE_FS_FULL_SIZE_INFORMATION {
        TotalAllocationUnits: i64::MAX,
        ActualAvailableAllocationUnits: 8,
        CallerAvailableAllocationUnits: 6,
        SectorsPerAllocationUnit: 2,
        BytesPerSector: 512,
    };
    let invalid = [
        FILE_FS_FULL_SIZE_INFORMATION {
            SectorsPerAllocationUnit: 0,
            ..valid
        },
        FILE_FS_FULL_SIZE_INFORMATION {
            TotalAllocationUnits: i64::MAX,
            ActualAvailableAllocationUnits: -1,
            ..valid
        },
        FILE_FS_FULL_SIZE_INFORMATION {
            CallerAvailableAllocationUnits: -1,
            ..valid
        },
        FILE_FS_FULL_SIZE_INFORMATION {
            TotalAllocationUnits: i64::MAX,
            ActualAvailableAllocationUnits: 5,
            CallerAvailableAllocationUnits: 6,
            ..valid
        },
        FILE_FS_FULL_SIZE_INFORMATION {
            TotalAllocationUnits: i64::MAX,
            ActualAvailableAllocationUnits: i64::MAX,
            CallerAvailableAllocationUnits: i64::MAX,
            ..valid
        },
        FILE_FS_FULL_SIZE_INFORMATION {
            TotalAllocationUnits: i64::MAX,
            ActualAvailableAllocationUnits: 1,
            CallerAvailableAllocationUnits: i64::MAX,
            ..valid
        },
    ];

    for info in invalid {
        for kind in [SpaceKind::Free, SpaceKind::AllocationGranularity] {
            assert_eq!(handle_space_from_info(info, kind), DirectSpace::Unavailable);
        }
    }
}

#[test]
fn handle_space_accepts_physical_free_above_quota_limited_total() {
    let info = FILE_FS_FULL_SIZE_INFORMATION {
        TotalAllocationUnits: 5,
        ActualAvailableAllocationUnits: 6,
        CallerAvailableAllocationUnits: 4,
        SectorsPerAllocationUnit: 2,
        BytesPerSector: 512,
    };

    assert_eq!(
        handle_space_from_info(info, SpaceKind::Free),
        DirectSpace::Hit(6144)
    );
    assert_eq!(
        handle_space_from_info(info, SpaceKind::Available),
        DirectSpace::Hit(4096)
    );
}

#[test]
fn rejects_handle_available_units_above_total_units() {
    let caller_above_total = FILE_FS_FULL_SIZE_INFORMATION {
        TotalAllocationUnits: 5,
        ActualAvailableAllocationUnits: 5,
        CallerAvailableAllocationUnits: 6,
        SectorsPerAllocationUnit: 2,
        BytesPerSector: 512,
    };
    assert_eq!(
        handle_space_from_info(caller_above_total, SpaceKind::Available),
        DirectSpace::Unavailable,
    );

    let negative_total = FILE_FS_FULL_SIZE_INFORMATION {
        TotalAllocationUnits: -1,
        ActualAvailableAllocationUnits: 8,
        CallerAvailableAllocationUnits: 6,
        SectorsPerAllocationUnit: 2,
        BytesPerSector: 512,
    };
    assert_eq!(
        handle_space_from_info(negative_total, SpaceKind::Available),
        DirectSpace::Unavailable,
    );
}
