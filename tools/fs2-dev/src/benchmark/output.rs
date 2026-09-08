use std::fs;
use std::path::{Path, PathBuf};

use walkdir::WalkDir;

use crate::{Result, invalid_data};

pub(super) struct DestinationGuard {
    path: PathBuf,
    _ancestry: Option<StagingGuard>,
}

impl DestinationGuard {
    pub(super) fn path(&self) -> &Path {
        &self.path
    }
}

pub(super) fn preflight(
    root: &Path,
    destination: &Path,
    label: &str,
    strict: bool,
    minimum_free_bytes: u64,
) -> Result<DestinationGuard> {
    preflight_with_probe(root, destination, strict, |parent, parent_exists| {
        // Do not follow the final entry, including a dangling link.
        let name = destination
            .file_name()
            .ok_or_else(|| invalid_data("benchmark output has no final component"))?;
        let metadata = if parent_exists {
            fs::symlink_metadata(parent.join(name))
        } else {
            Err(std::io::Error::from(std::io::ErrorKind::NotFound))
        };
        match metadata {
            Ok(_) => {
                return Err(invalid_data(format!(
                    "{label} already exists: {}",
                    destination.display()
                )));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        if strict {
            // Only query the existing parent whose ancestry is already retained.
            super::common::ensure_disk_headroom(parent, minimum_free_bytes)
        } else {
            super::common::ensure_output_headroom(destination, minimum_free_bytes)
        }
    })
}

fn preflight_with_probe(
    root: &Path,
    destination: &Path,
    strict: bool,
    probe: impl FnOnce(&Path, bool) -> Result<()>,
) -> Result<DestinationGuard> {
    super::common::require_strict_windows_local_volume(
        root,
        "strict benchmark output root",
        strict,
    )?;
    super::common::require_strict_windows_local_volume(
        destination,
        "strict benchmark output destination",
        strict,
    )?;
    let parent = destination
        .parent()
        .filter(|_| destination.file_name().is_some())
        .ok_or_else(|| invalid_data("benchmark output has no parent or final component"))?;
    let (ancestry, admitted_parent, parent_exists) = if strict {
        let relative = parent.strip_prefix(root).map_err(|_| {
            invalid_data("benchmark output must remain beneath the trusted benchmark root")
        })?;
        if !root.is_absolute()
            || root
                .components()
                .chain(relative.components())
                .any(|component| component == std::path::Component::ParentDir)
        {
            return Err(invalid_data(
                "benchmark output ancestry is not an absolute confined path",
            ));
        }
        let (held, admitted_parent, parent_exists) = retain_output_parent(root, parent)?;
        (Some(held), admitted_parent, parent_exists)
    } else {
        (None, parent.to_owned(), true)
    };
    probe(&admitted_parent, parent_exists)?;
    Ok(DestinationGuard {
        path: parent.join(
            destination
                .file_name()
                .expect("final component was checked"),
        ),
        _ancestry: ancestry,
    })
}

#[cfg(windows)]
fn retain_output_parent(root: &Path, parent: &Path) -> Result<(StagingGuard, PathBuf, bool)> {
    let mut held = super::windows_security::guard_publication_directory_ancestry(root)?;
    let relative = parent.strip_prefix(root)?;
    let mut current = root.to_owned();
    if relative.as_os_str().is_empty() {
        held.push(super::windows_security::open_private_directory(root)?);
    }
    for component in relative.components() {
        let std::path::Component::Normal(name) = component else {
            if component == std::path::Component::CurDir {
                continue;
            }
            return Err(invalid_data("benchmark output escaped its retained root"));
        };
        let next = current.join(name);
        // The parent is held before this no-follow lookup of its immediate child.
        match fs::symlink_metadata(&next) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok((held, current, false));
            }
            Err(error) => return Err(error.into()),
        }
        held.push(hold_windows_ancestry_component(
            &next,
            next == parent,
            false,
        )?);
        current = next;
    }
    Ok((held, current, true))
}

#[cfg(unix)]
fn retain_output_parent(root: &Path, parent: &Path) -> Result<(StagingGuard, PathBuf, bool)> {
    Ok(super::unix_security::retain_output_ancestry(root, parent)?)
}

#[cfg(not(any(unix, windows)))]
fn retain_output_parent(_root: &Path, _parent: &Path) -> Result<(StagingGuard, PathBuf, bool)> {
    Err(invalid_data(
        "strict benchmark output ancestry retention is unavailable",
    ))
}

pub(super) struct StagedDirectory {
    staging: PrivateStaging,
    work: PathBuf,
    root: PathBuf,
    destination: PathBuf,
    strict: bool,
}

pub(super) fn prepare_output_root(path: &Path, strict: bool) -> Result<()> {
    super::common::require_strict_windows_local_volume(
        path,
        "strict benchmark output root",
        strict,
    )?;
    prepare_output_root_platform(path)
}

#[cfg(windows)]
fn prepare_output_root_platform(path: &Path) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| invalid_data("benchmark output root has no parent"))?;
    let _parent_ancestry = super::windows_security::guard_publication_directory_ancestry(parent)?;
    drop(super::windows_security::create_or_open_private_directory(
        path,
    )?);
    Ok(())
}

