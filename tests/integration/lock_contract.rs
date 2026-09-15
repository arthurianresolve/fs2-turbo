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
        .args([
            "--exact",
            "lock_contract::cross_process_lock_probe",
            "--nocapture",
        ])
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

struct LegacyOnly {
    calls: std::cell::RefCell<Vec<&'static str>>,
    error: Option<i32>,
}

impl LegacyOnly {
    fn record(&self, method: &'static str) -> std::io::Result<()> {
        self.calls.borrow_mut().push(method);
        match self.error {
            Some(code) => Err(std::io::Error::from_raw_os_error(code)),
            None => Ok(()),
        }
    }
}

impl FileExt for LegacyOnly {
    fn duplicate(&self) -> std::io::Result<File> {
        tempfile::tempfile()
    }

    fn allocated_size(&self) -> std::io::Result<u64> {
        Ok(0)
    }

    fn allocate(&self, _len: u64) -> std::io::Result<()> {
        Ok(())
    }

    fn lock_shared(&self) -> std::io::Result<()> {
        self.record("lock_shared")
    }

    fn lock_exclusive(&self) -> std::io::Result<()> {
        self.record("lock_exclusive")
    }

    fn try_lock_shared(&self) -> std::io::Result<()> {
        self.record("try_lock_shared")
    }

    fn try_lock_exclusive(&self) -> std::io::Result<()> {
        self.record("try_lock_exclusive")
    }

    fn unlock(&self) -> std::io::Result<()> {
        self.record("unlock")
    }
}

#[test]
#[allow(deprecated)]
fn legacy_only_implements_the_non_lock_contract() {
    let file = LegacyOnly {
        calls: std::cell::RefCell::new(Vec::new()),
        error: None,
    };

    drop(FileExt::duplicate(&file).unwrap());
    assert_eq!(FileExt::allocated_size(&file).unwrap(), 0);
    FileExt::allocate(&file, 0).unwrap();
    assert!(file.calls.into_inner().is_empty());
}

#[test]
fn default_aliases_forward_once_and_preserve_results() {
    for error in [None, Some(13)] {
        let file = LegacyOnly {
            calls: std::cell::RefCell::new(Vec::new()),
            error,
        };
        let results = [
            file.fs2_lock_shared(),
            file.fs2_lock_exclusive(),
            file.fs2_try_lock_shared(),
            file.fs2_try_lock_exclusive(),
            file.fs2_unlock(),
        ];

        assert_eq!(
            file.calls.into_inner(),
            [
                "lock_shared",
                "lock_exclusive",
                "try_lock_shared",
                "try_lock_exclusive",
                "unlock",
            ]
        );
        for result in results {
            match error {
                Some(code) => assert_eq!(result.unwrap_err().raw_os_error(), Some(code)),
                None => result.unwrap(),
            }
        }
    }
}

#[test]
fn every_legacy_lock_method_completes_on_an_uncontended_file() {
    let temporary = tempfile::tempdir().unwrap();
    let file = open_file(&temporary.path().join("legacy-success"));

    FileExt::lock_shared(&file).unwrap();
    FileExt::unlock(&file).unwrap();
    FileExt::lock_exclusive(&file).unwrap();
    FileExt::unlock(&file).unwrap();
    FileExt::try_lock_shared(&file).unwrap();
    FileExt::unlock(&file).unwrap();
    FileExt::try_lock_exclusive(&file).unwrap();
    FileExt::unlock(&file).unwrap();
}
