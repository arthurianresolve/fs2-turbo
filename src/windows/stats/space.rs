use std::ffi::OsStr;
use std::fs::OpenOptions;
use std::io::{Error, Result};
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;
use std::path::Path;

#[cfg(test)]
use std::os::windows::io::{FromRawHandle, OwnedHandle};

use windows_sys::Wdk::Storage::FileSystem::{
    FileFsFullSizeInformation, NtQueryVolumeInformationFile,
};
use windows_sys::Wdk::System::SystemServices::FILE_FS_FULL_SIZE_INFORMATION;
use windows_sys::Win32::Foundation::{
    ERROR_BAD_NETPATH, ERROR_BAD_PATHNAME, ERROR_DIRECTORY, ERROR_INVALID_DRIVE,
    ERROR_INVALID_NAME, ERROR_INVALID_PARAMETER, ERROR_PATH_NOT_FOUND,
};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_DEVICE, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_OFFLINE,
    FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS, FILE_ATTRIBUTE_RECALL_ON_OPEN,
    FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_NO_RECALL,
    FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, GetDiskFreeSpaceExW, GetFileAttributesW,
    INVALID_FILE_ATTRIBUTES,
};
use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;

#[cfg(test)]
use windows_sys::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE};

use crate::stats::{FilesystemCounters, SpaceKind};
use crate::windows::path::{
    VOLUME_PATH_CAPACITY, copy_exact_drive_root, valid_drive_root_components, volume_path,
    with_wide_path,
};

use super::legacy::{legacy_space, legacy_statvfs};
use super::modern::{
    GetDiskSpaceInformation, checked_disk_space, disk_space_information_fn, modern_statvfs,
    modern_statvfs_with,
};
use super::provider::{FallbackReason, ProviderOutcome};

const ERROR_BAD_NETPATH_I32: i32 = ERROR_BAD_NETPATH as i32;
const ERROR_BAD_PATHNAME_I32: i32 = ERROR_BAD_PATHNAME as i32;
const ERROR_DIRECTORY_I32: i32 = ERROR_DIRECTORY as i32;
const ERROR_INVALID_DRIVE_I32: i32 = ERROR_INVALID_DRIVE as i32;
const ERROR_INVALID_NAME_I32: i32 = ERROR_INVALID_NAME as i32;
const ERROR_INVALID_PARAMETER_I32: i32 = ERROR_INVALID_PARAMETER as i32;
const ERROR_PATH_NOT_FOUND_I32: i32 = ERROR_PATH_NOT_FOUND as i32;

const UNSUITABLE_HANDLE_SPACE_ATTRIBUTES: u32 = FILE_ATTRIBUTE_DEVICE
    | FILE_ATTRIBUTE_OFFLINE
    | FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS
    | FILE_ATTRIBUTE_RECALL_ON_OPEN;

pub(crate) fn space(path: &Path, kind: SpaceKind) -> Result<u64> {
    let os_path = path.as_os_str();
    with_wide_path(path, |path_utf16| {
        space_with_wide_path(path_utf16, os_path, kind)
    })
}

fn space_with_wide_path(path_utf16: &[u16], path: &OsStr, kind: SpaceKind) -> Result<u64> {
    match direct_space(path_utf16, kind) {
        DirectSpace::Hit(value) => return Ok(value),
        DirectSpace::Unavailable => match handle_space(path_utf16, path, kind) {
            DirectSpace::Hit(value) => return Ok(value),
            DirectSpace::Unavailable => {}
        },
    }

    space_after_narrow_queries(path_utf16, kind, disk_space_information_fn())
}

fn space_after_narrow_queries(
    path: &[u16],
    kind: SpaceKind,
    get_disk_space_information: Option<GetDiskSpaceInformation>,
) -> Result<u64> {
    if let Some(value) = exact_root_space_from_path(path, kind, get_disk_space_information)? {
        return Ok(value);
    }

    let mut root_path = [0u16; VOLUME_PATH_CAPACITY];
    volume_path(path, &mut root_path)?;
    root_space_with(
        &root_path,
        kind,
        modern_statvfs_with(&root_path, get_disk_space_information),
    )
}

fn exact_root_space_from_path(
    path: &[u16],
    kind: SpaceKind,
    get_disk_space_information: Option<GetDiskSpaceInformation>,
) -> Result<Option<u64>> {
    if !appears_to_be_drive_root(path) {
        return Ok(None);
    }

    let mut root_path = [0u16; VOLUME_PATH_CAPACITY];
    if copy_exact_drive_root(path, &mut root_path) {
        let exact_root = root_space_with(
            &root_path,
            kind,
            modern_statvfs_with(&root_path, get_disk_space_information),
        );
        return space_after_exact_root(
            path,
            kind,
            &mut root_path,
            exact_root,
            get_disk_space_information,
        )
        .map(Some);
    }

    Ok(None)
}