#[cfg(unix)]
fn prepare_output_root_platform(path: &Path) -> Result<()> {
    // One descriptor-relative walk performs private creation and validation.
    drop(super::unix_security::prepare_directory(
        path,
        "trusted benchmark output root",
        false,
        true,
    )?);
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn prepare_output_root_platform(path: &Path) -> Result<()> {
    match fs::create_dir(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(error.into()),
    }
}

impl StagedDirectory {
    pub(super) fn new(root: &Path, destination: &Path, prefix: &str, strict: bool) -> Result<Self> {
        super::common::require_strict_windows_local_volume(
            root,
            "strict benchmark staging root",
            strict,
        )?;
        super::common::require_strict_windows_local_volume(
            destination,
            "strict benchmark publication destination",
            strict,
        )?;
        let staging = private_staging(root, prefix)?;
        let work = staging.path().join("output");
        create_staged_directory(&work)?;
        #[cfg(windows)]
        drop(super::windows_security::harden_new_private_directory(
            &work,
        )?);
        Ok(Self {
            staging,
            work,
            root: root.to_owned(),
            destination: destination.to_owned(),
            strict,
        })
    }

    pub(super) fn path(&self) -> &Path {
        &self.work
    }

    pub(super) fn publish(self) -> Result<()> {
        super::common::require_strict_windows_local_volume(
            &self.root,
            "strict benchmark staging root",
            self.strict,
        )?;
        super::common::require_strict_windows_local_volume(
            &self.destination,
            "strict benchmark publication destination",
            self.strict,
        )?;
        reject_staged_links(&self.work)?;
        rebase_report_paths(
            &self.work.join("report.json"),
            &[(&self.work, Path::new("."))],
        )?;
        harden_staged_permissions(&self.work)?;
        publish_noclobber(&self.work, &self.destination, Some(&self.root))?;
        drop(self.staging);
        Ok(())
    }
}

pub(super) struct StagedBundle {
    staging: PrivateStaging,
    anchor: PathBuf,
    root: PathBuf,
    strict: bool,
}

impl StagedBundle {
    pub(super) fn new(root: &Path, prefix: &str, strict: bool) -> Result<Self> {
        super::common::require_strict_windows_local_volume(
            root,
            "strict benchmark staging root",
            strict,
        )?;
        let staging = private_staging(root, prefix)?;
        let bundle = staging.path().join("bundle");
        create_staged_directory(&bundle)?;
        #[cfg(windows)]
        drop(super::windows_security::harden_new_private_directory(
            &bundle,
        )?);
        Ok(Self {
            staging,
            anchor: root.to_owned(),
            root: bundle,
            strict,
        })
    }

    pub(super) fn path(&self) -> &Path {
        &self.root
    }

    pub(super) fn publish(
        self,
        staged_report: &Path,
        report: &Path,
        staged_artifacts: &Path,
        artifacts: &Path,
    ) -> Result<()> {
        super::common::require_strict_windows_local_volume(
            &self.anchor,
            "strict benchmark staging root",
            self.strict,
        )?;
        super::common::require_strict_windows_local_volume(
            report,
            "strict benchmark report destination",
            self.strict,
        )?;
        super::common::require_strict_windows_local_volume(
            artifacts,
            "strict benchmark artifact destination",
            self.strict,
        )?;
        reject_staged_links(staged_artifacts)?;
        reject_staged_file_link(staged_report)?;
        rebase_report_paths(
            staged_report,
            &[
                (staged_artifacts, Path::new("artifacts")),
                (staged_report, Path::new("report.json")),
            ],
        )?;
        harden_staged_permissions(&self.root)?;
        publish_noclobber(staged_artifacts, artifacts, Some(&self.anchor))?;
        // The report is the commit marker: sibling artifacts are only a
        // completed publication when the no-clobber report move succeeds.
        if let Err(error) = publish_noclobber(staged_report, report, Some(&self.anchor)) {
            let rollback = rollback_published_artifacts(artifacts);
            let rollback = match rollback {
                Ok(()) => "published artifacts were rolled back".to_owned(),
                Err(rollback_error) => {
                    format!("artifact rollback also failed: {rollback_error}")
                }
            };
            return Err(invalid_data(format!(
                "artifacts were published but the report commit marker failed: {error}; {rollback}"
            )));
        }
        drop(self.staging);
        Ok(())
    }
}

struct PrivateStaging {
    _guard: StagingGuard,
    temporary: tempfile::TempDir,
}

impl PrivateStaging {
    fn path(&self) -> &Path {
        self.temporary.path()
    }
}

#[cfg(unix)]
fn create_staged_directory(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt as _;

    fs::DirBuilder::new().mode(0o700).create(path)
}

#[cfg(not(unix))]
fn create_staged_directory(path: &Path) -> std::io::Result<()> {
    fs::create_dir(path)
}

#[cfg(unix)]
fn harden_staged_permissions(root: &Path) -> Result<()> {
    for entry in WalkDir::new(root).follow_links(false) {
        let entry = entry?;
        if !entry.file_type().is_dir() && !entry.file_type().is_file() {
            return Err(invalid_data(format!(
                "staged benchmark output contains a non-regular entry: {}",
                entry.path().display()
            )));
        }
        super::unix_security::harden_publication_path(
            entry.path(),
            "staged benchmark publication",
        )?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn harden_staged_permissions(_root: &Path) -> Result<()> {
    Ok(())
}

#[cfg(windows)]
type StagingGuard = Vec<fs::File>;

#[cfg(unix)]
type StagingGuard = Vec<std::os::fd::OwnedFd>;

#[cfg(not(any(unix, windows)))]
type StagingGuard = ();

fn private_staging(root: &Path, prefix: &str) -> Result<PrivateStaging> {
    let parent = root.join("target").join(".fs2-secure-staging");
    let mut guard = prepare_private_staging_parent(root, &parent)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o700))?;
    }
    let temporary = tempfile::Builder::new().prefix(prefix).tempdir_in(parent)?;
    guard_private_staging_directory(temporary.path(), &mut guard)?;
    Ok(PrivateStaging {
        _guard: guard,
        temporary,
    })
}

