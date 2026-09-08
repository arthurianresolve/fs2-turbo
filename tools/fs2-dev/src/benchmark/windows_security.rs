use std::ffi::c_void;
use std::fs;
use std::io;
use std::mem::size_of;
use std::os::windows::ffi::OsStrExt as _;
use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _};
use std::path::{Path, PathBuf};

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, ERROR_INSUFFICIENT_BUFFER, GENERIC_ALL, GENERIC_WRITE,
    HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Security::Authorization::{SE_FILE_OBJECT, SetSecurityInfo};
use windows_sys::Win32::Security::{
    ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, ACL_REVISION, ACL_SIZE_INFORMATION, AclSizeInformation,
    AddAccessAllowedAceEx, CONTAINER_INHERIT_ACE, CreateWellKnownSid, DACL_SECURITY_INFORMATION,
    EqualSid, GetAce, GetAclInformation, GetKernelObjectSecurity, GetLengthSid,
    GetSecurityDescriptorControl, GetSecurityDescriptorDacl, GetSecurityDescriptorOwner,
    GetTokenInformation, INHERIT_ONLY_ACE, InitializeAcl, InitializeSecurityDescriptor, IsValidSid,
    OBJECT_INHERIT_ACE, OWNER_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION, PSID,
    SE_DACL_PROTECTED, SECURITY_ATTRIBUTES, SECURITY_DESCRIPTOR, SetSecurityDescriptorControl,
    SetSecurityDescriptorDacl, SetSecurityDescriptorOwner, TOKEN_QUERY, TOKEN_USER, TokenUser,
    WinBuiltinAdministratorsSid, WinLocalSystemSid,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateDirectoryW, CreateFileW, DDD_EXACT_MATCH_ON_REMOVE, DDD_NO_BROADCAST_SYSTEM,
    DDD_RAW_TARGET_PATH, DDD_REMOVE_DEFINITION, DELETE, DefineDosDeviceW, FILE_ADD_FILE,
    FILE_ADD_SUBDIRECTORY, FILE_ALL_ACCESS, FILE_APPEND_DATA, FILE_ATTRIBUTE_REPARSE_POINT,
    FILE_ATTRIBUTE_TAG_INFO, FILE_DELETE_CHILD, FILE_FLAG_BACKUP_SEMANTICS,
    FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE,
    FILE_TRAVERSE, FILE_WRITE_ATTRIBUTES, FileAttributeTagInfo, GetDriveTypeW,
    GetFileInformationByHandleEx, GetLogicalDrives, OPEN_EXISTING, QueryDosDeviceW, READ_CONTROL,
    WRITE_DAC, WRITE_OWNER,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

use crate::{Result, invalid_data};

const CARGO_CONFIG_ROOT: &str = "cargo-config-root";

pub(crate) struct IsolatedCargoDirectory {
    path: PathBuf,
    device: Vec<u16>,
    target: Vec<u16>,
    _directory: fs::File,
}

impl IsolatedCargoDirectory {
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for IsolatedCargoDirectory {
    fn drop(&mut self) {
        // SAFETY: both strings remain terminated and unchanged for the lifetime
        // of this exact local-session device mapping.
        unsafe {
            DefineDosDeviceW(
                DDD_REMOVE_DEFINITION
                    | DDD_EXACT_MATCH_ON_REMOVE
                    | DDD_NO_BROADCAST_SYSTEM
                    | DDD_RAW_TARGET_PATH,
                self.device.as_ptr(),
                self.target.as_ptr(),
            );
        }
    }
}

pub(crate) fn isolated_cargo_working_directory(
    private_root: &Path,
) -> Result<IsolatedCargoDirectory> {
    let directory_path = private_root.join(CARGO_CONFIG_ROOT);
    let directory = create_or_open_private_directory(&directory_path)?;
    let canonical = directory_path.canonicalize()?;
    require_canonical_local_fixed_volume(&canonical, "isolated Cargo configuration root")?;
    let target = dos_device_target(&canonical)?;
    // A drive-root working directory limits Cargo's hierarchical configuration
    // search to this owner/SYSTEM-only directory and the trusted Cargo home.
    let drives = unsafe { GetLogicalDrives() };
    if drives == 0 {
        return Err(io::Error::last_os_error().into());
    }

    for letter in (b'D'..=b'Z').rev() {
        let bit = 1_u32 << u32::from(letter - b'A');
        if drives & bit != 0 {
            continue;
        }
        let device = encode_path(Path::new(&format!("{}:", char::from(letter))));
        // SAFETY: the device and target are terminated UTF-16 strings.
        if unsafe {
            DefineDosDeviceW(
                DDD_NO_BROADCAST_SYSTEM | DDD_RAW_TARGET_PATH,
                device.as_ptr(),
                target.as_ptr(),
            )
        } == 0
        {
            continue;
        }
        if dos_device_matches(&device, &target) {
            return Ok(IsolatedCargoDirectory {
                path: PathBuf::from(format!("{}:\\", char::from(letter))),
                device,
                target,
                _directory: directory,
            });
        }
        // SAFETY: remove only the exact mapping just attempted.
        unsafe {
            DefineDosDeviceW(
                DDD_REMOVE_DEFINITION
                    | DDD_EXACT_MATCH_ON_REMOVE
                    | DDD_NO_BROADCAST_SYSTEM
                    | DDD_RAW_TARGET_PATH,
                device.as_ptr(),
                target.as_ptr(),
            );
        }
    }

    Err(invalid_data(
        "no unused DOS drive is available for Cargo configuration isolation",
    ))
}

fn dos_device_target(path: &Path) -> Result<Vec<u16>> {
    let value = path
        .to_str()
        .ok_or_else(|| invalid_data("isolated Cargo configuration root is not Unicode"))?;
    let disk_path = value.strip_prefix(r"\\?\").unwrap_or(value);
    if disk_path.starts_with(r"UNC\") {
        return Err(invalid_data(
            "isolated Cargo configuration root must be on a local disk",
        ));
    }
    Ok(encode_path(Path::new(&format!(r"\??\{disk_path}"))))
}

fn dos_device_matches(device: &[u16], expected: &[u16]) -> bool {
    let mut observed = vec![0_u16; 32_768];
    // SAFETY: the device is terminated and the output buffer is writable.
    let length = unsafe {
        QueryDosDeviceW(
            device.as_ptr(),
            observed.as_mut_ptr(),
            observed.len() as u32,
        )
    };
    if length == 0 {
        return false;
    }
    let observed_length = observed.iter().position(|unit| *unit == 0).unwrap_or(0);
    let expected_length = expected.len().saturating_sub(1);
    observed_length == expected_length && observed[..observed_length] == expected[..expected_length]
}

const SECURITY_DESCRIPTOR_REVISION_VALUE: u32 = 1;
const ACCESS_ALLOWED_ACE_TYPE_VALUE: u8 = 0;
const ACCESS_DENIED_ACE_TYPE_VALUE: u8 = 1;
const ACCESS_DENIED_OBJECT_ACE_TYPE_VALUE: u8 = 6;
const ACCESS_DENIED_CALLBACK_ACE_TYPE_VALUE: u8 = 10;
const ACCESS_DENIED_CALLBACK_OBJECT_ACE_TYPE_VALUE: u8 = 12;
const PRIVATE_ACE_FLAGS: u32 = OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE;
const DESTRUCTIVE_NAMESPACE_ACCESS: u32 =
    DELETE | FILE_DELETE_CHILD | WRITE_DAC | WRITE_OWNER | GENERIC_ALL;
const RETAINED_PATH_MUTATION_ACCESS: u32 =
    DESTRUCTIVE_NAMESPACE_ACCESS | FILE_ADD_FILE | FILE_WRITE_ATTRIBUTES | GENERIC_WRITE;
const DESCENDANT_DIRECTORY_MUTATION_ACCESS: u32 =
    RETAINED_PATH_MUTATION_ACCESS | FILE_ADD_SUBDIRECTORY;
const DESCENDANT_FILE_MUTATION_ACCESS: u32 = RETAINED_PATH_MUTATION_ACCESS | FILE_APPEND_DATA;
const DRIVE_FIXED: u32 = 3;

// NT SERVICE\TrustedInstaller: S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464.
// This exception is limited to volume roots, not ordinary or private directories.
const TRUSTED_INSTALLER_SID: [u8; 32] = [
    1, 6, 0, 0, 0, 0, 0, 5, 80, 0, 0, 0, 181, 137, 251, 56, 25, 132, 194, 203, 92, 108, 35, 109,
    87, 0, 119, 110, 192, 2, 100, 135,
];

#[derive(Clone, Copy)]
enum AncestorRole {
    Directory,
    VolumeRoot,
    FixtureRoot,
}

pub(crate) fn require_local_fixed_volume(path: &Path, label: &str) -> Result<()> {
    require_local_fixed_volume_with(path, label, false)
}

pub(crate) fn require_canonical_local_fixed_volume(path: &Path, label: &str) -> Result<()> {
    require_local_fixed_volume_with(path, label, true)
}

fn require_local_fixed_volume_with(
    path: &Path,
    label: &str,
    allow_canonical_verbatim_disk: bool,
) -> Result<()> {
    reject_ambiguous_path_with(path, allow_canonical_verbatim_disk)?;
    if !path.is_absolute() {
        return Err(invalid_data(format!(
            "{label} must be absolute: {}",
            path.display()
        )));
    }
    if !is_local_fixed_drive_with_options(path, allow_canonical_verbatim_disk, |root| {
        // SAFETY: root points to a NUL-terminated drive-root string.
        unsafe { GetDriveTypeW(root) }
    }) {
        return Err(invalid_data(format!(
            "{label} must reside on a local fixed Windows volume; UNC, mapped remote, removable, optical, RAM-disk, unknown, and unavailable roots are not accepted: {}",
            path.display()
        )));
    }
    Ok(())
}

pub(crate) fn is_local_fixed_drive(path: &Path) -> bool {
    is_local_fixed_drive_with(path, |root| {
        // SAFETY: root points to a NUL-terminated drive-root string.
        unsafe { GetDriveTypeW(root) }
    })
}

fn is_local_fixed_drive_with(path: &Path, drive_type: impl FnOnce(*const u16) -> u32) -> bool {
    is_local_fixed_drive_with_options(path, false, drive_type)
}

fn is_local_fixed_drive_with_options(
    path: &Path,
    allow_canonical_verbatim_disk: bool,
    drive_type: impl FnOnce(*const u16) -> u32,
) -> bool {
    use std::path::{Component, Prefix};

    let Some(Component::Prefix(prefix)) = path.components().next() else {
        return false;
    };
    let letter = match prefix.kind() {
        Prefix::Disk(letter) => letter,
        Prefix::VerbatimDisk(letter) if allow_canonical_verbatim_disk => letter,
        _ => return false,
    };
    let root = [u16::from(letter), u16::from(b':'), u16::from(b'\\'), 0];
    drive_type(root.as_ptr()) == DRIVE_FIXED
}

pub(crate) fn guard_directory_ancestry(path: &Path) -> Result<Vec<fs::File>> {
    guard_trusted_directory_ancestry(path, false)
}

pub(crate) fn guard_fixture_directory_ancestry(path: &Path) -> Result<Vec<fs::File>> {
    let held = guard_directory_ancestry(path)?;
    if path.parent().is_none() {
        // A directly used fixture root creates descendants, unlike traversal
        // through a root to an independently checked private directory.
        let fixture_root = held
            .last()
            .ok_or_else(|| invalid_data("fixture root ancestry retained no directory"))?;
        verify_trusted_ancestor(
            fixture_root,
            path,
            &PrivateSecurity::new()?,
            AncestorRole::FixtureRoot,
        )?;
    }
    Ok(held)
}

pub(crate) fn guard_publication_directory_ancestry(path: &Path) -> Result<Vec<fs::File>> {
    guard_trusted_directory_ancestry(path, false)
}

pub(crate) fn guard_canonical_directory_ancestry(path: &Path) -> Result<Vec<fs::File>> {
    guard_trusted_directory_ancestry(path, true)
}

fn guard_trusted_directory_ancestry(
    path: &Path,
    allow_canonical_verbatim_disk: bool,
) -> Result<Vec<fs::File>> {
    use std::path::Component;

    reject_ambiguous_path_with(path, allow_canonical_verbatim_disk)?;
    if !path.is_absolute() {
        return Err(invalid_data(format!(
            "benchmark directory path must be absolute: {}",
            path.display()
        )));
    }
    let mut current = std::path::PathBuf::new();
    let mut held = Vec::new();
    for component in path.components() {
        match component {
            Component::Prefix(_) => {
                current.push(component.as_os_str());
                continue;
            }
            Component::RootDir | Component::Normal(_) => current.push(component.as_os_str()),
            Component::CurDir => continue,
            Component::ParentDir => {
                return Err(invalid_data(format!(
                    "benchmark directory path may not contain '..': {}",
                    path.display()
                )));
            }
        }
        let directory = open_ancestry_directory(&current).map_err(|error| {
            invalid_data(format!(
                "unable to retain benchmark directory ancestry {}: {error}",
                current.display()
            ))
        })?;
        retain_physical_root_ancestry(&current, &directory, &mut held)?;
        held.push(directory);
    }
    if held.is_empty() {
        return Err(invalid_data(format!(
            "benchmark directory path has no openable component: {}",
            path.display()
        )));
    }
    Ok(held)
}

pub(crate) fn create_or_open_trusted_directory_ancestry(path: &Path) -> Result<Vec<fs::File>> {
    use std::path::Component;

    reject_ambiguous_path(path)?;
    if !path.is_absolute() {
        return Err(invalid_data(format!(
            "benchmark directory path must be absolute: {}",
            path.display()
        )));
    }
    let mut current = std::path::PathBuf::new();
    let mut held = Vec::new();
    for component in path.components() {
        match component {
            Component::Prefix(_) => {
                current.push(component.as_os_str());
                continue;
            }
            Component::RootDir | Component::Normal(_) => current.push(component.as_os_str()),
            Component::CurDir => continue,
            Component::ParentDir => {
                return Err(invalid_data(format!(
                    "benchmark directory path may not contain '..': {}",
                    path.display()
                )));
            }
        }
        let directory = match fs::symlink_metadata(&current) {
            Ok(_) => open_ancestry_directory(&current),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                create_or_open_private_directory(&current)
            }
            Err(error) => Err(error.into()),
        }
        .map_err(|error| {
            invalid_data(format!(
                "unable to retain or create benchmark directory ancestry {}: {error}",
                current.display()
            ))
        })?;
        retain_physical_root_ancestry(&current, &directory, &mut held)?;
        held.push(directory);
    }
    if held.is_empty() {
        return Err(invalid_data(format!(
            "benchmark directory path has no openable component: {}",
            path.display()
        )));
    }
    Ok(held)
}

fn retain_physical_root_ancestry(
    path: &Path,
    directory: &fs::File,
    held: &mut Vec<fs::File>,
) -> Result<()> {
    if path.parent().is_some() {
        return Ok(());
    }
    let physical = final_directory_path(directory)?;
    if physical.parent().is_some() {
        // A DOS drive can name an ordinary directory. Retain its real parents
        // too, and bind that ancestry to the already-open alias target.
        let ancestry = guard_trusted_directory_ancestry(&physical, true)?;
        let target = ancestry
            .last()
            .ok_or_else(|| invalid_data("physical drive ancestry is empty"))?;
        if directory_identity(directory)? != directory_identity(target)? {
            return Err(invalid_data("drive alias target changed during admission"));
        }
        held.extend(ancestry);
    }
    Ok(())
}

fn final_directory_path(directory: &fs::File) -> Result<PathBuf> {
    use std::os::windows::ffi::OsStringExt as _;
    use windows_sys::Win32::Storage::FileSystem::GetFinalPathNameByHandleW;

    let mut buffer = vec![0_u16; 32_768];
    // SAFETY: the retained handle is valid and the buffer is writable. Flags
    // zero request the normalized path on the actual DOS-named volume.
    let length = unsafe {
        GetFinalPathNameByHandleW(
            directory.as_raw_handle(),
            buffer.as_mut_ptr(),
            buffer.len() as u32,
            0,
        )
    };
    if length == 0 {
        return Err(io::Error::last_os_error().into());
    }
    if length as usize >= buffer.len() {
        return Err(invalid_data(
            "physical directory path exceeds the Windows path limit",
        ));
    }
    let path = PathBuf::from(std::ffi::OsString::from_wide(&buffer[..length as usize]));
    require_canonical_local_fixed_volume(&path, "physical directory ancestry")?;
    Ok(path)
}

fn directory_identity(directory: &fs::File) -> Result<(u64, [u8; 16])> {
    use windows_sys::Win32::Storage::FileSystem::{FILE_ID_INFO, FileIdInfo};

    let mut information = FILE_ID_INFO::default();
    // SAFETY: the handle is retained and the initialized output is correctly
    // sized. The full file ID also preserves identity on ReFS.
    if unsafe {
        GetFileInformationByHandleEx(
            directory.as_raw_handle(),
            FileIdInfo,
            std::ptr::from_mut(&mut information).cast(),
            size_of::<FILE_ID_INFO>() as u32,
        )
    } == 0
    {
        return Err(io::Error::last_os_error().into());
    }
    Ok((
        information.VolumeSerialNumber,
        information.FileId.Identifier,
    ))
}

pub(crate) fn reject_ambiguous_path(path: &Path) -> Result<()> {
    reject_ambiguous_path_with(path, false)
}

fn reject_ambiguous_path_with(path: &Path, allow_canonical_verbatim_disk: bool) -> Result<()> {
    use std::os::windows::ffi::OsStrExt as _;
    use std::path::{Component, Prefix};

    for component in path.components() {
        if let Component::Prefix(prefix) = component
            && !matches!(prefix.kind(), Prefix::Disk(_) | Prefix::UNC(_, _))
            && !(allow_canonical_verbatim_disk && matches!(prefix.kind(), Prefix::VerbatimDisk(_)))
        {
            return Err(invalid_data(format!(
                "benchmark path uses an unsupported Windows namespace prefix: {}",
                path.display()
            )));
        }
        let Component::Normal(component) = component else {
            continue;
        };
        let encoded = component.encode_wide().collect::<Vec<_>>();
        let has_forbidden_unit = encoded.iter().any(|unit| matches!(*unit, 0 | 0x3a));
        let normalized_alias = encoded.first() == Some(&0x20)
            || encoded.last() == Some(&0x20)
            || encoded.last() == Some(&0x2e);
        let component_text = component.to_string_lossy();
        let basename = component_text.split('.').next().unwrap_or_default();
        if has_forbidden_unit || normalized_alias || reserved_device_name(basename) {
            return Err(invalid_data(format!(
                "benchmark path contains a Win32-normalized or reserved component: {}",
                path.display()
            )));
        }
    }
    Ok(())
}

fn reserved_device_name(name: &str) -> bool {
    let name = name.to_ascii_uppercase();
    matches!(
        name.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CLOCK$" | "CONIN$" | "CONOUT$"
    ) || name
        .strip_prefix("COM")
        .or_else(|| name.strip_prefix("LPT"))
        .is_some_and(|number| {
            matches!(
                number,
                "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
            )
        })
}

pub(crate) fn create_or_open_private_directory(path: &Path) -> Result<fs::File> {
    let mut security = PrivateSecurity::new()?;
    let attributes = security.attributes();
    let encoded = encode_path(path);
    let created = unsafe {
        // SAFETY: the path and security descriptor remain valid for the call.
        CreateDirectoryW(encoded.as_ptr(), &attributes)
    };
    if created == 0 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(ERROR_ALREADY_EXISTS as i32) {
            return Err(error.into());
        }
    }

    let directory = open_directory(path, READ_CONTROL)?;
    verify_private_directory(&directory, path, &security)?;
    Ok(directory)
}

pub(super) fn create_or_open_trusted_directory(path: &Path) -> Result<fs::File> {
    match fs::symlink_metadata(path) {
        Ok(_) => open_ancestry_directory(path),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            create_or_open_private_directory(path)
        }
        Err(error) => Err(error.into()),
    }
}

