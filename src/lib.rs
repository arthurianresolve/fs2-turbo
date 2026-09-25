//! Cross-platform file locking, allocation, duplication, and filesystem statistics.
//!
//! The package is published as `fs2-turbo` and exports the `fs2` library crate.
//! Alias the package in `Cargo.toml` to preserve the established crate name:
//!
//! ```toml
//! [dependencies]
//! fs2 = { package = "fs2-turbo", version = "1" }
//! ```
//!
//! # Locking
//!
//! Rust 1.89 and newer provide inherent locking methods on [`std::fs::File`].
//! Inherent methods take precedence over extension-trait methods. Use the
//! collision-safe [`FileExt::fs2_lock_shared`],
//! [`FileExt::fs2_lock_exclusive`], [`FileExt::fs2_try_lock_shared`],
//! [`FileExt::fs2_try_lock_exclusive`], and [`FileExt::fs2_unlock`] methods
//! when the `fs2` implementation must be selected explicitly.
//!
//! ```
//! use fs2::FileExt;
//! use std::fs::File;
//! use std::io;
//!
//! fn with_exclusive_lock(file: &File) -> io::Result<()> {
//!     FileExt::fs2_lock_exclusive(file)?;
//!     FileExt::fs2_unlock(file)
//! }
//! ```
//!
//! # Filesystem statistics
//!
//! Use [`statvfs`] for one consistent snapshot. For repeated fresh snapshots
//! of the same filesystem, prepare an [`FsStatsQuery`] once and reuse it.

mod allocation;
mod stats;

#[cfg(unix)]
mod unix;
#[cfg(unix)]
use unix as sys;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows as sys;

use std::fs::File;
use std::io::{Error, Result};

pub use stats::{
    FsStats, FsStatsQuery, allocation_granularity, available_space, free_space, statvfs,
    total_space,
};

pub(crate) use allocation::AllocationState;

/// Extension trait for `std::fs::File` which provides allocation, duplication and locking methods.
///
/// On Rust 1.89 and later, `std::fs::File` also has inherent locking methods
/// whose names overlap this trait. Inherent methods take precedence over
/// extension traits, so use the explicit `fs2_*` methods when calling the
/// `fs2` implementation: `file.fs2_lock_shared()`,
/// `file.fs2_try_lock_shared()`, and `file.fs2_unlock()`.
///
/// ## Notes on File Locks
///
/// This library provides whole-file locks in both shared (read) and exclusive
/// (read-write) varieties.
///
/// File locks are a cross-platform hazard since the file lock APIs exposed by
/// operating system kernels vary in subtle and not-so-subtle ways.
///
/// The API exposed by this library can be safely used across platforms as long
/// as the following rules are followed:
///
///   * Multiple locks should not be created on an individual `File` instance
///     concurrently.
///   * Duplicated files should not be locked without great care.
///   * Files to be locked should be opened with at least read or write
///     permissions.
///   * File locks may only be relied upon to be advisory.
///
/// See the tests in `tests/integration/lib_integration.rs` for cross-platform lock behavior
/// that may be relied upon; see the tests in `unix` and `windows` for examples of
/// platform-specific behavior. File locks are implemented with
/// [`flock(2)`](http://man7.org/linux/man-pages/man2/flock.2.html) on Unix and
/// [`LockFileEx`](https://learn.microsoft.com/windows/win32/api/fileapi/nf-fileapi-lockfileex)
/// on Windows. Solaris uses process-associated `fcntl` record locks: they
/// coordinate separate processes, but independent handles in one process do
/// not provide the handle-scoped contention and close behavior of `flock`.
pub trait FileExt {
    /// Returns a duplicate instance of the file.
    ///
    /// The returned file will share the same file position as the original
    /// file.
    ///
    /// # Notes
    ///
    /// On Unix and Windows this retains the historical behavior, including an
    /// inheritable descriptor or handle. Prefer [`File::try_clone`] when the
    /// duplicate must not be inherited by a child process; use this method when
    /// retaining the historical inheritable behavior is required.
    #[deprecated(
        since = "1.0.0",
        note = "legacy duplicates are inheritable; use File::try_clone unless inheritance is required"
    )]
    fn duplicate(&self) -> Result<File>;

    /// Returns the amount of physical space allocated for a file.
    fn allocated_size(&self) -> Result<u64>;

    /// Ensures that at least `len` bytes of disk space are allocated for the
    /// file, and the file size is at least `len` bytes. Except for the Apple
    /// compatibility behavior noted below, after a successful call to
    /// `allocate`, subsequent writes to the file within the specified length
    /// are guaranteed not to fail because of lack of disk space.
    /// On platforms that cannot reserve or prove coverage of the requested
    /// range, this returns [`std::io::ErrorKind::Unsupported`].
    /// On Windows, sparse files may materialize holes through the existing EOF
    /// before restoring the sparse attribute; compressed files can return
    /// Unsupported.
    /// On macOS and iOS, the native primitive reserves file backing store from
    /// physical EOF; it does not expose portable extent-by-extent coverage of a
    /// previously sparse prefix.
    ///
    /// # Concurrency
    ///
    /// The caller must exclusively own changes to the file's logical length
    /// while this method runs. Some platform implementations use an exact-size
    /// operation to extend the file; a concurrent, non-cooperating resize can
    /// otherwise be overwritten. Advisory locks provide this exclusion only
    /// when every participant follows the same locking protocol.
    fn allocate(&self, len: u64) -> Result<()>;

    /// Locks the file for shared usage, blocking if the file is currently
    /// locked exclusively.
    #[inline]
    fn fs2_lock_shared(&self) -> Result<()> {
        self.lock_shared()
    }

    /// Locks the file for exclusive usage, blocking if the file is currently
    /// locked.
    #[inline]
    fn fs2_lock_exclusive(&self) -> Result<()> {
        self.lock_exclusive()
    }

    /// Locks the file for shared usage, or returns an error if the file is
    /// currently locked (see `lock_contended_error`).
    #[inline]
    fn fs2_try_lock_shared(&self) -> Result<()> {
        self.try_lock_shared()
    }

    /// Locks the file for exclusive usage, or returns an error if the file is
    /// currently locked (see `lock_contended_error`).
    #[inline]
    fn fs2_try_lock_exclusive(&self) -> Result<()> {
        self.try_lock_exclusive()
    }

    /// Unlocks the file.
    #[inline]
    fn fs2_unlock(&self) -> Result<()> {
        self.unlock()
    }

    /// Legacy shared-lock method. Prefer [`FileExt::fs2_lock_shared`] on Rust
    /// 1.89 and later.
    fn lock_shared(&self) -> Result<()>;

    /// Legacy exclusive-lock method. Prefer [`FileExt::fs2_lock_exclusive`].
    fn lock_exclusive(&self) -> Result<()>;

    /// Legacy non-blocking shared-lock method. Prefer
    /// [`FileExt::fs2_try_lock_shared`] on Rust 1.89 and later.
    fn try_lock_shared(&self) -> Result<()>;

    /// Legacy non-blocking exclusive-lock method. Prefer
    /// [`FileExt::fs2_try_lock_exclusive`].
    fn try_lock_exclusive(&self) -> Result<()>;

    /// Legacy unlock method. Prefer [`FileExt::fs2_unlock`] on Rust 1.89 and
    /// later.
    fn unlock(&self) -> Result<()>;
}

