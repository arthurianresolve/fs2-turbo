use std::io::{Error, ErrorKind, Result};
use std::sync::OnceLock;

use windows_sys::Win32::Foundation::{
    E_NOTIMPL, ERROR_CALL_NOT_IMPLEMENTED, ERROR_INVALID_FUNCTION, ERROR_NOT_SUPPORTED, HMODULE,
    RtlNtStatusToDosError, S_OK,
};
use windows_sys::Win32::Storage::FileSystem::DISK_SPACE_INFORMATION;
use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleA, GetProcAddress};

use crate::stats::FilesystemCounters;

use super::provider::{FallbackReason, ProviderOutcome};

const FACILITY_NT_BIT: u32 = 0x1000_0000;
const FACILITY_WIN32: u32 = 7;

pub(super) type GetDiskSpaceInformation = unsafe extern "system" fn(
    *const u16,
    *mut DISK_SPACE_INFORMATION,
) -> windows_sys::core::HRESULT;

static GET_DISK_SPACE_INFORMATION: OnceLock<Option<GetDiskSpaceInformation>> = OnceLock::new();

pub(crate) fn modern_statvfs(root_path: &[u16]) -> Result<ProviderOutcome<FilesystemCounters>> {
    modern_statvfs_with(root_path, disk_space_information_fn())
}

#[inline]
pub(super) fn disk_space_information_fn() -> Option<GetDiskSpaceInformation> {
    *GET_DISK_SPACE_INFORMATION.get_or_init(|| unsafe {
        // SAFETY: kernel32 is loaded in every Windows process and both string
        // literals are null-terminated. The symbol uses the documented ABI.
        let module = GetModuleHandleA(windows_sys::core::s!("kernel32.dll"));
        resolve_module_symbol(module, get_disk_space_information)
    })
}

pub(crate) fn get_disk_space_information(module: HMODULE) -> Option<GetDiskSpaceInformation> {
    unsafe {
        GetProcAddress(module, windows_sys::core::s!("GetDiskSpaceInformationW"))
            .map(|function| std::mem::transmute(function))
    }
}

pub(crate) fn resolve_module_symbol<T>(
    module: HMODULE,
    symbol: fn(HMODULE) -> Option<T>,
) -> Option<T> {
    if module.is_null() {
        None
    } else {
        symbol(module)
    }
}

pub(crate) fn modern_statvfs_with(
    root_path: &[u16],
    get_disk_space_information: Option<GetDiskSpaceInformation>,
) -> Result<ProviderOutcome<FilesystemCounters>> {
    let Some(get_disk_space_information) = get_disk_space_information else {
        return Ok(ProviderOutcome::Unavailable(
            FallbackReason::ProviderMissing,
        ));
    };
    let mut info = DISK_SPACE_INFORMATION::default();
    let result = unsafe {
        // SAFETY: `root_path` is null-terminated UTF-16 and `info` is valid output storage.
        get_disk_space_information(root_path.as_ptr(), &mut info)
    };
    if result != S_OK {
        if modern_statvfs_unavailable(result) {
            return Ok(ProviderOutcome::Unavailable(
                FallbackReason::ProviderUnavailable,
            ));
        }
        return Err(io_error_from_hresult(result));
    }

    counters_from_disk_space_information(info).map(ProviderOutcome::Value)
}

fn io_error_from_hresult(result: windows_sys::core::HRESULT) -> Error {
    let encoded = result as u32;
    let facility = (encoded >> 16) & 0x1fff;
    let error = if encoded & FACILITY_NT_BIT != 0 {
        let status = (encoded & !FACILITY_NT_BIT) as i32;
        unsafe {
            // SAFETY: this converts an integer NTSTATUS value and dereferences no pointers.
            RtlNtStatusToDosError(status)
        }
    } else if facility == FACILITY_WIN32 {
        encoded & 0xffff
    } else {
        return Error::from_raw_os_error(result);
    };
    Error::from_raw_os_error(error as i32)
}

pub(crate) const fn hresult_from_win32(error: u32) -> windows_sys::core::HRESULT {
    ((error & 0xffff) | 0x8007_0000) as windows_sys::core::HRESULT
}

#[inline(always)]
pub(crate) fn modern_statvfs_unavailable(result: windows_sys::core::HRESULT) -> bool {
    result == E_NOTIMPL
        || result == hresult_from_win32(ERROR_CALL_NOT_IMPLEMENTED)
        || result == hresult_from_win32(ERROR_INVALID_FUNCTION)
        || result == hresult_from_win32(ERROR_NOT_SUPPORTED)
}

pub(crate) fn counters_from_disk_space_information(
    info: DISK_SPACE_INFORMATION,
) -> Result<FilesystemCounters> {
    if info.CallerAvailableAllocationUnits > info.CallerTotalAllocationUnits {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "filesystem caller-available space exceeds caller-visible total",
        ));
    }
    if info.ActualAvailableAllocationUnits > info.ActualTotalAllocationUnits {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "filesystem free space exceeds physical total",
        ));
    }
    let allocation_granularity =
        u64::from(info.SectorsPerAllocationUnit) * u64::from(info.BytesPerSector);

    Ok(FilesystemCounters::windows_modern_bytes(
        allocation_granularity,
        checked_disk_space(allocation_granularity, info.ActualAvailableAllocationUnits)
            .ok_or_else(stats_overflow_error)?,
        checked_disk_space(allocation_granularity, info.CallerAvailableAllocationUnits)
            .ok_or_else(stats_overflow_error)?,
        checked_disk_space(allocation_granularity, info.ActualTotalAllocationUnits)
            .ok_or_else(stats_overflow_error)?,
    ))
}

#[cold]
#[inline(never)]
fn stats_overflow_error() -> Error {
    Error::new(ErrorKind::InvalidData, "filesystem space overflowed")
}

#[inline(always)]
pub(super) fn checked_disk_space(allocation_granularity: u64, units: u64) -> Option<u64> {
    allocation_granularity.checked_mul(units)
}
