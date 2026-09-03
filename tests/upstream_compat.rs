use std::fs::{File, OpenOptions};
use std::io::{Read, Result, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use fs2::{
    FileExt, FsStats, allocation_granularity, available_space, free_space, lock_contended_error,
    statvfs, total_space,
};
use tempfile::tempdir;

// Compile the complete upstream method surface in a downstream crate. The
// function is intentionally not called because several lock operations block
// when performed sequentially on one file.
#[allow(dead_code, deprecated)]
fn upstream_method_syntax<T: FileExt>(file: &T) -> Result<()> {
    let _ = file.duplicate()?;
    let _ = file.allocated_size()?;
    file.allocate(0)?;
    file.lock_shared()?;
    file.lock_exclusive()?;
    let _ = file.try_lock_shared();
    let _ = file.try_lock_exclusive();
    file.unlock()
}

#[test]
fn upstream_named_generic_function_items() {
    let _: fn(PathBuf) -> Result<FsStats> = statvfs::<PathBuf>;
    let _: fn(PathBuf) -> Result<u64> = free_space::<PathBuf>;
    let _: fn(PathBuf) -> Result<u64> = available_space::<PathBuf>;
    let _: fn(PathBuf) -> Result<u64> = total_space::<PathBuf>;
    let _: fn(PathBuf) -> Result<u64> = allocation_granularity::<PathBuf>;
}

#[test]
#[allow(deprecated)]
fn upstream_duplicate_and_allocation_surface() {
    let tempdir = tempdir().unwrap();
    let path = tempdir.path().join("fs2");
    let mut original = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .unwrap();

    original.write_all(b"fs2").unwrap();
    let mut duplicate = original.duplicate().unwrap();
    let mut at_shared_offset = Vec::new();
    duplicate.read_to_end(&mut at_shared_offset).unwrap();
    assert!(at_shared_offset.is_empty());

    duplicate.seek(SeekFrom::Start(0)).unwrap();
    duplicate.read_to_end(&mut at_shared_offset).unwrap();
    assert_eq!(at_shared_offset, b"fs2");

    original.allocate(0).unwrap();
    assert!(original.allocated_size().unwrap() >= original.metadata().unwrap().len());
}

#[test]
fn upstream_legacy_lock_surface() {
    let tempdir = tempdir().unwrap();
    let path = tempdir.path().join("fs2");
    let first = open_file(&path);
    let second = open_file(&path);

    FileExt::lock_exclusive(&first).unwrap();
    assert_eq!(
        FileExt::try_lock_shared(&second).unwrap_err().kind(),
        lock_contended_error().kind()
    );
    FileExt::unlock(&first).unwrap();
    FileExt::lock_shared(&second).unwrap();
    FileExt::unlock(&second).unwrap();
}

#[test]
fn upstream_statistics_surface() {
    let tempdir = tempdir().unwrap();
    let path = tempdir.path();
    let stats = statvfs(path).unwrap();

    assert!(free_space(path).unwrap() > 0);
    let _ = available_space(path).unwrap();
    assert!(total_space(path).unwrap() > 0);
    assert_eq!(
        allocation_granularity(path).unwrap(),
        stats.allocation_granularity()
    );
}

fn open_file(path: &Path) -> File {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .unwrap()
}