impl FileExt for File {
    #[inline]
    fn duplicate(&self) -> Result<File> {
        sys::duplicate(self)
    }
    #[inline]
    fn allocated_size(&self) -> Result<u64> {
        allocation::allocated_size(self)
    }
    #[inline]
    fn allocate(&self, len: u64) -> Result<()> {
        allocation::allocate(self, len)
    }
    #[inline]
    fn fs2_lock_shared(&self) -> Result<()> {
        sys::lock_shared(self, false)
    }
    #[inline]
    fn fs2_lock_exclusive(&self) -> Result<()> {
        sys::lock_exclusive(self, false)
    }
    #[inline]
    fn fs2_try_lock_shared(&self) -> Result<()> {
        sys::lock_shared(self, true)
    }
    #[inline]
    fn fs2_try_lock_exclusive(&self) -> Result<()> {
        sys::lock_exclusive(self, true)
    }
    #[inline]
    fn fs2_unlock(&self) -> Result<()> {
        sys::unlock(self)
    }
    #[inline]
    fn lock_shared(&self) -> Result<()> {
        sys::lock_shared(self, false)
    }
    #[inline]
    fn lock_exclusive(&self) -> Result<()> {
        sys::lock_exclusive(self, false)
    }
    #[inline]
    fn try_lock_shared(&self) -> Result<()> {
        sys::lock_shared(self, true)
    }
    #[inline]
    fn try_lock_exclusive(&self) -> Result<()> {
        sys::lock_exclusive(self, true)
    }
    #[inline]
    fn unlock(&self) -> Result<()> {
        sys::unlock(self)
    }
}

/// Returns the error that a call to a try lock method on a contended file will
/// return.
pub fn lock_contended_error() -> Error {
    sys::lock_error()
}

#[cfg(test)]
mod forwarding_tests {
    use super::{
        FileExt, allocation_granularity, available_space, free_space, lock_contended_error,
        statvfs, total_space,
    };
    use std::fs::{File, OpenOptions};
    use std::io::Result;

