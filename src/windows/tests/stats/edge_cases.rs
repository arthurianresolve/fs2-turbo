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
fn handle_queries_and_open_failures_preserve_fallback_boundaries() {
    let temporary = tempdir().unwrap();
    let path = temporary.path().join("handle-fallback");
    fs::File::create(&path).unwrap();
    let missing = temporary.path().join("missing");
    for (path, kind, open_fails, expected_calls, expected_hit) in [
        (path.as_path(), SpaceKind::Total, false, 0, false),
        (
            Path::new("."),
            SpaceKind::AllocationGranularity,
            false,
            0,
            false,
        ),
        (missing.as_path(), SpaceKind::Free, false, 0, false),
        (temporary.path(), SpaceKind::Free, false, 0, false),
        (path.as_path(), SpaceKind::Free, true, 1, false),
        (path.as_path(), SpaceKind::Free, false, 1, true),
        (path.as_path(), SpaceKind::Available, false, 1, true),
        (
            path.as_path(),
            SpaceKind::AllocationGranularity,
            false,
            1,
            true,
        ),
    ] {
        let encoded = wide_path(path).unwrap();
        let calls = Cell::new(0);
        let result = handle_space_with(&encoded, path.as_os_str(), kind, |requested| {
            assert_eq!(requested, path.as_os_str());
            calls.set(calls.get() + 1);
            if open_fails {
                None
            } else {
                Some(fs::File::open(requested).unwrap())
            }
        });
        assert_eq!(calls.get(), expected_calls, "{kind:?}");
        if expected_hit {
            assert!(
                matches!(result, DirectSpace::Hit(_)),
                "{kind:?}: {result:?}"
            );
        } else {
            assert_eq!(result, DirectSpace::Unavailable, "{kind:?}");
        }
    }
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
