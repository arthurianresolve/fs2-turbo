#![allow(unstable_name_collisions)]
#![allow(deprecated)]

#[cfg(all(feature = "legacy", feature = "current"))]
compile_error!("select exactly one compatibility subject");
#[cfg(not(any(feature = "legacy", feature = "current")))]
compile_error!("select exactly one compatibility subject");

#[cfg(feature = "current")]
extern crate fs2_current as fs2;
#[cfg(feature = "legacy")]
extern crate fs2_v04 as fs2;

use std::collections::hash_map::DefaultHasher;
use std::fs::{self, File, OpenOptions};
use std::hash::{Hash, Hasher};
use std::io::{Read, Result, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(windows)]
use std::ffi::c_void;
#[cfg(windows)]
use std::os::windows::io::AsRawHandle;

#[cfg(windows)]
const HANDLE_FLAG_INHERIT: u32 = 1;

#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetHandleInformation(object: *mut c_void, flags: *mut u32) -> i32;
}

#[cfg(feature = "current")]
use fs2::FsStatsQuery;
use fs2::{
    allocation_granularity, available_space, free_space, lock_contended_error, statvfs,
    total_space, FileExt, FsStats,
};

struct DownstreamFile(File);

impl FileExt for DownstreamFile {
    fn duplicate(&self) -> Result<File> {
        FileExt::duplicate(&self.0)
    }

    fn allocated_size(&self) -> Result<u64> {
        FileExt::allocated_size(&self.0)
    }

    fn allocate(&self, len: u64) -> Result<()> {
        FileExt::allocate(&self.0, len)
    }

    fn lock_shared(&self) -> Result<()> {
        FileExt::lock_shared(&self.0)
    }

    fn lock_exclusive(&self) -> Result<()> {
        FileExt::lock_exclusive(&self.0)
    }

    fn try_lock_shared(&self) -> Result<()> {
        FileExt::try_lock_shared(&self.0)
    }

    fn try_lock_exclusive(&self) -> Result<()> {
        FileExt::try_lock_exclusive(&self.0)
    }

    fn unlock(&self) -> Result<()> {
        FileExt::unlock(&self.0)
    }
}

fn legacy_method_surface<T: FileExt>(file: &T) -> Result<()> {
    let _ = file.duplicate()?;
    let _ = file.allocated_size()?;
    file.allocate(0)?;
    file.lock_shared()?;
    file.lock_exclusive()?;
    let _ = file.try_lock_shared();
    let _ = file.try_lock_exclusive();
    file.unlock()
}

fn concrete_file_method_surface(file: &File) -> Result<()> {
    let _ = file.duplicate()?;
    let _ = file.allocated_size()?;
    file.allocate(0)?;
    file.lock_shared()?;
    file.lock_exclusive()?;
    let _ = file.try_lock_shared();
    let _ = file.try_lock_exclusive();
    file.unlock()
}

#[cfg(feature = "current")]
fn current_method_surface<T: FileExt>(file: &T) -> Result<()> {
    file.fs2_lock_shared()?;
    file.fs2_lock_exclusive()?;
    let _ = file.fs2_try_lock_shared();
    let _ = file.fs2_try_lock_exclusive();
    file.fs2_unlock()
}

fn assert_legacy_item_surface() {
    let _: fn(&DownstreamFile) -> Result<()> = legacy_method_surface::<DownstreamFile>;
    let _: fn(&File) -> Result<()> = concrete_file_method_surface;
    let _: fn(PathBuf) -> Result<FsStats> = statvfs::<PathBuf>;
    let _: fn(PathBuf) -> Result<u64> = free_space::<PathBuf>;
    let _: fn(PathBuf) -> Result<u64> = available_space::<PathBuf>;
    let _: fn(PathBuf) -> Result<u64> = total_space::<PathBuf>;
    let _: fn(PathBuf) -> Result<u64> = allocation_granularity::<PathBuf>;
    let _: fn() -> std::io::Error = lock_contended_error;

    fn assert_fs_stats_traits<T: Clone + std::fmt::Debug + Eq + Hash>() {}
    assert_fs_stats_traits::<FsStats>();
}

#[cfg(feature = "current")]
fn assert_current_item_surface() {
    let _: fn(&DownstreamFile) -> Result<()> = current_method_surface::<DownstreamFile>;
    let query = FsStatsQuery::new(PathBuf::from("."));
    if let Ok(query) = query {
        let _: Result<FsStats> = query.snapshot();
    }
}

fn open_file(path: &Path) -> Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .truncate(false)
        .open(path)
}

fn create_file(path: &Path) -> Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(path)
}

fn verify_duplicate_and_allocation(path: &Path) -> Result<()> {
    let mut original = create_file(path)?;
    original.write_all(b"fs2")?;

    let mut duplicate = FileExt::duplicate(&original)?;
    #[cfg(windows)]
    {
        let mut flags = 0;
        let result = unsafe {
            // SAFETY: `duplicate` owns a valid handle and `flags` is writable output storage.
            GetHandleInformation(duplicate.as_raw_handle(), &mut flags)
        };
        if result == 0 {
            return Err(std::io::Error::last_os_error());
        }
        assert_ne!(flags & HANDLE_FLAG_INHERIT, 0);
    }

    let mut at_shared_offset = Vec::new();
    duplicate.read_to_end(&mut at_shared_offset)?;
    assert!(at_shared_offset.is_empty());

    duplicate.seek(SeekFrom::Start(0))?;
    duplicate.read_to_end(&mut at_shared_offset)?;
    assert_eq!(at_shared_offset, b"fs2");

    let file_size = original.metadata()?.len();
    FileExt::allocate(&original, file_size)?;
    assert!(FileExt::allocated_size(&original)? >= file_size);
    Ok(())
}

fn verify_locking(path: &Path) -> Result<()> {
    let first = open_file(path)?;
    let second = open_file(path)?;

    FileExt::lock_exclusive(&first)?;
    let contended = FileExt::try_lock_shared(&second).unwrap_err();
    assert_eq!(contended.kind(), lock_contended_error().kind());
    FileExt::unlock(&first)?;

    FileExt::lock_shared(&second)?;
    FileExt::unlock(&second)
}

fn verify_statistics(path: &Path) -> Result<()> {
    let path = path.to_owned();
    let stats = statvfs::<PathBuf>(path.clone())?;
    assert!(stats.total_space() > 0);
    assert!(stats.available_space() <= stats.total_space());
    assert!(stats.allocation_granularity() > 0);

    let _ = free_space::<PathBuf>(path.clone())?;
    let _ = available_space::<PathBuf>(path.clone())?;
    assert!(total_space::<PathBuf>(path.clone())? > 0);
    assert!(allocation_granularity::<PathBuf>(path)? > 0);

    let mut hasher = DefaultHasher::new();
    stats.hash(&mut hasher);
    let _ = hasher.finish();
    Ok(())
}

struct TempDirectory(PathBuf);

impl TempDirectory {
    fn new() -> Result<Self> {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("fs2-v04-compat-{}-{nonce}", std::process::id()));
        fs::create_dir(&path)?;
        Ok(Self(path))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn main() -> Result<()> {
    assert_legacy_item_surface();
    #[cfg(feature = "current")]
    assert_current_item_surface();
    let tempdir = TempDirectory::new()?;
    let file = tempdir.path().join("fs2");
    verify_duplicate_and_allocation(&file)?;
    verify_locking(&file)?;
    verify_statistics(tempdir.path())?;
    verify_statistics(&file)
}
