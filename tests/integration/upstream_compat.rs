use std::fs::{File, OpenOptions};
use std::io::{Read, Result, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use fs2::{
    FileExt, FsStats, allocation_granularity, available_space, free_space, lock_contended_error,
    statvfs, total_space,
};
use tempfile::tempdir;

const UPSTREAM_SURFACE_WORKER_RECEIPT: &str = "FS2_UPSTREAM_SURFACE_WORKER_RECEIPT";

// Exercise the complete upstream method surface through a downstream generic.
// Every acquired lock is released before the next operation.
#[allow(deprecated)]
fn upstream_method_syntax<T: FileExt>(file: &T) -> Result<()> {
    let duplicate = file.duplicate()?;
    drop(duplicate);
    let _ = file.allocated_size()?;
    file.allocate(0)?;
    file.lock_shared()?;
    file.unlock()?;
    file.lock_exclusive()?;
    file.unlock()?;
    file.try_lock_shared()?;
    file.unlock()?;
    file.try_lock_exclusive()?;
    file.unlock()
}

#[test]
fn upstream_named_generic_function_items() {
    let statvfs_path: fn(PathBuf) -> Result<FsStats> = statvfs::<PathBuf>;
    let free_space_path: fn(PathBuf) -> Result<u64> = free_space::<PathBuf>;
    let available_space_path: fn(PathBuf) -> Result<u64> = available_space::<PathBuf>;
    let total_space_path: fn(PathBuf) -> Result<u64> = total_space::<PathBuf>;
    let allocation_granularity_path: fn(PathBuf) -> Result<u64> = allocation_granularity::<PathBuf>;

    let tempdir = tempdir().unwrap();
    let path = tempdir.path().to_path_buf();
    let stats = statvfs_path(path.clone()).unwrap();
    let free = free_space_path(path.clone()).unwrap();
    let available = available_space_path(path.clone()).unwrap();
    let total = total_space_path(path.clone()).unwrap();

    assert!(free <= total);
    assert!(available <= total);
    assert_eq!(
        allocation_granularity_path(path).unwrap(),
        stats.allocation_granularity()
    );
}

#[test]
fn upstream_duplicate_and_allocation_surface() {
    let worker_dir = tempdir().unwrap();
    let receipt = worker_dir.path().join("completed");
    let status = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "upstream_compat::upstream_duplicate_and_allocation_surface_worker",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(UPSTREAM_SURFACE_WORKER_RECEIPT, &receipt)
        .status()
        .unwrap();

    assert!(status.success(), "upstream surface worker failed: {status}");
    assert_eq!(std::fs::read(receipt).unwrap(), b"completed");
}

#[test]
#[allow(deprecated)]
fn upstream_duplicate_and_allocation_surface_worker() {
    let Some(receipt) = std::env::var_os(UPSTREAM_SURFACE_WORKER_RECEIPT).map(PathBuf::from) else {
        return;
    };
    let mut original = tempfile::tempfile().unwrap();

    upstream_method_syntax(&original).unwrap();

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
    drop(duplicate);
    std::fs::write(receipt, b"completed").unwrap();
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
