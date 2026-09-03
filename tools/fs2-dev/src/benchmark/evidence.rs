use std::env;
use std::ffi::{OsStr, OsString};
#[cfg(not(windows))]
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Serialize, Serializer};
use sha2::{Digest, Sha256};

use crate::process;
use crate::{Result, invalid_data, lower_hex};

#[derive(Clone, Debug, Serialize)]
pub(super) struct DiskSnapshot {
    #[serde(serialize_with = "serialize_disk_path")]
    path: PathBuf,
    filesystem_id: Option<String>,
    free_space: u64,
    available_space: u64,
    total_space: u64,
    allocation_granularity: u64,
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct EnvironmentSnapshot {
    captured_unix_ms: u128,
    host_os: &'static str,
    host_arch: &'static str,
    hostname: Option<String>,
    cpu_identifier: Option<String>,
    logical_processors: Option<String>,
    process_affinity: Option<String>,
    #[serde(serialize_with = "serialize_power_plan")]
    power_plan: Option<String>,
    #[serde(serialize_with = "serialize_rustc_host")]
    rustc_host: String,
    cargo_build_target: Option<String>,
    #[serde(serialize_with = "serialize_rustc_verbose_version")]
    rustc_verbose_version: String,
    #[serde(serialize_with = "serialize_cargo_verbose_version")]
    cargo_verbose_version: String,
    #[serde(skip_serializing)]
    inherited_environment: EnvironmentBinding,
    disk: DiskSnapshot,
    observation_failures: Vec<String>,
}

#[derive(Clone, Debug)]
struct EnvironmentBinding {
    names: Vec<String>,
    sha256: String,
}

impl EnvironmentSnapshot {
    pub(super) fn capture(disk_path: &Path) -> Result<Self> {
        let mut rustc = rustc_command();
        rustc.arg("-vV");
        let rustc_verbose_version = command_text(rustc, "capture rustc version")?;
        let rustc_host = rustc_verbose_version
            .lines()
            .find_map(|line| line.strip_prefix("host: "))
            .map(str::to_owned)
            .ok_or_else(|| invalid_data("rustc -vV did not report a host target"))?;
        let mut cargo = process::cargo();
        cargo.arg("-vV");
        let cargo_verbose_version = command_text(cargo, "capture Cargo version")?;
        let inherited_environment = inherited_environment_binding(env::vars_os())?;

        let cpu_identifier = cpu_identifier();
        let logical_processors = normalized_processor_count(env::var_os("NUMBER_OF_PROCESSORS"))
            .or_else(|| {
                std::thread::available_parallelism()
                    .ok()
                    .map(|count| count.get().to_string())
            });
        let process_affinity = process_affinity();
        let power_plan = power_plan();
        let disk = DiskSnapshot::capture(disk_path)?;
        let mut observation_failures = Vec::new();
        if cpu_identifier.is_none() {
            observation_failures.push("CPU identity unavailable".to_owned());
        }
        if logical_processors.is_none() {
            observation_failures.push("logical processor count unavailable".to_owned());
        }
        if disk.filesystem_id.is_none() {
            observation_failures.push("filesystem identity unavailable".to_owned());
        }
        #[cfg(any(target_os = "linux", windows))]
        if process_affinity.is_none() {
            observation_failures.push("process affinity unavailable".to_owned());
        }
        #[cfg(windows)]
        if power_plan.is_none() {
            observation_failures.push("Windows power plan unavailable".to_owned());
        }

        Ok(Self {
            captured_unix_ms: SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis(),
            host_os: env::consts::OS,
            host_arch: env::consts::ARCH,
            hostname: presence_marker(
                env::var_os("COMPUTERNAME").or_else(|| env::var_os("HOSTNAME")),
                "<redacted>",
            ),
            cpu_identifier,
            logical_processors,
            process_affinity,
            power_plan,
            rustc_host,
            cargo_build_target: presence_marker(env::var_os("CARGO_BUILD_TARGET"), "<configured>"),
            rustc_verbose_version,
            cargo_verbose_version,
            inherited_environment,
            disk,
            observation_failures,
        })
    }