pub(crate) fn harden_new_private_directory(path: &Path) -> Result<fs::File> {
    let security = PrivateSecurity::new()?;
    let directory = open_directory(path, READ_CONTROL | WRITE_DAC | WRITE_OWNER)?;
    let result = unsafe {
        // SAFETY: the directory handle is valid, and the owner SID and ACL are
        // owned by `security` for the duration of the call.
        SetSecurityInfo(
            directory.as_raw_handle(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION
                | DACL_SECURITY_INFORMATION
                | PROTECTED_DACL_SECURITY_INFORMATION,
            security.owner.as_ptr().cast_mut().cast(),
            std::ptr::null_mut(),
            security.acl.as_ptr().cast(),
            std::ptr::null(),
        )
    };
    if result != 0 {
        return Err(io::Error::from_raw_os_error(result as i32).into());
    }
    verify_private_directory(&directory, path, &security)?;
    Ok(directory)
}

pub(super) fn open_private_directory(path: &Path) -> Result<fs::File> {
    let security = PrivateSecurity::new()?;
    let directory = open_directory(path, READ_CONTROL)?;
    verify_private_directory(&directory, path, &security)?;
    Ok(directory)
}

pub(super) fn open_trusted_ancestor(path: &Path) -> Result<fs::File> {
    open_ancestor(path, AncestorRole::Directory)
}

fn open_ancestor(path: &Path, role: AncestorRole) -> Result<fs::File> {
    let security = PrivateSecurity::new()?;
    let directory = open_directory(path, READ_CONTROL)?;
    verify_trusted_ancestor(&directory, path, &security, role)?;
    Ok(directory)
}

struct PrivateSecurity {
    owner: Vec<u8>,
    system: Option<Vec<u8>>,
    administrators: Vec<u8>,
    acl: Vec<u8>,
    descriptor: Box<SECURITY_DESCRIPTOR>,
}

impl PrivateSecurity {
    fn new() -> Result<Self> {
        let mut owner = current_user_sid()?;
        let system_sid = well_known_sid(WinLocalSystemSid)?;
        let administrators = well_known_sid(WinBuiltinAdministratorsSid)?;
        let mut system = if sid_bytes_equal(&owner, &system_sid) {
            None
        } else {
            Some(system_sid)
        };

        let fixed_ace_size = size_of::<ACCESS_ALLOWED_ACE>() - size_of::<u32>();
        let mut acl_size = size_of::<ACL>() + fixed_ace_size + owner.len();
        if let Some(system) = system.as_ref() {
            acl_size += fixed_ace_size + system.len();
        }
        let acl_size = u32::try_from(acl_size)
            .map_err(|_| invalid_data("benchmark private ACL is too large"))?;
        let mut acl = vec![0u8; acl_size as usize];
        let acl_pointer = acl.as_mut_ptr().cast::<ACL>();
        // SAFETY: `acl` is writable for `acl_size` bytes.
        if unsafe { InitializeAcl(acl_pointer, acl_size, ACL_REVISION) } == 0 {
            return Err(io::Error::last_os_error().into());
        }
        add_access_ace(acl_pointer, owner.as_mut_ptr().cast())?;
        if let Some(system) = system.as_mut() {
            add_access_ace(acl_pointer, system.as_mut_ptr().cast())?;
        }

        let mut descriptor = Box::new(unsafe {
            // SAFETY: the descriptor is initialized before being passed to Windows.
            std::mem::zeroed::<SECURITY_DESCRIPTOR>()
        });
        let descriptor_pointer = std::ptr::from_mut(descriptor.as_mut()).cast::<c_void>();
        // SAFETY: `descriptor_pointer` points to writable descriptor storage.
        if unsafe {
            InitializeSecurityDescriptor(descriptor_pointer, SECURITY_DESCRIPTOR_REVISION_VALUE)
        } == 0
        {
            return Err(io::Error::last_os_error().into());
        }
        // SAFETY: the owner SID and descriptor remain valid for the call.
        if unsafe { SetSecurityDescriptorOwner(descriptor_pointer, owner.as_mut_ptr().cast(), 0) }
            == 0
        {
            return Err(io::Error::last_os_error().into());
        }
        // SAFETY: `acl_pointer` and the descriptor remain valid for the call.
        if unsafe { SetSecurityDescriptorDacl(descriptor_pointer, 1, acl_pointer, 0) } == 0 {
            return Err(io::Error::last_os_error().into());
        }
        // SAFETY: the descriptor is initialized and writable.
        if unsafe {
            SetSecurityDescriptorControl(descriptor_pointer, SE_DACL_PROTECTED, SE_DACL_PROTECTED)
        } == 0
        {
            return Err(io::Error::last_os_error().into());
        }

        Ok(Self {
            owner,
            system,
            administrators,
            acl,
            descriptor,
        })
    }

    fn attributes(&mut self) -> SECURITY_ATTRIBUTES {
        SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: std::ptr::from_mut(self.descriptor.as_mut()).cast(),
            bInheritHandle: 0,
        }
    }
}