#[cfg(windows)]
fn prepare_private_staging_parent(root: &Path, parent: &Path) -> Result<StagingGuard> {
    // Retain the complete path that names the output root. Descendant handles
    // alone cannot prevent an attacker from rebinding an ancestor before the
    // final absolute-path MoveFileW publication.
    let mut held =
        super::windows_security::guard_publication_directory_ancestry(root).map_err(|error| {
            invalid_data(format!(
                "unable to retain benchmark output-root ancestry {}: {error}",
                root.display()
            ))
        })?;
    let target = root.join("target");
    held.push(
        super::windows_security::create_or_open_trusted_directory(&target).map_err(|error| {
            invalid_data(format!(
                "unable to retain or create benchmark staging target {}: {error}",
                target.display()
            ))
        })?,
    );
    held.push(
        super::windows_security::create_or_open_private_directory(parent).map_err(|error| {
            invalid_data(format!(
                "unable to secure benchmark staging parent {}: {error}",
                parent.display()
            ))
        })?,
    );
    Ok(held)
}

#[cfg(unix)]
fn prepare_private_staging_parent(root: &Path, parent: &Path) -> Result<StagingGuard> {
    let target = root.join("target");
    if fs::symlink_metadata(&target).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return Err(invalid_data(format!(
            "benchmark staging target is a link: {}",
            target.display()
        )));
    }
    Ok(super::unix_security::prepare_directory(
        parent,
        "benchmark private staging parent",
        false,
        true,
    )?)
}

#[cfg(not(any(unix, windows)))]
fn prepare_private_staging_parent(_root: &Path, parent: &Path) -> Result<StagingGuard> {
    create_directory_no_reparse(parent)?;
    Ok(())
}

#[cfg(windows)]
fn guard_private_staging_directory(path: &Path, held: &mut StagingGuard) -> Result<()> {
    held.push(super::windows_security::harden_new_private_directory(path)?);
    Ok(())
}