    pub(super) fn strict_failure_reason(&self) -> Option<String> {
        (!self.observation_failures.is_empty()).then(|| {
            format!(
                "required environment observations failed: {}",
                self.observation_failures.join("; ")
            )
        })
    }

    pub(super) fn drift_reasons(&self, completed: &Self) -> Vec<String> {
        let mut reasons = Vec::new();
        let mut compare = |name: &str, unchanged: bool| {
            if !unchanged {
                reasons.push(format!("environment changed during measurement: {name}"));
            }
        };
        compare("host OS", self.host_os == completed.host_os);
        compare("host architecture", self.host_arch == completed.host_arch);
        compare("host name", self.hostname == completed.hostname);
        compare(
            "CPU identifier",
            self.cpu_identifier == completed.cpu_identifier,
        );
        compare(
            "logical processor count",
            self.logical_processors == completed.logical_processors,
        );
        compare(
            "process affinity",
            self.process_affinity == completed.process_affinity,
        );
        compare("power plan", self.power_plan == completed.power_plan);
        compare("rustc host", self.rustc_host == completed.rustc_host);
        compare(
            "Cargo build target",
            self.cargo_build_target == completed.cargo_build_target,
        );
        compare(
            "rustc version",
            self.rustc_verbose_version == completed.rustc_verbose_version,
        );
        compare(
            "Cargo version",
            self.cargo_verbose_version == completed.cargo_verbose_version,
        );
        compare(
            "inherited environment variable names",
            self.inherited_environment.names == completed.inherited_environment.names,
        );
        compare(
            "inherited environment digest",
            self.inherited_environment.sha256 == completed.inherited_environment.sha256,
        );
        compare(
            "filesystem identity",
            self.disk.filesystem_id == completed.disk.filesystem_id,
        );
        compare(
            "environment observation failures",
            self.observation_failures == completed.observation_failures,
        );
        reasons
    }
}

fn inherited_environment_binding(
    variables: impl IntoIterator<Item = (OsString, OsString)>,
) -> Result<EnvironmentBinding> {
    let mut entries = variables
        .into_iter()
        .map(|(name, value)| {
            let displayed_name = process::display_os(&name);
            (
                native_environment_bytes(&name),
                native_environment_bytes(&value),
                displayed_name,
            )
        })
        .collect::<Vec<_>>();
    entries.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));

    let mut names = entries
        .iter()
        .map(|(_, _, displayed_name)| displayed_name.clone())
        .collect::<Vec<_>>();
    names.sort();

    let mut digest = Sha256::new();
    digest.update(b"fs2-inherited-environment-v1\0");
    update_environment_digest_component(&mut digest, env::consts::OS.as_bytes())?;
    digest.update(u64::try_from(entries.len())?.to_le_bytes());
    for (name, value, _) in entries {
        update_environment_digest_component(&mut digest, &name)?;
        update_environment_digest_component(&mut digest, &value)?;
    }

    Ok(EnvironmentBinding {
        names,
        sha256: lower_hex(digest.finalize()),
    })
}

fn presence_marker(value: Option<OsString>, marker: &str) -> Option<String> {
    value.map(|_| marker.to_owned())
}

fn serialize_disk_path<S>(_path: &Path, serializer: S) -> std::result::Result<S::Ok, S::Error>
where
    S: Serializer,
{
    serializer.serialize_str("fixture-filesystem")
}

fn serialize_power_plan<S>(
    value: &Option<String>,
    serializer: S,
) -> std::result::Result<S::Ok, S::Error>
where
    S: Serializer,
{
    value
        .as_deref()
        .map(normalized_power_plan)
        .serialize(serializer)
}

fn normalized_power_plan(value: &str) -> String {
    value
        .split_whitespace()
        .map(|word| {
            word.trim_matches(|character: char| !character.is_ascii_hexdigit() && character != '-')
        })
        .find(|word| is_guid(word))
        .map(|guid| format!("guid:{}", guid.to_ascii_lowercase()))
        .unwrap_or_else(|| "<custom>".to_owned())
}

fn is_guid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

