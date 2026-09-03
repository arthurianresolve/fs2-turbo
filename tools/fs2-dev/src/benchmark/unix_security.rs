use std::collections::VecDeque;
use std::env;
use std::ffi::{OsStr, OsString};
#[cfg(any(target_os = "espidf", target_os = "redox"))]
use std::fs;
use std::io;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::os::fd::AsRawFd;
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStringExt as _;
#[cfg(any(target_os = "espidf", target_os = "redox"))]
use std::os::unix::fs::MetadataExt as _;
use std::path::{Component, Path, PathBuf};

#[cfg(not(any(target_os = "espidf", target_os = "redox")))]
use rustix::fs::{AtFlags, readlinkat, statat};
use rustix::fs::{Mode, OFlags, fstat, mkdirat, open, openat};

const MAX_SYMLINK_HOPS: usize = 40;
const SHARED_WRITE_BITS: u32 = 0o022;
const STICKY_BIT: u32 = 0o1000;

enum PendingComponent {
    Parent,
    Normal {
        name: OsString,
        create_missing: bool,
    },
}

/// Creates and opens a Unix directory path without following symlinks.
///
/// Every component is checked while its descriptor is held. Group- or
/// world-writable components are rejected unless they are sticky ancestors;
/// the caller decides whether the final component may itself be sticky.
pub(super) fn prepare_directory(
    path: &Path,
    label: &str,
    allow_sticky_final: bool,
    create_missing: bool,
) -> io::Result<Vec<OwnedFd>> {
    let path = absolute_path(path)?;
    let components = normal_components(&path, label)?;
    let component_count = components.len();
    let mut pending = components
        .into_iter()
        .map(|name| PendingComponent::Normal {
            name,
            create_missing,
        })
        .collect::<VecDeque<_>>();

    let mut handles = Vec::with_capacity(component_count + 1);
    let root = open("/", directory_flags(), Mode::empty())?;
    validate_directory(
        &root,
        Path::new("/"),
        label,
        component_count != 0 || allow_sticky_final,
    )?;
    handles.push(root);

    let mut active = vec![0usize];
    let mut current = PathBuf::from("/");
    let mut symlink_hops = 0usize;
    while let Some(component) = pending.pop_front() {
        match component {
            PendingComponent::Parent => {
                if active.len() > 1 {
                    active.pop();
                    current.pop();
                }
            }
            PendingComponent::Normal {
                name,
                create_missing,
            } => {
                let parent_index = *active.last().expect("root directory handle is retained");
                let parent = &handles[parent_index];
                let next_path = current.join(&name);
                let directory = match openat(
                    parent,
                    name.as_os_str(),
                    directory_flags(),
                    Mode::empty(),
                ) {
                    Ok(directory) => directory,
                    Err(open_error) => {
                        if let Some(target) =
                            validated_symlink_target(parent, name.as_os_str(), &next_path, label)?
                        {
                            symlink_hops += 1;
                            if symlink_hops > MAX_SYMLINK_HOPS {
                                return Err(io::Error::new(
                                    io::ErrorKind::InvalidInput,
                                    format!(
                                        "{label} contains more than {MAX_SYMLINK_HOPS} symlink hops: {}",
                                        path.display()
                                    ),
                                ));
                            }
                            if target.is_absolute() {
                                active.truncate(1);
                                current = PathBuf::from("/");
                            }
                            prepend_symlink_target(&mut pending, &target, label)?;
                            continue;
                        }
                        if !create_missing || open_error != rustix::io::Errno::NOENT {
                            return Err(open_error.into());
                        }
                        match mkdirat(parent, name.as_os_str(), Mode::from_raw_mode(0o700)) {
                            Ok(()) => {}
                            Err(error) if error == rustix::io::Errno::EXIST => {}
                            Err(error) => return Err(error.into()),
                        }
                        openat(parent, name.as_os_str(), directory_flags(), Mode::empty())?
                    }
                };

                validate_directory(
                    &directory,
                    &next_path,
                    label,
                    !pending.is_empty() || allow_sticky_final,
                )?;
                handles.push(directory);
                active.push(handles.len() - 1);
                current = next_path;
            }
        }
    }

    let final_index = *active.last().expect("root directory handle is retained");
    validate_directory(&handles[final_index], &current, label, allow_sticky_final)?;
    let last_index = handles.len() - 1;
    handles.swap(final_index, last_index);
    Ok(handles)
}