#[cfg(unix)]
fn guard_private_staging_directory(path: &Path, held: &mut StagingGuard) -> Result<()> {
    held.extend(super::unix_security::prepare_directory(
        path,
        "benchmark private staging directory",
        false,
        false,
    )?);
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn guard_private_staging_directory(_path: &Path, _held: &mut StagingGuard) -> Result<()> {
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn create_directory_no_reparse(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || is_windows_reparse_point(path)? {
                return Err(invalid_data(format!(
                    "benchmark staging ancestry is a link or reparse point: {}",
                    path.display()
                )));
            }
            if !metadata.is_dir() {
                return Err(invalid_data(format!(
                    "benchmark staging ancestry is not a directory: {}",
                    path.display()
                )));
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir(path)?;
            reject_link_or_reparse(path, "benchmark staging ancestry")?;
        }
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn reject_link_or_reparse(path: &Path, label: &str) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || is_windows_reparse_point(path)? {
        return Err(invalid_data(format!(
            "{label} is a link or reparse point: {}",
            path.display()
        )));
    }
    Ok(())
}

fn reject_staged_links(root: &Path) -> Result<()> {
    for entry in WalkDir::new(root).follow_links(false) {
        let entry = entry?;
        if entry.file_type().is_symlink() || is_windows_reparse_point(entry.path())? {
            return Err(invalid_data(format!(
                "staged benchmark output contains a link or reparse point: {}",
                entry.path().display()
            )));
        }
    }
    Ok(())
}

fn reject_staged_file_link(path: &Path) -> Result<()> {
    if fs::symlink_metadata(path)?.file_type().is_symlink() || is_windows_reparse_point(path)? {
        return Err(invalid_data(format!(
            "staged benchmark report is a link or reparse point: {}",
            path.display()
        )));
    }
    Ok(())
}

fn rebase_report_paths(report: &Path, replacements: &[(&Path, &Path)]) -> Result<()> {
    if !report.exists() {
        return Ok(());
    }
    let mut value = serde_json::from_slice::<serde_json::Value>(&fs::read(report)?)?;
    rebase_json_value(&mut value, replacements);
    sanitize_serialized_report(&mut value);
    let mut output = serde_json::to_vec_pretty(&value)?;
    output.push(b'\n');
    fs::write(report, output)?;
    Ok(())
}

fn rebase_json_value(value: &mut serde_json::Value, replacements: &[(&Path, &Path)]) {
    match value {
        serde_json::Value::String(text) => {
            for (from, to) in replacements {
                if let Some(rebased) = rebase_path_string(text, from, to) {
                    *text = rebased;
                    break;
                }
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                rebase_json_value(item, replacements);
            }
        }
        serde_json::Value::Object(entries) => {
            for item in entries.values_mut() {
                rebase_json_value(item, replacements);
            }
        }
        _ => {}
    }
}

fn rebase_path_string(text: &str, from: &Path, to: &Path) -> Option<String> {
    let from = from.to_string_lossy();
    let to = to.to_string_lossy();
    let suffix = text.strip_prefix(from.as_ref())?;
    if suffix.is_empty()
        || suffix
            .as_bytes()
            .first()
            .is_some_and(|byte| *byte == b'/' || *byte == b'\\')
    {
        Some(format!("{to}{suffix}"))
    } else {
        None
    }
}

pub(super) fn sanitize_serialized_report(value: &mut serde_json::Value) {
    sanitize_json_value(value, None);
}

fn sanitize_json_value(value: &mut serde_json::Value, key: Option<&str>) {
    match value {
        serde_json::Value::String(text) => match key {
            Some("current_dir") => *text = "<working-directory>".to_owned(),
            Some("stdout") => *text = logical_log_path(text, "stdout"),
            Some("stderr") => *text = logical_log_path(text, "stderr"),
            _ if contains_host_path(text) => *text = "<host-path>".to_owned(),
            _ => {}
        },
        serde_json::Value::Array(items) => {
            for item in items {
                sanitize_json_value(item, key);
            }
        }
        serde_json::Value::Object(entries) => {
            let reserved = entries
                .keys()
                .cloned()
                .collect::<std::collections::BTreeSet<_>>();
            let original = std::mem::take(entries);
            let mut redacted_index = 0usize;
            for (name, mut item) in original {
                if key == Some("environment_overrides") && item.is_string() {
                    item = serde_json::Value::String("<configured>".to_owned());
                } else {
                    sanitize_json_value(&mut item, Some(&name));
                }
                let name = if contains_host_path(&name) {
                    loop {
                        let candidate = format!("<host-path-key-{redacted_index}>");
                        redacted_index += 1;
                        if !reserved.contains(&candidate) && !entries.contains_key(&candidate) {
                            break candidate;
                        }
                    }
                } else {
                    name
                };
                entries.insert(name, item);
            }
        }
        _ => {}
    }
}

fn logical_log_path(value: &str, fallback: &str) -> String {
    let file_name = Path::new(value)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| {
            !name.is_empty()
                && name.len() <= 128
                && name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
        })
        .unwrap_or(fallback);
    format!("logs/{file_name}")
}

fn contains_host_path(value: &str) -> bool {
    if Path::new(value).is_absolute() || value.contains(r"\\?\") || value.contains(r"\\.\") {
        return true;
    }

    let bytes = value.as_bytes();
    if bytes.len() >= 3 {
        for index in 0..=(bytes.len() - 3) {
            if bytes[index].is_ascii_alphabetic()
                && bytes[index + 1] == b':'
                && matches!(bytes[index + 2], b'/' | b'\\')
                && (index == 0 || is_path_boundary(bytes[index - 1]))
            {
                return true;
            }
        }
    }

    value
        .split(|character: char| {
            !character.is_ascii_alphanumeric()
                && !matches!(character, '_' | '-' | '.' | '/' | '\\' | '?')
        })
        .map(|token| token.trim_end_matches([')', ']', '}', ':', '`', '>']))
        .any(|token| {
            token.starts_with(r"\\")
                || token.starts_with('/')
                || token.starts_with(r"\Device\")
                || token.starts_with(r"\??\")
                || token
                    .strip_prefix('\\')
                    .is_some_and(|suffix| suffix.contains('\\') || suffix.contains('/'))
        })
}

fn is_path_boundary(byte: u8) -> bool {
    !byte.is_ascii_alphanumeric() && !matches!(byte, b'_' | b'-' | b'.' | b'/' | b'\\' | b'?')
}

fn rollback_published_artifacts(path: &Path) -> Result<()> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    if metadata.file_type().is_symlink() || is_windows_reparse_point(path)? {
        return Err(invalid_data(format!(
            "published artifact rollback refused link or reparse point: {}",
            path.display()
        )));
    }
    if !metadata.is_dir() {
        return Err(invalid_data(format!(
            "published artifact rollback refused non-directory path: {}",
            path.display()
        )));
    }
    reject_staged_links(path)?;
    fs::remove_dir_all(path)?;
    Ok(())
}

#[cfg(windows)]
fn is_windows_reparse_point(path: &Path) -> Result<bool> {
    use std::os::windows::fs::MetadataExt as _;
    use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;

    Ok(fs::symlink_metadata(path)?.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0)
}

#[cfg(not(windows))]
fn is_windows_reparse_point(_path: &Path) -> Result<bool> {
    Ok(false)
}

#[cfg(unix)]
fn publish_noclobber(source: &Path, destination: &Path, anchor: Option<&Path>) -> Result<()> {
    let (source_parent, source_name) = secure_parent(source, false, anchor)?;
    let (destination_parent, destination_name) = secure_parent(destination, true, anchor)?;
    atomic_rename_noclobber(
        &source_parent,
        &source_name,
        &destination_parent,
        &destination_name,
    )?;
    Ok(())
}

#[cfg(unix)]
fn secure_parent(
    path: &Path,
    create_missing: bool,
    anchor: Option<&Path>,
) -> Result<(std::os::fd::OwnedFd, std::ffi::OsString)> {
    if !path.is_absolute() {
        return Err(invalid_data("publication path must be absolute"));
    }
    let name = path
        .file_name()
        .ok_or_else(|| invalid_data("publication path has no final component"))?
        .to_owned();
    let parent = path
        .parent()
        .ok_or_else(|| invalid_data("publication path has no parent"))?;
    if let Some(anchor) = anchor {
        parent.strip_prefix(anchor).map_err(|_| {
            invalid_data(format!(
                "publication destination must remain beneath the trusted benchmark root: {}",
                anchor.display()
            ))
        })?;
    }
    let mut authority_guard = super::unix_security::prepare_directory(
        parent,
        "benchmark publication parent",
        false,
        create_missing,
    )?;
    let directory = authority_guard
        .pop()
        .ok_or_else(|| invalid_data("publication parent validation returned no directory"))?;
    Ok((directory, name))
}

#[cfg(all(
    unix,
    any(
        target_os = "android",
        target_os = "ios",
        target_os = "linux",
        target_os = "macos",
        target_os = "redox",
        target_os = "tvos",
        target_os = "visionos",
        target_os = "watchos"
    )
))]
fn atomic_rename_noclobber(
    source_parent: &std::os::fd::OwnedFd,
    source_name: &std::ffi::OsStr,
    destination_parent: &std::os::fd::OwnedFd,
    destination_name: &std::ffi::OsStr,
) -> std::io::Result<()> {
    rustix::fs::renameat_with(
        source_parent,
        source_name,
        destination_parent,
        destination_name,
        rustix::fs::RenameFlags::NOREPLACE,
    )
    .map_err(Into::into)
}

