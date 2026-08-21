use std::io::Result;
use std::path::Path;

use crate::stats::FilesystemCounters;
use crate::windows::path::{
    VOLUME_PATH_CAPACITY, copy_exact_drive_root, volume_path, with_wide_path,
};

#[derive(Debug)]
pub(crate) struct StatsQuery {
    pub(crate) root_path: [u16; VOLUME_PATH_CAPACITY],
}

impl StatsQuery {
    pub(crate) fn new(path: &Path) -> Result<Self> {
        with_wide_path(path, stats_query_from_wide_path)
    }

    pub(crate) fn counters(&self) -> Result<FilesystemCounters> {
        space::statvfs_root(&self.root_path)
    }
}

fn stats_query_from_wide_path(path: &[u16]) -> Result<StatsQuery> {
    let mut root_path = [0; VOLUME_PATH_CAPACITY];
    if !copy_exact_drive_root(path, &mut root_path) {
        volume_path(path, &mut root_path)?;
    }
    Ok(StatsQuery { root_path })
}

pub(crate) fn statvfs(path: &Path) -> Result<FilesystemCounters> {
    StatsQuery::new(path)?.counters()
}

mod legacy;
mod modern;
mod provider;
mod space;

pub(crate) use space::space;

#[cfg(test)]
pub(crate) mod test_support {
    pub(crate) use super::legacy::{
        ByteSpace, byte_space_result, cluster_geometry_result, legacy_space, legacy_space_with,
        legacy_statvfs, legacy_statvfs_after_geometry,
    };
    pub(crate) use super::modern::{
        counters_from_disk_space_information, get_disk_space_information, hresult_from_win32,
        modern_statvfs, modern_statvfs_unavailable, modern_statvfs_with, resolve_module_symbol,
    };
    pub(crate) use super::provider::{FallbackReason, ProviderOutcome};
    pub(crate) use super::space::{
        DirectSpace, direct_space, direct_space_result, exact_root_value, handle_space,
        handle_space_attributes_decision, handle_space_attributes_eligible, handle_space_from_info,
        handle_space_query_result, is_volume_resolution_error, root_space_with, space,
        space_after_exact_root, statvfs_root_with, with_owned_handle,
    };
}
