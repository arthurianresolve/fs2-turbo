mod allocation;
mod file;
mod lock;
mod overlapped;
mod path;
mod stats;

#[cfg(test)]
pub(crate) use allocation::{ALLOCATE_SPACE_EXTENDS_LENGTH, ALWAYS_RESERVE_RANGE, allocate_space};
pub(crate) use allocation::{allocate, allocation_state};
pub(crate) use file::duplicate;
pub(crate) use lock::{lock_error, lock_exclusive, lock_shared, unlock};
pub(crate) use stats::{StatsQuery, space, statvfs};

#[cfg(test)]
#[path = "tests.rs"]
// Keep the platform module implementation in `mod.rs` and place the
// platform-specific tests in `windows/tests.rs` for readability.
mod test;
