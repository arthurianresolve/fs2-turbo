use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use fs2::{FileExt, lock_contended_error};

fn open_file(path: &Path) -> File {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .unwrap()
}

fn assert_contended(result: std::io::Result<()>) {
    assert_eq!(result.unwrap_err().kind(), lock_contended_error().kind());
}

#[test]
fn shared_locks_are_compatible_but_exclusive_locks_are_not() {
    let tempdir = tempfile::tempdir().unwrap();
    let path = tempdir.path().join("fs2");
    let file1 = open_file(&path);
    let file2 = open_file(&path);
    let file3 = open_file(&path);

    file1.fs2_lock_shared().unwrap();
    file2.fs2_lock_shared().unwrap();
    assert_contended(file3.fs2_try_lock_exclusive());
    file1.fs2_unlock().unwrap();
    assert_contended(file3.fs2_try_lock_exclusive());
    file2.fs2_unlock().unwrap();
    file3.fs2_lock_exclusive().unwrap();
}

#[test]
fn exclusive_locks_block_shared_and_exclusive_locks() {
    let tempdir = tempfile::tempdir().unwrap();
    let path = tempdir.path().join("fs2");
    let file1 = open_file(&path);
    let file2 = open_file(&path);

    file1.fs2_lock_exclusive().unwrap();
    assert_contended(file2.fs2_try_lock_exclusive());
    assert_contended(file2.fs2_try_lock_shared());
    file1.fs2_unlock().unwrap();
    file2.fs2_lock_exclusive().unwrap();
}

#[test]
fn dropping_a_lock_owner_releases_the_lock() {
    let tempdir = tempfile::tempdir().unwrap();
    let path = tempdir.path().join("fs2");
    let file1 = open_file(&path);
    let file2 = open_file(&path);

    file1.fs2_lock_exclusive().unwrap();
    assert_contended(file2.fs2_try_lock_shared());
    drop(file1);
    file2.fs2_lock_shared().unwrap();
}

#[test]
fn blocking_acquisition_completes_after_release() {
    let tempdir = tempfile::tempdir().unwrap();
    let path = tempdir.path().join("fs2");
    let file1 = open_file(&path);
    let file2 = open_file(&path);
    let (ready_tx, ready_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();

    file1.fs2_lock_exclusive().unwrap();
    let worker = thread::spawn(move || {
        assert_contended(file2.fs2_try_lock_shared());
        ready_tx.send(()).unwrap();
        let result = file2.fs2_lock_shared().and_then(|()| file2.fs2_unlock());
        done_tx.send(result).unwrap();
    });

    ready_rx.recv().unwrap();
    assert!(matches!(
        done_rx.recv_timeout(Duration::from_millis(250)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    file1.fs2_unlock().unwrap();
    done_rx
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .unwrap();
    worker.join().unwrap();
}

#[test]
fn cross_process_exclusive_lock_is_observed() {
    let tempdir = tempfile::tempdir().unwrap();
    let path = tempdir.path().join("fs2-cross-process");
    let file = open_file(&path);
    file.fs2_lock_exclusive().unwrap();

    let status = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "cross_process_lock_probe", "--nocapture"])
        .env("FS2_LOCK_PROBE_PATH", &path)
        .status()
        .unwrap();

    file.fs2_unlock().unwrap();
    assert!(status.success());
}

#[test]
fn cross_process_lock_probe() {
    let Some(path) = std::env::var_os("FS2_LOCK_PROBE_PATH").map(PathBuf::from) else {
        return;
    };
    let file = open_file(&path);
    assert_contended(file.fs2_try_lock_exclusive());
}

#[test]
fn legacy_lock_methods_share_the_contract() {
    let tempdir = tempfile::tempdir().unwrap();
    let path = tempdir.path().join("fs2");
    let file1 = open_file(&path);
    let file2 = open_file(&path);

    FileExt::lock_shared(&file1).unwrap();
    assert_contended(FileExt::try_lock_exclusive(&file2));
    FileExt::unlock(&file1).unwrap();
    FileExt::lock_exclusive(&file2).unwrap();
    FileExt::unlock(&file2).unwrap();
}
