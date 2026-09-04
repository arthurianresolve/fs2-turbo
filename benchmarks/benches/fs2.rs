use std::hint::black_box;
use std::path::Path;

#[cfg(windows)]
use std::{fs, path::PathBuf};

#[cfg(windows)]
use std::os::windows::ffi::OsStrExt;

use criterion::{Criterion, criterion_group, criterion_main};
#[path = "common/bench_support.rs"]
mod bench_support;
use fs2::{
    FileExt, FsStatsQuery, allocation_granularity, available_space, free_space, statvfs,
    total_space,
};
use tempfile::tempdir;

struct StatsSubject;

impl bench_support::StatsSubject for StatsSubject {
    const SNAPSHOT_GROUP: &'static str = "stats_snapshot";
    const SNAPSHOT_ONE_ID: &'static str = "stats_snapshot/one_snapshot";
    const SNAPSHOT_FOUR_ID: &'static str = "stats_snapshot/four_convenience_queries";
    #[cfg(windows)]
    const FILE_FREE_BENCH: &'static str = "free_space_file_fallback";
    #[cfg(windows)]
    const FILE_FREE_ID: &'static str = "free_space_file_fallback";
    #[cfg(windows)]
    const FILE_AVAILABLE_BENCH: &'static str = "available_space_file_fallback";
    #[cfg(windows)]
    const FILE_AVAILABLE_ID: &'static str = "available_space_file_fallback";

    fn free_space(path: &Path) -> std::io::Result<u64> {
        free_space(path)
    }

    fn available_space(path: &Path) -> std::io::Result<u64> {
        available_space(path)
    }

    fn total_space(path: &Path) -> std::io::Result<u64> {
        total_space(path)
    }

    fn allocation_granularity(path: &Path) -> std::io::Result<u64> {
        allocation_granularity(path)
    }

    fn snapshot(path: &Path) -> std::io::Result<bench_support::Snapshot> {
        statvfs(path).map(|stats| {
            (
                stats.free_space(),
                stats.available_space(),
                stats.total_space(),
                stats.allocation_granularity(),
            )
        })
    }
}

#[cfg(windows)]
fn long_directory(root: &Path) -> PathBuf {
    let mut path = tempfile::Builder::new()
        .prefix("fs2-long-path-")
        .tempdir_in(root)
        .unwrap()
        .keep();
    let mut suffix = 0;
    while path.as_os_str().encode_wide().count() <= 300 {
        path.push(format!("fs2-long-path-segment-{suffix:02}"));
        fs::create_dir(&path).unwrap();
        suffix += 1;
    }
    path
}

fn bench_file_create(c: &mut Criterion) {
    bench_support::bench_file_create_delete(c, "file_create_delete", "file_create_delete");
}

fn bench_file_truncate(c: &mut Criterion) {
    bench_support::bench_file_open_truncate_delete(
        c,
        "file_open_truncate_delete",
        "file_open_truncate_delete",
    );
}

fn bench_file_allocate(c: &mut Criterion) {
    bench_support::bench_file_open_allocate_delete(
        c,
        "file_open_allocate_delete",
        "file_open_allocate_delete",
        FileExt::allocate,
    );
}

fn bench_file_allocate_already_satisfied(c: &mut Criterion) {
    let tempdir = tempdir().unwrap();
    let path = tempdir.path().join("file");
    let file = bench_support::open_file(&path);
    file.allocate(32 * 1024 * 1024).unwrap();

    c.bench_function("file_allocate_already_satisfied", |b| {
        bench_support::iter_primed!(b, "file_allocate_already_satisfied", || {
            file.allocate(32 * 1024 * 1024).unwrap()
        });
    });
}

fn bench_allocated_size(c: &mut Criterion) {
    let tempdir = tempdir().unwrap();
    let path = tempdir.path().join("file");
    let file = bench_support::open_file(&path);
    file.allocate(32 * 1024 * 1024).unwrap();

    c.bench_function("allocated_size", |b| {
        bench_support::iter_primed!(b, "allocated_size", || {
            black_box(file.allocated_size().unwrap())
        });
    });
}

#[allow(deprecated)]
fn bench_duplicate(c: &mut Criterion) {
    let tempdir = tempdir().unwrap();
    let path = tempdir.path().join("file");
    let file = bench_support::open_file(&path);

    c.bench_function("duplicate", |b| {
        bench_support::iter_primed!(b, "duplicate", || { black_box(file.duplicate().unwrap()) });
    });
}

