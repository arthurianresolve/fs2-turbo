use std::cell::Cell;
use std::ffi::OsString;
use std::fs;
use std::io::{Error, ErrorKind};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::io::IntoRawHandle;
use std::path::{Path, PathBuf};

use windows_sys::Wdk::System::SystemServices::FILE_FS_FULL_SIZE_INFORMATION;
use windows_sys::Win32::Foundation::{
    E_NOTIMPL, ERROR_ACCESS_DENIED, ERROR_BAD_NETPATH, ERROR_BAD_PATHNAME,
    ERROR_CALL_NOT_IMPLEMENTED, ERROR_DIRECTORY, ERROR_INVALID_DRIVE, ERROR_INVALID_FUNCTION,
    ERROR_INVALID_NAME, ERROR_INVALID_PARAMETER, ERROR_NOT_SUPPORTED, ERROR_PATH_NOT_FOUND,
    INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{
    DISK_SPACE_INFORMATION, FILE_ATTRIBUTE_ARCHIVE, FILE_ATTRIBUTE_DEVICE,
    FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_OFFLINE, FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS,
    FILE_ATTRIBUTE_RECALL_ON_OPEN, FILE_ATTRIBUTE_REPARSE_POINT, FILE_STANDARD_INFO,
    INVALID_FILE_ATTRIBUTES,
};

use crate::FileExt;
use crate::stats::{FilesystemCounters, SpaceKind};
use crate::windows::StatsQuery;
use crate::windows::allocation::allocation_state_result;
use crate::windows::path::{
    VOLUME_PATH_CAPACITY, copy_exact_drive_root, valid_drive_root_components, volume_path,
    wide_path, win32_bool_result, with_wide_path,
};
use crate::windows::stats::test_support::{
    ByteSpace, DirectSpace, FallbackReason, ProviderOutcome, byte_space_result,
    cluster_geometry_result, counters_from_disk_space_information, direct_space, exact_root_value,
    get_disk_space_information, handle_space, handle_space_attributes_decision,
    handle_space_attributes_eligible, handle_space_from_info, handle_space_query_result,
    hresult_from_win32, is_volume_resolution_error, legacy_space, legacy_space_with,
    legacy_statvfs, legacy_statvfs_after_geometry, modern_statvfs, modern_statvfs_unavailable,
    modern_statvfs_with, resolve_module_symbol, root_space_with, space, space_after_exact_root,
    statvfs_root_with, with_owned_handle,
};
use tempfile::tempdir;

const HRESULT_ACCESS_DENIED: i32 = 0x8007_0005_u32 as i32;
const HRESULT_E_FAIL: i32 = 0x8000_4005_u32 as i32;
const HRESULT_E_NOTIMPL: i32 = 0x8000_4001_u32 as i32;
const HRESULT_OBJECT_PATH_NOT_FOUND: i32 = 0xd000_003a_u32 as i32;
const PATH_ERROR_ENCODINGS: [(u32, i32); 7] = [
    (ERROR_BAD_NETPATH, 0x8007_0035_u32 as i32),
    (ERROR_BAD_PATHNAME, 0x8007_00a1_u32 as i32),
    (ERROR_DIRECTORY, 0x8007_010b_u32 as i32),
    (ERROR_INVALID_DRIVE, 0x8007_000f_u32 as i32),
    (ERROR_INVALID_NAME, 0x8007_007b_u32 as i32),
    (ERROR_INVALID_PARAMETER, 0x8007_0057_u32 as i32),
    (ERROR_PATH_NOT_FOUND, 0x8007_0003_u32 as i32),
];
const UNAVAILABLE_ERROR_ENCODINGS: [(u32, i32); 3] = [
    (ERROR_CALL_NOT_IMPLEMENTED, 0x8007_0078_u32 as i32),
    (ERROR_INVALID_FUNCTION, 0x8007_0001_u32 as i32),
    (ERROR_NOT_SUPPORTED, 0x8007_0032_u32 as i32),
];

mod handle;
mod modern;
mod path;
mod provider;
mod root;
mod validation;
