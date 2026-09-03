use std::fs::File;
use std::io::Result;

#[derive(Debug, Clone, Copy)]
pub(crate) struct AllocationState {
    pub(crate) allocated_size: u64,
    pub(crate) file_size: u64,
}

#[inline]
pub(crate) fn allocated_size(file: &File) -> Result<u64> {
    crate::modular_sys::allocation_state(file).map(|state| state.allocated_size)
}
