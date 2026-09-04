use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Deserialize;
use sha2::{Digest, Sha256};
use walkdir::{DirEntry, WalkDir};

use crate::process::{self, ProcessRecord};
use crate::{Result, invalid_data, lower_hex};

pub(crate) fn default_output(root: &Path, prefix: &str) -> Result<PathBuf> {
    let epoch = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
    Ok(root
        .join("target/measurement-runs")
        .join(format!("{prefix}-{epoch}")))
}

pub(crate) fn hash_file(path: &Path) -> Result<String> {
    let mut reader = BufReader::new(File::open(path)?);
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(lower_hex(digest.finalize()))
}

pub(crate) fn hash_bytes(bytes: &[u8]) -> String {
    lower_hex(Sha256::digest(bytes))
}

pub(crate) fn normalized_text_hash(path: &Path) -> Result<String> {
    let text = fs::read_to_string(path)?;
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    Ok(lower_hex(Sha256::digest(normalized.as_bytes())))
}

pub(crate) fn retain_artifact(source: &Path, destination: &Path) -> Result<PathBuf> {
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::copy(source, destination)?;
    Ok(destination.to_owned())
}

pub(crate) fn retain_bytes(bytes: &[u8], destination: &Path) -> Result<PathBuf> {
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut destination_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)?;
    destination_file.write_all(bytes)?;
    Ok(destination.to_owned())
}

#[derive(Deserialize)]
struct CargoArtifactMessage {
    reason: String,
    target: Option<CargoTarget>,
    executable: Option<PathBuf>,
}

#[derive(Deserialize)]
struct CargoTarget {
    name: String,
}

#[derive(Deserialize)]
struct CargoMetadata {
    packages: Vec<CargoMetadataPackage>,
}

#[derive(Deserialize)]
struct CargoMetadataPackage {
    name: String,
    manifest_path: PathBuf,
    source: Option<String>,
    targets: Vec<CargoMetadataTarget>,
}

#[derive(Deserialize)]
struct CargoMetadataTarget {
    src_path: PathBuf,
}

pub(crate) fn cargo_executable(message_log: &Path, target_name: &str) -> Result<PathBuf> {
    let contents = fs::read_to_string(message_log)?;
    let mut executable = None;
    for (index, line) in contents.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let message = serde_json::from_str::<CargoArtifactMessage>(line).map_err(|error| {
            invalid_data(format!(
                "malformed Cargo JSON message on line {}: {error}",
                index + 1
            ))
        })?;
        if message.reason == "compiler-artifact"
            && message
                .target
                .as_ref()
                .is_some_and(|target| target.name == target_name)
            && let Some(path) = message.executable
        {
            executable = Some(path);
        }
    }
    executable.filter(|path| path.is_file()).ok_or_else(|| {
        invalid_data(format!(
            "Cargo did not report an executable artifact for {target_name}"
        ))
    })
}

pub(crate) struct RetainedExecutable {
    path: PathBuf,
    file: File,
    metadata: fs::Metadata,
    sha256: String,
}

impl RetainedExecutable {
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn sha256(&self) -> &str {
        &self.sha256
    }

    pub(crate) fn validate(&self) -> Result<()> {
        let path_metadata = fs::symlink_metadata(&self.path)?;
        ensure_safe_tree_entry(&self.path, &path_metadata, "benchmark executable")?;
        let handle_metadata = self.file.metadata()?;
        if !path_metadata.is_file()
            || !handle_metadata.is_file()
            || !tree_metadata_matches(&self.metadata, &path_metadata)
            || !tree_metadata_matches(&self.metadata, &handle_metadata)
            || hash_open_file(&self.file)? != self.sha256
        {
            return Err(invalid_data(
                "benchmark executable identity or content changed after build",
            ));
        }
        Ok(())
    }
}

pub(crate) fn retained_cargo_executable(
    message_log: &Path,
    target_name: &str,
    target_root: &Path,
) -> Result<RetainedExecutable> {
    let reported = cargo_executable(message_log, target_name)?;
    if !reported.is_absolute() {
        return Err(invalid_data("Cargo reported a relative executable path"));
    }
    let target_root = target_root.canonicalize()?;
    let path = reported.canonicalize()?;
    if path.strip_prefix(&target_root).is_err() {
        return Err(invalid_data(format!(
            "Cargo reported an executable outside the expected target: {}",
            path.display()
        )));
    }
    let path_metadata = fs::symlink_metadata(&reported)?;
    ensure_safe_tree_entry(&reported, &path_metadata, "benchmark executable")?;
    let file = open_live_tree_file(&reported)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || !tree_metadata_matches(&path_metadata, &metadata) {
        return Err(invalid_data(
            "benchmark executable changed while its handle was retained",
        ));
    }
    let sha256 = hash_open_file(&file)?;
    let executable = RetainedExecutable {
        path,
        file,
        metadata,
        sha256,
    };
    executable.validate()?;
    Ok(executable)
}

fn hash_open_file(file: &File) -> Result<String> {
    let mut file = file.try_clone()?;
    file.seek(SeekFrom::Start(0))?;
    let mut reader = BufReader::new(file);
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(lower_hex(digest.finalize()))
}

pub(crate) struct TemporaryWorkspace {
    #[cfg(unix)]
    _guard: Vec<std::os::fd::OwnedFd>,
    #[cfg(windows)]
    _guard: Vec<fs::File>,
    temporary: tempfile::TempDir,
}

#[derive(Debug)]
pub(crate) struct RetainedRepository {
    path: PathBuf,
    _lexical_guard: Option<DirectoryGuard>,
    _resolved_guard: Option<DirectoryGuard>,
}

