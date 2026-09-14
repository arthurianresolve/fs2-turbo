use super::*;

#[test]
fn maps_modern_disk_space_information() {
    let info = DISK_SPACE_INFORMATION {
        ActualAvailableAllocationUnits: 8,
        ActualTotalAllocationUnits: 10,
        CallerTotalAllocationUnits: 6,
        CallerAvailableAllocationUnits: 6,
        SectorsPerAllocationUnit: 2,
        BytesPerSector: 512,
        ..Default::default()
    };

    let counters = counters_from_disk_space_information(info).unwrap();
    let stats = crate::FsStats::from_counters(counters).unwrap();
    assert_eq!(stats.allocation_granularity(), 1024);
    assert_eq!(stats.free_space(), 8192);
    assert_eq!(stats.available_space(), 6144);
    assert_eq!(stats.total_space(), 10_240);
}

#[test]
fn rejects_invalid_modern_scalar_snapshot() {
    let counters = FilesystemCounters::windows_modern_bytes(4096, 100, 101, 100);

    assert_eq!(
        crate::FsStats::from_counters(counters).unwrap_err().kind(),
        ErrorKind::InvalidData
    );
}

#[test]
fn rejects_invalid_modern_snapshot_stats() {
    let counters = FilesystemCounters::windows_modern_bytes(4096, 101, 100, 100);

    assert_eq!(
        crate::FsStats::from_counters(counters).unwrap_err().kind(),
        ErrorKind::InvalidData
    );
}

#[test]
fn rejects_modern_provider_specific_domain_violations() {
    for info in [
        DISK_SPACE_INFORMATION {
            ActualAvailableAllocationUnits: 8,
            ActualTotalAllocationUnits: 10,
            CallerTotalAllocationUnits: 5,
            CallerAvailableAllocationUnits: 6,
            SectorsPerAllocationUnit: 2,
            BytesPerSector: 512,
            ..Default::default()
        },
        DISK_SPACE_INFORMATION {
            ActualAvailableAllocationUnits: 11,
            ActualTotalAllocationUnits: 10,
            CallerTotalAllocationUnits: 6,
            CallerAvailableAllocationUnits: 6,
            SectorsPerAllocationUnit: 2,
            BytesPerSector: 512,
            ..Default::default()
        },
    ] {
        assert_eq!(
            counters_from_disk_space_information(info)
                .unwrap_err()
                .kind(),
            ErrorKind::InvalidData
        );
    }
}

#[test]
fn rejects_modern_disk_space_overflow() {
    let info = DISK_SPACE_INFORMATION {
        ActualAvailableAllocationUnits: u64::MAX,
        ActualTotalAllocationUnits: u64::MAX,
        CallerTotalAllocationUnits: u64::MAX,
        CallerAvailableAllocationUnits: u64::MAX,
        SectorsPerAllocationUnit: 8,
        BytesPerSector: 512,
        ..Default::default()
    };

    assert_eq!(
        counters_from_disk_space_information(info)
            .unwrap_err()
            .kind(),
        ErrorKind::InvalidData
    );
}

#[test]
fn rejects_modern_total_space_overflow_after_valid_free_counters() {
    let info = DISK_SPACE_INFORMATION {
        ActualAvailableAllocationUnits: 1,
        CallerTotalAllocationUnits: 1,
        CallerAvailableAllocationUnits: 1,
        ActualTotalAllocationUnits: u64::MAX,
        SectorsPerAllocationUnit: 8,
        BytesPerSector: 512,
        ..Default::default()
    };

    assert_eq!(
        counters_from_disk_space_information(info)
            .unwrap_err()
            .kind(),
        ErrorKind::InvalidData
    );
}

#[test]
fn rejects_modern_available_space_overflow_after_valid_free_counters() {
    let info = DISK_SPACE_INFORMATION {
        ActualAvailableAllocationUnits: 1,
        CallerTotalAllocationUnits: u64::MAX,
        CallerAvailableAllocationUnits: u64::MAX,
        ActualTotalAllocationUnits: 1,
        SectorsPerAllocationUnit: 8,
        BytesPerSector: 512,
        ..Default::default()
    };

    assert_eq!(
        counters_from_disk_space_information(info)
            .unwrap_err()
            .kind(),
        ErrorKind::InvalidData
    );
}

