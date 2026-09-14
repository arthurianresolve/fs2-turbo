use std::io::{Error, ErrorKind, Result};

use windows_sys::Win32::Storage::FileSystem::{GetDiskFreeSpaceExW, GetDiskFreeSpaceW};

use crate::stats::{FilesystemCounters, SpaceKind, invalid_stats};
use crate::windows::path::win32_bool_result;

pub(crate) fn legacy_statvfs(root_path: &[u16]) -> Result<FilesystemCounters> {
    legacy_statvfs_after_geometry(root_path, cluster_geometry(root_path))
}

pub(crate) fn legacy_statvfs_after_geometry(
    root_path: &[u16],
    geometry: Result<u64>,
) -> Result<FilesystemCounters> {
    let geometry = geometry?;
    let bytes = byte_space(root_path)?;

    Ok(FilesystemCounters::windows_legacy_bytes(
        geometry,
        bytes.actual_free,
        bytes.caller_available,
        bytes.caller_total,
    ))
}
#[derive(Clone, Copy, Debug)]
pub(crate) struct ByteSpace {
    pub(crate) actual_free: u64,
    pub(crate) caller_available: u64,
    pub(crate) caller_total: u64,
}

pub(crate) fn legacy_space(root_path: &[u16], kind: SpaceKind) -> Result<u64> {
    legacy_space_with(
        kind,
        || byte_space(root_path),
        || cluster_geometry(root_path),
    )
}

pub(crate) fn legacy_space_with(
    kind: SpaceKind,
    byte_query: impl FnOnce() -> Result<ByteSpace>,
    geometry_query: impl FnOnce() -> Result<u64>,
) -> Result<u64> {
    match kind {
        SpaceKind::Free => byte_query().map(|space| space.actual_free),
        SpaceKind::Available => byte_query().map(|space| space.caller_available),
        SpaceKind::Total => byte_query().map(|space| space.caller_total),
        SpaceKind::AllocationGranularity => geometry_query(),
    }
}

fn cluster_geometry(root_path: &[u16]) -> Result<u64> {
    let mut sectors_per_cluster = 0;
    let mut bytes_per_sector = 0;
    let mut free_clusters = 0;
    let mut total_clusters = 0;
    let ret = unsafe {
        // SAFETY: `root_path` is null-terminated UTF-16 and all output pointers are valid.
        GetDiskFreeSpaceW(
            root_path.as_ptr(),
            &mut sectors_per_cluster,
            &mut bytes_per_sector,
            &mut free_clusters,
            &mut total_clusters,
        )
    };
    cluster_geometry_result(ret, sectors_per_cluster, bytes_per_sector)
}

#[inline(always)]
pub(crate) fn cluster_geometry_result(
    result: i32,
    sectors_per_cluster: u32,
    bytes_per_sector: u32,
) -> Result<u64> {
    win32_bool_result(result)?;
    let allocation_granularity = u64::from(sectors_per_cluster) * u64::from(bytes_per_sector);
    if allocation_granularity == 0 {
        Err(invalid_stats("filesystem allocation granularity is zero"))
    } else {
        Ok(allocation_granularity)
    }
}

fn byte_space(root_path: &[u16]) -> Result<ByteSpace> {
    let mut free_bytes_available_to_caller = 0;
    let mut total_number_of_bytes = 0;
    let mut total_number_of_free_bytes = 0;
    let ret = unsafe {
        // SAFETY: `root_path` is null-terminated UTF-16 and all output pointers are valid.
        GetDiskFreeSpaceExW(
            root_path.as_ptr(),
            &mut free_bytes_available_to_caller,
            &mut total_number_of_bytes,
            &mut total_number_of_free_bytes,
        )
    };
    byte_space_result(
        ret,
        free_bytes_available_to_caller,
        total_number_of_bytes,
        total_number_of_free_bytes,
    )
}

#[inline]
pub(crate) fn byte_space_result(
    result: i32,
    caller_available: u64,
    caller_total: u64,
    actual_free: u64,
) -> Result<ByteSpace> {
    win32_bool_result(result)?;
    if caller_available > caller_total || caller_available > actual_free {
        return Err(byte_space_domain_error());
    }
    Ok(ByteSpace {
        actual_free,
        caller_available,
        caller_total,
    })
}

#[cold]
#[inline(never)]
fn byte_space_domain_error() -> Error {
    Error::new(
        ErrorKind::InvalidData,
        "filesystem available space exceeds physical free space",
    )
}