impl RetainedRepository {
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

#[cfg(windows)]
pub(crate) type DirectoryGuard = Vec<fs::File>;
#[cfg(unix)]
pub(crate) type DirectoryGuard = Vec<std::os::fd::OwnedFd>;
#[cfg(not(any(unix, windows)))]
pub(crate) type DirectoryGuard = ();

pub(crate) fn require_strict_windows_local_volume(
    path: &Path,
    label: &str,
    strict: bool,
) -> Result<()> {
    #[cfg(windows)]
    if strict {
        super::windows_security::require_local_fixed_volume(path, label)?;
    }
    #[cfg(not(windows))]
    let _ = (path, label, strict);
    Ok(())
}

#[cfg(windows)]
pub(crate) fn retain_directory_ancestry(path: &Path, label: &str) -> Result<DirectoryGuard> {
    require_strict_windows_local_volume(path, label, true)?;
    super::windows_security::guard_directory_ancestry(path)
}

#[cfg(unix)]
pub(crate) fn retain_directory_ancestry(path: &Path, label: &str) -> Result<DirectoryGuard> {
    Ok(super::unix_security::prepare_directory(
        path, label, false, false,
    )?)
}

#[cfg(not(any(unix, windows)))]
pub(crate) fn retain_directory_ancestry(_path: &Path, label: &str) -> Result<DirectoryGuard> {
    Err(invalid_data(format!(
        "{label} ancestry retention is unavailable on this platform"
    )))
}

#[cfg(windows)]
fn retain_canonical_directory_ancestry(path: &Path, label: &str) -> Result<DirectoryGuard> {
    super::windows_security::require_canonical_local_fixed_volume(path, label)?;
    super::windows_security::guard_canonical_directory_ancestry(path)
}

pub(crate) fn retain_fixture_directory_ancestry(
    path: &Path,
    label: &str,
) -> Result<DirectoryGuard> {
    #[cfg(windows)]
    {
        require_strict_windows_local_volume(path, label, true)?;
        super::windows_security::guard_fixture_directory_ancestry(path)
    }
    #[cfg(not(windows))]
    {
        retain_directory_ancestry(path, label)
    }
}

#[cfg(not(windows))]
fn retain_canonical_directory_ancestry(path: &Path, label: &str) -> Result<DirectoryGuard> {
    retain_directory_ancestry(path, label)
}

impl TemporaryWorkspace {
    pub(crate) fn path(&self) -> &Path {
        self.temporary.path()
    }
}

pub(crate) fn temporary_workspace(
    root: &Path,
    prefix: &str,
    strict: bool,
) -> Result<TemporaryWorkspace> {
    require_strict_windows_local_volume(root, "strict benchmark repository root", strict)?;
    let configured = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("target"));
    let target = if configured.is_absolute() {
        configured
    } else {
        root.join(configured)
    };
    require_strict_windows_local_volume(&target, "strict benchmark workspace root", strict)?;
    #[cfg(all(not(unix), not(windows)))]
    fs::create_dir_all(&target)?;
    #[cfg(unix)]
    let mut guard =
        super::unix_security::prepare_directory(&target, "benchmark workspace target", true, true)?;
    #[cfg(windows)]
    {
        let parent = target.join(".fs2-secure-workspaces");
        let mut guard =
            super::windows_security::create_or_open_trusted_directory_ancestry(&target)?;
        guard.push(super::windows_security::create_or_open_private_directory(
            &parent,
        )?);
        let temporary = tempfile::Builder::new().prefix(prefix).tempdir_in(parent)?;
        guard.push(super::windows_security::harden_new_private_directory(
            temporary.path(),
        )?);
        Ok(TemporaryWorkspace {
            _guard: guard,
            temporary,
        })
    }
    #[cfg(unix)]
    {
        let temporary = tempfile::Builder::new().prefix(prefix).tempdir_in(target)?;
        guard.extend(super::unix_security::prepare_directory(
            temporary.path(),
            "benchmark temporary workspace",
            false,
            false,
        )?);
        Ok(TemporaryWorkspace {
            _guard: guard,
            temporary,
        })
    }

    #[cfg(all(not(windows), not(unix)))]
    {
        let temporary = tempfile::Builder::new().prefix(prefix).tempdir_in(target)?;
        Ok(TemporaryWorkspace { temporary })
    }
}

pub(crate) fn retain_selected_repository(
    path: &Path,
    label: &str,
    strict: bool,
) -> Result<RetainedRepository> {
    require_strict_windows_local_volume(path, label, strict)?;
    let lexical_guard = strict
        .then(|| retain_directory_ancestry(path, label))
        .transpose()?;
    let path = path.canonicalize()?;
    let resolved_guard = strict
        .then(|| retain_canonical_directory_ancestry(&path, label))
        .transpose()?;
    Ok(RetainedRepository {
        path,
        _lexical_guard: lexical_guard,
        _resolved_guard: resolved_guard,
    })
}

pub(crate) fn repository_state(
    path: &Path,
    label: &str,
    strict: bool,
) -> Result<(RetainedRepository, String)> {
    let repository = retain_selected_repository(path, label, strict)?;
    let path = repository.path();
    if !path.join("Cargo.toml").is_file() {
        return Err(invalid_data(format!(
            "{label} is not a crate checkout: {}",
            path.display()
        )));
    }
    let commit = head_commit(path, "resolve checkout commit")?;
    let ignored = git_output(
        path,
        [
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--ignored=matching",
        ],
        "inspect ignored checkout material",
    )?;
    let observed_commit = head_commit(path, "recheck checkout commit")?;
    if observed_commit != commit {
        return Err(invalid_data(format!(
            "{label} HEAD changed while checkout status was inspected: {commit} -> {observed_commit}"
        )));
    }
    if ignored
        .stdout
        .split(|byte| *byte == 0)
        .any(repository_state_record_is_dirty)
    {
        return Err(invalid_data(format!(
            "{label} checkout contains ignored material that copy_tree would stage: {}",
            path.display()
        )));
    }
    Ok((repository, commit))
}

pub(crate) fn resolve_ref(repo: &Path, revision: &str) -> Result<String> {
    git_text(
        repo,
        ["rev-parse", "--verify", &format!("{revision}^{{commit}}")],
        "resolve Git revision",
    )
    .map(|value| value.trim().to_owned())
}

