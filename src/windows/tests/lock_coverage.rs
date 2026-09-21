use std::io::{Error, ErrorKind};

#[test]
fn propagates_overlapped_construction_failure() {
    let file = tempfile::tempfile().unwrap();
    let error =
        super::lock_file_with_overlapped(&file, 0, Err(Error::other("event creation failed")))
            .unwrap_err();

    assert_eq!(error.kind(), ErrorKind::Other);
}