    struct DefaultForwarders<'a>(&'a File);

    #[allow(deprecated)]
    impl FileExt for DefaultForwarders<'_> {
        fn duplicate(&self) -> Result<File> {
            FileExt::duplicate(self.0)
        }

        fn allocated_size(&self) -> Result<u64> {
            FileExt::allocated_size(self.0)
        }

        fn allocate(&self, len: u64) -> Result<()> {
            FileExt::allocate(self.0, len)
        }

        fn lock_shared(&self) -> Result<()> {
            FileExt::lock_shared(self.0)
        }

        fn lock_exclusive(&self) -> Result<()> {
            FileExt::lock_exclusive(self.0)
        }

        fn try_lock_shared(&self) -> Result<()> {
            FileExt::try_lock_shared(self.0)
        }

        fn try_lock_exclusive(&self) -> Result<()> {
            FileExt::try_lock_exclusive(self.0)
        }

        fn unlock(&self) -> Result<()> {
            FileExt::unlock(self.0)
        }
    }

    #[allow(deprecated)]
    fn assert_exclusive_lock_is_contended(file: &File) {
        let result = FileExt::try_lock_exclusive(file);
        assert!(result.is_err());
    }

    #[allow(deprecated)]
    fn assert_shared_lock_is_contended(file: &File) {
        let result = FileExt::try_lock_shared(file);
        assert!(result.is_err());
    }

    #[allow(deprecated)]
    #[test]
    fn public_forwarders_execute_in_the_unit_test_binary() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("forwarding");
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        let contender = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();

        let duplicate = FileExt::duplicate(&file).unwrap();
        assert_eq!(
            duplicate.metadata().unwrap().len(),
            file.metadata().unwrap().len()
        );

        #[cfg(any(
            target_os = "windows",
            target_os = "freebsd",
            target_os = "android",
            target_os = "emscripten",
            target_os = "macos",
            target_os = "ios",
            all(target_os = "linux", not(target_env = "uclibc")),
        ))]
        {
            FileExt::allocate(&file, 4096).unwrap();
            assert!(file.metadata().unwrap().len() >= 4096);
            assert!(FileExt::allocated_size(&file).unwrap() >= 4096);
        }

        FileExt::fs2_lock_shared(&file).unwrap();
        assert_exclusive_lock_is_contended(&contender);
        FileExt::fs2_unlock(&file).unwrap();
        FileExt::fs2_lock_exclusive(&file).unwrap();
        assert_shared_lock_is_contended(&contender);
        FileExt::fs2_unlock(&file).unwrap();
        FileExt::fs2_try_lock_shared(&file).unwrap();
        assert_exclusive_lock_is_contended(&contender);
        FileExt::fs2_unlock(&file).unwrap();
        FileExt::fs2_try_lock_exclusive(&file).unwrap();
        assert_shared_lock_is_contended(&contender);
        FileExt::fs2_unlock(&file).unwrap();

        FileExt::lock_shared(&file).unwrap();
        assert_exclusive_lock_is_contended(&contender);
        FileExt::unlock(&file).unwrap();
        FileExt::lock_exclusive(&file).unwrap();
        assert_shared_lock_is_contended(&contender);
        FileExt::unlock(&file).unwrap();
        FileExt::try_lock_shared(&file).unwrap();
        assert_exclusive_lock_is_contended(&contender);
        FileExt::unlock(&file).unwrap();
        FileExt::try_lock_exclusive(&file).unwrap();
        assert_shared_lock_is_contended(&contender);
        FileExt::unlock(&file).unwrap();

        let default_forwarders = DefaultForwarders(&file);
        drop(FileExt::duplicate(&default_forwarders).unwrap());
        FileExt::allocated_size(&default_forwarders).unwrap();
        FileExt::allocate(&default_forwarders, 0).unwrap();
        FileExt::fs2_lock_shared(&default_forwarders).unwrap();
        assert_exclusive_lock_is_contended(&contender);
        FileExt::fs2_unlock(&default_forwarders).unwrap();
        FileExt::fs2_lock_exclusive(&default_forwarders).unwrap();
        assert_shared_lock_is_contended(&contender);
        FileExt::fs2_unlock(&default_forwarders).unwrap();
        FileExt::fs2_try_lock_shared(&default_forwarders).unwrap();
        assert_exclusive_lock_is_contended(&contender);
        FileExt::fs2_unlock(&default_forwarders).unwrap();
        FileExt::fs2_try_lock_exclusive(&default_forwarders).unwrap();
        assert_shared_lock_is_contended(&contender);
        FileExt::fs2_unlock(&default_forwarders).unwrap();

        let _ = lock_contended_error();

        let stats = statvfs(path.clone()).unwrap();
        let free = free_space(path.clone()).unwrap();
        let available = available_space(path.clone()).unwrap();
        let total = total_space(path.clone()).unwrap();
        assert!(free <= total);
        assert!(available <= total);
        assert_eq!(
            allocation_granularity(path).unwrap(),
            stats.allocation_granularity()
        );
    }
}