fn bench_lock_unlock(c: &mut Criterion) {
    let (_tempdir, file) = bench_support::lock_fixture();

    c.bench_function("lock_unlock", |b| {
        bench_support::iter_primed!(b, "lock_unlock", || {
            black_box(file.fs2_lock_exclusive()).unwrap();
            black_box(file.fs2_unlock()).unwrap();
        });
    });

    c.bench_function("legacy_lock_unlock", |b| {
        bench_support::iter_primed!(b, "legacy_lock_unlock", || {
            black_box(FileExt::lock_exclusive(&file)).unwrap();
            black_box(FileExt::unlock(&file)).unwrap();
        });
    });
}

fn bench_try_lock_exclusive_unlock(c: &mut Criterion) {
    let (_tempdir, file) = bench_support::lock_fixture();

    c.bench_function("try_lock_exclusive_unlock", |b| {
        bench_support::iter_primed!(b, "try_lock_exclusive_unlock", || {
            black_box(file.fs2_try_lock_exclusive()).unwrap();
            black_box(file.fs2_unlock()).unwrap();
        });
    });
}

fn bench_free_space(c: &mut Criterion) {
    bench_support::bench_scalar_space(c, "free_space", "free_space", |path| free_space(path));
}

fn bench_available_space(c: &mut Criterion) {
    bench_support::bench_scalar_space(c, "available_space", "available_space", |path| {
        available_space(path)
    });
}

fn bench_total_space(c: &mut Criterion) {
    bench_support::bench_scalar_space(c, "total_space", "total_space", |path| total_space(path));
}

fn bench_stats_snapshot(c: &mut Criterion) {
    bench_support::bench_stats_snapshot::<StatsSubject>(c);
}

fn bench_prepared_stats(c: &mut Criterion) {
    let fixture = bench_support::StatsFixture::new();
    let path = fixture.path();
    let query = FsStatsQuery::new(path).unwrap();
    let path_snapshot = |path: &Path| -> (u64, u64, u64, u64) {
        bench_support::observe(
            statvfs(path).map(|stats| {
                (
                    stats.free_space(),
                    stats.available_space(),
                    stats.total_space(),
                    stats.allocation_granularity(),
                )
            }),
            (0, 0, 0, 0),
            "prepared_stats.path_snapshot",
        )
    };
    let query_snapshot = |query: &FsStatsQuery| -> (u64, u64, u64, u64) {
        bench_support::observe(
            query.snapshot().map(|stats| {
                (
                    stats.free_space(),
                    stats.available_space(),
                    stats.total_space(),
                    stats.allocation_granularity(),
                )
            }),
            (0, 0, 0, 0),
            "prepared_stats.query_snapshot",
        )
    };
    #[cfg(windows)]
    let long_path = long_directory(path);
    let mut group = c.benchmark_group("prepared_stats");

    group.bench_function("construct_query", |b| {
        let _probe = bench_support::FailureProbe::new("prepared_stats.construct_query");
        bench_support::iter_primed!(b, "prepared_stats/construct_query", || {
            black_box(FsStatsQuery::new(path).unwrap())
        });
    });
    #[cfg(windows)]
    group.bench_function("construct_long_query", |b| {
        let _probe = bench_support::FailureProbe::new("prepared_stats.construct_long_query");
        bench_support::iter_primed!(b, "prepared_stats/construct_long_query", || {
            black_box(FsStatsQuery::new(&long_path).unwrap())
        });
    });
    group.bench_function("one_prepared_snapshot", |b| {
        let _probe = bench_support::FailureProbe::new("prepared_stats.one_prepared_snapshot");
        bench_support::iter_primed!(b, "prepared_stats/one_prepared_snapshot", || {
            black_box(query_snapshot(&query))
        });
    });
    group.bench_function("four_top_level_snapshots", |b| {
        let _probe = bench_support::FailureProbe::new("prepared_stats.four_top_level_snapshots");
        bench_support::iter_primed!(b, "prepared_stats/four_top_level_snapshots", || {
            black_box((
                path_snapshot(path),
                path_snapshot(path),
                path_snapshot(path),
                path_snapshot(path),
            ))
        });
    });
    group.bench_function("construct_and_four_prepared_snapshots", |b| {
        let _probe = bench_support::FailureProbe::new(
            "prepared_stats.construct_and_four_prepared_snapshots",
        );
        bench_support::iter_primed!(
            b,
            "prepared_stats/construct_and_four_prepared_snapshots",
            || {
                let query = FsStatsQuery::new(path).unwrap();
                black_box((
                    query_snapshot(&query),
                    query_snapshot(&query),
                    query_snapshot(&query),
                    query_snapshot(&query),
                ))
            },
        );
    });
    group.finish();
}