fn prepend_symlink_target(
    pending: &mut VecDeque<PendingComponent>,
    target: &Path,
    label: &str,
) -> io::Result<()> {
    let mut target_components = Vec::new();
    for component in target.components() {
        match component {
            Component::RootDir | Component::CurDir => {}
            Component::ParentDir => target_components.push(PendingComponent::Parent),
            Component::Normal(name) => target_components.push(PendingComponent::Normal {
                name: name.to_owned(),
                create_missing: false,
            }),
            Component::Prefix(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!(
                        "{label} symlink target is not a Unix path: {}",
                        target.display()
                    ),
                ));
            }
        }
    }
    for component in target_components.into_iter().rev() {
        pending.push_front(component);
    }
    Ok(())
}

#[cfg(not(any(target_os = "espidf", target_os = "redox")))]
fn validated_symlink_target(
    parent: &OwnedFd,
    name: &OsStr,
    path: &Path,
    label: &str,
) -> io::Result<Option<PathBuf>> {
    let metadata = match statat(parent, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(metadata) => metadata,
        Err(error) if error == rustix::io::Errno::NOENT => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if metadata.st_mode & libc::S_IFMT != libc::S_IFLNK {
        return Ok(None);
    }
    validate_symlink_owner(metadata.st_uid, path, label)?;
    let target = readlinkat(parent, name, Vec::new())?;
    Ok(Some(PathBuf::from(OsString::from_vec(target.into_bytes()))))
}

#[cfg(any(target_os = "espidf", target_os = "redox"))]
fn validated_symlink_target(
    _parent: &OwnedFd,
    _name: &OsStr,
    path: &Path,
    label: &str,
) -> io::Result<Option<PathBuf>> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if !metadata.file_type().is_symlink() {
        return Ok(None);
    }
    validate_symlink_owner(metadata.uid(), path, label)?;
    Ok(Some(fs::read_link(path)?))
}

fn validate_symlink_owner(uid: u32, path: &Path, label: &str) -> io::Result<()> {
    // SAFETY: geteuid has no preconditions and does not dereference pointers.
    let effective_uid = unsafe { libc::geteuid() };
    if uid != effective_uid && uid != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "{label} contains an untrusted symlink: {} has uid {}, expected uid {} or root",
                path.display(),
                uid,
                effective_uid
            ),
        ));
    }
    Ok(())
}

/// Retains existing output ancestry without following descendants or creating missing parents.
pub(super) fn retain_output_ancestry(
    anchor: &Path,
    parent: &Path,
) -> io::Result<(Vec<OwnedFd>, PathBuf, bool)> {
    let label = "benchmark output preflight parent";
    let relative = parent
        .strip_prefix(anchor)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "output escaped its anchor"))?;
    let components = normal_components(relative, label)?;
    let mut handles = prepare_directory(anchor, label, false, false)?;
    let mut current = anchor.to_owned();
    for (index, name) in components.iter().enumerate() {
        let directory = handles.last().expect("anchor directory is retained");
        let next = match openat(
            directory,
            name.as_os_str(),
            directory_flags(),
            Mode::empty(),
        ) {
            Ok(next) => next,
            Err(error) if error == rustix::io::Errno::NOENT => {
                return Ok((handles, current, false));
            }
            Err(error) => return Err(error.into()),
        };
        current.push(name);
        validate_directory(&next, &current, label, index + 1 != components.len())?;
        handles.push(next);
    }
    Ok((handles, current, true))
}