fn serialize_rustc_host<S>(value: &str, serializer: S) -> std::result::Result<S::Ok, S::Error>
where
    S: Serializer,
{
    serializer.serialize_str(normalized_host_target(value).unwrap_or("<custom>"))
}

fn serialize_rustc_verbose_version<S>(
    value: &str,
    serializer: S,
) -> std::result::Result<S::Ok, S::Error>
where
    S: Serializer,
{
    serializer.serialize_str(&normalized_tool_facts(value, "rustc"))
}

fn serialize_cargo_verbose_version<S>(
    value: &str,
    serializer: S,
) -> std::result::Result<S::Ok, S::Error>
where
    S: Serializer,
{
    serializer.serialize_str(&normalized_tool_facts(value, "cargo"))
}

fn normalized_tool_facts(value: &str, tool: &str) -> String {
    let version = value
        .lines()
        .next()
        .and_then(|line| line.strip_prefix(tool))
        .and_then(|line| line.strip_prefix(' '))
        .and_then(|line| line.split_whitespace().next())
        .filter(|version| is_release(version));
    let Some(version) = version else {
        return format!("{tool} <custom>");
    };

    let mut facts = vec![format!("{tool} {version}")];
    if let Some(commit_hash) = fact(value, "commit-hash: ")
        .filter(|hash| hash.len() == 40 && hash.bytes().all(|byte| byte.is_ascii_hexdigit()))
    {
        facts.push(format!("commit-hash: {}", commit_hash.to_ascii_lowercase()));
    }
    if let Some(commit_date) = fact(value, "commit-date: ").filter(|date| is_date(date)) {
        facts.push(format!("commit-date: {commit_date}"));
    }
    if let Some(host) = fact(value, "host: ").and_then(normalized_host_target) {
        facts.push(format!("host: {host}"));
    }
    if let Some(release) = fact(value, "release: ").filter(|release| is_release(release)) {
        facts.push(format!("release: {release}"));
    }
    if let Some(llvm) = fact(value, "LLVM version: ").filter(|llvm| {
        !llvm.is_empty()
            && llvm.len() <= 32
            && llvm
                .bytes()
                .all(|byte| byte.is_ascii_digit() || byte == b'.')
    }) {
        facts.push(format!("LLVM version: {llvm}"));
    }
    facts.join("\n")
}

fn fact<'a>(value: &'a str, prefix: &str) -> Option<&'a str> {
    value
        .lines()
        .find_map(|line| line.strip_prefix(prefix))
        .map(str::trim)
}

fn is_release(value: &str) -> bool {
    value.len() <= 64
        && value
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_digit())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_' | b'+'))
}

fn is_date(value: &str) -> bool {
    value.len() == 10
        && value.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 4 | 7) {
                byte == b'-'
            } else {
                byte.is_ascii_digit()
            }
        })
}

fn normalized_host_target(value: &str) -> Option<&str> {
    matches!(
        value,
        "aarch64-apple-darwin"
            | "aarch64-pc-windows-msvc"
            | "aarch64-unknown-freebsd"
            | "aarch64-unknown-linux-gnu"
            | "aarch64-unknown-linux-musl"
            | "aarch64-unknown-netbsd"
            | "aarch64-unknown-openbsd"
            | "i686-pc-windows-gnu"
            | "i686-pc-windows-msvc"
            | "i686-unknown-linux-gnu"
            | "loongarch64-unknown-linux-gnu"
            | "powerpc64le-unknown-linux-gnu"
            | "riscv64gc-unknown-linux-gnu"
            | "s390x-unknown-linux-gnu"
            | "x86_64-apple-darwin"
            | "x86_64-pc-windows-gnu"
            | "x86_64-pc-windows-msvc"
            | "x86_64-sun-solaris"
            | "x86_64-unknown-freebsd"
            | "x86_64-unknown-illumos"
            | "x86_64-unknown-linux-gnu"
            | "x86_64-unknown-linux-musl"
            | "x86_64-unknown-netbsd"
            | "x86_64-unknown-openbsd"
    )
    .then_some(value)
}

fn normalized_processor_count(value: Option<OsString>) -> Option<String> {
    value
        .and_then(|value| value.into_string().ok())
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|count| *count > 0)
        .map(|count| count.to_string())
}