fn bench_windows_file_space_fallback(c: &mut Criterion) {
    bench_support::bench_windows_file_space_fallback::<StatsSubject>(c);
}

fn bench_windows_root_stats(c: &mut Criterion) {
    #[cfg(windows)]
    {
        let current = std::env::current_dir().unwrap();
        let root = current.ancestors().last().unwrap().to_owned();
        let query = FsStatsQuery::new(&root).unwrap();
        let mut group = c.benchmark_group("windows_root_stats");

        group.bench_function("construct_query", |b| {
            let _probe = bench_support::FailureProbe::new("windows_root_stats.construct_query");
            bench_support::iter_primed!(b, "windows_root_stats/construct_query", || {
                black_box(FsStatsQuery::new(&root).unwrap())
            });
        });
        group.bench_function("one_top_level_snapshot", |b| {
            let _probe =
                bench_support::FailureProbe::new("windows_root_stats.one_top_level_snapshot");
            bench_support::iter_primed!(b, "windows_root_stats/one_top_level_snapshot", || {
                black_box(bench_support::observe(
                    statvfs(&root).map(|stats| {
                        (
                            stats.free_space(),
                            stats.available_space(),
                            stats.total_space(),
                            stats.allocation_granularity(),
                        )
                    }),
                    (0, 0, 0, 0),
                    "windows_root_stats.one_top_level_snapshot",
                ))
            });
        });
        group.bench_function("construct_and_snapshot", |b| {
            let _probe =
                bench_support::FailureProbe::new("windows_root_stats.construct_and_snapshot");
            bench_support::iter_primed!(b, "windows_root_stats/construct_and_snapshot", || {
                let query = FsStatsQuery::new(&root).unwrap();
                black_box(bench_support::observe(
                    query.snapshot().map(|stats| {
                        (
                            stats.free_space(),
                            stats.available_space(),
                            stats.total_space(),
                            stats.allocation_granularity(),
                        )
                    }),
                    (0, 0, 0, 0),
                    "windows_root_stats.construct_and_snapshot",
                ))
            });
        });
        group.bench_function("one_prepared_snapshot", |b| {
            let _probe =
                bench_support::FailureProbe::new("windows_root_stats.one_prepared_snapshot");
            bench_support::iter_primed!(b, "windows_root_stats/one_prepared_snapshot", || {
                black_box(bench_support::observe(
                    query.snapshot().map(|stats| {
                        (
                            stats.free_space(),
                            stats.available_space(),
                            stats.total_space(),
                            stats.allocation_granularity(),
                        )
                    }),
                    (0, 0, 0, 0),
                    "windows_root_stats.one_prepared_snapshot",
                ))
            });
        });
        group.bench_function("free_space", |b| {
            let _probe = bench_support::FailureProbe::new("windows_root_stats.free_space");
            bench_support::iter_primed!(b, "windows_root_stats/free_space", || {
                black_box(bench_support::observe(
                    free_space(&root),
                    0,
                    "windows_root_stats.free_space",
                ))
            });
        });
        group.bench_function("available_space", |b| {
            let _probe = bench_support::FailureProbe::new("windows_root_stats.available_space");
            bench_support::iter_primed!(b, "windows_root_stats/available_space", || {
                black_box(bench_support::observe(
                    available_space(&root),
                    0,
                    "windows_root_stats.available_space",
                ))
            });
        });
        group.bench_function("total_space", |b| {
            let _probe = bench_support::FailureProbe::new("windows_root_stats.total_space");
            bench_support::iter_primed!(b, "windows_root_stats/total_space", || {
                black_box(bench_support::observe(
                    total_space(&root),
                    0,
                    "windows_root_stats.total_space",
                ))
            });
        });
        group.bench_function("allocation_granularity", |b| {
            let _probe =
                bench_support::FailureProbe::new("windows_root_stats.allocation_granularity");
            bench_support::iter_primed!(b, "windows_root_stats/allocation_granularity", || {
                black_box(bench_support::observe(
                    allocation_granularity(&root),
                    0,
                    "windows_root_stats.allocation_granularity",
                ))
            });
        });
        group.finish();
    }

    #[cfg(not(windows))]
    let _ = c;
}

criterion_group!(
    benches,
    bench_file_create,
    bench_file_truncate,
    bench_file_allocate,
    bench_file_allocate_already_satisfied,
    bench_allocated_size,
    bench_duplicate,
    bench_lock_unlock,
    bench_try_lock_exclusive_unlock,
    bench_free_space,
    bench_available_space,
    bench_total_space,
    bench_stats_snapshot,
    bench_prepared_stats,
    bench_windows_file_space_fallback,
    bench_windows_root_stats,
);
criterion_main!(benches);
