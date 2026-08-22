#[derive(Debug, Clone, Copy)]
pub(crate) struct AllocationState {
    pub(crate) allocated_size: u64,
    pub(crate) file_size: u64,
}
