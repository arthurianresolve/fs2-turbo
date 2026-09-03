// Temporary migration bridge; removed after public forwarding is complete.
#[path = "file.rs"]
mod file;
pub(crate) use self::file::duplicate;
