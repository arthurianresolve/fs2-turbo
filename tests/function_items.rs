#![cfg(unix)]

use std::fs::{File, OpenOptions};
use std::io::Result;

use fs2::{FileExt, FsStats, FsStatsQuery};

fn open_file(path: &std::path::Path) -> File {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(path)
        .unwrap()
}

#[test]
#[allow(deprecated)]
fn public_non_generic_function_items_execute() {
    let directory = tempfile::tempdir().unwrap();
    let file = open_file(&directory.path().join("function-items"));

    let duplicate: fn(&File) -> Result<File> = <File as FileExt>::duplicate;
    let allocated_size: fn(&File) -> Result<u64> = <File as FileExt>::allocated_size;
    let allocate: fn(&File, u64) -> Result<()> = <File as FileExt>::allocate;
    drop(std::hint::black_box(duplicate)(&file).unwrap());
    let _ = std::hint::black_box(allocated_size)(&file).unwrap();
    std::hint::black_box(allocate)(&file, 0).unwrap();

    let lock_shared: fn(&File) -> Result<()> = <File as FileExt>::lock_shared;
    let lock_exclusive: fn(&File) -> Result<()> = <File as FileExt>::lock_exclusive;
    let try_lock_shared: fn(&File) -> Result<()> = <File as FileExt>::try_lock_shared;
    let try_lock_exclusive: fn(&File) -> Result<()> = <File as FileExt>::try_lock_exclusive;
    let unlock: fn(&File) -> Result<()> = <File as FileExt>::unlock;
    let fs2_lock_shared: fn(&File) -> Result<()> = <File as FileExt>::fs2_lock_shared;
    let fs2_lock_exclusive: fn(&File) -> Result<()> = <File as FileExt>::fs2_lock_exclusive;
    let fs2_try_lock_shared: fn(&File) -> Result<()> = <File as FileExt>::fs2_try_lock_shared;
    let fs2_try_lock_exclusive: fn(&File) -> Result<()> = <File as FileExt>::fs2_try_lock_exclusive;
    let fs2_unlock: fn(&File) -> Result<()> = <File as FileExt>::fs2_unlock;

    for (acquire, release) in [
        (lock_shared, unlock),
        (lock_exclusive, unlock),
        (try_lock_shared, unlock),
        (try_lock_exclusive, unlock),
        (fs2_lock_shared, fs2_unlock),
        (fs2_lock_exclusive, fs2_unlock),
        (fs2_try_lock_shared, fs2_unlock),
        (fs2_try_lock_exclusive, fs2_unlock),
    ] {
        std::hint::black_box(acquire)(&file).unwrap();
        std::hint::black_box(release)(&file).unwrap();
    }

    let stats = FsStatsQuery::new(directory.path())
        .unwrap()
        .snapshot()
        .unwrap();
    for accessor in [
        FsStats::free_space as fn(&FsStats) -> u64,
        FsStats::available_space,
        FsStats::total_space,
        FsStats::allocation_granularity,
    ] {
        let _ = std::hint::black_box(accessor)(&stats);
    }
}
