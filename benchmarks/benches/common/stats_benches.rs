use std::hint::black_box;
use std::io::Result;
use std::path::Path;

use criterion::Criterion;

#[cfg(windows)]
use super::open_file;
use super::{FailureProbe, StatsFixture, observe};

pub(crate) type Snapshot = (u64, u64, u64, u64);

pub(crate) trait StatsSubject {
    const SNAPSHOT_GROUP: &'static str;
    const SNAPSHOT_ONE_ID: &'static str;
    const SNAPSHOT_FOUR_ID: &'static str;
    #[cfg(windows)]
    const FILE_FREE_BENCH: &'static str;
    #[cfg(windows)]
    const FILE_FREE_ID: &'static str;
    #[cfg(windows)]
    const FILE_AVAILABLE_BENCH: &'static str;
    #[cfg(windows)]
    const FILE_AVAILABLE_ID: &'static str;

    fn free_space(path: &Path) -> Result<u64>;
    fn available_space(path: &Path) -> Result<u64>;
    fn total_space(path: &Path) -> Result<u64>;
    fn allocation_granularity(path: &Path) -> Result<u64>;
    fn snapshot(path: &Path) -> Result<Snapshot>;
}

pub(crate) fn bench_stats_snapshot<S: StatsSubject>(criterion: &mut Criterion) {
    let fixture = StatsFixture::new();
    let path = fixture.path();
    let mut group = criterion.benchmark_group(S::SNAPSHOT_GROUP);

    group.bench_function("one_snapshot", |bencher| {
        let _probe = FailureProbe::new("stats_snapshot.one_snapshot");
        super::iter_primed!(bencher, S::SNAPSHOT_ONE_ID, || {
            black_box(observe(
                S::snapshot(path),
                (0, 0, 0, 0),
                "stats_snapshot.one_snapshot",
            ))
        });
    });
    group.bench_function("four_convenience_queries", |bencher| {
        let _probe = FailureProbe::new("stats_snapshot.four_convenience_queries");
        super::iter_primed!(bencher, S::SNAPSHOT_FOUR_ID, || {
            black_box((
                observe(S::free_space(path), 0, "stats_snapshot.free_space"),
                observe(
                    S::available_space(path),
                    0,
                    "stats_snapshot.available_space",
                ),
                observe(S::total_space(path), 0, "stats_snapshot.total_space"),
                observe(
                    S::allocation_granularity(path),
                    0,
                    "stats_snapshot.allocation_granularity",
                ),
            ))
        });
    });
    group.finish();
}

pub(crate) fn bench_windows_file_space_fallback<S: StatsSubject>(criterion: &mut Criterion) {
    #[cfg(windows)]
    {
        let tempdir = tempfile::tempdir().unwrap();
        let path = tempdir.path().join("file");
        open_file(&path);

        criterion.bench_function(S::FILE_FREE_BENCH, |bencher| {
            let _probe = FailureProbe::new("free_space_file_fallback");
            super::iter_primed!(bencher, S::FILE_FREE_ID, || {
                black_box(observe(S::free_space(&path), 0, "free_space_file_fallback"))
            });
        });
        criterion.bench_function(S::FILE_AVAILABLE_BENCH, |bencher| {
            let _probe = FailureProbe::new("available_space_file_fallback");
            super::iter_primed!(bencher, S::FILE_AVAILABLE_ID, || {
                black_box(observe(
                    S::available_space(&path),
                    0,
                    "available_space_file_fallback",
                ))
            });
        });
    }

    #[cfg(not(windows))]
    let _ = criterion;
}
