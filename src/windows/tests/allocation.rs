use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::windows::io::AsRawHandle;

use tempfile::tempdir;
use windows_sys::Win32::System::IO::DeviceIoControl;
use windows_sys::Win32::System::Ioctl::FSCTL_SET_SPARSE;

use crate::FileExt;

#[test]
fn allocation_below_sparse_eof_preserves_tail() {
    const FILE_LENGTH: u64 = 16 * 1024 * 1024;
    const REQUESTED_ALLOCATION: u64 = 1024 * 1024;
    const TAIL_OFFSET: u64 = FILE_LENGTH - 4096;
    const SENTINEL: &[u8] = b"fs2 sparse tail";

    let temporary = tempdir().unwrap();
    let mut file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(temporary.path().join("sparse"))
        .unwrap();
    let mut returned = 0;
    let result = unsafe {
        // SAFETY: `file` owns a valid handle, this control code has no input or
        // output buffer, and `returned` is valid output storage.
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
    assert_ne!(result, 0, "{}", std::io::Error::last_os_error());

    file.set_len(FILE_LENGTH).unwrap();
    file.seek(SeekFrom::Start(TAIL_OFFSET)).unwrap();
    file.write_all(SENTINEL).unwrap();
    file.flush().unwrap();
    assert!(file.allocated_size().unwrap() < REQUESTED_ALLOCATION);

    file.allocate(REQUESTED_ALLOCATION).unwrap();

    assert_eq!(file.metadata().unwrap().len(), FILE_LENGTH);
    let mut tail = vec![0; SENTINEL.len()];
    file.seek(SeekFrom::Start(TAIL_OFFSET)).unwrap();
    file.read_exact(&mut tail).unwrap();
    assert_eq!(tail, SENTINEL);
}
