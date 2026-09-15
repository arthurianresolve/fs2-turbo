use std::fs::File;
use std::io::{Error, Read, Seek, SeekFrom, Write};

use super::{AllocationState, allocate_with_state};
use super::{extend_file_length_after_snapshot_with, finish_allocation};

#[test]
fn completion_extends_length_and_preserves_existing_contents() {
    let mut file = tempfile::tempfile().unwrap();
    file.write_all(b"fs2!").unwrap();

    finish_allocation(&file, 16, 4, false).unwrap();
    assert_eq!(file.metadata().unwrap().len(), 16);
    assert_eq!(file.stream_position().unwrap(), 4);

    file.seek(SeekFrom::Start(0)).unwrap();
    let mut contents = [0; 4];
    file.read_exact(&mut contents).unwrap();
    assert_eq!(&contents, b"fs2!");
}

#[test]
fn completion_does_not_shrink_a_file_after_a_stale_snapshot() {
    let file = tempfile::tempfile().unwrap();
    file.set_len(16).unwrap();

    finish_allocation(&file, 8, 4, false).unwrap();
    assert_eq!(file.metadata().unwrap().len(), 16);
}

#[test]
fn completion_preserves_satisfied_reservations_and_lengths() {
    let file = tempfile::tempfile().unwrap();
    file.set_len(16).unwrap();

    for (len, observed_size, reservation_can_set_length) in
        [(16, 4, true), (8, 16, false), (0, 0, false)]
    {
        finish_allocation(&file, len, observed_size, reservation_can_set_length).unwrap();
        assert_eq!(file.metadata().unwrap().len(), 16);
    }
}

#[test]
fn completion_propagates_length_extension_failure() {
    let temporary = tempfile::NamedTempFile::new().unwrap();
    let read_only = File::open(temporary.path()).unwrap();

    assert!(finish_allocation(&read_only, 1, 0, false).is_err());
    assert_eq!(read_only.metadata().unwrap().len(), 0);
}

#[test]
fn completion_propagates_snapshot_failure() {
    let file = tempfile::tempfile().unwrap();
    let error = Error::other("file length snapshot failed");

    assert!(extend_file_length_after_snapshot_with(&file, 1, Err(error)).is_err());
}

#[test]
fn shared_allocation_propagates_platform_reservation_failure() {
    let temporary = tempfile::NamedTempFile::new().unwrap();
    let read_only = File::open(temporary.path()).unwrap();
    let state = AllocationState {
        allocated_size: 0,
        file_size: 0,
    };

    assert!(allocate_with_state(&read_only, 1, Ok(state)).is_err());
}