fn add_access_ace(acl: *mut ACL, sid: PSID) -> Result<()> {
    // SAFETY: the caller supplies an initialized ACL and a valid SID.
    if unsafe { AddAccessAllowedAceEx(acl, ACL_REVISION, PRIVATE_ACE_FLAGS, FILE_ALL_ACCESS, sid) }
        == 0
    {
        return Err(io::Error::last_os_error().into());
    }
    Ok(())
}

fn open_directory(path: &Path, security_access: u32) -> Result<fs::File> {
    open_directory_with_access(path, FILE_READ_ATTRIBUTES | FILE_TRAVERSE | security_access)
}

fn open_directory_with_access(path: &Path, desired_access: u32) -> Result<fs::File> {
    open_directory_with_share(path, desired_access, FILE_SHARE_READ | FILE_SHARE_WRITE)
}

fn open_ancestry_directory(path: &Path) -> Result<fs::File> {
    // The metadata probe is only an early rejection. The retained handle is
    // independently opened without reparse traversal or delete sharing and
    // its ACL is validated before it becomes part of the authority chain.
    reject_reparse_metadata(path)?;
    if path.parent().is_none() {
        // The root itself cannot be renamed, but its effective delete-child
        // rights can replace private evidence after retained handles close.
        return open_ancestor(path, AncestorRole::VolumeRoot);
    }
    open_trusted_ancestor(path)
}