pub(super) fn harden_publication_path(path: &Path, label: &str) -> io::Result<()> {
    let object = open(
        path,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::empty(),
    )?;
    let stat = fstat(&object)?;
    // SAFETY: geteuid has no preconditions and does not dereference pointers.
    let effective_uid = unsafe { libc::geteuid() };
    if stat.st_uid != effective_uid {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "{label} is not owned by the current user: {} has uid {}, expected uid {}",
                path.display(),
                stat.st_uid,
                effective_uid
            ),
        ));
    }
    let file_type = stat.st_mode & libc::S_IFMT;
    let mode = if file_type == libc::S_IFDIR {
        0o700
    } else if file_type == libc::S_IFREG {
        if stat.st_mode & libc::S_IXUSR != 0 {
            0o700
        } else {
            0o600
        }
    } else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "{label} is not a regular file or directory: {}",
                path.display()
            ),
        ));
    };
    rustix::fs::fchmod(&object, Mode::from_raw_mode(mode))?;
    validate_filesystem_authority(&object, path, label)?;
    let hardened = fstat(&object)?;
    if hardened.st_mode as u32 & 0o077 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "{label} retains group or world permissions after hardening: {}",
                path.display()
            ),
        ));
    }
    Ok(())
}

fn normal_components(path: &Path, label: &str) -> io::Result<Vec<OsString>> {
    let mut normal = Vec::new();
    for component in path.components() {
        match component {
            Component::RootDir | Component::CurDir => {}
            Component::Normal(name) => normal.push(name.to_owned()),
            Component::ParentDir => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("{label} path must not contain '..': {}", path.display()),
                ));
            }
            Component::Prefix(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("{label} is not a Unix path: {}", path.display()),
                ));
            }
        }
    }
    Ok(normal)
}

fn directory_flags() -> OFlags {
    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC
}

fn validate_directory(
    directory: &OwnedFd,
    path: &Path,
    label: &str,
    allow_sticky: bool,
) -> io::Result<()> {
    validate_filesystem_authority(directory, path, label)?;

    let stat = fstat(directory)?;
    // SAFETY: geteuid has no preconditions and does not dereference pointers.
    let effective_uid = unsafe { libc::geteuid() };
    if stat.st_uid != effective_uid && stat.st_uid != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "{label} is not securely owned: {} has uid {}, expected uid {} or root",
                path.display(),
                stat.st_uid,
                effective_uid
            ),
        ));
    }

    let mode = stat.st_mode as u32;
    let shared_writable = mode & SHARED_WRITE_BITS != 0;
    let sticky = mode & STICKY_BIT != 0;
    if shared_writable && !(allow_sticky && sticky) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "{label} is not securely isolated: {} is group- or world-writable (mode {:04o})",
                path.display(),
                mode & 0o7777
            ),
        ));
    }

    Ok(())
}

