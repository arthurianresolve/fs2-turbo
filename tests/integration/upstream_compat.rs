use std::fs::{File, OpenOptions};
use std::io::{Read, Result, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use fs2::{
    FileExt, FsStats, FsStatsQuery, allocation_granularity, available_space, free_space,
    lock_contended_error, statvfs, total_space,
};
use tempfile::tempdir;

const UPSTREAM_SURFACE_WORKER_RECEIPT: &str = "FS2_UPSTREAM_SURFACE_WORKER_RECEIPT";

const PUBLIC_API_CONTRACTS: &[&str] = &[
    "FileExt::duplicate",
    "FileExt::allocated_size",
    "FileExt::allocate",
    "FileExt::lock_shared",
    "FileExt::unlock",
    "FileExt::lock_exclusive",
    "FileExt::try_lock_shared",
    "FileExt::try_lock_exclusive",
    "FileExt::fs2_lock_shared",
    "FileExt::fs2_unlock",
    "FileExt::fs2_lock_exclusive",
    "FileExt::fs2_try_lock_shared",
    "FileExt::fs2_try_lock_exclusive",
    "lock_contended_error",
    "statvfs",
    "free_space",
    "available_space",
    "total_space",
    "allocation_granularity",
    "FsStats::free_space",
    "FsStats::available_space",
    "FsStats::total_space",
    "FsStats::allocation_granularity",
    "FsStatsQuery::new",
    "FsStatsQuery::snapshot",
];

fn assert_snapshot_contract(stats: FsStats) {
    assert!(stats.available_space() <= stats.free_space());
    assert!(stats.available_space() <= stats.total_space());
    assert!(stats.allocation_granularity() > 0);
}

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
    assert_snapshot_contract(stats);

    let query = FsStatsQuery::new(tempdir.path()).unwrap();
    assert_snapshot_contract(query.snapshot().unwrap());
}

#[test]
#[allow(deprecated)]
fn public_api_contract_inventory() -> Result<()> {
    let directory = tempdir()?;
    let path = directory.path();
    let file = open_file(&path.join("contract-inventory"));
    let mut exercised = Vec::with_capacity(PUBLIC_API_CONTRACTS.len());

    drop(FileExt::duplicate(&file)?);
    exercised.push("FileExt::duplicate");
    let _ = FileExt::allocated_size(&file)?;
    exercised.push("FileExt::allocated_size");
    FileExt::allocate(&file, 0)?;
    exercised.push("FileExt::allocate");

    FileExt::lock_shared(&file)?;
    exercised.push("FileExt::lock_shared");
    FileExt::unlock(&file)?;
    exercised.push("FileExt::unlock");
    FileExt::lock_exclusive(&file)?;
    exercised.push("FileExt::lock_exclusive");
    FileExt::unlock(&file)?;
    FileExt::try_lock_shared(&file)?;
    exercised.push("FileExt::try_lock_shared");
    FileExt::unlock(&file)?;
    FileExt::try_lock_exclusive(&file)?;
    exercised.push("FileExt::try_lock_exclusive");
    FileExt::unlock(&file)?;

    FileExt::fs2_lock_shared(&file)?;
    exercised.push("FileExt::fs2_lock_shared");
    FileExt::fs2_unlock(&file)?;
    exercised.push("FileExt::fs2_unlock");
    FileExt::fs2_lock_exclusive(&file)?;
    exercised.push("FileExt::fs2_lock_exclusive");
    FileExt::fs2_unlock(&file)?;
    FileExt::fs2_try_lock_shared(&file)?;
    exercised.push("FileExt::fs2_try_lock_shared");
    FileExt::fs2_unlock(&file)?;
    FileExt::fs2_try_lock_exclusive(&file)?;
    exercised.push("FileExt::fs2_try_lock_exclusive");
    FileExt::fs2_unlock(&file)?;

    let _ = lock_contended_error();
    exercised.push("lock_contended_error");
    let stats = statvfs(path)?;
    exercised.push("statvfs");
    let _ = free_space(path)?;
    exercised.push("free_space");
    let _ = available_space(path)?;
    exercised.push("available_space");
    let _ = total_space(path)?;
    exercised.push("total_space");
    let _ = allocation_granularity(path)?;
    exercised.push("allocation_granularity");

    let _ = stats.free_space();
    exercised.push("FsStats::free_space");
    let _ = stats.available_space();
    exercised.push("FsStats::available_space");
    let _ = stats.total_space();
    exercised.push("FsStats::total_space");
    let _ = stats.allocation_granularity();
    exercised.push("FsStats::allocation_granularity");

    let query = FsStatsQuery::new(path)?;
    exercised.push("FsStatsQuery::new");
    let queried = query.snapshot()?;
    assert_snapshot_contract(queried);
    exercised.push("FsStatsQuery::snapshot");

    assert_eq!(exercised, PUBLIC_API_CONTRACTS);
    Ok(())
}

#[cfg(unix)]
#[test]
fn unix_statistics_reject_embedded_nul_paths() {
    use std::ffi::OsStr;
    use std::io::ErrorKind;
    use std::os::unix::ffi::OsStrExt;

    let query_path = Path::new(OsStr::from_bytes(b"/fs2\0query"));
    assert_eq!(
        FsStatsQuery::new(query_path).unwrap_err().kind(),
        ErrorKind::InvalidInput
    );

    let mut long_path = vec![b'a'; 3584];
    long_path[0] = b'/';
    let midpoint = long_path.len() / 2;
    long_path[midpoint] = 0;
    let long_path = Path::new(OsStr::from_bytes(&long_path));
    assert_eq!(
        statvfs(long_path).unwrap_err().kind(),
        ErrorKind::InvalidInput
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
    let query = FsStatsQuery::new(path).unwrap();
    let queried = query.snapshot().unwrap();

    assert!(free_space(path).unwrap() > 0);
    let _ = available_space(path).unwrap();
    assert!(total_space(path).unwrap() > 0);
    assert!(stats.free_space() <= stats.total_space());
    assert!(stats.available_space() <= stats.total_space());
    assert!(queried.free_space() <= queried.total_space());
    assert!(queried.available_space() <= queried.total_space());
    assert_snapshot_contract(stats);
    assert_snapshot_contract(queried);
    assert_eq!(
        allocation_granularity(path).unwrap(),
        stats.allocation_granularity()
    );
    assert_eq!(
        queried.allocation_granularity(),
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
