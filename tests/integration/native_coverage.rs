#[cfg(any(target_os = "linux", windows))]
const REQUIRE_NATIVE_FIXTURES: &str = "FS2_COVERAGE_REQUIRE_NATIVE_FIXTURES";

#[cfg(any(target_os = "linux", windows))]
macro_rules! native_fixture_unavailable {
    ($context:expr, $error:expr) => {{
        if std::env::var_os(REQUIRE_NATIVE_FIXTURES).as_deref() == Some(std::ffi::OsStr::new("1")) {
            panic!(
                "required native coverage fixture is unavailable ({}): {}",
                $context, $error
            );
        }
        eprintln!(
            "native coverage fixture unavailable ({}): {}",
            $context, $error
        );
    }};
}

#[cfg(target_os = "linux")]
#[test]
fn linux_keep_size_reservation_extends_logical_length() {
    use std::os::fd::AsRawFd;

    use fs2::FileExt;

    const RESERVED_LENGTH: u64 = 64 * 1024;

    let temporary = tempfile::NamedTempFile::new().unwrap();
    let file = temporary.as_file();
    let result = unsafe {
        // SAFETY: `file` owns the descriptor, and this bounded operation affects
        // only the temporary file while preserving its logical length.
        libc::fallocate(
            file.as_raw_fd(),
            libc::FALLOC_FL_KEEP_SIZE,
            0,
            RESERVED_LENGTH.try_into().unwrap(),
        )
    };
    if result != 0 {
        let error = std::io::Error::last_os_error();
        let unsupported = match error.raw_os_error() {
            Some(code) => [libc::EOPNOTSUPP, libc::ENOSYS, libc::EINVAL].contains(&code),
            None => false,
        };
        if unsupported {
            native_fixture_unavailable!("Linux FALLOC_FL_KEEP_SIZE", &error);
            return;
        }
        panic!("unable to reserve the Linux keep-size fixture: {error}");
    }

    assert_eq!(file.metadata().unwrap().len(), 0);
    let allocated = FileExt::allocated_size(file).unwrap();
    if allocated < RESERVED_LENGTH {
        let error = std::io::Error::other(format!(
            "filesystem reported only {allocated} allocated bytes after reserving {RESERVED_LENGTH}"
        ));
        native_fixture_unavailable!("Linux allocated-block accounting", &error);
        return;
    }

    FileExt::allocate(file, RESERVED_LENGTH).unwrap();
    assert_eq!(file.metadata().unwrap().len(), RESERVED_LENGTH);
    assert!(FileExt::allocated_size(file).unwrap() >= RESERVED_LENGTH);
}

#[cfg(windows)]
fn mark_sparse(file: &std::fs::File) -> std::io::Result<()> {
    use std::os::windows::io::AsRawHandle;

    use windows_sys::Win32::System::IO::DeviceIoControl;
    use windows_sys::Win32::System::Ioctl::FSCTL_SET_SPARSE;

    let mut returned = 0;
    let result = unsafe {
        // SAFETY: `file` owns a synchronous handle, this control has no input
        // or output buffer, and `returned` remains valid for the call.
        DeviceIoControl(
            file.as_raw_handle(),
            FSCTL_SET_SPARSE,
            std::ptr::null(),
            0,
            std::ptr::null_mut(),
            0,
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    if result == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(windows)]
#[test]
fn windows_overlapped_sparse_allocation_preserves_tail() {
    use std::fs::{self, OpenOptions};
    use std::io::{Read, Seek, SeekFrom, Write};
    use std::os::windows::fs::{MetadataExt, OpenOptionsExt};

    use fs2::FileExt;
    use windows_sys::Win32::Foundation::{
        ERROR_INVALID_FUNCTION, ERROR_INVALID_PARAMETER, ERROR_NOT_SUPPORTED,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_SPARSE_FILE, FILE_FLAG_OVERLAPPED,
    };

    const FILE_LENGTH: u64 = 2 * 1024 * 1024;
    const REQUESTED_ALLOCATION: u64 = 1024 * 1024;
    const SENTINEL: &[u8] = b"fs2 sparse tail";
    const TAIL_OFFSET: u64 = FILE_LENGTH - 4096;

    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("overlapped-sparse");
    let mut setup = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&path)
        .unwrap();
    if let Err(error) = mark_sparse(&setup) {
        let unsupported = match error.raw_os_error() {
            Some(code) => [
                ERROR_INVALID_FUNCTION as i32,
                ERROR_INVALID_PARAMETER as i32,
                ERROR_NOT_SUPPORTED as i32,
            ]
            .contains(&code),
            None => false,
        };
        if unsupported {
            native_fixture_unavailable!("Windows sparse files", &error);
            return;
        }
        panic!("unable to create the Windows sparse fixture: {error}");
    }
    setup.set_len(FILE_LENGTH).unwrap();
    setup.seek(SeekFrom::Start(TAIL_OFFSET)).unwrap();
    setup.write_all(SENTINEL).unwrap();
    setup.flush().unwrap();
    drop(setup);

    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(FILE_FLAG_OVERLAPPED)
        .open(&path)
        .unwrap();
    if let Err(error) = FileExt::allocate(&file, REQUESTED_ALLOCATION) {
        if error.kind() == std::io::ErrorKind::Unsupported {
            native_fixture_unavailable!("Windows sparse-range allocation", &error);
            return;
        }
        panic!("Windows sparse-range allocation failed: {error}");
    }
    drop(file);

    let metadata = fs::metadata(&path).unwrap();
    assert_ne!(
        metadata.file_attributes() & FILE_ATTRIBUTE_SPARSE_FILE,
        0,
        "allocation should restore the sparse-file attribute"
    );
    let mut reader = std::fs::File::open(&path).unwrap();
    assert!(FileExt::allocated_size(&reader).unwrap() >= REQUESTED_ALLOCATION);
    let mut tail = vec![0; SENTINEL.len()];
    reader.seek(SeekFrom::Start(TAIL_OFFSET)).unwrap();
    reader.read_exact(&mut tail).unwrap();
    assert_eq!(tail, SENTINEL);
}