pub(crate) fn clone_revision(
    repo: &Path,
    destination: &Path,
    revision: &str,
    log_root: &Path,
    label: &str,
) -> Result<Vec<ProcessRecord>> {
    let mut clone = Command::new("git");
    configure_git_output(&mut clone);
    clone
        .args([
            "clone",
            "--no-local",
            "--no-hardlinks",
            "--no-checkout",
            "--quiet",
        ])
        .arg(git_local_path(repo)?)
        .arg(destination);
    let clone_record = process::run_logged_attempt(
        &mut clone,
        format!("clone {label} benchmark source"),
        &log_root.join(format!("{label}.clone.stdout.log")),
        &log_root.join(format!("{label}.clone.stderr.log")),
    );
    let mut checkout = Command::new("git");
    configure_git_output(&mut checkout);
    checkout
        .current_dir(destination)
        .args(["checkout", "--detach", "--quiet", revision]);
    let checkout_record = if clone_record.succeeded() {
        process::run_logged_attempt(
            &mut checkout,
            format!("checkout {label} benchmark revision"),
            &log_root.join(format!("{label}.checkout.stdout.log")),
            &log_root.join(format!("{label}.checkout.stderr.log")),
        )
    } else {
        ProcessRecord::skipped(
            &checkout,
            format!("checkout {label} benchmark revision"),
            log_root.join(format!("{label}.checkout.stdout.log")),
            log_root.join(format!("{label}.checkout.stderr.log")),
            "clone failed",
        )
    };
    if clone_record.succeeded() && checkout_record.succeeded() {
        let alternates = destination.join(".git/objects/info/alternates");
        match fs::symlink_metadata(&alternates) {
            Ok(_) => {
                return Err(invalid_data(
                    "benchmark clone retained a dependency on the source object store",
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let observed = head_commit(destination, "verify materialized benchmark revision")?;
        if observed != revision {
            return Err(invalid_data(format!(
                "materialized benchmark revision differs from requested commit: {revision} -> {observed}"
            )));
        }
    }
    Ok(vec![clone_record, checkout_record])
}

#[cfg(windows)]
fn enable_git_long_paths(command: &mut Command) {
    command.args(["-c", "core.longpaths=true"]);
}

#[cfg(not(windows))]
fn enable_git_long_paths(_command: &mut Command) {}

fn git_local_path(path: &Path) -> Result<std::ffi::OsString> {
    let path = path.canonicalize()?;
    #[cfg(unix)]
    let path = path.into_os_string();
    #[cfg(windows)]
    let path = {
        use std::os::windows::ffi::{OsStrExt as _, OsStringExt as _};

        const VERBATIM: &[u16] = &[b'\\' as u16, b'\\' as u16, b'?' as u16, b'\\' as u16];
        const UNC: &[u16] = &[b'U' as u16, b'N' as u16, b'C' as u16, b'\\' as u16];
        let encoded = path.as_os_str().encode_wide().collect::<Vec<_>>();
        if let Some(path) = encoded
            .strip_prefix(VERBATIM)
            .and_then(|path| path.strip_prefix(UNC))
        {
            let mut native = vec![b'\\' as u16, b'\\' as u16];
            native.extend_from_slice(path);
            std::ffi::OsString::from_wide(&native)
        } else {
            std::ffi::OsString::from_wide(encoded.strip_prefix(VERBATIM).unwrap_or(&encoded))
        }
    };
    Ok(path)
}

#[derive(Clone, Copy)]
enum TreeDigestMetadata {
    Exact,
    Publication,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RetainedTreeEntryKind {
    Directory,
    File,
    Other,
}

struct RetainedTreeEntry {
    path: PathBuf,
    relative: PathBuf,
    metadata: fs::Metadata,
    kind: RetainedTreeEntryKind,
    file: Option<File>,
}

struct RetainedLiveTree {
    entries: Vec<RetainedTreeEntry>,
    _lexical_root_guard: DirectoryGuard,
    _resolved_root_guard: DirectoryGuard,
    _nested_directory_guards: Vec<DirectoryGuard>,
}

impl RetainedLiveTree {
    fn validate(&self, label: &str) -> Result<()> {
        for entry in &self.entries {
            validate_retained_tree_entry(entry, label)?;
        }
        Ok(())
    }
}

pub(crate) fn tree_digest(path: &Path) -> Result<String> {
    tree_digest_with_metadata(path, TreeDigestMetadata::Exact)
}

pub(crate) fn publication_tree_digest(path: &Path) -> Result<String> {
    tree_digest_with_metadata(path, TreeDigestMetadata::Publication)
}

fn tree_digest_with_metadata(path: &Path, metadata_policy: TreeDigestMetadata) -> Result<String> {
    let mut tree = retain_live_tree(path, "tree digest source")?;
    tree.entries
        .sort_by(|left, right| left.path.cmp(&right.path));
    let mut digest = Sha256::new();
    digest.update(b"fs2-tree-digest-v2\0");
    for entry in &tree.entries {
        if entry.relative.as_os_str().is_empty() {
            continue;
        }
        if entry.kind == RetainedTreeEntryKind::Directory {
            digest.update(b"D");
            update_path_digest(&mut digest, &entry.relative)?;
            update_metadata_digest(&mut digest, &entry.metadata, metadata_policy)?;
            continue;
        }
        if entry.kind != RetainedTreeEntryKind::File
            || entry.path.extension() == Some(OsStr::new("pyc"))
        {
            continue;
        }
        digest.update(b"F");
        update_path_digest(&mut digest, &entry.relative)?;
        let (file, metadata) = open_retained_tree_file(entry, "tree digest source")?;
        update_metadata_digest(&mut digest, &metadata, metadata_policy)?;
        digest.update(metadata.len().to_le_bytes());
        let mut reader = BufReader::new(file);
        let mut buffer = [0u8; 64 * 1024];
        loop {
            let read = reader.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            digest.update(&buffer[..read]);
        }
        validate_open_retained_tree_file(entry, reader.get_ref(), "tree digest source")?;
    }
    tree.validate("tree digest source")?;
    Ok(lower_hex(digest.finalize()))
}

#[cfg(windows)]
fn tree_metadata_is_windows_reparse_point(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt as _;
    use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;

    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn tree_metadata_is_windows_reparse_point(_metadata: &fs::Metadata) -> bool {
    false
}

fn update_metadata_digest(
    digest: &mut Sha256,
    metadata: &fs::Metadata,
    metadata_policy: TreeDigestMetadata,
) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        const USER_EXECUTE: u32 = 0o100;
        const FILE_TYPE_MASK: u32 = 0o170000;

        let source_mode = metadata.mode();
        let mode = match metadata_policy {
            TreeDigestMetadata::Exact => source_mode,
            TreeDigestMetadata::Publication => {
                let permissions = if metadata.is_dir() || source_mode & USER_EXECUTE != 0 {
                    0o700
                } else {
                    0o600
                };
                (source_mode & FILE_TYPE_MASK) | permissions
            }
        };
        digest.update(mode.to_le_bytes());
    }
    #[cfg(not(unix))]
    let _ = metadata_policy;
    #[cfg(not(unix))]
    digest.update([u8::from(metadata.permissions().readonly())]);
    Ok(())
}

fn retain_live_tree(path: &Path, label: &str) -> Result<RetainedLiveTree> {
    let lexical_root = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()?.join(path)
    };
    let lexical_root_guard = retain_canonical_directory_ancestry(&lexical_root, label)?;
    let root = lexical_root.canonicalize()?;
    let resolved_root_guard = retain_canonical_directory_ancestry(&root, label)?;
    let mut nested_directory_guards = Vec::new();
    let mut entries = Vec::new();
    let mut walker = WalkDir::new(&root).follow_links(false).into_iter();
    while let Some(entry) = walker.next() {
        let entry = entry?;
        if !included_entry(&root, &entry) {
            if entry.file_type().is_dir() {
                walker.skip_current_dir();
            }
            continue;
        }
        let relative = entry.path().strip_prefix(&root)?.to_owned();
        let mut metadata = fs::symlink_metadata(entry.path())?;
        ensure_safe_tree_entry(entry.path(), &metadata, label)?;
        let kind = retained_tree_entry_kind(&metadata);
        if kind != retained_dir_entry_kind(entry.file_type()) {
            return Err(invalid_data(format!(
                "{label} entry type changed during traversal: {}",
                entry.path().display()
            )));
        }
        if kind == RetainedTreeEntryKind::Directory && !relative.as_os_str().is_empty() {
            let guard = retain_nested_live_tree_directory(entry.path(), label)?;
            let guarded_metadata = fs::symlink_metadata(entry.path())?;
            ensure_safe_tree_entry(entry.path(), &guarded_metadata, label)?;
            if !tree_metadata_matches(&metadata, &guarded_metadata) {
                return Err(invalid_data(format!(
                    "{label} directory changed while its namespace was retained: {}",
                    entry.path().display()
                )));
            }
            metadata = guarded_metadata;
            nested_directory_guards.push(guard);
        }
        let file = if kind == RetainedTreeEntryKind::File {
            let file = open_live_tree_file(entry.path())?;
            let opened_metadata = file.metadata()?;
            ensure_safe_tree_entry(entry.path(), &opened_metadata, label)?;
            if !tree_metadata_matches(&metadata, &opened_metadata) {
                return Err(invalid_data(format!(
                    "{label} file changed while its handle was retained: {}",
                    entry.path().display()
                )));
            }
            metadata = opened_metadata;
            Some(file)
        } else {
            None
        };
        entries.push(RetainedTreeEntry {
            path: entry.path().to_owned(),
            relative,
            metadata,
            kind,
            file,
        });
    }
    let tree = RetainedLiveTree {
        entries,
        _lexical_root_guard: lexical_root_guard,
        _resolved_root_guard: resolved_root_guard,
        _nested_directory_guards: nested_directory_guards,
    };
    tree.validate(label)?;
    Ok(tree)
}

#[cfg(any(unix, windows))]
fn retain_nested_live_tree_directory(path: &Path, label: &str) -> Result<DirectoryGuard> {
    let mut ancestry = retain_canonical_directory_ancestry(path, label)?;
    let directory = ancestry.pop().ok_or_else(|| {
        invalid_data(format!(
            "{label} directory ancestry is empty: {}",
            path.display()
        ))
    })?;
    Ok(vec![directory])
}

#[cfg(not(any(unix, windows)))]
fn retain_nested_live_tree_directory(path: &Path, label: &str) -> Result<DirectoryGuard> {
    retain_canonical_directory_ancestry(path, label)
}

fn retained_tree_entry_kind(metadata: &fs::Metadata) -> RetainedTreeEntryKind {
    if metadata.is_dir() {
        RetainedTreeEntryKind::Directory
    } else if metadata.is_file() {
        RetainedTreeEntryKind::File
    } else {
        RetainedTreeEntryKind::Other
    }
}

fn retained_dir_entry_kind(file_type: fs::FileType) -> RetainedTreeEntryKind {
    if file_type.is_dir() {
        RetainedTreeEntryKind::Directory
    } else if file_type.is_file() {
        RetainedTreeEntryKind::File
    } else {
        RetainedTreeEntryKind::Other
    }
}

fn ensure_safe_tree_entry(path: &Path, metadata: &fs::Metadata, label: &str) -> Result<()> {
    if metadata.file_type().is_symlink() || tree_metadata_is_windows_reparse_point(metadata) {
        return Err(invalid_data(format!(
            "{label} rejects links and reparse points: {}",
            path.display()
        )));
    }
    Ok(())
}

fn validate_retained_tree_entry(entry: &RetainedTreeEntry, label: &str) -> Result<()> {
    let metadata = fs::symlink_metadata(&entry.path)?;
    ensure_safe_tree_entry(&entry.path, &metadata, label)?;
    if entry.kind != retained_tree_entry_kind(&metadata)
        || !tree_metadata_matches(&entry.metadata, &metadata)
    {
        return Err(invalid_data(format!(
            "{label} entry changed during the operation: {}",
            entry.path.display()
        )));
    }
    Ok(())
}

#[cfg(unix)]
fn open_live_tree_file(path: &Path) -> Result<File> {
    use std::os::unix::fs::OpenOptionsExt as _;

    Ok(OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)?)
}

#[cfg(windows)]
fn open_live_tree_file(path: &Path) -> Result<File> {
    use std::os::windows::fs::OpenOptionsExt as _;
    use windows_sys::Win32::Storage::FileSystem::{FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ};

    Ok(OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?)
}

#[cfg(not(any(unix, windows)))]
fn open_live_tree_file(path: &Path) -> Result<File> {
    Ok(File::open(path)?)
}

fn open_retained_tree_file(entry: &RetainedTreeEntry, label: &str) -> Result<(File, fs::Metadata)> {
    validate_retained_tree_entry(entry, label)?;
    let file = entry
        .file
        .as_ref()
        .ok_or_else(|| invalid_data(format!("{label} expected a retained file handle")))?
        .try_clone()?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || !tree_metadata_matches(&entry.metadata, &metadata) {
        return Err(invalid_data(format!(
            "{label} file changed while it was opened: {}",
            entry.path.display()
        )));
    }
    validate_retained_tree_entry(entry, label)?;
    Ok((file, metadata))
}

fn validate_open_retained_tree_file(
    entry: &RetainedTreeEntry,
    file: &File,
    label: &str,
) -> Result<()> {
    let metadata = file.metadata()?;
    if !metadata.is_file() || !tree_metadata_matches(&entry.metadata, &metadata) {
        return Err(invalid_data(format!(
            "{label} file changed while it was read: {}",
            entry.path.display()
        )));
    }
    validate_retained_tree_entry(entry, label)
}

#[cfg(unix)]
fn tree_metadata_matches(expected: &fs::Metadata, observed: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt as _;

    retained_tree_entry_kind(expected) == retained_tree_entry_kind(observed)
        && expected.dev() == observed.dev()
        && expected.ino() == observed.ino()
        && expected.mode() == observed.mode()
        && expected.len() == observed.len()
        && expected.mtime() == observed.mtime()
        && expected.mtime_nsec() == observed.mtime_nsec()
        && expected.ctime() == observed.ctime()
        && expected.ctime_nsec() == observed.ctime_nsec()
}

#[cfg(windows)]
fn tree_metadata_matches(expected: &fs::Metadata, observed: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt as _;

    retained_tree_entry_kind(expected) == retained_tree_entry_kind(observed)
        && expected.file_attributes() == observed.file_attributes()
        && expected.creation_time() == observed.creation_time()
        && expected.last_write_time() == observed.last_write_time()
        && expected.file_size() == observed.file_size()
}

#[cfg(not(any(unix, windows)))]
fn tree_metadata_matches(_expected: &fs::Metadata, _observed: &fs::Metadata) -> bool {
    false
}

fn update_path_digest(digest: &mut Sha256, path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        digest.update(b"unix\0");
        let bytes = path.as_os_str().as_bytes();
        digest.update(
            u64::try_from(bytes.len())
                .map_err(|_| invalid_data("tree path is too long to hash"))?
                .to_le_bytes(),
        );
        digest.update(bytes);
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        digest.update(b"windows-utf16le\0");
        let units = path.as_os_str().encode_wide().collect::<Vec<_>>();
        let byte_len = units
            .len()
            .checked_mul(std::mem::size_of::<u16>())
            .and_then(|length| u64::try_from(length).ok())
            .ok_or_else(|| invalid_data("tree path is too long to hash"))?;
        digest.update(byte_len.to_le_bytes());
        for unit in units {
            digest.update(unit.to_le_bytes());
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        digest.update(b"portable\0");
        let path = path.to_string_lossy();
        digest.update(
            u64::try_from(path.len())
                .map_err(|_| invalid_data("tree path is too long to hash"))?
                .to_le_bytes(),
        );
        digest.update(path.as_bytes());
    }
    Ok(())
}

pub(crate) fn copy_tree(source: &Path, destination: &Path) -> Result<()> {
    if destination.exists() {
        return Err(invalid_data(format!(
            "copy destination already exists: {}",
            destination.display()
        )));
    }
    let tree = retain_live_tree(source, "benchmark staging source")?;
    for entry in &tree.entries {
        let target = destination.join(&entry.relative);
        if entry.kind == RetainedTreeEntryKind::Directory {
            fs::create_dir_all(&target)?;
        } else if entry.kind == RetainedTreeEntryKind::File
            && entry.path.extension() != Some(OsStr::new("pyc"))
        {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            let (mut source_file, source_metadata) =
                open_retained_tree_file(entry, "benchmark staging source")?;
            let mut target_file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&target)?;
            std::io::copy(&mut source_file, &mut target_file)?;
            fs::set_permissions(&target, source_metadata.permissions())?;
            validate_open_retained_tree_file(entry, &source_file, "benchmark staging source")?;
        }
    }
    tree.validate("benchmark staging source")
}

fn included_entry(root: &Path, entry: &DirEntry) -> bool {
    let Ok(relative) = entry.path().strip_prefix(root) else {
        return false;
    };
    let Some(first) = relative.components().next() else {
        return true;
    };
    !matches!(
        first.as_os_str().to_str(),
        Some(".git" | "target" | "__pycache__")
    )
}

pub(crate) fn prepare_harness(
    root: &Path,
    name: &str,
    repository: &Path,
    package_name: &str,
    benchmark_inputs: &Path,
    lockfile: &Path,
) -> Result<PathBuf> {
    let package = root.join(name);
    copy_tree(benchmark_inputs, &package)?;
    fs::copy(lockfile, package.join("Cargo.lock"))?;
    let manifest = package.join("Cargo.toml");
    let text = fs::read_to_string(&manifest)?;
    let repository = repository
        .to_str()
        .ok_or_else(|| invalid_data("benchmark repository path is not valid Unicode"))?
        .replace('\\', "/");
    let replacement = match package_name {
        "fs2" => format!("fs2 = {{ path = {repository:?} }}"),
        "fs4" => format!(
            "fs2 = {{ package = \"fs4\", path = {repository:?}, default-features = false, features = [\"sync\"] }}"
        ),
        _ => {
            return Err(invalid_data(format!(
                "unsupported subject package: {package_name}"
            )));
        }
    };
    let rewritten = rewrite_subject_dependency(&text, &replacement)?;
    fs::write(&manifest, format!("{rewritten}\n[workspace]\n"))?;
    Ok(manifest)
}

pub(crate) fn subject_package_name(repository: &Path) -> Result<String> {
    let manifest = repository.join("Cargo.toml");
    let metadata = fs::symlink_metadata(&manifest)?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return Err(invalid_data(format!(
            "subject Cargo.toml must be a regular file: {}",
            manifest.display()
        )));
    }

    let contents = fs::read_to_string(&manifest)?;
    let mut section = "";
    let mut package_name = None;
    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            section = trimmed;
            continue;
        }
        if section != "[package]" {
            continue;
        }
        let Some((key, value)) = trimmed.split_once('=') else {
            continue;
        };
        if key.trim() != "name" {
            continue;
        }
        if package_name.is_some() {
            return Err(invalid_data(format!(
                "subject Cargo.toml declares package.name more than once: {}",
                manifest.display()
            )));
        }
        package_name = Some(match value.trim() {
            "\"fs2\"" => "fs2".to_owned(),
            "\"fs2-turbo\"" => "fs2-turbo".to_owned(),
            unsupported => {
                return Err(invalid_data(format!(
                    "unsupported subject package name {unsupported}: {}",
                    manifest.display()
                )));
            }
        });
    }
    package_name.ok_or_else(|| {
        invalid_data(format!(
            "subject Cargo.toml does not declare package.name: {}",
            manifest.display()
        ))
    })
}