fn open_directory_with_share(
    path: &Path,
    desired_access: u32,
    share_mode: u32,
) -> Result<fs::File> {
    open_directory_with_options(path, desired_access, share_mode, true)
}

fn open_directory_with_options(
    path: &Path,
    desired_access: u32,
    share_mode: u32,
    open_reparse_point: bool,
) -> Result<fs::File> {
    let encoded = encode_path(path);
    let flags = FILE_FLAG_BACKUP_SEMANTICS
        | if open_reparse_point {
            FILE_FLAG_OPEN_REPARSE_POINT
        } else {
            0
        };
    let handle = unsafe {
        // SAFETY: `encoded` is terminated and the returned handle is checked.
        CreateFileW(
            encoded.as_ptr(),
            desired_access,
            share_mode,
            std::ptr::null(),
            OPEN_EXISTING,
            flags,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error().into());
    }

    let mut information = FILE_ATTRIBUTE_TAG_INFO::default();
    let information_result = unsafe {
        // SAFETY: `handle` and the output buffer are valid.
        GetFileInformationByHandleEx(
            handle,
            FileAttributeTagInfo,
            std::ptr::from_mut(&mut information).cast(),
            size_of::<FILE_ATTRIBUTE_TAG_INFO>() as u32,
        )
    };
    if information_result == 0 || information.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        let error = if information_result == 0 {
            io::Error::last_os_error().into()
        } else {
            invalid_data(format!(
                "benchmark private directory is a reparse point: {}",
                path.display()
            ))
        };
        // SAFETY: `handle` is valid and has not transferred to `File`.
        unsafe {
            CloseHandle(handle);
        }
        return Err(error);
    }

    Ok(unsafe {
        // SAFETY: ownership of the validated handle transfers to `File`.
        fs::File::from_raw_handle(handle)
    })
}

