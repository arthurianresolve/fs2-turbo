use std::fs::File;
use std::hint::black_box;
use std::io::Result;
use std::path::Path;

use criterion::{Criterion, criterion_group, criterion_main};
#[path = "common/bench_support.rs"]
mod bench_support;
use fs2::{FileExt, allocation_granularity, available_space, free_space, statvfs, total_space};
use tempfile::tempdir;

#[cfg(all(feature = "subject-fs2", feature = "subject-fs4"))]
compile_error!("subject-fs2 and subject-fs4 are mutually exclusive");

struct StatsSubject;

impl bench_support::StatsSubject for StatsSubject {
    const SNAPSHOT_GROUP: &'static str = "fs_compat/stats_snapshot";
    const SNAPSHOT_ONE_ID: &'static str = "fs_compat/stats_snapshot/one_snapshot";
    const SNAPSHOT_FOUR_ID: &'static str = "fs_compat/stats_snapshot/four_convenience_queries";
    #[cfg(windows)]
    const FILE_FREE_BENCH: &'static str = "fs_compat/free_space_file_fallback";
    #[cfg(windows)]
    const FILE_FREE_ID: &'static str = "fs_compat/free_space_file_fallback";
    #[cfg(windows)]
    const FILE_AVAILABLE_BENCH: &'static str = "fs_compat/available_space_file_fallback";
    #[cfg(windows)]
    const FILE_AVAILABLE_ID: &'static str = "fs_compat/available_space_file_fallback";

    fn free_space(path: &Path) -> Result<u64> {
        free_space(path)
    }

    fn available_space(path: &Path) -> Result<u64> {
        available_space(path)
    }

    fn total_space(path: &Path) -> Result<u64> {
        total_space(path)
    }

    fn allocation_granularity(path: &Path) -> Result<u64> {
        allocation_granularity(path)
    }

    fn snapshot(path: &Path) -> Result<bench_support::Snapshot> {
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

#[cfg(not(any(feature = "subject-fs2", feature = "subject-fs4")))]
compile_error!("select one benchmark subject");

// Cargo feature unification may enable both subjects during `--all-features`
// checks. The default fs2 subject takes precedence; controlled fs4 runs disable
// default features and select only `subject-fs4`.
#[cfg(feature = "subject-fs2")]
fn lock_exclusive(file: &File) -> Result<()> {
    FileExt::lock_exclusive(file)
}

#[cfg(all(not(feature = "subject-fs2"), feature = "subject-fs4"))]
fn lock_exclusive(file: &File) -> Result<()> {
    FileExt::lock_exclusive(file)
}

fn bench_file_create(c: &mut Criterion) {
    bench_support::bench_file_create_delete(
        c,
        "fs_compat/file_create_delete",
        "fs_compat/file_create_delete",
    );
}

fn bench_file_truncate(c: &mut Criterion) {
    bench_support::bench_file_open_truncate_delete(
        c,
        "fs_compat/file_open_truncate_delete",
        "fs_compat/file_open_truncate_delete",
    );
}

fn bench_file_allocate(c: &mut Criterion) {
    bench_support::bench_file_open_allocate_delete(
        c,
        "fs_compat/file_open_allocate_delete",
        "fs_compat/file_open_allocate_delete",
        FileExt::allocate,
    );
}

fn bench_file_allocate_already_satisfied(c: &mut Criterion) {
    let tempdir = tempdir().unwrap();
    let path = tempdir.path().join("file");
    let file = bench_support::open_file(&path);
    FileExt::allocate(&file, 32 * 1024 * 1024).unwrap();

    c.bench_function("fs_compat/file_allocate_already_satisfied", |b| {
        bench_support::iter_primed!(b, "fs_compat/file_allocate_already_satisfied", || {
            FileExt::allocate(&file, 32 * 1024 * 1024).unwrap()
        });
    });
}

fn bench_allocated_size(c: &mut Criterion) {
    let tempdir = tempdir().unwrap();
    let path = tempdir.path().join("file");
    let file = bench_support::open_file(&path);
    FileExt::allocate(&file, 32 * 1024 * 1024).unwrap();

    c.bench_function("fs_compat/allocated_size", |b| {
        bench_support::iter_primed!(b, "fs_compat/allocated_size", || {
            black_box(FileExt::allocated_size(&file).unwrap())
        });
    });
}

fn bench_lock_unlock(c: &mut Criterion) {
    let (_tempdir, file) = bench_support::lock_fixture();

    c.bench_function("fs_compat/lock_unlock", |b| {
        bench_support::iter_primed!(b, "fs_compat/lock_unlock", || {
            black_box(lock_exclusive(&file)).unwrap();
            black_box(FileExt::unlock(&file)).unwrap();
        });
    });
}

fn bench_free_space(c: &mut Criterion) {
    bench_support::bench_scalar_space(c, "fs_compat/free_space", "free_space", |path| {
        free_space(path)
    });
}

fn bench_available_space(c: &mut Criterion) {
    bench_support::bench_scalar_space(c, "fs_compat/available_space", "available_space", |path| {
        available_space(path)
    });
}

fn bench_total_space(c: &mut Criterion) {
    bench_support::bench_scalar_space(c, "fs_compat/total_space", "total_space", |path| {
        total_space(path)
    });
}

fn bench_stats_snapshot(c: &mut Criterion) {
    bench_support::bench_stats_snapshot::<StatsSubject>(c);
}

fn bench_windows_file_space_fallback(c: &mut Criterion) {
    bench_support::bench_windows_file_space_fallback::<StatsSubject>(c);
}

fn bench_windows_root_stats(c: &mut Criterion) {
    #[cfg(windows)]
    {
        let current = std::env::current_dir().unwrap();
        let root = current.ancestors().last().unwrap().to_owned();
        let mut group = c.benchmark_group("fs_compat/windows_root_stats");

        group.bench_function("one_top_level_snapshot", |b| {
            let _probe =
                bench_support::FailureProbe::new("windows_root_stats.one_top_level_snapshot");
            bench_support::iter_primed!(
                b,
                "fs_compat/windows_root_stats/one_top_level_snapshot",
                || {
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
                },
            );
        });
        group.bench_function("total_space", |b| {
            let _probe = bench_support::FailureProbe::new("windows_root_stats.total_space");
            bench_support::iter_primed!(b, "fs_compat/windows_root_stats/total_space", || {
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
            bench_support::iter_primed!(
                b,
                "fs_compat/windows_root_stats/allocation_granularity",
                || {
                    black_box(bench_support::observe(
                        allocation_granularity(&root),
                        0,
                        "windows_root_stats.allocation_granularity",
                    ))
                },
            );
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
    bench_lock_unlock,
    bench_free_space,
    bench_available_space,
    bench_total_space,
    bench_stats_snapshot,
    bench_windows_file_space_fallback,
    bench_windows_root_stats,
);
criterion_main!(benches);
