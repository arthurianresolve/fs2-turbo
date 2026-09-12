use super::*;

#[test]
fn relative_space_queries_resolve_the_volume_path() {
    assert!(space(Path::new("."), SpaceKind::Free).is_ok());
}

#[test]
fn rejects_unresolvable_volume_paths() {
    let unavailable_drive = (b'A'..=b'Z')
        .map(char::from)
        .find(|letter| {
            let root = format!("{letter}:\\");
            let mut canonical = [0; VOLUME_PATH_CAPACITY];
            volume_path(&wide_path(Path::new(&root)).unwrap(), &mut canonical).is_err()
        })
        .expect("Windows should expose at least one unavailable drive letter");
    let unavailable_root = format!("{unavailable_drive}:\\missing");

    assert!(StatsQuery::new(Path::new(&unavailable_root)).is_err());
}

#[test]
fn space_rejects_unresolvable_volume_paths() {
    let unavailable_drive = (b'A'..=b'Z')
        .map(char::from)
        .find(|letter| {
            let root = format!("{letter}:\\");
            let mut canonical = [0; VOLUME_PATH_CAPACITY];
            volume_path(&wide_path(Path::new(&root)).unwrap(), &mut canonical).is_err()
        })
        .expect("Windows should expose at least one unavailable drive letter");
    let unavailable_root = format!("{unavailable_drive}:\\missing");

    assert!(space(Path::new(&unavailable_root), SpaceKind::Free).is_err());
}

#[test]
fn evaluates_drive_root_components_independently() {
    let upper = u16::from(b'C');
    let lower = u16::from(b'c');
    let colon = u16::from(b':');
    let slash = u16::from(b'/');
    let backslash = u16::from(b'\\');

    assert!(valid_drive_root_components(upper, colon, backslash, 0));
    assert!(valid_drive_root_components(lower, colon, slash, 0));
    assert!(!valid_drive_root_components(
        u16::from(b'1'),
        colon,
        backslash,
        0
    ));
    assert!(!valid_drive_root_components(
        upper,
        u16::from(b'x'),
        backslash,
        0
    ));
    assert!(!valid_drive_root_components(
        upper,
        colon,
        u16::from(b'x'),
        0
    ));
    assert!(!valid_drive_root_components(
        upper,
        colon,
        backslash,
        u16::from(b'x')
    ));
}

#[test]
fn prepares_short_and_long_wide_paths() {
    for mut encoded in [
        vec![u16::from(b'x'); VOLUME_PATH_CAPACITY - 2],
        vec![u16::from(b'x'); VOLUME_PATH_CAPACITY - 1],
    ] {
        encoded.push(0xd800);
        let length = encoded.len();
        let path = PathBuf::from(OsString::from_wide(&encoded));
        let prepared = with_wide_path(&path, |path| Ok(path.to_vec())).unwrap();

        assert_eq!(&prepared[..length], encoded);
        assert_eq!(prepared[length], 0);
    }
}

#[test]
fn reserves_inline_storage_only_for_bounded_paths() {
    let long = OsString::from_wide(&vec![u16::from(b'x'); VOLUME_PATH_CAPACITY]);
    let path = Path::new(&long);
    let encoded = with_wide_path(path, |path| Ok(path.to_vec())).unwrap();

    assert!(encoded.len() > VOLUME_PATH_CAPACITY);
}

#[test]
fn rejects_null_after_inline_path_without_invoking_operation() {
    let mut encoded = vec![u16::from(b'x'); VOLUME_PATH_CAPACITY];
    encoded.extend([0, u16::from(b'y')]);
    let path = PathBuf::from(OsString::from_wide(&encoded));
    let invoked = Cell::new(false);

    let error = with_wide_path(&path, |_| {
        invoked.set(true);
        Ok(())
    })
    .unwrap_err();

    assert_eq!(error.kind(), ErrorKind::InvalidInput);
    assert!(!invoked.get());
}

#[test]
fn statistics_reject_embedded_null_paths() {
    let mut encoded: Vec<_> = std::env::current_dir()
        .unwrap()
        .as_os_str()
        .encode_wide()
        .collect();
    encoded.extend([0, u16::from(b'x')]);
    let path = PathBuf::from(OsString::from_wide(&encoded));

    assert_eq!(
        wide_path(&path).unwrap_err().kind(),
        ErrorKind::InvalidInput
    );
    assert_eq!(
        crate::statvfs(&path).unwrap_err().kind(),
        ErrorKind::InvalidInput
    );
    assert_eq!(
        crate::free_space(&path).unwrap_err().kind(),
        ErrorKind::InvalidInput
    );
    assert_eq!(
        crate::available_space(&path).unwrap_err().kind(),
        ErrorKind::InvalidInput
    );
    assert_eq!(
        crate::total_space(&path).unwrap_err().kind(),
        ErrorKind::InvalidInput
    );
    assert_eq!(
        crate::allocation_granularity(&path).unwrap_err().kind(),
        ErrorKind::InvalidInput
    );
    assert_eq!(
        crate::FsStatsQuery::new(&path).unwrap_err().kind(),
        ErrorKind::InvalidInput
    );
}
