use std::collections::BTreeSet;
use std::fs::{File, OpenOptions};
use std::hint::black_box;
use std::path::Path;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use tempfile::{TempDir, tempdir};

#[path = "basic_benches.rs"]
mod basic_benches;
#[path = "bench_prime.rs"]
mod prime;
#[path = "bench_reporting.rs"]
mod reporting;
#[path = "stats_benches.rs"]
mod stats_benches;

pub(crate) use basic_benches::{
    bench_file_create_delete, bench_file_open_allocate_delete, bench_file_open_truncate_delete,
    bench_scalar_space,
};
pub(crate) use prime::iter_primed;
pub(crate) use reporting::{FailureProbe, observe};
pub(crate) use stats_benches::{
    Snapshot, StatsSubject, bench_stats_snapshot, bench_windows_file_space_fallback,
};

pub(crate) struct StatsFixture {
    path: PathBuf,
    _temporary: Option<TempDir>,
}

impl StatsFixture {
    pub(crate) fn new() -> Self {
        if let Some(path) = std::env::var_os("FS2_BENCH_STATS_PATH") {
            Self {
                path: PathBuf::from(path),
                _temporary: None,
            }
        } else {
            let temporary = tempdir().unwrap();
            Self {
                path: temporary.path().to_owned(),
                _temporary: Some(temporary),
            }
        }
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

pub(crate) fn record_prime_once<F, O>(label: &'static str, operation: &mut F)
where
    F: FnMut() -> O,
{
    static PRIMED: OnceLock<Mutex<BTreeSet<&'static str>>> = OnceLock::new();
    if !PRIMED
        .get_or_init(|| Mutex::new(BTreeSet::new()))
        .lock()
        .unwrap()
        .insert(label)
    {
        return;
    }
    let started = Instant::now();
    let _ = black_box(operation());
    reporting::report_prime(label, started.elapsed().as_nanos());
}

pub(crate) fn open_file(path: &Path) -> File {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .unwrap()
}

pub(crate) fn lock_fixture() -> (TempDir, File) {
    let tempdir = tempdir().unwrap();
    let file = open_file(&tempdir.path().join("file"));
    (tempdir, file)
}
