// Temporary migration bridge; removed after public forwarding is complete.
#[path = "path.rs"]
mod path;
#[path = "file.rs"]
mod file;
pub(crate) use self::file::duplicate;
#[path = "allocation.rs"]
mod allocation;
pub(crate) use self::allocation::allocate;
pub(crate) use self::allocation::allocation_state;
pub(crate) use self::allocation::{ALLOCATE_SPACE_EXTENDS_LENGTH, ALWAYS_RESERVE_RANGE, allocate_space};
#[path = "lock.rs"]
mod lock;
pub(crate) use self::lock::lock_shared;
pub(crate) use self::lock::lock_exclusive;
pub(crate) fn try_lock_shared(file: &std::fs::File) -> std::io::Result<()> {
    lock::lock_shared(file, true)
}
pub(crate) fn try_lock_exclusive(file: &std::fs::File) -> std::io::Result<()> {
    lock::lock_exclusive(file, true)
}
pub(crate) use self::lock::unlock;
pub(crate) use self::lock::lock_error;
#[path = "stats/mod.rs"]
mod stats;
pub(crate) use self::stats::statvfs;
pub(crate) fn free_space(path: &std::path::Path) -> std::io::Result<u64> {
    stats::space(path, crate::stats::SpaceKind::Free)
}
pub(crate) fn available_space(path: &std::path::Path) -> std::io::Result<u64> {
    stats::space(path, crate::stats::SpaceKind::Available)
}
pub(crate) fn total_space(path: &std::path::Path) -> std::io::Result<u64> {
    stats::space(path, crate::stats::SpaceKind::Total)
}
