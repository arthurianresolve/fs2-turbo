// Temporary migration bridge; removed after public forwarding is complete.
#[path = "path.rs"]
mod path;
#[path = "file.rs"]
mod file;
pub(crate) use self::file::duplicate;
