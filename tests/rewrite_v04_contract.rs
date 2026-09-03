#![allow(deprecated)]

use fs2::{
    FileExt, allocation_granularity, available_space, free_space, lock_contended_error, statvfs,
    total_space,
};
use std::fs::OpenOptions;

#[test]
fn v04_surface_remains_callable() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("contract");
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .unwrap();

    let _duplicate = FileExt::duplicate(&file).unwrap();
    let _ = FileExt::allocated_size(&file).unwrap();
    FileExt::allocate(&file, 1).unwrap();
    FileExt::lock_shared(&file).unwrap();
    FileExt::unlock(&file).unwrap();
    FileExt::lock_exclusive(&file).unwrap();
    FileExt::unlock(&file).unwrap();
    FileExt::try_lock_shared(&file).unwrap();
    FileExt::unlock(&file).unwrap();
    FileExt::try_lock_exclusive(&file).unwrap();
    FileExt::unlock(&file).unwrap();

    let snapshot = statvfs(&path).unwrap();
    let _ = snapshot.free_space();
    let _ = snapshot.available_space();
    let _ = snapshot.total_space();
    let _ = snapshot.allocation_granularity();
    let _ = free_space(&path).unwrap();
    let _ = available_space(&path).unwrap();
    let _ = total_space(&path).unwrap();
    let _ = allocation_granularity(&path).unwrap();
    let _ = lock_contended_error();
}
#[test]
fn explicit_fs2_lock_aliases_remain_callable() {
    let directory = tempfile::tempdir().unwrap();
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(directory.path().join("aliases"))
        .unwrap();
    FileExt::fs2_lock_shared(&file).unwrap();
    FileExt::fs2_unlock(&file).unwrap();
    FileExt::fs2_lock_exclusive(&file).unwrap();
    FileExt::fs2_unlock(&file).unwrap();
    FileExt::fs2_try_lock_shared(&file).unwrap();
    FileExt::fs2_unlock(&file).unwrap();
    FileExt::fs2_try_lock_exclusive(&file).unwrap();
    FileExt::fs2_unlock(&file).unwrap();
}
