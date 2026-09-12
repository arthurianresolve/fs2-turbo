use super::*;

#[test]
fn copies_only_exact_drive_roots() {
    let mut root_path = [0; VOLUME_PATH_CAPACITY];
    assert!(copy_exact_drive_root(
        &wide_path(std::path::Path::new("c:/")).unwrap(),
        &mut root_path
    ));
    assert_eq!(
        &root_path[..4],
        &[u16::from(b'c'), u16::from(b':'), u16::from(b'\\'), 0]
    );

    for path in ["C:", r"C:\directory", r"\", r"\\server\share\"] {
        root_path.fill(0);
        assert!(!copy_exact_drive_root(
            &wide_path(std::path::Path::new(path)).unwrap(),
            &mut root_path
        ));
    }

    for path in [
        vec![],
        vec![u16::from(b'1'), u16::from(b':'), u16::from(b'\\'), 0],
        vec![u16::from(b'C'), u16::from(b'x'), u16::from(b'\\'), 0],
        vec![u16::from(b'C'), u16::from(b':'), u16::from(b'x'), 0],
        vec![
            u16::from(b'C'),
            u16::from(b':'),
            u16::from(b'\\'),
            u16::from(b'x'),
        ],
    ] {
        root_path.fill(0);
        assert!(!copy_exact_drive_root(&path, &mut root_path));
    }
}

#[test]
fn exact_drive_root_matches_canonical_resolution() {
    let current = std::env::current_dir().unwrap();
    let root = current.ancestors().last().unwrap();
    let query = StatsQuery::new(root).unwrap();
    let mut canonical = [0; VOLUME_PATH_CAPACITY];
    volume_path(&wide_path(root).unwrap(), &mut canonical).unwrap();

    assert_eq!(query.root_path, canonical);
}

#[test]
fn exact_drive_root_scalars_match_snapshot() {
    let current = std::env::current_dir().unwrap();
    let root = current.ancestors().last().unwrap();
    let stats = crate::statvfs(root).unwrap();

    assert_eq!(space(root, SpaceKind::Total).unwrap(), stats.total_space());
    assert_eq!(
        space(root, SpaceKind::AllocationGranularity).unwrap(),
        stats.allocation_granularity()
    );
}

#[test]
fn exact_drive_root_scalar_errors_match_canonical_resolution() {
    let missing_root = (b'A'..=b'Z').find_map(|drive| {
        let root = PathBuf::from(format!("{}:\\", char::from(drive)));
        let mut canonical = [0; VOLUME_PATH_CAPACITY];
        volume_path(&wide_path(&root).unwrap(), &mut canonical)
            .err()
            .map(|error| (root, error))
    });
    let Some((root, expected)) = missing_root else {
        return;
    };

    for kind in [
        SpaceKind::Free,
        SpaceKind::Available,
        SpaceKind::Total,
        SpaceKind::AllocationGranularity,
    ] {
        let actual = space(&root, kind).unwrap_err();
        assert_eq!(actual.kind(), expected.kind(), "{kind:?}");
        assert_eq!(actual.raw_os_error(), expected.raw_os_error(), "{kind:?}");
    }
}

#[test]
fn exact_drive_root_preserves_provider_errors() {
    let code = ERROR_ACCESS_DENIED as i32;
    let error = std::io::Error::from_raw_os_error(code);

    assert!(matches!(
        exact_root_value(Err(error)),
        Err(error) if error.raw_os_error() == Some(code)
    ));
}

#[test]
fn exact_drive_root_returns_provider_error_for_unavailable_drive() {
    let unavailable_root = (b'A'..=b'Z')
        .map(|letter| format!("{}:\\", char::from(letter)))
        .find(|root| !Path::new(root).exists())
        .expect("Windows should expose at least one unavailable drive letter");

    let error = space(Path::new(&unavailable_root), SpaceKind::Free).unwrap_err();
    assert!(error.raw_os_error().is_some());
}

#[test]
fn propagates_exact_root_query_errors_without_volume_fallback() {
    let mut root_path = [0u16; VOLUME_PATH_CAPACITY];
    let error = Error::other("exact root query failed");

    assert!(
        space_after_exact_root(&[0], SpaceKind::Free, &mut root_path, Err(error), None).is_err()
    );

    let tempdir = tempdir().unwrap();
    let path = wide_path(tempdir.path()).unwrap();
    assert!(
        space_after_exact_root(
            &path,
            SpaceKind::Free,
            &mut root_path,
            Err(Error::other("exact root query failed")),
            None
        )
        .is_err()
    );
}

#[test]
fn statistics_preserve_unavailable_volume_errors() {
    let unavailable_root = (b'A'..=b'Z')
        .map(|letter| format!("{}:\\", char::from(letter)))
        .find(|root| !Path::new(root).exists())
        .expect("Windows should expose at least one unavailable drive letter");

    assert!(crate::statvfs(Path::new(&unavailable_root)).is_err());
}

#[test]
fn exact_drive_root_only_resolves_volume_for_path_errors() {
    for (win32_error, _) in PATH_ERROR_ENCODINGS {
        let error = std::io::Error::from_raw_os_error(win32_error as i32);
        assert_eq!(
            exact_root_value(Err(error)).unwrap(),
            ProviderOutcome::Unavailable(FallbackReason::VolumeResolution)
        );
    }
}