fn rewrite_subject_dependency(manifest: &str, replacement: &str) -> Result<String> {
    const SUBJECT_DEPENDENCY: &str = "fs2 = { path = \"..\" }";
    if manifest
        .lines()
        .filter(|line| line.trim() == SUBJECT_DEPENDENCY)
        .count()
        != 1
    {
        return Err(invalid_data(
            "benchmark manifest must contain exactly one canonical fs2 dependency",
        ));
    }
    if manifest.lines().any(|line| line.trim() == "[workspace]") {
        return Err(invalid_data(
            "benchmark manifest already declares a workspace",
        ));
    }

    let mut output = String::with_capacity(manifest.len() + replacement.len());
    for line in manifest.lines() {
        if line.trim() == SUBJECT_DEPENDENCY {
            output.push_str(replacement);
        } else {
            output.push_str(line);
        }
        output.push('\n');
    }
    Ok(output.trim_end().to_owned())
}

pub(crate) fn processes_succeeded(processes: &[ProcessRecord]) -> bool {
    processes.iter().all(ProcessRecord::succeeded)
}

pub(crate) fn subject_features(benchmark: &str, package_name: &str) -> Result<Vec<String>> {
    if benchmark == "fs_compat" {
        Ok(vec![
            "--no-default-features".to_owned(),
            "--features".to_owned(),
            format!("subject-{package_name}"),
        ])
    } else if package_name == "fs2" {
        Ok(Vec::new())
    } else {
        Err(invalid_data(format!(
            "benchmark {benchmark} does not support package {package_name}"
        )))
    }
}

