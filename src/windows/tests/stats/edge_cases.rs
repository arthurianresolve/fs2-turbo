use super::*;
use crate::windows::stats::test_support::{direct_space_result, handle_space_with};

#[test]
fn long_paths_preserve_every_heap_tail_code_unit() {
    for length in [VOLUME_PATH_CAPACITY + 1, 4096] {
        let mut encoded = vec![u16::from(b'x'); length];
        encoded[length - 2] = 0xd800;
        encoded[length - 1] = u16::from(b'y');
        let path = PathBuf::from(OsString::from_wide(&encoded));
        let prepared = with_wide_path(&path, |path| Ok(path.to_vec())).unwrap();
        encoded.push(0);
        assert_eq!(prepared, encoded);
    }
}

#[test]
fn zero_legacy_geometry_is_rejected_for_either_zero_factor() {
    for (sectors, bytes) in [(0, 512), (8, 0), (0, 0)] {
        let error = cluster_geometry_result(1, sectors, bytes).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::InvalidData);
    }
}

#[test]
fn narrow_direct_results_do_not_claim_other_counter_domains() {
    for kind in [SpaceKind::Total, SpaceKind::AllocationGranularity] {
        assert_eq!(
            direct_space_result(1, 1, 2, 3, kind),
            DirectSpace::Unavailable
        );
    }
}

#[test]
fn handle_open_failure_is_a_fallback_not_a_fabricated_counter() {
    let temporary = tempdir().unwrap();
    let path = temporary.path().join("handle-fallback");
    fs::File::create(&path).unwrap();
    let encoded = wide_path(&path).unwrap();
    let calls = Cell::new(0);
    let result = handle_space_with(&encoded, path.as_os_str(), SpaceKind::Free, |requested| {
        assert_eq!(requested, path.as_os_str());
        calls.set(calls.get() + 1);
        None
    });
    assert_eq!(calls.get(), 1);
    assert_eq!(result, DirectSpace::Unavailable);
}

#[test]
fn recoverable_exact_root_errors_retry_a_valid_volume() {
    let temporary = tempdir().unwrap();
    let path = wide_path(temporary.path()).unwrap();
    for (code, _) in PATH_ERROR_ENCODINGS {
        for kind in [
            SpaceKind::Free,
            SpaceKind::Available,
            SpaceKind::Total,
            SpaceKind::AllocationGranularity,
        ] {
            let mut root = [0; VOLUME_PATH_CAPACITY];
            let result = space_after_exact_root(
                &path,
                kind,
                &mut root,
                Err(Error::from_raw_os_error(code as i32)),
                None,
            );
            assert!(result.is_ok(), "{kind:?}: {result:?}");
            assert_ne!(root[0], 0);
        }
    }
}
