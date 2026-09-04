use std::cell::Cell;
use std::fs::{self, File};
use std::hint::black_box;
use std::io;
use std::path::{Path, PathBuf};

use criterion::Criterion;

use super::{FailureProbe, StatsFixture, iter_primed, observe, open_file};

pub(crate) fn bench_file_create_delete(
    criterion: &mut Criterion,
    benchmark: &'static str,
    prime_label: &'static str,
) {
    const PATH_COUNT: usize = 4_096;

    let temporary = tempfile::tempdir().unwrap();
    let paths = (0..PATH_COUNT)
        .map(|index| temporary.path().join(format!("file-{index}")))
        .collect::<Vec<PathBuf>>();
    for path in &paths {
        black_box(open_file(path));
        fs::remove_file(path).unwrap();
    }
    let index = Cell::new(0usize);
    criterion.bench_function(benchmark, |bencher| {
        iter_primed!(bencher, prime_label, || {
            let path = &paths[index.get() & (PATH_COUNT - 1)];
            index.set(index.get() + 1);
            black_box(open_file(path));
            fs::remove_file(path).unwrap();
        });
    });
}

pub(crate) fn bench_file_open_truncate_delete(
    criterion: &mut Criterion,
    benchmark: &'static str,
    prime_label: &'static str,
) {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("file");
    criterion.bench_function(benchmark, |bencher| {
        iter_primed!(bencher, prime_label, || {
            let file = open_file(&path);
            file.set_len(32 * 1024 * 1024).unwrap();
            fs::remove_file(&path).unwrap();
        });
    });
}

pub(crate) fn bench_file_open_allocate_delete<F>(
    criterion: &mut Criterion,
    benchmark: &'static str,
    prime_label: &'static str,
    allocate: F,
) where
    F: Copy + Fn(&File, u64) -> io::Result<()>,
{
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("file");
    criterion.bench_function(benchmark, |bencher| {
        iter_primed!(bencher, prime_label, || {
            let file = open_file(&path);
            allocate(&file, 32 * 1024 * 1024).unwrap();
            fs::remove_file(&path).unwrap();
        });
    });
}

pub(crate) fn bench_scalar_space<F>(
    criterion: &mut Criterion,
    benchmark: &'static str,
    failure_label: &'static str,
    operation: F,
) where
    F: Copy + Fn(&Path) -> io::Result<u64>,
{
    let fixture = StatsFixture::new();
    criterion.bench_function(benchmark, |bencher| {
        let _probe = FailureProbe::new(failure_label);
        iter_primed!(bencher, benchmark, || {
            black_box(observe(operation(fixture.path()), 0, failure_label))
        });
    });
}