fn update_environment_digest_component(digest: &mut Sha256, value: &[u8]) -> Result<()> {
    digest.update(u64::try_from(value.len())?.to_le_bytes());
    digest.update(value);
    Ok(())
}

#[cfg(unix)]
fn native_environment_bytes(value: &OsStr) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt as _;

    value.as_bytes().to_vec()
}

#[cfg(windows)]
fn native_environment_bytes(value: &OsStr) -> Vec<u8> {
    use std::os::windows::ffi::OsStrExt as _;

    value
        .encode_wide()
        .flat_map(|unit| unit.to_le_bytes())
        .collect()
}

#[cfg(not(any(unix, windows)))]
fn native_environment_bytes(value: &OsStr) -> Vec<u8> {
    value.as_encoded_bytes().to_vec()
}

impl DiskSnapshot {
    pub(super) fn capture(path: &Path) -> Result<Self> {
        let path = path.canonicalize()?;
        let stats = fs2::statvfs(&path)?;
        Ok(Self {
            filesystem_id: filesystem_id(&path),
            path,
            free_space: stats.free_space(),
            available_space: stats.available_space(),
            total_space: stats.total_space(),
            allocation_granularity: stats.allocation_granularity(),
        })
    }
}

#[cfg(unix)]
fn filesystem_id(path: &Path) -> Option<String> {
    use std::os::unix::fs::MetadataExt as _;

    fs::metadata(path)
        .ok()
        .map(|metadata| metadata.dev().to_string())
}

#[cfg(windows)]
fn filesystem_id(path: &Path) -> Option<String> {
    use std::os::windows::ffi::OsStrExt as _;
    use std::ptr::null_mut;

    use windows_sys::Win32::Storage::FileSystem::{GetVolumeInformationW, GetVolumePathNameW};

    let mut encoded_path: Vec<_> = path.as_os_str().encode_wide().collect();
    encoded_path.push(0);
    let mut volume_path = vec![0u16; 32_768];
    // SAFETY: both buffers are writable, nul-terminated UTF-16 buffers with the
    // lengths passed to the Windows APIs.
    if unsafe {
        GetVolumePathNameW(
            encoded_path.as_ptr(),
            volume_path.as_mut_ptr(),
            volume_path.len() as u32,
        )
    } == 0
    {
        return None;
    }

    let mut serial = 0;
    let mut maximum_component_length = 0;
    let mut filesystem_flags = 0;
    // SAFETY: volume_path was initialized by GetVolumePathNameW and the output
    // pointers refer to live u32 values. Optional name buffers are null.
    if unsafe {
        GetVolumeInformationW(
            volume_path.as_ptr(),
            null_mut(),
            0,
            &mut serial,
            &mut maximum_component_length,
            &mut filesystem_flags,
            null_mut(),
            0,
        )
    } == 0
    {
        None
    } else {
        Some(format!("{serial:08x}"))
    }
}

#[cfg(not(any(unix, windows)))]
fn filesystem_id(_path: &Path) -> Option<String> {
    None
}

#[cfg(target_os = "linux")]
fn process_affinity() -> Option<String> {
    fs::read_to_string("/proc/self/status")
        .ok()?
        .lines()
        .find_map(|line| line.strip_prefix("Cpus_allowed_list:\t").map(str::to_owned))
}

#[cfg(windows)]
fn process_affinity() -> Option<String> {
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetProcessAffinityMask};

    let mut process_mask = 0usize;
    let mut system_mask = 0usize;
    // SAFETY: the pseudo-handle is valid in this process and both output
    // pointers refer to initialized writable values.
    let succeeded =
        unsafe { GetProcessAffinityMask(GetCurrentProcess(), &mut process_mask, &mut system_mask) };
    (succeeded != 0).then(|| format!("{process_mask:x}/{system_mask:x}"))
}

#[cfg(not(any(target_os = "linux", windows)))]
fn process_affinity() -> Option<String> {
    None
}

#[cfg(windows)]
fn power_plan() -> Option<String> {
    let mut command = Command::new("powercfg");
    command.arg("/getactivescheme");
    process::capture(&mut command, "capture active Windows power plan")
        .ok()
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|output| output.trim().to_owned())
        .filter(|output| !output.is_empty())
}