fn reject_reparse_metadata(path: &Path) -> Result<()> {
    use std::os::windows::fs::MetadataExt as _;

    if fs::symlink_metadata(path)?.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        Err(invalid_data(format!(
            "benchmark directory ancestry is a reparse point: {}",
            path.display()
        )))
    } else {
        Ok(())
    }
}

fn verify_private_directory(
    directory: &fs::File,
    path: &Path,
    expected: &PrivateSecurity,
) -> Result<()> {
    let mut descriptor = read_security_descriptor(directory)?;
    let descriptor_pointer = descriptor.as_mut_ptr().cast::<c_void>();

    let mut control = 0;
    let mut revision = 0;
    // SAFETY: `descriptor_pointer` contains a descriptor returned by Windows.
    if unsafe { GetSecurityDescriptorControl(descriptor_pointer, &mut control, &mut revision) } == 0
    {
        return Err(io::Error::last_os_error().into());
    }
    if control & SE_DACL_PROTECTED == 0 {
        return Err(invalid_data(format!(
            "benchmark private directory DACL is not protected: {}",
            path.display()
        )));
    }

    let owner = security_descriptor_owner(descriptor_pointer)?;
    if !sid_matches(owner, &expected.owner) {
        return Err(invalid_data(format!(
            "benchmark private directory owner differs from the current user: {}",
            path.display()
        )));
    }

    let dacl = security_descriptor_dacl(descriptor_pointer)?.ok_or_else(|| {
        invalid_data(format!(
            "benchmark private directory has no DACL: {}",
            path.display()
        ))
    })?;
    verify_acl(dacl, path, expected)
}

fn verify_trusted_ancestor(
    directory: &fs::File,
    path: &Path,
    expected: &PrivateSecurity,
    role: AncestorRole,
) -> Result<()> {
    let mut descriptor = read_security_descriptor(directory)?;
    let descriptor_pointer = descriptor.as_mut_ptr().cast::<c_void>();
    let owner = security_descriptor_owner(descriptor_pointer)?;
    if !trusted_sid(owner, expected, role) {
        return Err(trusted_ancestor_error(path));
    }
    let dacl = security_descriptor_dacl(descriptor_pointer)?
        .ok_or_else(|| trusted_ancestor_error(path))?;
    verify_trusted_ancestor_acl(dacl, path, expected, role)
}

fn read_security_descriptor(directory: &fs::File) -> Result<Vec<u8>> {
    let requested = OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION;
    let mut required = 0;
    // SAFETY: the handle is valid and this probe intentionally has no output buffer.
    let probe = unsafe {
        GetKernelObjectSecurity(
            directory.as_raw_handle(),
            requested,
            std::ptr::null_mut(),
            0,
            &mut required,
        )
    };
    let probe_error = io::Error::last_os_error();
    if probe != 0
        || required == 0
        || probe_error.raw_os_error() != Some(ERROR_INSUFFICIENT_BUFFER as i32)
    {
        return Err(probe_error.into());
    }
    let mut descriptor = vec![0u8; required as usize];
    // SAFETY: `descriptor` is writable for the size requested by Windows.
    if unsafe {
        GetKernelObjectSecurity(
            directory.as_raw_handle(),
            requested,
            descriptor.as_mut_ptr().cast(),
            required,
            &mut required,
        )
    } == 0
    {
        return Err(io::Error::last_os_error().into());
    }

    Ok(descriptor)
}

fn security_descriptor_owner(descriptor: *mut c_void) -> Result<PSID> {
    let mut owner = std::ptr::null_mut();
    let mut owner_defaulted = 0;
    // SAFETY: the descriptor is valid and the output pointers are writable.
    if unsafe { GetSecurityDescriptorOwner(descriptor, &mut owner, &mut owner_defaulted) } == 0 {
        return Err(io::Error::last_os_error().into());
    }
    Ok(owner)
}

fn security_descriptor_dacl(descriptor: *mut c_void) -> Result<Option<*mut ACL>> {
    let mut dacl_present = 0;
    let mut dacl_defaulted = 0;
    let mut dacl = std::ptr::null_mut();
    // SAFETY: the descriptor is valid and the output pointers are writable.
    if unsafe {
        GetSecurityDescriptorDacl(
            descriptor,
            &mut dacl_present,
            &mut dacl,
            &mut dacl_defaulted,
        )
    } == 0
    {
        return Err(io::Error::last_os_error().into());
    }
    Ok((dacl_present != 0 && !dacl.is_null()).then_some(dacl))
}

