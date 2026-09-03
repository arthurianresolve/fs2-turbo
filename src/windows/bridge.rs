// Temporary migration bridge; removed after public forwarding is complete.
#[path = "path.rs"]
mod path;
#[path = "file.rs"]
mod file;
pub(crate) use self::file::duplicate;
#[path = "allocation.rs"]
mod allocation;
pub(crate) use self::allocation::allocation_state;
