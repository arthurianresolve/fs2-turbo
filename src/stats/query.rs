use std::borrow::Cow;
#[cfg(unix)]
use std::ffi::CString;
use std::io::Result;
#[cfg(unix)]
use std::io::{Error, ErrorKind};
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use super::FsStats;

/// A prepared filesystem-statistics query for repeated snapshots.
///
/// Construction resolves and validates the platform path representation once.
/// Each call to [`FsStatsQuery::snapshot`] acquires fresh filesystem counters;
/// counter values are never cached. Recreate the query after changing the
/// process working directory or the path's mount, junction, or symbolic-link
/// mapping.
///
/// # Examples
///
/// ```
/// # fn main() -> std::io::Result<()> {
/// use fs2::FsStatsQuery;
///
/// let query = FsStatsQuery::new(".")?;
/// let first = query.snapshot()?;
/// let second = query.snapshot()?;
/// # let _ = (first, second);
/// # Ok(())
/// # }
/// ```
#[derive(Debug)]
pub struct FsStatsQuery {
    inner: crate::sys::StatsQuery,
}

impl FsStatsQuery {
    /// Prepares repeated statistics queries for the filesystem containing
    /// `path`.
    pub fn new(path: impl AsRef<Path>) -> Result<Self> {
        Self::new_path(path.as_ref())
    }

    #[cfg(unix)]
    fn new_path(path: &Path) -> Result<Self> {
        let path = absolute_path(path)?;
        let path = CString::new(path.as_ref().as_os_str().as_bytes())
            .map_err(|_| Error::new(ErrorKind::InvalidInput, "path contained a null"))?;
        Ok(Self {
            inner: crate::sys::StatsQuery::new(path),
        })
    }

    #[cfg(not(unix))]
    fn new_path(path: &Path) -> Result<Self> {
        let path = absolute_path(path)?;
        crate::sys::StatsQuery::new(path.as_ref()).map(|inner| Self { inner })
    }

    /// Acquires a fresh statistics snapshot.
    pub fn snapshot(&self) -> Result<FsStats> {
        self.inner.counters().and_then(FsStats::from_counters)
    }
}

fn absolute_path(path: &Path) -> Result<Cow<'_, Path>> {
    if path.is_absolute() {
        Ok(Cow::Borrowed(path))
    } else {
        std::path::absolute(path).map(Cow::Owned)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::process::Command;

    const REMOVED_CWD_WORKER: &str = "FS2_REMOVED_CWD_COVERAGE_WORKER";

    #[test]
    fn relative_path_reports_removed_current_directory() {
        if std::env::var_os(REMOVED_CWD_WORKER).is_some() {
            let original_directory = std::env::current_dir().unwrap();
            let directory = tempfile::tempdir().unwrap();
            std::env::set_current_dir(directory.path()).unwrap();
            std::fs::remove_dir(directory.path()).unwrap();

            let result = super::FsStatsQuery::new(".");

            std::env::set_current_dir(original_directory).unwrap();
            assert!(result.is_err());
            return;
        }

        let module = module_path!().split_once("::").unwrap().1;
        let status = Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg(format!(
                "{module}::relative_path_reports_removed_current_directory"
            ))
            .arg("--nocapture")
            .env(REMOVED_CWD_WORKER, "1")
            .status()
            .unwrap();

        assert!(status.success(), "removed-current-directory worker failed");
    }
}
