use std::fs::{File, OpenOptions};
use std::io;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;
use std::path::{Component, Prefix};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation as winerror;
use windows_sys::Win32::Storage::FileSystem as winfs;
use windows_sys::Win32::System::IO::CancelIoEx;
use windows_sys::Win32::System::Ioctl as ioctl;

const WORKER_ENV: &str = "FS2_COVERAGE_PENDING_CONTROL_WORKER";
const DRAINED: &str = "FS2_NATIVE_PENDING_DRAINED";
const UNAVAILABLE: &str = "FS2_NATIVE_PENDING_UNAVAILABLE";
// GetDriveTypeW's fixed-drive return tag from the Windows SDK.
const DRIVE_FIXED: u32 = 3;

struct ProbeChild(Child);

impl Drop for ProbeChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn coverage_allocated_range_rejects_unrepresentable_lengths() {
    let file = tempfile::tempfile().unwrap();
    let error = super::requested_range_is_allocated(&file, u64::MAX).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
}

#[test]
fn coverage_regular_allocation_rejects_unrepresentable_lengths() {
    let file = tempfile::tempfile().unwrap();
    let state = super::allocation_state(&file).unwrap();
    let error = super::allocate_regular_space(&file, state, u64::MAX).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
}

#[test]
fn coverage_pending_device_control_is_drained() {
    let directory = tempfile::tempdir().unwrap();
    let stdout_path = directory.path().join("pending.stdout");
    let stderr_path = directory.path().join("pending.stderr");
    let module = module_path!().split_once("::").unwrap().1;
    let mut child = ProbeChild(
        Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg(format!("{module}::coverage_native_pending_worker"))
            .arg("--nocapture")
            .arg("--test-threads=1")
            .env(WORKER_ENV, "1")
            .stdin(Stdio::null())
            .stdout(Stdio::from(File::create(&stdout_path).unwrap()))
            .stderr(Stdio::from(File::create(&stderr_path).unwrap()))
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(15);
    let status = loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            break status;
        }
        assert!(Instant::now() < deadline, "pending-control probe timed out");
        thread::sleep(Duration::from_millis(10));
    };
    let stdout = std::fs::read_to_string(&stdout_path).unwrap();
    let stderr = std::fs::read_to_string(&stderr_path).unwrap();
    assert!(status.success(), "{status}:\n{stdout}\n{stderr}");
    if stdout.contains(UNAVAILABLE) {
        eprintln!("{stdout}");
    } else {
        assert!(stdout.contains(DRAINED), "worker did not run: {stdout}");
    }
}

#[test]
fn coverage_native_pending_worker() {
    if std::env::var_os(WORKER_ENV).as_deref() != Some(std::ffi::OsStr::new("1")) {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let drive = match directory.path().components().next() {
        Some(Component::Prefix(prefix)) => match prefix.kind() {
            Prefix::Disk(drive) | Prefix::VerbatimDisk(drive) => drive,
            _ => {
                println!("{UNAVAILABLE}: temporary directory is not on a local drive");
                return;
            }
        },
        _ => panic!("temporary directory is not absolute"),
    };
    let root = [u16::from(drive), u16::from(b':'), u16::from(b'\\'), 0];
    // SAFETY: root is a terminated drive-root string, valid for this call.
    if unsafe { winfs::GetDriveTypeW(root.as_ptr()) } != DRIVE_FIXED {
        println!("{UNAVAILABLE}: pending-control probe requires a fixed local drive");
        return;
    }
    let file = Arc::new(
        OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .custom_flags(winfs::FILE_FLAG_OVERLAPPED)
            .open(directory.path().join("pending-oplock"))
            .unwrap(),
    );
    let finished = Arc::new(AtomicBool::new(false));
    let cancel_file = Arc::clone(&file);
    let cancel_finished = Arc::clone(&finished);
    let cancel = thread::spawn(move || {
        thread::sleep(Duration::from_millis(25));
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if cancel_finished.load(Ordering::Relaxed) {
                return Ok(false);
            }
            // SAFETY: this private handle stays owned by the Arc. Its only I/O
            // request is the probe; the production helper drains it before return.
            if unsafe { CancelIoEx(cancel_file.as_raw_handle(), std::ptr::null()) } != 0 {
                return Ok(true);
            }
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(winerror::ERROR_NOT_FOUND as i32) {
                return Err(error);
            }
            thread::sleep(Duration::from_millis(10));
        }
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "no pending control request could be canceled",
        ))
    });
    let input = ioctl::REQUEST_OPLOCK_INPUT_BUFFER {
        StructureVersion: u16::try_from(ioctl::REQUEST_OPLOCK_CURRENT_VERSION).unwrap(),
        StructureLength: u16::try_from(std::mem::size_of::<ioctl::REQUEST_OPLOCK_INPUT_BUFFER>())
            .unwrap(),
        RequestedOplockLevel: ioctl::OPLOCK_LEVEL_CACHE_READ,
        Flags: ioctl::REQUEST_OPLOCK_INPUT_FLAG_REQUEST,
    };
    let mut output = ioctl::REQUEST_OPLOCK_OUTPUT_BUFFER::default();
    // SAFETY: the owned file and both correctly sized buffers remain alive until
    // the production helper has observed completion, including cancellation.
    let result = unsafe {
        super::overlapped_device_io_control(
            &file,
            ioctl::FSCTL_REQUEST_OPLOCK,
            std::ptr::from_ref(&input).cast(),
            u32::try_from(std::mem::size_of_val(&input)).unwrap(),
            std::ptr::from_mut(&mut output).cast(),
            u32::try_from(std::mem::size_of_val(&output)).unwrap(),
        )
    };
    finished.store(true, Ordering::Relaxed);
    let canceled = cancel.join().unwrap().unwrap();
    match result {
        Err((error, _))
            if error.raw_os_error() == Some(winerror::ERROR_OPERATION_ABORTED as i32) =>
        {
            assert!(
                canceled,
                "completion reported cancellation without our request"
            );
        }
        Ok(_) => {
            assert_eq!(
                u32::from(output.StructureVersion),
                ioctl::REQUEST_OPLOCK_CURRENT_VERSION
            );
        }
        Err((error, _)) => {
            let unsupported = [
                winerror::ERROR_INVALID_FUNCTION,
                winerror::ERROR_NOT_SUPPORTED,
                winerror::ERROR_OPLOCK_NOT_GRANTED,
                winerror::ERROR_CANNOT_GRANT_REQUESTED_OPLOCK,
            ]
            .contains(&(error.raw_os_error().unwrap_or_default() as u32));
            if unsupported {
                println!("{UNAVAILABLE}: {error}");
                return;
            }
            panic!("pending-control probe failed: {error}");
        }
    }
    println!("{DRAINED}");
}