fn verify_acl(acl: *mut ACL, path: &Path, expected: &PrivateSecurity) -> Result<()> {
    let mut information = ACL_SIZE_INFORMATION::default();
    // SAFETY: the ACL is part of the validated descriptor and output storage is sized.
    if unsafe {
        GetAclInformation(
            acl,
            std::ptr::from_mut(&mut information).cast(),
            size_of::<ACL_SIZE_INFORMATION>() as u32,
            AclSizeInformation,
        )
    } == 0
    {
        return Err(io::Error::last_os_error().into());
    }
    let expected_count = if expected.system.is_some() { 2 } else { 1 };
    if information.AceCount != expected_count {
        return Err(private_acl_error(path));
    }

    let mut owner_count = 0;
    let mut system_count = 0;
    for index in 0..information.AceCount {
        let mut ace = std::ptr::null_mut();
        // SAFETY: `index` is bounded by the ACE count reported for this ACL.
        if unsafe { GetAce(acl, index, &mut ace) } == 0 {
            return Err(io::Error::last_os_error().into());
        }
        // SAFETY: an access-allowed ACE begins with an ACE header and fixed fields.
        let header = unsafe { &*ace.cast::<ACE_HEADER>() };
        // SAFETY: the ACE type is checked before SID contents are used below.
        let allowed = unsafe { &*ace.cast::<ACCESS_ALLOWED_ACE>() };
        if header.AceType != ACCESS_ALLOWED_ACE_TYPE_VALUE
            || u32::from(header.AceFlags) != PRIVATE_ACE_FLAGS
            || allowed.Mask != FILE_ALL_ACCESS
        {
            return Err(private_acl_error(path));
        }
        let sid = std::ptr::from_ref(&allowed.SidStart)
            .cast_mut()
            .cast::<c_void>();
        if sid_matches(sid, &expected.owner) {
            owner_count += 1;
        } else if expected
            .system
            .as_ref()
            .is_some_and(|system| sid_matches(sid, system))
        {
            system_count += 1;
        } else {
            return Err(private_acl_error(path));
        }
    }

    if owner_count != 1 || system_count != usize::from(expected.system.is_some()) {
        return Err(private_acl_error(path));
    }
    Ok(())
}

fn verify_trusted_ancestor_acl(
    acl: *mut ACL,
    path: &Path,
    expected: &PrivateSecurity,
    role: AncestorRole,
) -> Result<()> {
    let mut information = ACL_SIZE_INFORMATION::default();
    // SAFETY: the ACL is part of the validated descriptor and output storage is sized.
    if unsafe {
        GetAclInformation(
            acl,
            std::ptr::from_mut(&mut information).cast(),
            size_of::<ACL_SIZE_INFORMATION>() as u32,
            AclSizeInformation,
        )
    } == 0
    {
        return Err(io::Error::last_os_error().into());
    }

    for index in 0..information.AceCount {
        let mut ace = std::ptr::null_mut();
        // SAFETY: `index` is bounded by the ACE count reported for this ACL.
        if unsafe { GetAce(acl, index, &mut ace) } == 0 {
            return Err(io::Error::last_os_error().into());
        }
        // SAFETY: each ACE begins with an ACE header.
        let header = unsafe { &*ace.cast::<ACE_HEADER>() };
        if usize::from(header.AceSize) < size_of::<ACE_HEADER>() + size_of::<u32>() {
            return Err(trusted_ancestor_error(path));
        }
        // SAFETY: every access-control ACE stores its access mask directly
        // after the header; the size check above covers this read.
        let mask = unsafe {
            ace.cast::<u8>()
                .add(size_of::<ACE_HEADER>())
                .cast::<u32>()
                .read_unaligned()
        };
        let flags = u32::from(header.AceFlags);
        let mutates_current =
            flags & INHERIT_ONLY_ACE == 0 && mask & RETAINED_PATH_MUTATION_ACCESS != 0;
        // Root traversal does not create inheriting children. Each ordinary
        // descendant is checked separately; a fixture root keeps the full check.
        let check_descendants = !matches!(role, AncestorRole::VolumeRoot);
        let mutates_descendant_directory = check_descendants
            && flags & CONTAINER_INHERIT_ACE != 0
            && mask & DESCENDANT_DIRECTORY_MUTATION_ACCESS != 0;
        let mutates_descendant_file = check_descendants
            && flags & OBJECT_INHERIT_ACE != 0
            && mask & DESCENDANT_FILE_MUTATION_ACCESS != 0;
        if !(mutates_current || mutates_descendant_directory || mutates_descendant_file)
            || denied_ace(header.AceType)
        {
            continue;
        }
        if header.AceType != ACCESS_ALLOWED_ACE_TYPE_VALUE
            || usize::from(header.AceSize) < size_of::<ACCESS_ALLOWED_ACE>()
        {
            return Err(trusted_ancestor_error(path));
        }
        // SAFETY: the ACE type and minimum size were checked above.
        let allowed = unsafe { &*ace.cast::<ACCESS_ALLOWED_ACE>() };
        let sid = std::ptr::from_ref(&allowed.SidStart)
            .cast_mut()
            .cast::<c_void>();
        if !trusted_sid(sid, expected, role) {
            return Err(trusted_ancestor_error(path));
        }
    }
    Ok(())
}

fn denied_ace(ace_type: u8) -> bool {
    matches!(
        ace_type,
        ACCESS_DENIED_ACE_TYPE_VALUE
            | ACCESS_DENIED_OBJECT_ACE_TYPE_VALUE
            | ACCESS_DENIED_CALLBACK_ACE_TYPE_VALUE
            | ACCESS_DENIED_CALLBACK_OBJECT_ACE_TYPE_VALUE
    )
}

fn trusted_sid(sid: PSID, expected: &PrivateSecurity, role: AncestorRole) -> bool {
    sid_matches(sid, &expected.owner)
        || expected
            .system
            .as_ref()
            .is_some_and(|system| sid_matches(sid, system))
        || sid_matches(sid, &expected.administrators)
        || (!matches!(role, AncestorRole::Directory) && sid_matches(sid, &TRUSTED_INSTALLER_SID))
}

fn private_acl_error(path: &Path) -> Box<dyn std::error::Error + Send + Sync> {
    invalid_data(format!(
        "benchmark private directory ACL is not limited to the current user and SYSTEM: {}",
        path.display()
    ))
}

fn trusted_ancestor_error(path: &Path) -> Box<dyn std::error::Error + Send + Sync> {
    invalid_data(format!(
        "benchmark ancestry permits replacement or reparse mutation outside the current user, SYSTEM, or Administrators: {}",
        path.display()
    ))
}