pub(crate) struct CargoWorkingDirectory {
    path: PathBuf,
    #[cfg(windows)]
    _mapping: super::windows_security::IsolatedCargoDirectory,
}

impl CargoWorkingDirectory {
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

pub(crate) fn isolated_cargo_working_directory(
    _requested: &Path,
    private_root: &Path,
) -> Result<CargoWorkingDirectory> {
    #[cfg(windows)]
    {
        let mapping = super::windows_security::isolated_cargo_working_directory(private_root)?;
        Ok(CargoWorkingDirectory {
            path: mapping.path().to_owned(),
            _mapping: mapping,
        })
    }

    #[cfg(not(windows))]
    {
        let _ = private_root;
        Ok(CargoWorkingDirectory {
            path: _requested.to_owned(),
        })
    }
}

pub(crate) fn generate_lockfile(
    root: &Path,
    manifest: &Path,
    target: &Path,
    log_root: &Path,
    label: &str,
) -> Result<ProcessRecord> {
    let private_root = target
        .parent()
        .ok_or_else(|| invalid_data("benchmark target directory has no parent"))?;
    let cargo_directory = isolated_cargo_working_directory(root, private_root)?;
    let mut command = process::cargo();
    command
        .current_dir(cargo_directory.path())
        .env("CARGO_TARGET_DIR", target)
        .args(["generate-lockfile", "--manifest-path"])
        .arg(manifest)
        .arg("--offline");
    Ok(process::run_logged_attempt(
        &mut command,
        format!("generate {label} lockfile"),
        &log_root.join(format!("{label}.lock.stdout.log")),
        &log_root.join(format!("{label}.lock.stderr.log")),
    ))
}

pub(crate) fn prebuild(
    root: &Path,
    manifest: &Path,
    target: &Path,
    benchmark: &str,
    features: &[String],
    log_root: &Path,
    label: &str,
) -> Result<ProcessRecord> {
    let private_root = target
        .parent()
        .ok_or_else(|| invalid_data("benchmark target directory has no parent"))?;
    let cargo_directory = isolated_cargo_working_directory(root, private_root)?;
    let mut command = process::cargo();
    command
        .current_dir(cargo_directory.path())
        .args(["bench", "--manifest-path"])
        .arg(manifest)
        .args([
            "--bench",
            benchmark,
            "--no-run",
            "--locked",
            "--offline",
            "--message-format=json-render-diagnostics",
            "--target-dir",
        ])
        .arg(target)
        .args(features);
    Ok(process::run_logged_attempt(
        &mut command,
        format!("prebuild {label} {benchmark}"),
        &log_root.join(format!("{label}-{benchmark}.stdout.log")),
        &log_root.join(format!("{label}-{benchmark}.stderr.log")),
    ))
}

pub(crate) fn validate_path_dependencies(
    working_directory: &Path,
    private_root: &Path,
    manifest: &Path,
    features: &[String],
    source_roots: &[&Path],
) -> Result<()> {
    let harness_root = manifest
        .parent()
        .ok_or_else(|| invalid_data("benchmark manifest has no parent"))?
        .canonicalize()?;
    let mut digested_roots = Vec::with_capacity(source_roots.len() + 1);
    digested_roots.push(harness_root);
    for root in source_roots {
        digested_roots.push(root.canonicalize()?);
    }
    digested_roots.sort();
    digested_roots.dedup();

    let cargo_directory = isolated_cargo_working_directory(working_directory, private_root)?;
    let mut command = process::cargo();
    command
        .current_dir(cargo_directory.path())
        .args([
            "metadata",
            "--format-version",
            "1",
            "--locked",
            "--offline",
            "--manifest-path",
        ])
        .arg(manifest)
        .args(features);
    let output = process::capture(&mut command, "resolve strict benchmark dependency closure")?;
    let metadata: CargoMetadata = serde_json::from_slice(&output.stdout)?;
    validate_metadata_path_dependencies(&metadata, &digested_roots)
}

fn validate_metadata_path_dependencies(
    metadata: &CargoMetadata,
    digested_roots: &[PathBuf],
) -> Result<()> {
    if metadata.packages.is_empty() {
        return Err(invalid_data(
            "Cargo metadata returned an empty package graph",
        ));
    }
    for package in metadata
        .packages
        .iter()
        .filter(|package| package.source.is_none())
    {
        let manifest = package.manifest_path.canonicalize()?;
        let package_root = manifest
            .parent()
            .ok_or_else(|| invalid_data("Cargo package manifest has no parent"))?;
        if !digested_roots
            .iter()
            .any(|root| tree_digest_covers(root, package_root))
        {
            return Err(invalid_data(format!(
                "strict benchmark dependency {} resolves outside the recorded source trees: {}",
                package.name,
                manifest.display()
            )));
        }
        for target in &package.targets {
            let source = target.src_path.canonicalize()?;
            if !digested_roots
                .iter()
                .any(|root| tree_digest_covers(root, &source))
            {
                return Err(invalid_data(format!(
                    "strict benchmark target for {} resolves outside the recorded source trees: {}",
                    package.name,
                    source.display()
                )));
            }
        }
    }
    Ok(())
}

fn tree_digest_covers(root: &Path, path: &Path) -> bool {
    let Ok(relative) = path.strip_prefix(root) else {
        return false;
    };
    relative.components().next().is_none_or(|component| {
        !matches!(
            component.as_os_str().to_str(),
            Some(".git" | "target" | "__pycache__")
        )
    })
}

pub(crate) fn ensure_disk_headroom(path: &Path, minimum_free_bytes: u64) -> Result<()> {
    let stats = fs2::statvfs(path)?;
    if stats.available_space() < minimum_free_bytes {
        Err(invalid_data(format!(
            "benchmark workspace has {} available bytes; at least {minimum_free_bytes} are required",
            stats.available_space()
        )))
    } else {
        Ok(())
    }
}

pub(crate) fn ensure_output_headroom(output: &Path, minimum_free_bytes: u64) -> Result<()> {
    let mut probe = output;
    while !probe.exists() {
        probe = probe
            .parent()
            .ok_or_else(|| invalid_data("benchmark output has no existing ancestor"))?;
    }
    ensure_disk_headroom(probe, minimum_free_bytes)
}

fn git_text<const N: usize>(repo: &Path, arguments: [&str; N], label: &str) -> Result<String> {
    let output = git_output(repo, arguments, label)?;
    Ok(String::from_utf8(output.stdout)?)
}

fn head_commit(repo: &Path, label: &str) -> Result<String> {
    git_text(repo, ["rev-parse", "--verify", "HEAD^{commit}"], label)
        .map(|value| value.trim().to_owned())
}

fn configure_git_output(command: &mut Command) {
    enable_git_long_paths(command);
    command.args([
        "-c",
        "core.fsmonitor=false",
        "-c",
        "core.untrackedCache=false",
    ]);
    command.env("GIT_OPTIONAL_LOCKS", "0");
}

pub(crate) fn git_output<const N: usize>(
    repo: &Path,
    arguments: [&str; N],
    label: &str,
) -> Result<std::process::Output> {
    let mut command = Command::new("git");
    configure_git_output(&mut command);
    command
        .current_dir(repo)
        .arg("-C")
        .arg(repo)
        .args(arguments);
    let output = process::capture(&mut command, label)?;
    Ok(output)
}

pub(crate) fn repository_state_record_is_dirty(record: &[u8]) -> bool {
    if record.is_empty() {
        return false;
    }
    if let Some(path) = record.strip_prefix(b"!! ") {
        return copy_tree_would_stage_ignored_entry(path);
    }
    true
}

fn copy_tree_would_stage_ignored_entry(path: &[u8]) -> bool {
    let Some(first_component) = path.split(|byte| *byte == b'/').next() else {
        return false;
    };
    if first_component == b".git"
        || first_component == b"target"
        || first_component == b"__pycache__"
    {
        return false;
    }
    let Some(file_name) = path.rsplit(|byte| *byte == b'/').next() else {
        return false;
    };
    let has_pyc_extension = file_name
        .iter()
        .rposition(|byte| *byte == b'.')
        .is_some_and(|dot| dot != 0 && &file_name[dot + 1..] == b"pyc");
    !has_pyc_extension
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn git(repo: &Path, args: &[&str]) {
        let output = Command::new("git")
            .current_dir(repo)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn subject_features_reject_incompatible_workloads() {
        assert!(subject_features("fs2_legacy", "fs4").is_err());
        assert_eq!(
            subject_features("fs_compat", "fs4").unwrap(),
            ["--no-default-features", "--features", "subject-fs4"]
        );
    }

    #[test]
    fn cargo_executable_uses_the_reported_target_path() {
        let directory = tempfile::tempdir().unwrap();
        let executable = directory.path().join("custom-target/release/probe.exe");
        fs::create_dir_all(executable.parent().unwrap()).unwrap();
        fs::write(&executable, []).unwrap();
        let log = directory.path().join("cargo.json");
        fs::write(
            &log,
            format!(
                "{{\"reason\":\"compiler-artifact\",\"target\":{{\"name\":\"probe\"}},\"executable\":{}}}\n",
                serde_json::to_string(&executable).unwrap()
            ),
        )
        .unwrap();

        assert_eq!(cargo_executable(&log, "probe").unwrap(), executable);
    }

    #[test]
    fn retained_cargo_executable_is_confined_and_content_bound() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("target");
        let executable = target.join("release/probe.exe");
        fs::create_dir_all(executable.parent().unwrap()).unwrap();
        fs::write(&executable, b"probe").unwrap();
        let log = directory.path().join("cargo.json");
        fs::write(
            &log,
            format!(
                "{{\"reason\":\"compiler-artifact\",\"target\":{{\"name\":\"probe\"}},\"executable\":{}}}\n",
                serde_json::to_string(&executable).unwrap()
            ),
        )
        .unwrap();

        let retained = retained_cargo_executable(&log, "probe", &target).unwrap();
        assert_eq!(retained.path(), executable.canonicalize().unwrap());
        assert_eq!(retained.sha256(), hash_file(&executable).unwrap());
        retained.validate().unwrap();
    }

    #[test]
    fn tree_digest_includes_directory_topology() {
        let left = test_tempdir();
        let right = test_tempdir();
        fs::write(left.path().join("file"), b"same").unwrap();
        fs::write(right.path().join("file"), b"same").unwrap();
        fs::create_dir(left.path().join("empty")).unwrap();

        assert_ne!(
            tree_digest(left.path()).unwrap(),
            tree_digest(right.path()).unwrap()
        );
    }

    #[cfg(unix)]
    #[test]
    fn publication_tree_digest_predicts_hardened_permissions() {
        use std::os::unix::fs::PermissionsExt as _;

        let tree = tempfile::tempdir().unwrap();
        let directory = tree.path().join("directory");
        fs::create_dir(&directory).unwrap();
        let executable = directory.join("executable");
        let data = directory.join("data");
        fs::write(&executable, b"executable").unwrap();
        fs::write(&data, b"data").unwrap();
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o755)).unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
        fs::set_permissions(&data, fs::Permissions::from_mode(0o644)).unwrap();

