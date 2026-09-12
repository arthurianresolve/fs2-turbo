use std::fs::File;
use std::io::{Error, Result};
use std::os::unix::io::{AsRawFd, FromRawFd};

pub(crate) fn duplicate(file: &File) -> Result<File> {
    let fd = unsafe {
        // SAFETY: `file` owns a valid descriptor for the duration of this call.
        libc::dup(file.as_raw_fd())
    };
    if fd < 0 {
        Err(Error::last_os_error())
    } else {
        // SAFETY: a successful `dup` returns a new descriptor owned by the caller.
        Ok(unsafe { File::from_raw_fd(fd) })
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::fs::File;
    use std::os::unix::io::AsRawFd;

    use tempfile::tempdir;

    #[test]
    fn duplicate_new_fd() {
        let tempdir = tempdir().unwrap();
        let path = tempdir.path().join("fs2");
        let file1 = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .unwrap();
        let file2 = super::duplicate(&file1).unwrap();
        assert!(file1.as_raw_fd() != file2.as_raw_fd());
    }

    #[test]
    fn duplicate_status_flags() {
        fn flags(file: &File) -> libc::c_int {
            unsafe {
                // SAFETY: `file` owns a valid descriptor for the duration of this call.
                libc::fcntl(file.as_raw_fd(), libc::F_GETFL, 0)
            }
        }

        let tempdir = tempdir().unwrap();
        let path = tempdir.path().join("fs2");
        let file1 = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .unwrap();
        let file2 = super::duplicate(&file1).unwrap();

        assert_eq!(flags(&file1), flags(&file2));
    }

    #[test]
    fn duplicate_is_inheritable() {
        let tempdir = tempdir().unwrap();
        let path = tempdir.path().join("fs2");
        let file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .unwrap();
        let file = super::duplicate(&file).unwrap();

        let flags = unsafe {
            // SAFETY: `file` owns a valid descriptor for the duration of this call.
            libc::fcntl(file.as_raw_fd(), libc::F_GETFD)
        };
        assert_ne!(flags, -1);
        assert_eq!(flags & libc::FD_CLOEXEC, 0);
    }
}