#[cfg(target_os = "linux")]
fn validate_filesystem_authority(directory: &OwnedFd, path: &Path, label: &str) -> io::Result<()> {
    const V9FS_MAGIC: u32 = 0x0102_1997;
    let mut filesystem = std::mem::MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: filesystem points to writable storage and directory is a live descriptor.
    if unsafe { libc::fstatfs(directory.as_raw_fd(), filesystem.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a successful fstatfs initialized the output structure.
    let filesystem = unsafe { filesystem.assume_init() };
    let filesystem_type = filesystem.f_type as u32;
    if trusted_linux_filesystem(filesystem_type) {
        return Ok(());
    }

    let detail = if filesystem_type == V9FS_MAGIC {
        "9p/DrvFs".to_owned()
    } else {
        format!("filesystem type 0x{filesystem_type:08x}")
    };
    Err(io::Error::new(
        io::ErrorKind::PermissionDenied,
        format!(
            "{label} authority cannot be proven on {detail} at {}; use a supported local POSIX filesystem",
            path.display()
        ),
    ))
}

#[cfg(target_os = "linux")]
fn trusted_linux_filesystem(filesystem_type: u32) -> bool {
    matches!(
        filesystem_type,
        0x0000_3434 // NILFS2
            | 0x0001_1954 // UFS
            | 0x0102_1994 // tmpfs
            | 0x2fc1_2fc1 // ZFS
            | 0x3153_464a // JFS
            | 0x5265_4973 // ReiserFS
            | 0x5846_5342 // XFS
            | 0x8584_58f6 // ramfs
            | 0x9123_683e // Btrfs
            | 0xca45_1a4e // bcachefs
            | 0xef53 // ext2/ext3/ext4
            | 0xf2f5_2010 // F2FS
    )
}

#[cfg(target_os = "macos")]
fn validate_filesystem_authority(directory: &OwnedFd, path: &Path, label: &str) -> io::Result<()> {
    let mut filesystem = std::mem::MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: filesystem points to writable storage and directory is a live descriptor.
    if unsafe { libc::fstatfs(directory.as_raw_fd(), filesystem.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a successful fstatfs initialized the output structure.
    let filesystem = unsafe { filesystem.assume_init() };
    let local = filesystem.f_flags & libc::MNT_LOCAL as u32 != 0;
    let name_end = filesystem
        .f_fstypename
        .iter()
        .position(|character| *character == 0)
        .unwrap_or(filesystem.f_fstypename.len());
    let name: Vec<u8> = filesystem.f_fstypename[..name_end]
        .iter()
        .map(|character| *character as u8)
        .collect();
    let supported = matches!(name.as_slice(), b"apfs" | b"hfs" | b"ufs" | b"tmpfs");
    if !local || !supported {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "{label} authority cannot be proven on macOS filesystem '{}' at {}",
                String::from_utf8_lossy(&name),
                path.display()
            ),
        ));
    }
    reject_macos_extended_acl(directory, path, label)
}

#[cfg(target_os = "macos")]
fn reject_macos_extended_acl(directory: &OwnedFd, path: &Path, label: &str) -> io::Result<()> {
    const ACL_TYPE_EXTENDED: libc::c_int = 0x0000_0100;
    const ACL_FIRST_ENTRY: libc::c_int = 0;
    unsafe extern "C" {
        fn acl_get_fd_np(fd: libc::c_int, acl_type: libc::c_int) -> *mut libc::c_void;
        fn acl_get_entry(
            acl: *mut libc::c_void,
            entry_id: libc::c_int,
            entry: *mut *mut libc::c_void,
        ) -> libc::c_int;
        fn acl_free(object: *mut libc::c_void) -> libc::c_int;
    }

    // SAFETY: acl_get_fd_np only observes the live descriptor and returns owned ACL storage.
    let acl = unsafe { acl_get_fd_np(directory.as_raw_fd(), ACL_TYPE_EXTENDED) };
    if acl.is_null() {
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ENOENT) {
            return Ok(());
        }
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "{label} ACL authority cannot be inspected at {}: {}",
                path.display(),
                error
            ),
        ));
    }

    let mut entry = std::ptr::null_mut();
    // SAFETY: acl is live and entry points to writable pointer storage.
    let result = unsafe { acl_get_entry(acl, ACL_FIRST_ENTRY, &mut entry) };
    let error = io::Error::last_os_error();
    // SAFETY: acl was allocated by acl_get_fd_np and is released exactly once.
    let _ = unsafe { acl_free(acl) };
    if result == 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "{label} has an extended ACL whose authority is not accepted: {}",
                path.display()
            ),
        ));
    }
    if error.raw_os_error() == Some(libc::EINVAL) {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "{label} ACL authority cannot be inspected at {}: {error}",
                path.display()
            ),
        ))
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn validate_filesystem_authority(_directory: &OwnedFd, path: &Path, label: &str) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::PermissionDenied,
        format!(
            "{label} filesystem authority cannot be proven on this Unix platform at {}",
            path.display()
        ),
    ))
}