fn current_user_sid() -> Result<Vec<u8>> {
    let mut token = std::ptr::null_mut();
    // SAFETY: the output pointer is writable and the pseudo-process handle is valid.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error().into());
    }
    let token = OwnedHandle(token);
    let mut required = 0;
    // SAFETY: this size probe intentionally supplies no output buffer.
    unsafe {
        GetTokenInformation(token.0, TokenUser, std::ptr::null_mut(), 0, &mut required);
    }
    if required == 0 {
        return Err(io::Error::last_os_error().into());
    }
    let mut information = vec![0u8; required as usize];
    // SAFETY: `information` is writable for the size returned by the probe.
    if unsafe {
        GetTokenInformation(
            token.0,
            TokenUser,
            information.as_mut_ptr().cast(),
            required,
            &mut required,
        )
    } == 0
    {
        return Err(io::Error::last_os_error().into());
    }
    // SAFETY: Windows populated the buffer with a TOKEN_USER value.
    let token_user = unsafe { information.as_ptr().cast::<TOKEN_USER>().read_unaligned() };
    copy_sid(token_user.User.Sid)
}

fn well_known_sid(kind: i32) -> Result<Vec<u8>> {
    let mut required = 0;
    // SAFETY: this size probe intentionally supplies no SID buffer.
    unsafe {
        CreateWellKnownSid(
            kind,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut required,
        );
    }
    if required == 0 {
        return Err(io::Error::last_os_error().into());
    }
    let mut sid = vec![0u8; required as usize];
    // SAFETY: `sid` is writable for the size returned by the probe.
    if unsafe {
        CreateWellKnownSid(
            kind,
            std::ptr::null_mut(),
            sid.as_mut_ptr().cast(),
            &mut required,
        )
    } == 0
    {
        return Err(io::Error::last_os_error().into());
    }
    Ok(sid)
}

fn copy_sid(sid: PSID) -> Result<Vec<u8>> {
    // SAFETY: validation only reads the SID supplied by Windows.
    if sid.is_null() || unsafe { IsValidSid(sid) } == 0 {
        return Err(invalid_data("Windows returned an invalid user SID"));
    }
    // SAFETY: the SID was validated immediately above.
    let length = unsafe { GetLengthSid(sid) } as usize;
    // SAFETY: a valid SID is readable for the length reported by Windows.
    Ok(unsafe { std::slice::from_raw_parts(sid.cast::<u8>(), length) }.to_vec())
}

fn sid_bytes_equal(left: &[u8], right: &[u8]) -> bool {
    sid_matches(left.as_ptr().cast_mut().cast(), right)
}

fn sid_matches(candidate: PSID, expected: &[u8]) -> bool {
    // SAFETY: `candidate` is checked and `expected` owns a complete SID buffer.
    !candidate.is_null() && unsafe { EqualSid(candidate, expected.as_ptr().cast_mut().cast()) } != 0
}

