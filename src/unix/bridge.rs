// Temporary migration bridge; removed after public forwarding is complete.
#[path = "file.rs"]
mod file;
pub(crate) use self::file::duplicate;
#[path = "allocation.rs"]
mod allocation;
pub(crate) use self::allocation::allocation_state;
pub(crate) use self::allocation::{
    ALLOCATE_SPACE_EXTENDS_LENGTH, ALWAYS_RESERVE_RANGE, allocate_space,
};