#[inline(always)]
fn appears_to_be_drive_root(path: &[u16]) -> bool {
    path.len() >= 4 && valid_drive_root_components(path[0], path[1], path[2], path[3])
}

pub(crate) fn space_after_exact_root(
    path: &[u16],
    kind: SpaceKind,
    root_path: &mut [u16; VOLUME_PATH_CAPACITY],
    exact_root: Result<u64>,
    get_disk_space_information: Option<GetDiskSpaceInformation>,
) -> Result<u64> {
    if let ProviderOutcome::Value(value) = exact_root_value(exact_root)? {
        return Ok(value);
    }

    root_path.fill(0);
    volume_path(path, root_path)?;
    root_space_with(
        root_path,
        kind,
        modern_statvfs_with(root_path, get_disk_space_information),
    )
}

pub(super) fn statvfs_root(root_path: &[u16]) -> Result<FilesystemCounters> {
    statvfs_root_with(root_path, modern_statvfs(root_path)?)
}

#[inline(always)]
pub(crate) fn statvfs_root_with(
    root_path: &[u16],
    modern: ProviderOutcome<FilesystemCounters>,
) -> Result<FilesystemCounters> {
    match modern {
        ProviderOutcome::Value(counters) => Ok(counters),
        ProviderOutcome::Unavailable(_) => legacy_statvfs(root_path),
    }
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum DirectSpace {
    Hit(u64),
    Unavailable,
}

pub(crate) fn direct_space(path: &[u16], kind: SpaceKind) -> DirectSpace {
    if matches!(kind, SpaceKind::Total | SpaceKind::AllocationGranularity) {
        return DirectSpace::Unavailable;
    }
    let mut caller_available = 0;
    let mut caller_total = 0;
    let mut actual_free = 0;
    let ret = unsafe {
        // SAFETY: `path` is null-terminated and both output pointers are valid.
        GetDiskFreeSpaceExW(
            path.as_ptr(),
            &mut caller_available,
            &mut caller_total,
            &mut actual_free,
        )
    };
    direct_space_result(ret, caller_available, caller_total, actual_free, kind)
}

#[inline(always)]
pub(crate) fn direct_space_result(
    result: i32,
    caller_available: u64,
    caller_total: u64,
    actual_free: u64,
    kind: SpaceKind,
) -> DirectSpace {
    if result == 0 || caller_available > caller_total || caller_available > actual_free {
        DirectSpace::Unavailable
    } else {
        match kind {
            SpaceKind::Free => DirectSpace::Hit(actual_free),
            SpaceKind::Available => DirectSpace::Hit(caller_available),
            SpaceKind::Total | SpaceKind::AllocationGranularity => DirectSpace::Unavailable,
        }
    }
}

pub(crate) fn handle_space(path: &[u16], os_path: &OsStr, kind: SpaceKind) -> DirectSpace {
    if matches!(kind, SpaceKind::Total)
        || matches!(kind, SpaceKind::AllocationGranularity)
            && !allocation_handle_path_eligible(path, os_path)
    {
        return DirectSpace::Unavailable;
    }

    let attributes = unsafe {
        // SAFETY: `path` is null-terminated for the duration of the call.
        GetFileAttributesW(path.as_ptr())
    };
    if !handle_space_attributes_eligible(attributes, kind) {
        return DirectSpace::Unavailable;
    }

    let handle = match handle_space_handle(os_path) {
        Some(handle) => handle,
        None => return DirectSpace::Unavailable,
    };

    let mut status = IO_STATUS_BLOCK::default();
    let mut info = FILE_FS_FULL_SIZE_INFORMATION::default();
    let result = unsafe {
        // SAFETY: `handle` remains valid, and `status` and `info` are writable,
        // correctly sized output storage for this query class.
        NtQueryVolumeInformationFile(
            handle.as_raw_handle(),
            &mut status,
            std::ptr::from_mut(&mut info).cast(),
            std::mem::size_of::<FILE_FS_FULL_SIZE_INFORMATION>() as u32,
            FileFsFullSizeInformation,
        )
    };
    handle_space_query_result(result, info, kind)
}

fn allocation_handle_path_eligible(path: &[u16], os_path: &OsStr) -> bool {
    Path::new(os_path).is_absolute()
        && (drive_path_below_root(path, 0)
            || path.starts_with(&[
                u16::from(b'\\'),
                u16::from(b'\\'),
                u16::from(b'?'),
                u16::from(b'\\'),
            ]) && drive_path_below_root(path, 4))
}

fn drive_path_below_root(path: &[u16], offset: usize) -> bool {
    path.len() > offset + 4
        && valid_drive_root_components(path[offset], path[offset + 1], path[offset + 2], 0)
}

fn handle_space_handle(path: &OsStr) -> Option<std::fs::File> {
    OpenOptions::new()
        .access_mode(0)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_NO_RECALL)
        .open(Path::new(path))
        .ok()
}