        let exact_before = tree_digest(tree.path()).unwrap();
        let predicted = publication_tree_digest(tree.path()).unwrap();
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        fs::set_permissions(&data, fs::Permissions::from_mode(0o600)).unwrap();
        let exact_after = tree_digest(tree.path()).unwrap();

        assert_ne!(exact_before, exact_after);
        assert_eq!(predicted, exact_after);
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn tree_operations_reject_nested_links_and_reparse_points() {
        let tree = tempfile::tempdir().unwrap();
        let external = tempfile::tempdir().unwrap();
        let nested = tree.path().join("nested");
        fs::create_dir(&nested).unwrap();
        let link = nested.join("link");
        #[cfg(unix)]
        std::os::unix::fs::symlink(external.path(), &link).unwrap();
        #[cfg(windows)]
        {
            let status = Command::new("cmd")
                .args(["/c", "mklink", "/J"])
                .arg(&link)
                .arg(external.path())
                .status()
                .unwrap();
            assert!(status.success());
        }

        assert!(tree_digest(tree.path()).is_err());
        let destination = tree.path().join("copy");
        assert!(copy_tree(tree.path(), &destination).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn tree_operations_reject_lower_trust_writable_nested_directories() {
        use std::os::unix::fs::PermissionsExt as _;

        let workspace = test_tempdir();
        let source = workspace.path().join("source");
        let nested = source.join("nested");
        fs::create_dir_all(&nested).unwrap();
        fs::write(nested.join("file"), "contents\n").unwrap();
        fs::set_permissions(&nested, fs::Permissions::from_mode(0o777)).unwrap();

        assert!(tree_digest(&source).is_err());
        assert!(copy_tree(&source, &workspace.path().join("copy")).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn retained_tree_rejects_a_raced_nested_namespace() {
        let workspace = test_tempdir();
        let source = workspace.path().join("source");
        let nested = source.join("nested");
        let external = workspace.path().join("external");
        fs::create_dir_all(&nested).unwrap();
        fs::create_dir(&external).unwrap();
        fs::write(nested.join("file"), "original\n").unwrap();
        fs::write(external.join("file"), "replacement\n").unwrap();
        let retained = retain_live_tree(&source, "test live tree").unwrap();
        let file = retained
            .entries
            .iter()
            .find(|entry| entry.relative == Path::new("nested/file"))
            .unwrap();

        fs::remove_dir_all(&nested).unwrap();
        std::os::unix::fs::symlink(&external, &nested).unwrap();

        assert!(open_retained_tree_file(file, "test live tree").is_err());
        assert!(retained.validate("test live tree").is_err());
    }

    #[test]
    fn subject_dependency_rewrite_requires_the_canonical_manifest_line() {
        assert!(rewrite_subject_dependency("[dependencies]\nfs2=\"1\"", "replacement").is_err());
        assert_eq!(
            rewrite_subject_dependency(
                "[dependencies]\nfs2 = { path = \"..\" }\n",
                "fs2 = { path = \"subject\" }",
            )
            .unwrap(),
            "[dependencies]\nfs2 = { path = \"subject\" }"
        );
    }

    #[test]
    fn subject_package_name_requires_a_supported_root_package() {
        let source = test_tempdir();
        fs::write(
            source.path().join("Cargo.toml"),
            "[package]\nname = \"fs2-turbo\"\nversion = \"1.0.0\"\n",
        )
        .unwrap();
        assert_eq!(subject_package_name(source.path()).unwrap(), "fs2-turbo");

        fs::write(
            source.path().join("Cargo.toml"),
            "[package]\nname = \"other\"\nversion = \"1.0.0\"\n",
        )
        .unwrap();
        assert!(subject_package_name(source.path()).is_err());
    }

    #[test]
    fn strict_dependency_closure_stays_inside_digested_trees() {
        let workspace = tempfile::tempdir().unwrap();
        let trusted = workspace.path().join("trusted");
        let external = workspace.path().join("external");
        let excluded = trusted.join("target/generated");
        for package in [&trusted, &external, &excluded] {
            fs::create_dir_all(package).unwrap();
            fs::write(
                package.join("Cargo.toml"),
                "[package]\nname='fixture'\nversion='0.0.0'\n",
            )
            .unwrap();
        }
        let roots = [trusted.canonicalize().unwrap()];
        let package = |name: &str, root: &Path| CargoMetadataPackage {
            name: name.to_owned(),
            manifest_path: root.join("Cargo.toml"),
            source: None,
            targets: Vec::new(),
        };

        let inside = CargoMetadata {
            packages: vec![package("inside", &trusted)],
        };
        assert!(validate_metadata_path_dependencies(&inside, &roots).is_ok());

        let outside = CargoMetadata {
            packages: vec![package("outside", &external)],
        };
        assert!(validate_metadata_path_dependencies(&outside, &roots).is_err());

        let ignored = CargoMetadata {
            packages: vec![package("ignored", &excluded)],
        };
        assert!(validate_metadata_path_dependencies(&ignored, &roots).is_err());

        let external_source = external.join("entry.rs");
        fs::write(&external_source, "pub fn external() {}\n").unwrap();
        let external_target = CargoMetadata {
            packages: vec![CargoMetadataPackage {
                name: "external-target".to_owned(),
                manifest_path: trusted.join("Cargo.toml"),
                source: None,
                targets: vec![CargoMetadataTarget {
                    src_path: external_source,
                }],
            }],
        };
        assert!(validate_metadata_path_dependencies(&external_target, &roots).is_err());
    }

    #[test]
    fn repository_state_matches_copy_tree_ignored_boundaries() {
        assert!(!copy_tree_would_stage_ignored_entry(b"target/debug/file"));
        assert!(!copy_tree_would_stage_ignored_entry(
            b"__pycache__/module.py"
        ));
        assert!(!copy_tree_would_stage_ignored_entry(b"module.pyc"));
        assert!(copy_tree_would_stage_ignored_entry(b".pyc"));
        assert!(copy_tree_would_stage_ignored_entry(b"build.rs"));
        assert!(copy_tree_would_stage_ignored_entry(b"nested/target/file"));

        let repo = tempfile::tempdir().unwrap();
        git(repo.path(), &["init", "--quiet"]);
        fs::write(
            repo.path().join("Cargo.toml"),
            "[package]\nname = \"fs2-test\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        fs::write(repo.path().join(".gitignore"), "").unwrap();
        git(repo.path(), &["add", "Cargo.toml", ".gitignore"]);
        git(
            repo.path(),
            &[
                "-c",
                "user.name=Codex",
                "-c",
                "user.email=codex@example.com",
                "commit",
                "--quiet",
                "-m",
                "init",
            ],
        );

        let clean_state = repository_state(repo.path(), "repo", true);
        assert!(clean_state.is_ok(), "{clean_state:?}");
        fs::write(repo.path().join("notes.txt"), "dirty\n").unwrap();
        assert!(repository_state(repo.path(), "repo", false).is_err());

        let ignored_build = tempfile::tempdir().unwrap();
        git(ignored_build.path(), &["init", "--quiet"]);
        fs::write(
            ignored_build.path().join("Cargo.toml"),
            "[package]\nname = \"fs2-test\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        fs::write(ignored_build.path().join(".gitignore"), "build.rs\n").unwrap();
        git(ignored_build.path(), &["add", "Cargo.toml", ".gitignore"]);
        git(
            ignored_build.path(),
            &[
                "-c",
                "user.name=Codex",
                "-c",
                "user.email=codex@example.com",
                "commit",
                "--quiet",
                "-m",
                "init",
            ],
        );
        fs::write(ignored_build.path().join("build.rs"), "fn main() {}\n").unwrap();
        assert!(repository_state(ignored_build.path(), "repo", false).is_err());

        let ignored_target = tempfile::tempdir().unwrap();
        git(ignored_target.path(), &["init", "--quiet"]);
        fs::write(
            ignored_target.path().join("Cargo.toml"),
            "[package]\nname = \"fs2-test\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        fs::write(ignored_target.path().join(".gitignore"), "target/\n").unwrap();
        git(ignored_target.path(), &["add", "Cargo.toml", ".gitignore"]);
        git(
            ignored_target.path(),
            &[
                "-c",
                "user.name=Codex",
                "-c",
                "user.email=codex@example.com",
                "commit",
                "--quiet",
                "-m",
                "init",
            ],
        );
        fs::create_dir_all(ignored_target.path().join("target/debug")).unwrap();
        fs::write(
            ignored_target.path().join("target/debug/output.txt"),
            "ignored\n",
        )
        .unwrap();
        assert!(repository_state(ignored_target.path(), "repo", false).is_ok());

        let ignored_pycache = tempfile::tempdir().unwrap();
        git(ignored_pycache.path(), &["init", "--quiet"]);
        fs::write(
            ignored_pycache.path().join("Cargo.toml"),
            "[package]\nname = \"fs2-test\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        fs::write(ignored_pycache.path().join(".gitignore"), "__pycache__/\n").unwrap();
        git(ignored_pycache.path(), &["add", "Cargo.toml", ".gitignore"]);
        git(
            ignored_pycache.path(),
            &[
                "-c",
                "user.name=Codex",
                "-c",
                "user.email=codex@example.com",
                "commit",
                "--quiet",
                "-m",
                "init",
            ],
        );
        fs::create_dir_all(ignored_pycache.path().join("__pycache__")).unwrap();
        fs::write(
            ignored_pycache.path().join("__pycache__/module.pyc"),
            "ignored\n",
        )
        .unwrap();
        assert!(repository_state(ignored_pycache.path(), "repo", false).is_ok());

        let ignored_pyc = tempfile::tempdir().unwrap();
        git(ignored_pyc.path(), &["init", "--quiet"]);
        fs::write(
            ignored_pyc.path().join("Cargo.toml"),
            "[package]\nname = \"fs2-test\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        fs::write(ignored_pyc.path().join(".gitignore"), "*.pyc\n").unwrap();
        git(ignored_pyc.path(), &["add", "Cargo.toml", ".gitignore"]);
        git(
            ignored_pyc.path(),
            &[
                "-c",
                "user.name=Codex",
                "-c",
                "user.email=codex@example.com",
                "commit",
                "--quiet",
                "-m",
                "init",
            ],
        );
        fs::write(ignored_pyc.path().join("module.pyc"), "ignored\n").unwrap();
        assert!(repository_state(ignored_pyc.path(), "repo", false).is_ok());
    }

    #[test]
    fn git_output_disables_repository_status_accelerators() {
        let mut command = Command::new("git");
        configure_git_output(&mut command);
        let arguments = command
            .get_args()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect::<Vec<_>>();

        assert!(arguments.ends_with(&[
            "-c".to_owned(),
            "core.fsmonitor=false".to_owned(),
            "-c".to_owned(),
            "core.untrackedCache=false".to_owned(),
        ]));
    }

    #[test]
    fn repository_state_does_not_execute_configured_fsmonitor() {
        let repo = test_tempdir();
        git(repo.path(), &["init", "--quiet"]);
        fs::write(
            repo.path().join("Cargo.toml"),
            "[package]\nname = \"fs2-test\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        let hook = repo.path().join("fsmonitor-test-hook");
        fs::write(&hook, "#!/bin/sh\nprintf invoked > fsmonitor-was-invoked\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
        }
        git(repo.path(), &["add", "Cargo.toml", "fsmonitor-test-hook"]);
        git(
            repo.path(),
            &[
                "-c",
                "user.name=Codex",
                "-c",
                "user.email=codex@example.com",
                "commit",
                "--quiet",
                "-m",
                "init",
            ],
        );
        git(
            repo.path(),
            &["config", "core.fsmonitor", "./fsmonitor-test-hook"],
        );
        git(repo.path(), &["config", "core.untrackedCache", "true"]);

        let state = repository_state(repo.path(), "repo", false);

        assert!(state.is_ok(), "{state:?}");
        assert!(!repo.path().join("fsmonitor-was-invoked").exists());
    }

    #[cfg(windows)]
    #[test]
    fn strict_repository_state_rejects_unc_before_git_access() {
        let error = repository_state(
            Path::new(r"\\server\share\selected-repository"),
            "strict selected repository",
            true,
        )
        .expect_err("UNC repository must be rejected before Git access");

        assert!(error.to_string().contains("local fixed Windows volume"));
    }

    #[test]
    #[cfg(windows)]
    fn benchmark_git_commands_enable_long_paths_on_windows() {
        let mut command = Command::new("git");
        enable_git_long_paths(&mut command);

        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            ["-c", "core.longpaths=true"]
        );
    }

    #[test]
    fn clone_revision_materializes_only_recorded_commit_bytes() {
        let repo = tempfile::tempdir().unwrap();
        git(repo.path(), &["init", "--quiet"]);
        fs::write(
            repo.path().join("Cargo.toml"),
            "[package]\nname = \"fs2-test\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        fs::write(repo.path().join(".gitignore"), "build.rs\n").unwrap();
        git(repo.path(), &["add", "Cargo.toml", ".gitignore"]);
        git(
            repo.path(),
            &[
                "-c",
                "user.name=Codex",
                "-c",
                "user.email=codex@example.com",
                "commit",
                "--quiet",
                "-m",
                "init",
            ],
        );
        fs::write(
            repo.path().join("build.rs"),
            "fn main() { panic!(\"ignored input executed\") }\n",
        )
        .unwrap();
        let revision = resolve_ref(repo.path(), "HEAD").unwrap();
        let work = tempfile::tempdir().unwrap();
        let destination = work.path().join("materialized");
        let logs = work.path().join("logs");

        let records =
            clone_revision(repo.path(), &destination, &revision, &logs, "subject").unwrap();

        assert!(processes_succeeded(&records));
        assert_eq!(resolve_ref(&destination, "HEAD").unwrap(), revision);
        assert!(!destination.join("build.rs").exists());
    }

    #[cfg(windows)]
    fn test_tempdir() -> tempfile::TempDir {
        super::super::windows_security::private_test_tempdir()
    }

    #[cfg(not(windows))]
    fn test_tempdir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }
}
