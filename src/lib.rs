//! Extended utilities for working with files and filesystems in Rust.

#![doc(html_root_url = "https://docs.rs/fs2/0.4.3")]

#[cfg(windows)]
extern crate winapi;

mod allocation;
mod stats;
pub(crate) use crate::allocation::AllocationState;

#[cfg(unix)]
#[path = "legacy_unix.rs"]
mod legacy_unix;
#[cfg(unix)]
use crate::legacy_unix as sys;
#[cfg(unix)]
#[path = "unix/bridge.rs"]
mod unix;
#[cfg(unix)]
use crate::unix as modular_sys;

#[cfg(windows)]
#[path = "legacy_windows.rs"]
mod legacy_windows;
#[cfg(windows)]
use crate::legacy_windows as sys;
#[cfg(windows)]
#[path = "windows/bridge.rs"]
mod windows;
#[cfg(windows)]
use crate::windows as modular_sys;

use std::fs::File;
use std::io::{Error, Result};
use std::path::Path;

/// Extension trait for `std::fs::File` which provides allocation, duplication and locking methods.
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
/// See the tests in `lib.rs` for cross-platform lock behavior that may be
/// relied upon; see the tests in `unix.rs` and `windows.rs` for examples of
/// platform-specific behavior. File locks are implemented with
/// [`flock(2)`](http://man7.org/linux/man-pages/man2/flock.2.html) on Unix and
/// [`LockFile`](https://msdn.microsoft.com/en-us/library/windows/desktop/aa365202(v=vs.85).aspx)
/// on Windows.
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
    fn lock_shared(&self) -> Result<()>;

    /// Locks the file for exclusive usage, blocking if the file is currently
    /// locked.
    fn lock_exclusive(&self) -> Result<()>;

    /// Locks the file for shared usage, or returns a an error if the file is
    /// currently locked (see `lock_contended_error`).
    fn try_lock_shared(&self) -> Result<()>;

    /// Locks the file for shared usage, or returns a an error if the file is
    /// currently locked (see `lock_contended_error`).
    fn try_lock_exclusive(&self) -> Result<()>;

    /// Unlocks the file.
    fn unlock(&self) -> Result<()>;
}

impl FileExt for File {
    fn duplicate(&self) -> Result<File> {
        modular_sys::duplicate(self)
    }
    fn allocated_size(&self) -> Result<u64> {
        allocation::allocated_size(self)
    }
    fn allocate(&self, len: u64) -> Result<()> {
        allocation::allocate(self, len)
    }
    fn lock_shared(&self) -> Result<()> {
        modular_sys::lock_shared(self, false)
    }
    fn lock_exclusive(&self) -> Result<()> {
        modular_sys::lock_exclusive(self, false)
    }
    fn try_lock_shared(&self) -> Result<()> {
        modular_sys::try_lock_shared(self)
    }
    fn try_lock_exclusive(&self) -> Result<()> {
        modular_sys::try_lock_exclusive(self)
    }
    fn unlock(&self) -> Result<()> {
        modular_sys::unlock(self)
    }
}

/// Returns the error that a call to a try lock method on a contended file will
/// return.
pub fn lock_contended_error() -> Error {
    modular_sys::lock_error()
}

/// `FsStats` contains some common stats about a file system.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct FsStats {
    free_space: u64,
    available_space: u64,
    total_space: u64,
    allocation_granularity: u64,
}

impl FsStats {
    #[inline]
    pub(crate) const fn from_parts(
        free_space: u64,
        available_space: u64,
        total_space: u64,
        allocation_granularity: u64,
    ) -> Self {
        Self {
            free_space,
            available_space,
            total_space,
            allocation_granularity,
        }
    }

    /// Returns the number of free bytes in the file system containing the provided
    /// path.
    pub fn free_space(&self) -> u64 {
        self.free_space
    }

    /// Returns the available space in bytes to non-priveleged users in the file
    /// system containing the provided path.
    pub fn available_space(&self) -> u64 {
        self.available_space
    }

    /// Returns the total space in bytes in the file system containing the provided
    /// path.
    pub fn total_space(&self) -> u64 {
        self.total_space
    }

    /// Returns the filesystem's disk space allocation granularity in bytes.
    /// The provided path may be for any file in the filesystem.
    ///
    /// On Posix, this is equivalent to the filesystem's block size.
    /// On Windows, this is equivalent to the filesystem's cluster size.
    pub fn allocation_granularity(&self) -> u64 {
        self.allocation_granularity
    }
}

/// Get the stats of the file system containing the provided path.
pub fn statvfs<P>(path: P) -> Result<FsStats> where P: AsRef<Path> {
    stats::statvfs(path)
}

/// Returns the number of free bytes in the file system containing the provided
/// path.
pub fn free_space<P>(path: P) -> Result<u64> where P: AsRef<Path> {
    stats::free_space(path)
}

/// Returns the available space in bytes to non-priveleged users in the file
/// system containing the provided path.
pub fn available_space<P>(path: P) -> Result<u64> where P: AsRef<Path> {
    stats::available_space(path)
}

/// Returns the total space in bytes in the file system containing the provided
/// path.
pub fn total_space<P>(path: P) -> Result<u64> where P: AsRef<Path> {
    stats::total_space(path)
}

/// Returns the filesystem's disk space allocation granularity in bytes.
/// The provided path may be for any file in the filesystem.
///
/// On Posix, this is equivalent to the filesystem's block size.
/// On Windows, this is equivalent to the filesystem's cluster size.
pub fn allocation_granularity<P>(path: P) -> Result<u64> where P: AsRef<Path> {
    stats::allocation_granularity(path)
}