#[cfg(all(
    unix,
    not(any(
        target_os = "android",
        target_os = "ios",
        target_os = "linux",
        target_os = "macos",
        target_os = "redox",
        target_os = "tvos",
        target_os = "visionos",
        target_os = "watchos"
    ))
))]
fn atomic_rename_noclobber(
    _source_parent: &std::os::fd::OwnedFd,
    _source_name: &std::ffi::OsStr,
    _destination_parent: &std::os::fd::OwnedFd,
    _destination_name: &std::ffi::OsStr,
) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "atomic no-replace output publication is unavailable on this Unix target",
    ))
}

#[cfg(windows)]
fn publish_noclobber(source: &Path, destination: &Path, anchor: Option<&Path>) -> Result<()> {
    use std::os::windows::ffi::OsStrExt as _;
    use std::os::windows::io::FromRawHandle as _;
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, DELETE, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, MoveFileW, OPEN_EXISTING,
    };

    super::windows_security::reject_ambiguous_path(destination)?;
    if let Some(anchor) = anchor {
        super::windows_security::reject_ambiguous_path(anchor)?;
    }
    let held_parents =
        hold_windows_parent_ancestry(destination, true, anchor).map_err(|error| {
            invalid_data(format!(
                "unable to bind publication destination ancestry: {error}"
            ))
        })?;
    let _destination_parent = held_parents
        .last()
        .ok_or_else(|| invalid_data("publication destination has no opened parent"))?;
    let source_path = source
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let source_handle = unsafe {
        // SAFETY: `source_path` is terminated. DELETE is the access required
        // for rename, and the validated staging entry is not traversed through
        // a reparse point.
        CreateFileW(
            source_path.as_ptr(),
            DELETE,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            std::ptr::null_mut(),
        )
    };
    if source_handle == INVALID_HANDLE_VALUE {
        return Err(invalid_data(format!(
            "unable to open staged output for publication: {}",
            std::io::Error::last_os_error()
        )));
    }
    let source_handle = unsafe {
        // SAFETY: ownership of the newly opened source handle transfers to File.
        fs::File::from_raw_handle(source_handle)
    };
    let destination_path = destination
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    // MoveFileW requires the source handle to be closed. The source remains
    // protected by the private staging directory while destination ancestry
    // handles prevent namespace replacement through the move.
    drop(source_handle);
    let result = unsafe {
        // SAFETY: both paths are terminated and remain alive for the call.
        // MoveFileW fails when the destination already exists.
        MoveFileW(source_path.as_ptr(), destination_path.as_ptr())
    };
    if result == 0 {
        Err(invalid_data(format!(
            "no-clobber publication rename failed: {}",
            std::io::Error::last_os_error()
        )))
    } else {
        Ok(())
    }
}

#[cfg(windows)]
fn hold_windows_parent_ancestry(
    path: &Path,
    create_missing: bool,
    anchor: Option<&Path>,
) -> Result<Vec<fs::File>> {
    use std::path::Component;

    let parent = path
        .parent()
        .ok_or_else(|| invalid_data("publication path has no parent"))?;
    if let Some(anchor) = anchor {
        let relative_parent = parent.strip_prefix(anchor).map_err(|_| {
            invalid_data(format!(
                "publication destination must remain beneath the trusted benchmark root: {}",
                anchor.display()
            ))
        })?;
        let mut current = anchor.to_owned();
        let mut held = vec![hold_windows_ancestry_component(
            anchor,
            current == parent,
            create_missing,
        )?];
        for component in relative_parent.components() {
            match component {
                Component::CurDir => continue,
                Component::Normal(component) => current.push(component),
                _ => {
                    return Err(invalid_data(
                        "publication path escaped the benchmark output anchor",
                    ));
                }
            }
            held.push(hold_windows_ancestry_component(
                &current,
                current == parent,
                create_missing,
            )?);
        }
        return Ok(held);
    }
    let mut current = PathBuf::new();
    let mut held = Vec::new();
    for component in parent.components() {
        match component {
            Component::Prefix(_) | Component::RootDir => current.push(component.as_os_str()),
            Component::CurDir => continue,
            Component::ParentDir => {
                return Err(invalid_data(
                    "publication path may not contain parent-directory components",
                ));
            }
            Component::Normal(component) => current.push(component),
        }
        if current == parent {
            held.push(
                hold_windows_ancestry_component(&current, true, create_missing).map_err(
                    |error| {
                        invalid_data(format!(
                            "unable to secure publication parent {}: {error}",
                            current.display()
                        ))
                    },
                )?,
            );
        } else {
            held.push(
                hold_windows_ancestry_component(&current, false, create_missing).map_err(
                    |error| {
                        invalid_data(format!(
                            "unable to secure publication ancestry {}: {error}",
                            current.display()
                        ))
                    },
                )?,
            );
        }
    }
    Ok(held)
}

