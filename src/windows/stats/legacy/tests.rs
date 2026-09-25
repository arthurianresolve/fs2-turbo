use std::cell::{Cell, RefCell};

use super::{ByteSpace, Error, ErrorKind, Result, SpaceKind, legacy_space};

struct QueryFixture {
    bytes: Option<Result<ByteSpace>>,
    geometry: Option<Result<u64>>,
    byte_calls: usize,
    geometry_calls: usize,
}

thread_local! {
    static QUERY_FIXTURE: RefCell<Option<QueryFixture>> = const { RefCell::new(None) };
}

struct QueryGuard(Option<QueryFixture>);

impl QueryGuard {
    fn install(bytes: Option<Result<ByteSpace>>, geometry: Option<Result<u64>>) -> Self {
        Self(QUERY_FIXTURE.with(|slot| {
            slot.replace(Some(QueryFixture {
                bytes,
                geometry,
                byte_calls: 0,
                geometry_calls: 0,
            }))
        }))
    }
}

impl Drop for QueryGuard {
    fn drop(&mut self) {
        QUERY_FIXTURE.with(|slot| {
            slot.replace(self.0.take());
        });
    }
}

pub(super) fn query_byte_space(root_path: &[u16]) -> Result<ByteSpace> {
    match byte_query() {
        Some(result) => result,
        None => super::byte_space(root_path),
    }
}

pub(super) fn query_cluster_geometry(root_path: &[u16]) -> Result<u64> {
    match geometry_query() {
        Some(result) => result,
        None => super::cluster_geometry(root_path),
    }
}

fn byte_query() -> Option<Result<ByteSpace>> {
    QUERY_FIXTURE.with(|slot| {
        let mut slot = slot.borrow_mut();
        let fixture = slot.as_mut()?;
        fixture.byte_calls += 1;
        Some(
            fixture
                .bytes
                .take()
                .expect("unexpected or repeated byte query"),
        )
    })
}

fn geometry_query() -> Option<Result<u64>> {
    QUERY_FIXTURE.with(|slot| {
        let mut slot = slot.borrow_mut();
        let fixture = slot.as_mut()?;
        fixture.geometry_calls += 1;
        Some(
            fixture
                .geometry
                .take()
                .expect("unexpected or repeated geometry query"),
        )
    })
}

pub(crate) fn legacy_space_with(
    kind: SpaceKind,
    byte_query: impl FnOnce() -> Result<ByteSpace>,
    geometry_query: impl FnOnce() -> Result<u64>,
) -> Result<u64> {
    let (bytes, geometry) = match kind {
        SpaceKind::AllocationGranularity => (None, Some(geometry_query())),
        _ => (Some(byte_query()), None),
    };
    let guard = QueryGuard::install(bytes, geometry);
    let result = legacy_space(&[0], kind);
    QUERY_FIXTURE.with(|slot| {
        let slot = slot.borrow();
        let fixture = slot.as_ref().unwrap();
        let expected = match kind {
            SpaceKind::AllocationGranularity => (0, 1),
            _ => (1, 0),
        };
        assert_eq!((fixture.byte_calls, fixture.geometry_calls), expected);
    });
    drop(guard);
    result
}

#[test]
fn selected_provider_errors_are_preserved_without_cross_querying() {
    let byte_calls = Cell::new(0);
    let geometry_calls = Cell::new(0);
    let bytes = || {
        byte_calls.set(byte_calls.get() + 1);
        Err(Error::new(ErrorKind::PermissionDenied, "byte query failed"))
    };
    let geometry = || {
        geometry_calls.set(geometry_calls.get() + 1);
        Err(Error::new(ErrorKind::NotFound, "geometry query failed"))
    };
    for kind in [
        SpaceKind::Free,
        SpaceKind::Available,
        SpaceKind::Total,
        SpaceKind::AllocationGranularity,
    ] {
        byte_calls.set(0);
        geometry_calls.set(0);
        let error = legacy_space_with(kind, bytes, geometry).unwrap_err();
        let (expected_kind, expected_message, expected_calls) = match kind {
            SpaceKind::AllocationGranularity => {
                (ErrorKind::NotFound, "geometry query failed", (0, 1))
            }
            _ => (ErrorKind::PermissionDenied, "byte query failed", (1, 0)),
        };
        assert_eq!(error.kind(), expected_kind);
        assert_eq!(error.to_string(), expected_message);
        assert_eq!((byte_calls.get(), geometry_calls.get()), expected_calls);
    }
}

#[test]
fn query_wrappers_preserve_native_errors_without_a_fixture() {
    let invalid_root = [u16::from(b'?'), u16::from(b':'), u16::from(b'\\'), 0];
    assert!(query_byte_space(&invalid_root).is_err());
    assert!(query_cluster_geometry(&invalid_root).is_err());
}

#[test]
fn fixture_restores_the_outer_query_after_unwinding() {
    let outer = QueryGuard::install(None, Some(Ok(4096)));
    let result = std::panic::catch_unwind(|| {
        let _inner = QueryGuard::install(None, Some(Ok(8192)));
        panic!("exercise fixture cleanup");
    });
    assert!(result.is_err());
    assert_eq!(geometry_query().unwrap().unwrap(), 4096);
    drop(outer);
    assert!(geometry_query().is_none());
    assert!(byte_query().is_none());
}

#[test]
fn fixtures_are_private_to_the_current_thread() {
    let guard = QueryGuard::install(None, Some(Ok(4096)));
    std::thread::spawn(|| {
        assert!(geometry_query().is_none());
        assert!(byte_query().is_none());
    })
    .join()
    .unwrap();
    assert_eq!(geometry_query().unwrap().unwrap(), 4096);
    drop(guard);
}
