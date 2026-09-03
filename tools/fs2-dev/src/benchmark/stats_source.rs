use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use crate::{Result, invalid_data};

pub(super) const BASELINE_PACKAGE: &str = "fs2-benchmark-baseline";
pub(super) const CANDIDATE_PACKAGE: &str = "fs2-benchmark-candidate";

pub(super) struct ManifestSpec<'a> {
    pub(super) project: &'a Path,
    pub(super) harness_source: &'a Path,
    pub(super) paired_core_source: &'a Path,
    pub(super) paired_protocol_source: &'a Path,
    pub(super) paired_stats_protocol_source: &'a Path,
    pub(super) baseline_source: &'a Path,
    pub(super) candidate_source: &'a Path,
}

pub(super) fn write_manifest(spec: ManifestSpec<'_>) -> Result<()> {
    let ManifestSpec {
        project,
        harness_source,
        paired_core_source,
        paired_protocol_source,
        paired_stats_protocol_source,
        baseline_source,
        candidate_source,
    } = spec;
    fs::create_dir_all(project.join("src"))?;
    fs::copy(harness_source, project.join("src/main.rs"))?;
    fs::copy(paired_core_source, project.join("src/paired.rs"))?;
    fs::copy(
        paired_protocol_source,
        project.join("src/paired_protocol.rs"),
    )?;
    fs::copy(
        paired_stats_protocol_source,
        project.join("src/paired_stats_protocol.rs"),
    )?;
    let baseline = manifest_path(baseline_source)?;
    let candidate = manifest_path(candidate_source)?;
    fs::write(
        project.join("Cargo.toml"),
        format!(
            "[package]\nname = \"fs2-paired-stats\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\n[dependencies]\nfs2_baseline = {{ package = {BASELINE_PACKAGE:?}, path = {baseline:?} }}\nfs2_candidate = {{ package = {CANDIDATE_PACKAGE:?}, path = {candidate:?} }}\n\n[workspace]\n"
        ),
    )?;
    Ok(())
}

pub(super) fn rename_package(source: &Path, replacement: &str) -> Result<()> {
    let manifest_path = source.join("Cargo.toml");
    let mut manifest = open_manifest_no_follow(&manifest_path)?;
    let mut contents = String::new();
    manifest.read_to_string(&mut contents)?;
    let mut section = "";
    let mut replacements = 0usize;
    let mut output = String::with_capacity(contents.len() + replacement.len());
    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            section = trimmed;
        }
        if section == "[package]" && matches!(trimmed, "name = \"fs2\"" | "name = \"fs2-turbo\"") {
            output.push_str(&format!("name = {replacement:?}"));
            replacements += 1;
        } else {
            output.push_str(line);
        }
        output.push('\n');
    }
    if replacements != 1 {
        return Err(invalid_data(
            "benchmark source must contain one canonical fs2 or fs2-turbo package name",
        ));
    }
    manifest.seek(SeekFrom::Start(0))?;
    manifest.write_all(output.as_bytes())?;
    manifest.set_len(u64::try_from(output.len())?)?;
    manifest.sync_data()?;
    Ok(())
}

fn open_manifest_no_follow(path: &Path) -> Result<fs::File> {
    let file = open_manifest_handle(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || manifest_is_windows_reparse_point(&metadata) {
        return Err(invalid_data(format!(
            "benchmark Cargo.toml must be a regular file, not a link or reparse point: {}",
            path.display()
        )));
    }
    Ok(file)
}

#[cfg(unix)]
fn open_manifest_handle(path: &Path) -> Result<fs::File> {
    use rustix::fs::{Mode, OFlags, open};

    Ok(fs::File::from(open(
        path,
        OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::empty(),
    )?))
}

#[cfg(windows)]
fn open_manifest_handle(path: &Path) -> Result<fs::File> {
    use std::os::windows::fs::OpenOptionsExt as _;
    use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;

    let mut options = fs::OpenOptions::new();
    options.read(true).write(true);
    options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    Ok(options.open(path)?)
}

#[cfg(not(any(unix, windows)))]
fn open_manifest_handle(path: &Path) -> Result<fs::File> {
    if fs::symlink_metadata(path)?.file_type().is_symlink() {
        return Err(invalid_data(format!(
            "benchmark Cargo.toml may not be a link: {}",
            path.display()
        )));
    }
    Ok(fs::OpenOptions::new().read(true).write(true).open(path)?)
}

#[cfg(windows)]
fn manifest_is_windows_reparse_point(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt as _;
    use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;

    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn manifest_is_windows_reparse_point(_metadata: &fs::Metadata) -> bool {
    false
}

fn manifest_path(path: &Path) -> Result<String> {
    path.canonicalize()?
        .to_str()
        .map(str::to_owned)
        .ok_or_else(|| invalid_data("benchmark source path is not valid Unicode"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_rename_is_scoped_to_the_package_table() {
        let source = tempfile::tempdir().unwrap();
        fs::write(
            source.path().join("Cargo.toml"),
            "[package]\nname = \"fs2\"\n\n[dependencies]\nname = \"fs2\"\n",
        )
        .unwrap();
        rename_package(source.path(), BASELINE_PACKAGE).unwrap();
        let manifest = fs::read_to_string(source.path().join("Cargo.toml")).unwrap();
        assert!(manifest.contains("name = \"fs2-benchmark-baseline\""));
        assert!(manifest.contains("[dependencies]\nname = \"fs2\""));
    }

    #[cfg(unix)]
    #[test]
    fn package_rename_does_not_follow_a_manifest_symlink() {
        let source = tempfile::tempdir().unwrap();
        let external = tempfile::NamedTempFile::new().unwrap();
        fs::write(external.path(), "[package]\nname = \"fs2\"\n").unwrap();
        std::os::unix::fs::symlink(external.path(), source.path().join("Cargo.toml")).unwrap();

        assert!(rename_package(source.path(), BASELINE_PACKAGE).is_err());
        assert_eq!(
            fs::read_to_string(external.path()).unwrap(),
            "[package]\nname = \"fs2\"\n"
        );
    }
}