#[cfg(windows)]
fn hold_windows_ancestry_component(
    path: &Path,
    publication_parent: bool,
    create_missing: bool,
) -> Result<fs::File> {
    if publication_parent {
        if create_missing {
            super::windows_security::create_or_open_private_directory(path)
        } else {
            super::windows_security::open_private_directory(path)
        }
    } else if create_missing {
        super::windows_security::create_or_open_trusted_directory(path)
    } else {
        super::windows_security::open_trusted_ancestor(path)
    }
}

#[cfg(not(any(unix, windows)))]
fn publish_noclobber(_source: &Path, _destination: &Path, _anchor: Option<&Path>) -> Result<()> {
    Err(invalid_data(
        "atomic no-replace output publication is unavailable on this platform",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    fn protect_publication_parent(path: &Path) {
        drop(super::super::windows_security::harden_new_private_directory(path).unwrap());
    }

    #[cfg(not(windows))]
    fn protect_publication_parent(_path: &Path) {}

    #[cfg(windows)]
    fn publication_tempdir() -> tempfile::TempDir {
        super::super::windows_security::private_test_tempdir()
    }

    #[cfg(not(windows))]
    fn publication_tempdir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn report_publication_uses_relative_bundle_ids_and_redacts_external_paths() {
        let staged = Path::new(r"C:\Users\sentinel-user\private-stage\artifacts");
        let mut value = serde_json::json!({
            "bundle_binary": format!(r"{}\binary\paired.exe", staged.display()),
            "baseline_repository": r"C:\Users\sentinel-user\private-repository\baseline",
            "fixture": "/home/sentinel-user/private-fixture/input",
            "message": r"failed beneath C:\Users\sentinel-user\private-directory",
            "root_path_message": "baseline is not a crate checkout: /secret",
            "colon_path": "path:/home/sentinel-user/private-colon",
            "angle_path": r"<C:\Users\sentinel-user\private-angle>",
            "nt_path": r"error:\Device\HarddiskVolume3\Users\sentinel-user\private-nt",
            "closing_paren_path": "tag)/home/sentinel-user/private-paren",
            "closing_bracket_path": r"tag]\\server\share\sentinel-user\private-bracket",
            "closing_brace_path": r"tag}\Device\HarddiskVolume3\Users\sentinel-user\private-brace",
            "closing_drive_path": r"tag)C:\sentinel-user\private-drive",
            "dynamic": {},
            "safe_fact": "rustc 1.88.0"
        });
        value["dynamic"].as_object_mut().unwrap().insert(
            "path:/home/sentinel-user/private-key".to_owned(),
            serde_json::Value::String("safe".to_owned()),
        );
        value["dynamic"].as_object_mut().unwrap().insert(
            r"tag)C:\sentinel-user\private-drive-key".to_owned(),
            serde_json::Value::String("safe".to_owned()),
        );

        rebase_json_value(&mut value, &[(staged, Path::new("artifacts"))]);
        sanitize_serialized_report(&mut value);

        assert_eq!(
            value["bundle_binary"],
            serde_json::Value::String(r"artifacts\binary\paired.exe".to_owned())
        );
        assert_eq!(value["baseline_repository"], "<host-path>");
        assert_eq!(value["fixture"], "<host-path>");
        assert_eq!(value["message"], "<host-path>");
        assert_eq!(value["root_path_message"], "<host-path>");
        assert_eq!(value["colon_path"], "<host-path>");
        assert_eq!(value["angle_path"], "<host-path>");
        assert_eq!(value["nt_path"], "<host-path>");
        assert_eq!(value["closing_paren_path"], "<host-path>");
        assert_eq!(value["closing_bracket_path"], "<host-path>");
        assert_eq!(value["closing_brace_path"], "<host-path>");
        assert_eq!(value["closing_drive_path"], "<host-path>");
        assert_eq!(value["safe_fact"], "rustc 1.88.0");
        let json = serde_json::to_string(&value).unwrap();
        assert!(!json.contains("sentinel-user"));
        assert!(!json.contains("private-repository"));
        assert!(!json.contains("private-fixture"));
        assert!(!json.contains("private-directory"));
    }

    #[test]
    fn preflight_admits_missing_output_and_preserves_existing_content() {
        let root = publication_tempdir();
        protect_publication_parent(root.path());
        let destination = root.path().join("results").join("report.json");
        let held = preflight(root.path(), &destination, "report", true, 0).unwrap();
        assert!(!destination.parent().unwrap().exists());
        fs::create_dir(destination.parent().unwrap()).unwrap();
        protect_publication_parent(destination.parent().unwrap());
        fs::write(&destination, b"victim").unwrap();
        assert!(preflight(root.path(), &destination, "report", true, 0).is_err());
        assert_eq!(fs::read(&destination).unwrap(), b"victim");
        drop(held);
    }

    #[test]
    fn preflight_probes_only_after_retaining_the_existing_parent() {
        let root = publication_tempdir();
        protect_publication_parent(root.path());
        let parent = root.path().join("results");
        let destination = parent.join("report.json");
        fs::create_dir(&parent).unwrap();
        protect_publication_parent(&parent);
        let held = preflight_with_probe(root.path(), &destination, true, |probe, parent_exists| {
            assert!(parent_exists);
            assert_eq!(probe, parent);
            assert!(probe.is_dir());
            #[cfg(windows)]
            assert!(fs::rename(probe, root.path().join("replacement")).is_err());
            Ok(())
        })
        .unwrap();
        drop(held);
    }

    #[test]
    fn preflight_rejects_output_outside_anchor_before_probe() {
        let root = publication_tempdir();
        let external = publication_tempdir();
        let mut queried = false;
        let result = preflight_with_probe(
            root.path(),
            &external.path().join("report.json"),
            true,
            |_, _| {
                queried = true;
                Ok(())
            },
        );
        assert!(result.is_err());
        assert!(!queried);
    }

    #[cfg(windows)]
    #[test]
    fn preflight_rejects_unc_and_namespace_aliases_before_probe() {
        for destination in [
            r"\\server\share\report.json",
            r"\\?\UNC\server\share\report.json",
            r"\\?\C:\report.json",
            r"C:\trusted\.. \report.json",
            r"C:\trusted\report.json:stream",
        ] {
            let mut queried = false;
            let result = preflight_with_probe(
                Path::new(r"C:\trusted"),
                Path::new(destination),
                true,
                |_, _| {
                    queried = true;
                    Ok(())
                },
            );
            assert!(result.is_err(), "{destination}");
            assert!(!queried, "{destination}");
        }
    }

    #[cfg(windows)]
    #[test]
    fn preflight_rejects_reparse_parent_before_probe() {
        let root = publication_tempdir();
        let external = publication_tempdir();
        let link = root.path().join("redirect");
        let status = std::process::Command::new("cmd")
            .args(["/c", "mklink", "/J"])
            .arg(&link)
            .arg(external.path())
            .status()
            .unwrap();
        assert!(status.success());
        let mut queried = false;
        let result = preflight_with_probe(root.path(), &link.join("report.json"), true, |_, _| {
            queried = true;
            Ok(())
        });
        assert!(result.is_err());
        assert!(!queried);
    }

    #[cfg(unix)]
    #[test]
    fn preflight_rejects_mutable_parent_before_probe() {
        use std::os::unix::fs::PermissionsExt as _;

        let root = publication_tempdir();
        let parent = root.path().join("mutable");
        fs::create_dir(&parent).unwrap();
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o777)).unwrap();
        let mut queried = false;
        let result =
            preflight_with_probe(root.path(), &parent.join("report.json"), true, |_, _| {
                queried = true;
                Ok(())
            });
        assert!(result.is_err());
        assert!(!queried);
    }

    #[test]
    fn preflight_stops_at_missing_parent_without_creating_or_probing_descendants() {
        let root = publication_tempdir();
        let destination = root.path().join("absent/nested/report.json");
        let held = preflight_with_probe(root.path(), &destination, true, |probe, parent_exists| {
            assert_eq!(probe, root.path());
            assert!(!parent_exists);
            Ok(())
        })
        .unwrap();
        assert!(!root.path().join("absent").exists());
        drop(held);
    }

    #[cfg(unix)]
    #[test]
    fn preflight_rejects_descendant_links_and_dangling_leaf_collisions() {
        let root = publication_tempdir();
        let external = publication_tempdir();
        let link = root.path().join("redirect");
        std::os::unix::fs::symlink(external.path(), &link).unwrap();
        let mut queried = false;
        let result = preflight_with_probe(root.path(), &link.join("report"), true, |_, _| {
            queried = true;
            Ok(())
        });
        assert!(result.is_err());
        assert!(!queried);

        let dangling = root.path().join("dangling");
        std::os::unix::fs::symlink("missing", &dangling).unwrap();
        assert!(preflight(root.path(), &dangling, "report", true, 0).is_err());
    }

    #[test]
    fn exploratory_preflight_does_not_claim_retained_ancestry() {
        let mut queried = false;
        let held = preflight_with_probe(
            Path::new("root"),
            Path::new("elsewhere/report"),
            false,
            |_, _| {
                queried = true;
                Ok(())
            },
        )
        .unwrap();
        assert!(queried);
        assert!(held._ancestry.is_none());
    }

    #[test]
    fn publication_never_replaces_existing_file() {
        let root = publication_tempdir();
        protect_publication_parent(root.path());
        let destination = root.path().join("report.json");
        fs::write(&destination, b"victim").unwrap();
        let staged = private_staging(root.path(), "publish-file-").unwrap();
        let source = staged.path().join("report.json");
        fs::write(&source, b"replacement").unwrap();

        assert!(publish_noclobber(&source, &destination, Some(root.path())).is_err());
        assert_eq!(fs::read(destination).unwrap(), b"victim");
    }

    #[test]
    fn publication_rejects_destination_outside_trusted_anchor() {
        let root = publication_tempdir();
        protect_publication_parent(root.path());
        let external = publication_tempdir();
        protect_publication_parent(external.path());
        let staged = private_staging(root.path(), "publish-outside-").unwrap();
        let source = staged.path().join("report.json");
        fs::write(&source, b"result").unwrap();
        let destination = external.path().join("report.json");

        assert!(publish_noclobber(&source, &destination, Some(root.path())).is_err());
        assert!(!destination.exists());
    }

    #[cfg(windows)]
    #[test]
    fn publication_rejects_win32_normalization_aliases() {
        let root = publication_tempdir();
        protect_publication_parent(root.path());
        let staged = private_staging(root.path(), "publish-alias-").unwrap();
        let source = staged.path().join("report.json");
        fs::write(&source, b"result").unwrap();
        let destination = root.path().join(".. ").join("report.json");

        assert!(publish_noclobber(&source, &destination, Some(root.path())).is_err());
    }

    #[test]
    fn publication_moves_a_new_directory() {
        let root = publication_tempdir();
        protect_publication_parent(root.path());
        let destination = root.path().join("output");
        let staged =
            StagedDirectory::new(root.path(), &destination, "publish-new-", false).unwrap();
        fs::write(staged.path().join("result"), b"result").unwrap();

        staged.publish().unwrap();

        assert_eq!(fs::read(destination.join("result")).unwrap(), b"result");
    }

    #[test]
    fn explicit_output_root_supports_secure_publication() {
        let parent = publication_tempdir();
        let root = parent.path().join("trusted-output");
        prepare_output_root(&root, false).unwrap();
        let destination = root.join("result");
        let staged = StagedDirectory::new(&root, &destination, "publish-explicit-", false).unwrap();
        fs::write(staged.path().join("result"), b"result").unwrap();

        staged.publish().unwrap();

        assert_eq!(fs::read(destination.join("result")).unwrap(), b"result");
    }

    #[test]
    fn publication_never_replaces_existing_directory() {
        let root = publication_tempdir();
        protect_publication_parent(root.path());
        let destination = root.path().join("output");
        fs::create_dir(&destination).unwrap();
        fs::write(destination.join("victim"), b"victim").unwrap();
        let staged =
            StagedDirectory::new(root.path(), &destination, "publish-dir-", false).unwrap();
        fs::write(staged.path().join("result"), b"result").unwrap();

        assert!(staged.publish().is_err());
        assert_eq!(fs::read(destination.join("victim")).unwrap(), b"victim");
    }

    #[test]
    fn staged_links_are_rejected_before_publication() {
        let root = publication_tempdir();
        protect_publication_parent(root.path());
        let destination = root.path().join("output");
        let staged =
            StagedDirectory::new(root.path(), &destination, "publish-link-", false).unwrap();
        let external = root.path().join("external");
        #[cfg(unix)]
        {
            fs::write(&external, b"victim").unwrap();
            std::os::unix::fs::symlink(&external, staged.path().join("link")).unwrap();
        }
        #[cfg(windows)]
        {
            fs::create_dir(&external).unwrap();
            fs::write(external.join("victim"), b"victim").unwrap();
            let status = std::process::Command::new("cmd")
                .args(["/c", "mklink", "/J"])
                .arg(staged.path().join("link"))
                .arg(&external)
                .status()
                .unwrap();
            assert!(status.success());
        }

        assert!(staged.publish().is_err());
        #[cfg(unix)]
        assert_eq!(fs::read(external).unwrap(), b"victim");
        #[cfg(windows)]
        assert_eq!(fs::read(external.join("victim")).unwrap(), b"victim");
        assert!(!destination.exists());
    }

    #[test]
    fn private_staging_rejects_target_link_ancestry() {
        let root = publication_tempdir();
        let external = root.path().join("external");
        fs::create_dir(&external).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&external, root.path().join("target")).unwrap();
        #[cfg(windows)]
        {
            let status = std::process::Command::new("cmd")
                .args(["/c", "mklink", "/J"])
                .arg(root.path().join("target"))
                .arg(&external)
                .status()
                .unwrap();
            assert!(status.success());
        }

        assert!(private_staging(root.path(), "publish-target-link-").is_err());
    }

    #[cfg(windows)]
    #[test]
    fn held_publication_ancestry_cannot_be_renamed() {
        let root = publication_tempdir();
        protect_publication_parent(root.path());
        let parent = root.path().join("results");
        fs::create_dir(&parent).unwrap();
        protect_publication_parent(&parent);
        let destination = parent.join("report.json");
        let held = hold_windows_parent_ancestry(&destination, false, Some(root.path())).unwrap();

        assert!(fs::rename(&parent, root.path().join("replacement")).is_err());
        drop(held);
    }

    #[test]
    fn concurrent_publishers_have_one_winner() {
        let root = publication_tempdir();
        protect_publication_parent(root.path());
        let destination = root.path().join("output");
        let first = StagedDirectory::new(root.path(), &destination, "publish-a-", false).unwrap();
        let second = StagedDirectory::new(root.path(), &destination, "publish-b-", false).unwrap();
        fs::write(first.path().join("winner"), b"a").unwrap();
        fs::write(second.path().join("winner"), b"b").unwrap();

        let first = std::thread::spawn(move || first.publish());
        let second = std::thread::spawn(move || second.publish());
        let results = [first.join().unwrap(), second.join().unwrap()];

        assert_eq!(
            results.iter().filter(|result| result.is_ok()).count(),
            1,
            "{results:?}"
        );
        let winner = fs::read(destination.join("winner")).unwrap();
        assert!(winner == b"a" || winner == b"b");
    }
}