#[cfg(test)]
pub(crate) fn with_owned_handle<T>(
    handle: HANDLE,
    operation: impl FnOnce(OwnedHandle) -> T,
) -> Option<T> {
    owned_handle(handle).map(operation)
}

#[cfg(test)]
fn owned_handle(handle: HANDLE) -> Option<OwnedHandle> {
    if handle == INVALID_HANDLE_VALUE {
        None
    } else {
        // SAFETY: the caller provides a HANDLE from the OS and transfers ownership.
        Some(unsafe { OwnedHandle::from_raw_handle(handle) })
    }
}

pub(crate) fn handle_space_query_result(
    result: i32,
    info: FILE_FS_FULL_SIZE_INFORMATION,
    kind: SpaceKind,
) -> DirectSpace {
    if result != 0 {
        DirectSpace::Unavailable
    } else {
        handle_space_from_info(info, kind)
    }
}

pub(crate) fn handle_space_attributes_eligible(attributes: u32, kind: SpaceKind) -> bool {
    let valid_attributes = attributes != INVALID_FILE_ATTRIBUTES;
    let kind_specific_attributes = match kind {
        SpaceKind::AllocationGranularity => FILE_ATTRIBUTE_REPARSE_POINT,
        SpaceKind::Free | SpaceKind::Available | SpaceKind::Total => FILE_ATTRIBUTE_DIRECTORY,
    };
    let suitable_attributes =
        attributes & (UNSUITABLE_HANDLE_SPACE_ATTRIBUTES | kind_specific_attributes) == 0;
    handle_space_attributes_decision(valid_attributes, suitable_attributes)
}

pub(crate) const fn handle_space_attributes_decision(
    valid_attributes: bool,
    suitable_attributes: bool,
) -> bool {
    valid_attributes && suitable_attributes
}

pub(crate) fn handle_space_from_info(
    info: FILE_FS_FULL_SIZE_INFORMATION,
    kind: SpaceKind,
) -> DirectSpace {
    let granularity = u64::from(info.SectorsPerAllocationUnit) * u64::from(info.BytesPerSector);
    if granularity == 0 {
        return DirectSpace::Unavailable;
    }
    let Ok(actual_units) = u64::try_from(info.ActualAvailableAllocationUnits) else {
        return DirectSpace::Unavailable;
    };
    let Ok(caller_units) = u64::try_from(info.CallerAvailableAllocationUnits) else {
        return DirectSpace::Unavailable;
    };
    let Ok(total_units) = u64::try_from(info.TotalAllocationUnits) else {
        return DirectSpace::Unavailable;
    };
    // TotalAllocationUnits may be quota-limited while ActualAvailableAllocationUnits
    // is physical free space, so only compare counters from matching domains.
    if caller_units > total_units || caller_units > actual_units {
        return DirectSpace::Unavailable;
    }
    let Some(actual_free) = checked_disk_space(granularity, actual_units) else {
        return DirectSpace::Unavailable;
    };
    let Some(caller_available) = checked_disk_space(granularity, caller_units) else {
        return DirectSpace::Unavailable;
    };
    if caller_available > actual_free {
        return DirectSpace::Unavailable;
    }

    match kind {
        SpaceKind::Free => DirectSpace::Hit(actual_free),
        SpaceKind::Available => DirectSpace::Hit(caller_available),
        SpaceKind::AllocationGranularity => DirectSpace::Hit(granularity),
        SpaceKind::Total => DirectSpace::Unavailable,
    }
}

pub(crate) fn exact_root_value(result: Result<u64>) -> Result<ProviderOutcome<u64>> {
    match result {
        Ok(value) => Ok(ProviderOutcome::Value(value)),
        Err(error) if is_volume_resolution_error(&error) => Ok(ProviderOutcome::Unavailable(
            FallbackReason::VolumeResolution,
        )),
        Err(error) => Err(error),
    }
}

pub(crate) fn is_volume_resolution_error(error: &Error) -> bool {
    let Some(code) = error.raw_os_error() else {
        return false;
    };
    matches!(
        code,
        ERROR_BAD_NETPATH_I32
            | ERROR_BAD_PATHNAME_I32
            | ERROR_DIRECTORY_I32
            | ERROR_INVALID_DRIVE_I32
            | ERROR_INVALID_NAME_I32
            | ERROR_INVALID_PARAMETER_I32
            | ERROR_PATH_NOT_FOUND_I32
    )
}

pub(crate) fn root_space_with(
    root_path: &[u16],
    kind: SpaceKind,
    modern: Result<ProviderOutcome<FilesystemCounters>>,
) -> Result<u64> {
    match modern? {
        ProviderOutcome::Value(counters) => counters.space(kind),
        ProviderOutcome::Unavailable(_) => legacy_space(root_path, kind),
    }
}