#[cfg(not(windows))]
fn power_plan() -> Option<String> {
    None
}

#[cfg(windows)]
fn cpu_identifier() -> Option<String> {
    Some(format!("architecture:{}", env::consts::ARCH))
}

#[cfg(not(windows))]
fn cpu_identifier() -> Option<String> {
    let linux_identifier = fs::read_to_string("/proc/cpuinfo")
        .ok()
        .and_then(|contents| {
            contents.lines().find_map(|line| {
                line.split_once(':')
                    .filter(|(name, _)| matches!(name.trim(), "model name" | "Hardware"))
                    .map(|(_, value)| value.trim().to_owned())
            })
        });
    if linux_identifier.is_some() {
        return linux_identifier;
    }
    #[cfg(target_os = "macos")]
    {
        let mut command = Command::new("sysctl");
        command.args(["-n", "machdep.cpu.brand_string"]);
        process::capture(&mut command, "capture macOS CPU identity")
            .ok()
            .and_then(|output| String::from_utf8(output.stdout).ok())
            .map(|output| output.trim().to_owned())
            .filter(|output| !output.is_empty())
    }
    #[cfg(not(target_os = "macos"))]
    None
}

fn rustc_command() -> Command {
    Command::new(env::var_os("RUSTC").unwrap_or_else(|| "rustc".into()))
}

