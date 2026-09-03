use std::fs::File;
use std::io::Result;

use crate::modular_sys as sys;

#[cfg(test)]
mod tests;

#[derive(Debug, Clone, Copy)]
pub(crate) struct AllocationState {
    pub(crate) allocated_size: u64,
    pub(crate) file_size: u64,
}

#[inline]
pub(crate) fn allocated_size(file: &File) -> Result<u64> {
    sys::allocation_state(file).map(|state| state.allocated_size)
}

#[inline]
#[cfg(not(windows))]
pub(crate) fn allocate(file: &File, len: u64) -> Result<()> {
    allocate_with_state(file, len, sys::allocation_state(file))
}

#[inline]
#[cfg(windows)]
pub(crate) fn allocate(file: &File, len: u64) -> Result<()> {
    sys::allocate(file, len)
}

#[cfg(any(not(windows), test))]
fn allocate_with_state(file: &File, len: u64, state: Result<AllocationState>) -> Result<()> {
    let state = state?;
    let reservation_needed = reservation_needed(state, len, sys::ALWAYS_RESERVE_RANGE);
    let reservation_can_set_length = reservation_needed && sys::ALLOCATE_SPACE_EXTENDS_LENGTH;

    if reservation_needed {
        // On platforms with ALLOCATE_SPACE_EXTENDS_LENGTH, reserving physical space
        // also guarantees the logical file length is at least `len` when needed.
        // Keep length-extension checks explicit to avoid implicit control-flow coupling.
        sys::allocate_space(file, state, len)?;
    }

    if !reservation_can_set_length && state.file_size < len {
        extend_file_length_after_snapshot(file, len)?;
    }

    Ok(())
}

#[inline]
#[cfg(any(not(windows), test))]
fn reservation_needed(state: AllocationState, len: u64, always_reserve_range: bool) -> bool {
    state.allocated_size < len || (len != 0 && always_reserve_range)
}

#[cfg(any(not(windows), test))]
fn extend_file_length_after_snapshot(file: &File, len: u64) -> Result<()> {
    if file.metadata()?.len() < len {
        // FileExt::allocate requires exclusive ownership of logical-length
        // changes because set_len is exact, not an atomic max-length operation.
        file.set_len(len)
    } else {
        Ok(())
    }
}