fn encode_path(path: &Path) -> Vec<u16> {
    path.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

#[cfg(test)]
pub(super) fn private_test_tempdir() -> tempfile::TempDir {
    // GitHub-hosted runners redirect TEMP beneath the shared D:\a tree. Keep
    // security fixtures under a per-user root that satisfies the real policy.
    let base = ["USERPROFILE", "LOCALAPPDATA"]
        .into_iter()
        .filter_map(std::env::var_os)
        .map(std::path::PathBuf::from)
        .find(|path| path.is_absolute() && path.is_dir())
        .expect("Windows tests require LOCALAPPDATA or USERPROFILE");
    let parent = base.join("fs2-dev-test-fixtures");
    drop(create_or_open_trusted_directory_ancestry(&parent).unwrap());

    let temporary = tempfile::Builder::new()
        .prefix("fs2-dev-")
        .tempdir_in(parent)
        .unwrap();
    drop(harden_new_private_directory(temporary.path()).unwrap());
    temporary
}

struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        // SAFETY: this type exclusively owns the token handle.
        unsafe {
            CloseHandle(self.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    static MAPPED_DRIVE_TEST: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn private_tempdir() -> tempfile::TempDir {
        private_test_tempdir()
    }

    #[test]
    fn unc_volume_is_rejected_before_drive_type_lookup() {
        let mut queried = false;
        let accepted = is_local_fixed_drive_with(Path::new(r"\\server\share\benchmark"), |_| {
            queried = true;
            DRIVE_FIXED
        });

        assert!(!accepted);
        assert!(!queried);
        assert!(
            require_local_fixed_volume(Path::new(r"\\server\share\benchmark"), "strict fixture")
                .unwrap_err()
                .to_string()
                .contains("local fixed Windows volume")
        );
    }

    #[test]
    fn mapped_remote_drive_is_rejected() {
        assert!(!is_local_fixed_drive_with(
            Path::new(r"Z:\benchmark"),
            |_| 4
        ));
    }

    #[test]
    fn local_fixed_drive_is_accepted() {
        assert!(is_local_fixed_drive_with(
            Path::new(r"C:\benchmark"),
            |_| DRIVE_FIXED
        ));
    }

    #[test]
    fn verbatim_disk_is_accepted_only_as_canonical_output() {
        let path = Path::new(r"\\?\C:\benchmark");

        assert!(!is_local_fixed_drive_with(path, |_| DRIVE_FIXED));
        assert!(is_local_fixed_drive_with_options(path, true, |_| {
            DRIVE_FIXED
        }));
        assert!(reject_ambiguous_path(path).is_err());
    }

    #[test]
    fn creates_missing_ancestry_with_private_directories() {
        let root = private_tempdir();
        let nested = root.path().join("first").join("second");

        let held = create_or_open_trusted_directory_ancestry(&nested).unwrap();

        assert!(nested.is_dir());
        assert!(!held.is_empty());
    }

    #[test]
    fn current_user_ownership_does_not_bypass_shared_destructive_acl() {
        let root = private_tempdir();
        let status = Command::new("icacls")
            .arg(root.path())
            .args(["/grant", "*S-1-5-11:(OI)(CI)(F)"])
            .status()
            .unwrap();
        assert!(status.success());

        let error = open_trusted_ancestor(root.path()).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("replacement or reparse mutation")
        );
    }

    #[test]
    fn current_user_ownership_does_not_bypass_shared_reparse_acl() {
        let root = private_tempdir();
        let status = Command::new("icacls")
            .arg(root.path())
            .args(["/grant", "*S-1-5-11:(W)"])
            .status()
            .unwrap();
        assert!(status.success());

        let error = open_trusted_ancestor(root.path()).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("replacement or reparse mutation")
        );
    }

    #[test]
    fn inheritable_append_to_descendant_files_is_rejected() {
        let root = private_tempdir();
        let status = Command::new("icacls")
            .arg(root.path())
            .args(["/grant", "*S-1-5-11:(OI)(IO)(AD)"])
            .status()
            .unwrap();
        assert!(status.success());

        let error = open_trusted_ancestor(root.path()).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("replacement or reparse mutation")
        );
    }

    #[test]
    fn inheritable_descendant_directory_mutation_is_rejected() {
        let root = private_tempdir();
        let status = Command::new("icacls")
            .arg(root.path())
            .args(["/grant", "*S-1-5-11:(CI)(IO)(AD)"])
            .status()
            .unwrap();
        assert!(status.success());

        let error = open_trusted_ancestor(root.path()).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("replacement or reparse mutation")
        );
    }

    #[test]
    fn isolated_cargo_directory_hides_physical_ancestor_configuration() {
        let _serial = MAPPED_DRIVE_TEST.lock().unwrap();
        let root = private_tempdir();
        let config = root.path().join(".cargo");
        fs::create_dir(&config).unwrap();
        fs::write(config.join("config.toml"), "[build\n").unwrap();
        let isolated = isolated_cargo_working_directory(root.path()).unwrap();
        let physical_root = isolated.path().canonicalize().unwrap();
        fs::write(
            physical_root.join("Cargo.toml"),
            "[package]\nname = \"cargo-config-fence\"\nversion = \"0.0.0\"\nedition = \"2021\"\n[lib]\npath = \"lib.rs\"\n",
        )
        .unwrap();
        fs::write(physical_root.join("lib.rs"), "").unwrap();
        let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
        let manifest = physical_root.join("Cargo.toml");
        let metadata = |working_directory: &Path| {
            Command::new(&cargo)
                .current_dir(working_directory)
                .args([
                    "metadata",
                    "--format-version",
                    "1",
                    "--no-deps",
                    "--offline",
                    "--manifest-path",
                ])
                .arg(&manifest)
                .output()
                .unwrap()
        };

        assert!(!metadata(root.path()).status.success());
        let isolated_output = metadata(isolated.path());
        assert!(
            isolated_output.status.success(),
            "{}",
            String::from_utf8_lossy(&isolated_output.stderr)
        );
    }

    #[test]
    fn add_subdirectory_only_ancestry_remains_compatible() {
        let root = private_tempdir();
        let status = Command::new("icacls")
            .arg(root.path())
            .args(["/grant", "*S-1-5-11:(AD)"])
            .status()
            .unwrap();
        assert!(status.success());

        open_trusted_ancestor(root.path()).unwrap();
    }

    #[test]
    fn volume_root_authority_is_checked_without_weakening_fixture_rules() {
        let _serial = MAPPED_DRIVE_TEST.lock().unwrap();
        let root = private_tempdir();
        let isolated = isolated_cargo_working_directory(root.path()).unwrap();
        let evidence = isolated.path().join("evidence");
        drop(create_or_open_private_directory(&evidence).unwrap());
        let grant = |rights: &str| {
            let status = Command::new("icacls")
                .arg(isolated.path())
                .arg("/grant:r")
                .arg(format!("*S-1-5-11:{rights}"))
                .status()
                .unwrap();
            assert!(status.success());
        };

        for rights in ["(DC,AD)", "(WDAC)", "(WO)", "(W)"] {
            grant(rights);
            assert!(guard_directory_ancestry(&evidence).is_err(), "{rights}");
            assert!(guard_publication_directory_ancestry(&evidence).is_err());
            let verbatim = PathBuf::from(format!(r"\\?\{}", evidence.display()));
            assert!(guard_canonical_directory_ancestry(&verbatim).is_err());
            assert!(guard_fixture_directory_ancestry(isolated.path()).is_err());
            let uncreated = isolated.path().join("not-created");
            assert!(create_or_open_trusted_directory_ancestry(&uncreated).is_err());
            assert!(!uncreated.exists());
        }

        grant("(AD)");
        drop(guard_publication_directory_ancestry(isolated.path()).unwrap());
        drop(guard_directory_ancestry(&evidence).unwrap());
        drop(guard_fixture_directory_ancestry(isolated.path()).unwrap());

        grant("(OI)(CI)(IO)(M)");
        // Model a real root's inherit-only grants directly. A directory-backed
        // alias must instead satisfy the ordinary physical-ancestor policy.
        drop(open_ancestor(isolated.path(), AncestorRole::VolumeRoot).unwrap());
        assert!(guard_publication_directory_ancestry(isolated.path()).is_err());
        assert!(guard_directory_ancestry(&evidence).is_err());
        assert!(guard_fixture_directory_ancestry(isolated.path()).is_err());
        assert!(open_trusted_ancestor(isolated.path()).is_err());
    }

    #[test]
    fn directory_backed_drive_checks_and_retains_its_physical_ancestry() {
        let _serial = MAPPED_DRIVE_TEST.lock().unwrap();
        let root = private_tempdir();
        let shared = root.path().join("shared");
        drop(create_or_open_private_directory(&shared).unwrap());
        let alias = isolated_cargo_working_directory(&shared).unwrap();
        let evidence = alias.path().join("evidence");
        drop(create_or_open_private_directory(&evidence).unwrap());
        assert!(
            final_directory_path(&alias._directory)
                .unwrap()
                .parent()
                .is_some()
        );
        drop(guard_publication_directory_ancestry(&evidence).unwrap());
        drop(guard_fixture_directory_ancestry(&evidence).unwrap());

        let status = Command::new("icacls")
            .arg(&shared)
            .args(["/grant", "*S-1-5-11:(DC,AD)"])
            .status()
            .unwrap();
        assert!(status.success());
        assert!(guard_publication_directory_ancestry(&evidence).is_err());
        assert!(guard_directory_ancestry(&evidence).is_err());
        assert!(guard_fixture_directory_ancestry(&evidence).is_err());
        let verbatim = PathBuf::from(format!(r"\\?\{}", evidence.display()));
        assert!(guard_canonical_directory_ancestry(&verbatim).is_err());
        assert!(create_or_open_trusted_directory_ancestry(&evidence).is_err());
    }

    #[test]
    fn trusted_installer_exception_is_exact_and_root_only() {
        let security = PrivateSecurity::new().unwrap();
        let sid = TRUSTED_INSTALLER_SID.as_ptr().cast_mut().cast();
        assert!(trusted_sid(sid, &security, AncestorRole::VolumeRoot));
        assert!(trusted_sid(sid, &security, AncestorRole::FixtureRoot));
        assert!(!trusted_sid(sid, &security, AncestorRole::Directory));
        let mut other_service = TRUSTED_INSTALLER_SID;
        other_service[31] ^= 1;
        assert!(!trusted_sid(
            other_service.as_mut_ptr().cast(),
            &security,
            AncestorRole::VolumeRoot,
        ));
    }
}