fn command_text(mut command: Command, label: &str) -> Result<String> {
    let output = process::capture(&mut command, label)?;
    Ok(String::from_utf8(output.stdout)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn binding(entries: &[(&str, &str)]) -> (Vec<String>, String) {
        let binding = inherited_environment_binding(
            entries
                .iter()
                .map(|(name, value)| (OsString::from(name), OsString::from(value))),
        )
        .expect("environment binding should succeed");
        (binding.names, binding.sha256)
    }

    fn snapshot(inherited_environment: EnvironmentBinding) -> EnvironmentSnapshot {
        EnvironmentSnapshot {
            captured_unix_ms: 1,
            host_os: "test-os",
            host_arch: "test-arch",
            hostname: Some("<redacted>".to_owned()),
            cpu_identifier: Some("test-cpu".to_owned()),
            logical_processors: Some("4".to_owned()),
            process_affinity: Some("f/f".to_owned()),
            power_plan: Some("test-plan".to_owned()),
            rustc_host: "test-host".to_owned(),
            cargo_build_target: Some("<configured>".to_owned()),
            rustc_verbose_version: "rustc test".to_owned(),
            cargo_verbose_version: "cargo test".to_owned(),
            inherited_environment,
            disk: DiskSnapshot {
                path: PathBuf::from("fixture"),
                filesystem_id: Some("filesystem".to_owned()),
                free_space: 1,
                available_space: 1,
                total_space: 2,
                allocation_granularity: 4_096,
            },
            observation_failures: Vec::new(),
        }
    }

    #[test]
    fn environment_binding_is_order_independent_and_records_only_sorted_names() {
        let (names, digest) = binding(&[("Z_VAR", "secret-z"), ("A_VAR", "secret-a")]);
        let (reordered_names, reordered_digest) =
            binding(&[("A_VAR", "secret-a"), ("Z_VAR", "secret-z")]);

        assert_eq!(names, vec!["A_VAR".to_owned(), "Z_VAR".to_owned()]);
        assert_eq!(names, reordered_names);
        assert_eq!(digest, reordered_digest);
        assert!(!names.iter().any(|name| name.contains("secret")));
    }

    #[test]
    fn environment_binding_detects_name_and_value_changes() {
        let (_, original) = binding(&[("A_VAR", "value")]);
        let (_, renamed) = binding(&[("B_VAR", "value")]);
        let (_, changed_value) = binding(&[("A_VAR", "other")]);

        assert_ne!(original, renamed);
        assert_ne!(original, changed_value);
    }

    #[test]
    fn report_serialization_omits_inherited_environment_values_and_binding() {
        let snapshot = snapshot(
            inherited_environment_binding([
                (
                    OsString::from("CC_SECRET"),
                    OsString::from("sentinel-cc-secret"),
                ),
                (OsString::from("LOW_ENTROPY_SECRET"), OsString::from("1234")),
            ])
            .unwrap(),
        );

        let json = serde_json::to_string(&snapshot).unwrap();

        assert!(!json.contains("CC_SECRET"));
        assert!(!json.contains("sentinel-cc-secret"));
        assert!(!json.contains("LOW_ENTROPY_SECRET"));
        assert!(!json.contains("1234"));
        assert!(!json.contains("inherited_environment"));
        assert!(!json.contains("sha256"));
        assert!(json.contains("\"cargo_build_target\":\"<configured>\""));
    }

    #[test]
    fn private_environment_drift_state_does_not_change_serialized_evidence() {
        let initial = snapshot(
            inherited_environment_binding([(OsString::from("A"), OsString::from("secret-a"))])
                .unwrap(),
        );
        let completed = snapshot(
            inherited_environment_binding([(OsString::from("A"), OsString::from("secret-b"))])
                .unwrap(),
        );

        assert_eq!(
            serde_json::to_string(&initial).unwrap(),
            serde_json::to_string(&completed).unwrap()
        );
        assert!(
            initial
                .drift_reasons(&completed)
                .iter()
                .any(|reason| reason.contains("inherited environment digest"))
        );
    }

    #[test]
    fn explicit_environment_values_are_normalized_or_redacted() {
        let sentinel = OsString::from("sentinel-explicit-secret");

        assert_eq!(
            presence_marker(Some(sentinel.clone()), "<configured>"),
            Some("<configured>".to_owned())
        );
        assert_eq!(normalized_processor_count(Some(sentinel)), None);
        assert_eq!(
            normalized_processor_count(Some(OsString::from("16"))),
            Some("16".to_owned())
        );
    }

    #[test]
    fn report_serialization_omits_host_paths_and_custom_observation_text() {
        let mut snapshot = snapshot(inherited_environment_binding([]).unwrap());
        snapshot.disk.path =
            PathBuf::from(r"C:\Users\sentinel-user\private-fixture\benchmark-input");
        snapshot.power_plan = Some(
            "Power Scheme GUID: 381b4222-f694-41f0-9685-ff5bb260df2e (sentinel-private-plan)"
                .to_owned(),
        );
        snapshot.rustc_host = "sentinel-private-host".to_owned();
        snapshot.rustc_verbose_version = concat!(
            "rustc 1.88.0 (sentinel-rustc-banner)\n",
            "commit-hash: 0123456789abcdef0123456789abcdef01234567\n",
            "commit-date: 2025-06-26\n",
            "host: x86_64-pc-windows-msvc\n",
            "release: 1.88.0\n",
            "LLVM version: 20.1.7\n",
            "sentinel-rustc-wrapper-stdout"
        )
        .to_owned();
        snapshot.cargo_verbose_version = concat!(
            "cargo 1.88.0 (sentinel-cargo-banner)\n",
            "commit-hash: 89abcdef0123456789abcdef0123456789abcdef\n",
            "commit-date: 2025-05-12\n",
            "host: x86_64-pc-windows-msvc\n",
            "release: 1.88.0\n",
            "sentinel-cargo-wrapper-stdout"
        )
        .to_owned();

        let json = serde_json::to_string(&snapshot).unwrap();

        for private_text in [
            "sentinel-user",
            "private-fixture",
            "sentinel-private-plan",
            "sentinel-private-host",
            "sentinel-rustc-banner",
            "sentinel-rustc-wrapper-stdout",
            "sentinel-cargo-banner",
            "sentinel-cargo-wrapper-stdout",
        ] {
            assert!(!json.contains(private_text), "serialized {private_text}");
        }
        assert!(json.contains("\"path\":\"fixture-filesystem\""));
        assert!(json.contains("guid:381b4222-f694-41f0-9685-ff5bb260df2e"));
        assert!(json.contains("rustc 1.88.0"));
        assert!(json.contains("cargo 1.88.0"));
        assert!(json.contains("x86_64-pc-windows-msvc"));
    }
}