#[test]
fn modern_and_legacy_stats_have_valid_domains() {
    let tempdir = tempdir().unwrap();
    let mut root_path = [0u16; VOLUME_PATH_CAPACITY];
    volume_path(&wide_path(tempdir.path()).unwrap(), &mut root_path).unwrap();

    let legacy = crate::FsStats::from_counters(legacy_statvfs(&root_path).unwrap()).unwrap();
    assert!(legacy.allocation_granularity() > 0);
    assert!(legacy.available_space() <= legacy.free_space());
    assert!(legacy.total_space() > 0);

    if let ProviderOutcome::Value(modern) = modern_statvfs(&root_path).unwrap() {
        let modern = crate::FsStats::from_counters(modern).unwrap();
        assert_eq!(
            modern.allocation_granularity(),
            legacy.allocation_granularity()
        );
        assert!(modern.available_space() <= modern.free_space());
        assert!(modern.free_space() <= modern.total_space());

        // Each scalar query acquires a fresh snapshot; space counters can
        // change between calls while the test is running.
        for kind in [
            SpaceKind::Free,
            SpaceKind::Available,
            SpaceKind::Total,
            SpaceKind::AllocationGranularity,
        ] {
            assert!(
                space(tempdir.path(), kind).is_ok(),
                "scalar query failed for {kind:?}"
            );
        }
    }
}

#[test]
fn windows_errors_map_to_documented_hresult_values() {
    assert_eq!(E_NOTIMPL, HRESULT_E_NOTIMPL);
    assert_eq!(
        hresult_from_win32(ERROR_ACCESS_DENIED),
        HRESULT_ACCESS_DENIED
    );
    for (win32_error, hresult) in PATH_ERROR_ENCODINGS {
        assert_eq!(hresult_from_win32(win32_error), hresult);
    }
    for (win32_error, hresult) in UNAVAILABLE_ERROR_ENCODINGS {
        assert_eq!(hresult_from_win32(win32_error), hresult);
    }
}

#[test]
fn modern_provider_only_falls_back_for_unavailable_errors() {
    assert!(modern_statvfs_unavailable(HRESULT_E_NOTIMPL));
    for (_, hresult) in UNAVAILABLE_ERROR_ENCODINGS {
        assert!(modern_statvfs_unavailable(hresult));
    }
    assert!(!modern_statvfs_unavailable(HRESULT_ACCESS_DENIED));
}

#[test]
fn distinguishes_unavailable_and_failed_modern_api() {
    unsafe extern "system" fn unavailable_api(
        _root_path: *const u16,
        _info: *mut DISK_SPACE_INFORMATION,
    ) -> windows_sys::core::HRESULT {
        HRESULT_E_NOTIMPL
    }

    unsafe extern "system" fn failed_api(
        _root_path: *const u16,
        _info: *mut DISK_SPACE_INFORMATION,
    ) -> windows_sys::core::HRESULT {
        HRESULT_E_FAIL
    }

    unsafe extern "system" fn access_denied_api(
        _root_path: *const u16,
        _info: *mut DISK_SPACE_INFORMATION,
    ) -> windows_sys::core::HRESULT {
        HRESULT_ACCESS_DENIED
    }

    unsafe extern "system" fn missing_path_api(
        _root_path: *const u16,
        _info: *mut DISK_SPACE_INFORMATION,
    ) -> windows_sys::core::HRESULT {
        HRESULT_OBJECT_PATH_NOT_FOUND
    }

    let root_path = [0u16; VOLUME_PATH_CAPACITY];
    assert_eq!(
        modern_statvfs_with(&root_path, None).unwrap(),
        ProviderOutcome::Unavailable(FallbackReason::ProviderMissing)
    );
    assert_eq!(
        modern_statvfs_with(&root_path, Some(unavailable_api)).unwrap(),
        ProviderOutcome::Unavailable(FallbackReason::ProviderUnavailable)
    );
    assert_eq!(
        modern_statvfs_with(&root_path, Some(failed_api))
            .unwrap_err()
            .raw_os_error(),
        Some(HRESULT_E_FAIL)
    );

    let access_denied = modern_statvfs_with(&root_path, Some(access_denied_api)).unwrap_err();
    assert_eq!(access_denied.kind(), ErrorKind::PermissionDenied);
    assert_eq!(
        access_denied.raw_os_error(),
        Some(ERROR_ACCESS_DENIED as i32)
    );

    let missing_path = modern_statvfs_with(&root_path, Some(missing_path_api)).unwrap_err();
    assert_eq!(missing_path.kind(), ErrorKind::NotFound);
    assert_eq!(
        missing_path.raw_os_error(),
        Some(ERROR_PATH_NOT_FOUND as i32)
    );
}