fn absolute_path(path: &Path) -> io::Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(env::current_dir()?.join(path))
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::fs::symlink;

    use super::prepare_directory;

    #[test]
    fn accepts_private_directory() {
        let root = tempfile::tempdir().unwrap();
        let private = root.path().join("private");

        let handles = prepare_directory(&private, "test directory", false, true).unwrap();

        assert!(private.is_dir());
        assert!(!handles.is_empty());
    }

    #[test]
    fn rejects_non_sticky_shared_workspace_parent() {
        let root = tempfile::tempdir().unwrap();
        let shared = root.path().join("shared");
        fs::create_dir(&shared).unwrap();
        fs::set_permissions(&shared, fs::Permissions::from_mode(0o777)).unwrap();

        let error = prepare_directory(&shared, "test directory", true, true).unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn accepts_private_directory_below_trusted_sticky_parent() {
        let root = tempfile::tempdir().unwrap();
        let shared = root.path().join("shared");
        fs::create_dir(&shared).unwrap();
        fs::set_permissions(&shared, fs::Permissions::from_mode(0o1777)).unwrap();
        let private = shared.join("private");

        let handles = prepare_directory(&private, "test directory", false, true).unwrap();

        assert!(private.is_dir());
        assert!(!handles.is_empty());
    }

    #[test]
    fn accepts_protected_symlink_and_validates_its_target() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("target");
        fs::create_dir(&target).unwrap();
        let alias = root.path().join("alias");
        symlink(&target, &alias).unwrap();

        let handles = prepare_directory(&alias, "test directory", false, false).unwrap();

        assert!(!handles.is_empty());
    }

    #[test]
    fn rejects_mutable_ancestry_hidden_by_a_symlink_chain() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("target");
        fs::create_dir(&target).unwrap();
        let mutable = root.path().join("mutable");
        fs::create_dir(&mutable).unwrap();
        fs::set_permissions(&mutable, fs::Permissions::from_mode(0o777)).unwrap();
        symlink(&target, mutable.join("inner")).unwrap();
        let alias = root.path().join("alias");
        symlink("mutable/inner", &alias).unwrap();

        let error = prepare_directory(&alias, "test directory", false, false).unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn accepts_protected_relative_symlink_chain() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("target");
        fs::create_dir(&target).unwrap();
        let middle = root.path().join("middle");
        fs::create_dir(&middle).unwrap();
        symlink("../target", middle.join("inner")).unwrap();
        let alias = root.path().join("alias");
        symlink("middle/inner", &alias).unwrap();

        let handles = prepare_directory(&alias, "test directory", false, false).unwrap();

        assert!(!handles.is_empty());
    }

    #[test]
    fn does_not_create_a_dangling_symlink_target() {
        let root = tempfile::tempdir().unwrap();
        let missing = root.path().join("missing");
        let alias = root.path().join("alias");
        symlink("missing/target", &alias).unwrap();

        let error = prepare_directory(&alias, "test directory", false, true).unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
        assert!(!missing.exists());
    }

    #[test]
    fn rejects_excessive_symlink_hops() {
        let root = tempfile::tempdir().unwrap();
        let first = root.path().join("first");
        let second = root.path().join("second");
        symlink("second", &first).unwrap();
        symlink("first", &second).unwrap();

        let error = prepare_directory(&first, "test directory", false, false).unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    }

    #[test]
    fn rejects_sticky_shared_publication_parent() {
        let root = tempfile::tempdir().unwrap();
        let shared = root.path().join("shared");
        fs::create_dir(&shared).unwrap();
        fs::set_permissions(&shared, fs::Permissions::from_mode(0o1777)).unwrap();

        let error = prepare_directory(&shared, "publication parent", false, false).unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn rejects_network_userspace_and_layered_linux_filesystems() {
        use super::trusted_linux_filesystem;

        assert!(trusted_linux_filesystem(0xef53));
        assert!(!trusted_linux_filesystem(0x0102_1997)); // 9p
        assert!(!trusted_linux_filesystem(0x6573_5546)); // FUSE
        assert!(!trusted_linux_filesystem(0xff53_4d42)); // CIFS
        assert!(!trusted_linux_filesystem(0x794c_7630)); // OverlayFS
    }
}
