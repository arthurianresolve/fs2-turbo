use std::collections::BTreeMap;
use std::fs;
use std::process::{Command, Output};

#[cfg(windows)]
use std::ffi::OsStr;
#[cfg(windows)]
use std::os::windows::ffi::OsStrExt as _;
#[cfg(windows)]
use std::path::{Path, PathBuf};

#[cfg(windows)]
use windows_sys::Win32::Storage::FileSystem::{
    DDD_EXACT_MATCH_ON_REMOVE, DDD_NO_BROADCAST_SYSTEM, DDD_RAW_TARGET_PATH, DDD_REMOVE_DEFINITION,
    DefineDosDeviceW, GetLogicalDrives, QueryDosDeviceW,
};

use serde_json::{Value, json};

fn command() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_fs2-dev"));
    command
        .env("CARGO", env!("CARGO"))
        .env("CARGO_NET_OFFLINE", "true")
        .env("FS2_DEV_PROCESS_TIMEOUT_SECONDS", "60");
    command
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "status: {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(windows)]
static WINDOWS_AUTHORITY_TEST: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(windows)]
struct TestDrive {
    path: PathBuf,
    device: Vec<u16>,
    target: Vec<u16>,
}

#[cfg(windows)]
impl Drop for TestDrive {
    fn drop(&mut self) {
        // SAFETY: remove only this fixture's exact local-session mapping.
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

#[cfg(windows)]
fn wide(value: &OsStr) -> Vec<u16> {
    value.encode_wide().chain(std::iter::once(0)).collect()
}

#[cfg(windows)]
fn test_drive(target_path: &Path) -> TestDrive {
    let canonical = target_path.canonicalize().unwrap();
    let canonical = canonical.to_str().unwrap();
    let disk_path = canonical.strip_prefix(r"\\?\").unwrap_or(canonical);
    assert!(!disk_path.starts_with(r"UNC\"));
    let target = wide(OsStr::new(&format!(r"\??\{disk_path}")));
    // SAFETY: this query has no pointer arguments or side effects.
    let drives = unsafe { GetLogicalDrives() };
    assert_ne!(drives, 0);

    for letter in (b'D'..=b'Z').rev() {
        let bit = 1_u32 << u32::from(letter - b'A');
        if drives & bit != 0 {
            continue;
        }
        let device = wide(OsStr::new(&format!("{}:", char::from(letter))));
        // SAFETY: both input buffers are terminated and remain live for the call.
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

        let mut observed = vec![0_u16; 32_768];
        // SAFETY: the device is terminated and the output buffer is writable.
        let length = unsafe {
            QueryDosDeviceW(
                device.as_ptr(),
                observed.as_mut_ptr(),
                observed.len() as u32,
            )
        };
        let observed_length = observed.iter().position(|unit| *unit == 0).unwrap_or(0);
        let expected_length = target.len() - 1;
        if length != 0
            && observed_length == expected_length
            && observed[..observed_length] == target[..expected_length]
        {
            return TestDrive {
                path: PathBuf::from(format!("{}:\\", char::from(letter))),
                device,
                target,
            };
        }

        // SAFETY: remove only the exact mapping just created by this iteration.
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
    panic!("no unused DOS drive is available for the integration fixture");
}

#[cfg(windows)]
fn windows_fixture_root() -> tempfile::TempDir {
    let parent = ["USERPROFILE", "LOCALAPPDATA"]
        .into_iter()
        .filter_map(std::env::var_os)
        .map(PathBuf::from)
        .find(|path| path.is_absolute() && path.is_dir())
        .expect("Windows integration tests require USERPROFILE or LOCALAPPDATA");
    tempfile::Builder::new()
        .prefix("fs2-dev-cli-")
        .tempdir_in(parent)
        .unwrap()
}

#[cfg(windows)]
fn copied_cli(root: &Path) -> PathBuf {
    let destination = root.join(format!("fs2-dev-fixture{}", std::env::consts::EXE_SUFFIX));
    fs::copy(env!("CARGO_BIN_EXE_fs2-dev"), &destination).unwrap();
    destination
}

#[cfg(windows)]
fn authority_command(executable: &Path, root: &Path) -> Command {
    let mut command = Command::new(executable);
    command
        .arg("matrix")
        .env("CARGO", root.join("missing-cargo"))
        .env("CARGO_NET_OFFLINE", "true")
        .env("FS2_DEV_PROCESS_TIMEOUT_SECONDS", "5")
        .env("USERPROFILE", root)
        .env("LOCALAPPDATA", root);
    command
}

#[cfg(windows)]
fn run_icacls(path: &Path, arguments: &[&str]) {
    let output = Command::new("icacls")
        .arg(path)
        .args(arguments)
        .output()
        .unwrap();
    assert_success(&output);
}

// Synthetic exports exercise the validator and its report transport. They are
// test inputs only, never measurements of library coverage.
struct CoverageFixture {
    directory: tempfile::TempDir,
    target: &'static str,
}

impl CoverageFixture {
    fn new(target: &'static str) -> Self {
        let directory = tempfile::tempdir().unwrap();
        for profile in ["combined", "unit", "integration"] {
            fs::write(
                directory.path().join(format!("{profile}.json")),
                synthetic_export(target, profile).to_string(),
            )
            .unwrap();
        }
        let mut lcov = String::from("SF:src/synthetic_fixture.rs\n");
        for line in 1..=2_000 {
            use std::fmt::Write as _;
            writeln!(lcov, "DA:{line},1").unwrap();
        }
        lcov.push_str("end_of_record\n");
        fs::write(directory.path().join("coverage.lcov"), lcov).unwrap();
        Self { directory, target }
    }

    fn command(&self) -> Command {
        let mut command = command();
        command.current_dir(self.directory.path()).args([
            "coverage",
            "--target",
            self.target,
            "--json",
            "combined.json",
            "--lcov",
            "coverage.lcov",
            "--unit-json",
            "unit.json",
            "--integration-json",
            "integration.json",
            "--diagnostics-json",
            "diagnostics with spaces.json",
        ]);
        command
    }

    fn report_path(&self) -> std::path::PathBuf {
        self.directory.path().join("diagnostics with spaces.json")
    }

    fn assert_rejected(&self, expected: &str, previous: &[u8]) {
        let output = self.command().output().unwrap();
        assert_eq!(output.status.code(), Some(1));
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(stderr.contains("fs2-dev:"), "{stderr}");
        assert!(stderr.contains(expected), "{stderr}");
        assert_eq!(fs::read(self.report_path()).unwrap(), previous);
    }
}

fn synthetic_function(name: &str, source: &str, line: u64, column: u64, hits: u64) -> Value {
    json!({
        "name": name,
        "count": hits,
        "filenames": [source],
        "regions": [[line, column, line, column + 1, hits, 0, 0, 0]]
    })
}

fn synthetic_export(target: &str, profile: &str) -> Value {
    let contracts: &[(&str, &[(u64, u64)])] = &[
        (
            "src/lib.rs",
            &[
                (140, 5),
                (147, 5),
                (154, 5),
                (161, 5),
                (167, 5),
                (193, 5),
                (197, 5),
                (201, 5),
                (205, 5),
                (209, 5),
                (213, 5),
                (217, 5),
                (221, 5),
                (225, 5),
                (229, 5),
                (233, 5),
                (237, 5),
                (241, 5),
                (248, 1),
            ],
        ),
        (
            "src/stats.rs",
            &[(33, 1), (41, 1), (49, 1), (57, 1), (65, 1)],
        ),
        ("src/stats/query.rs", &[(42, 5), (63, 5)]),
        (
            "src/stats/snapshot.rs",
            &[(42, 5), (48, 5), (57, 5), (67, 5)],
        ),
    ];
    let windows: &[(&str, &[(u64, u64)])] = &[
        (
            "src/windows/stats/modern.rs",
            &[(83, 1), (100, 1), (105, 1)],
        ),
        (
            "src/windows/stats/space.rs",
            &[(128, 1), (378, 1), (388, 1)],
        ),
    ];
    let mut functions = Vec::new();
    for &(source, locations) in contracts.iter().chain(
        windows
            .iter()
            .filter(|_| target == "x86_64-pc-windows-msvc"),
    ) {
        for &(line, column) in locations {
            functions.push(synthetic_function(
                &format!("synthetic:{source}:{line}:{column}"),
                source,
                line,
                column,
                1,
            ));
        }
    }

    let private_hits = u64::from(profile != "integration");
    functions.push(synthetic_function(
        "synthetic_private",
        "src/synthetic_fixture.rs",
        7,
        1,
        private_hits,
    ));
    functions.push(synthetic_function(
        "13invalid_stats",
        "src/stats.rs",
        20,
        1,
        private_hits,
    ));
    functions.push(synthetic_function(
        "synthetic_covered_shape",
        "src/synthetic_fixture.rs",
        10,
        1,
        1,
    ));
    functions.push(synthetic_function(
        "synthetic_unexecuted_instance",
        "src/synthetic_fixture.rs",
        10,
        1,
        0,
    ));
    let mut alternate = synthetic_function(
        "synthetic_alternate_shape",
        "src/synthetic_fixture.rs",
        10,
        1,
        private_hits,
    );
    alternate["regions"][0][2] = json!(11);
    functions.push(alternate);
    for hits in [0, 1] {
        functions.push(synthetic_function(
            &format!("external_fixture_{hits}"),
            "/rustc/synthetic-fixture/library/core/src/option.rs",
            1,
            1,
            hits,
        ));
    }

    let mut files = BTreeMap::<&str, (u64, u64)>::new();
    for function in &functions {
        let entry = files
            .entry(function["filenames"][0].as_str().unwrap())
            .or_default();
        entry.0 += 1;
        entry.1 += u64::from(function["count"].as_u64().unwrap() != 0);
    }
    let files = files.into_iter().map(|(filename, (count, covered))| {
        json!({"filename": filename, "summary": {"instantiations": {"count": count, "covered": covered}}})
    }).collect::<Vec<_>>();
    let covered = functions
        .iter()
        .filter(|function| function["count"].as_u64().unwrap() != 0)
        .count();
    json!({
        "type": "llvm.coverage.json.export",
        "data": [{
            "files": files,
            "totals": {
                "functions": {"count": 200, "covered": 200},
                "instantiations": {"count": functions.len(), "covered": covered},
                "lines": {"count": 2000, "covered": 2000},
                "regions": {"count": 2000, "covered": 2000}
            },
            "functions": functions
        }]
    })
}

#[test]
fn coverage_command_emits_diagnostics_for_every_native_policy() {
    for target in [
        "x86_64-unknown-linux-gnu",
        "aarch64-apple-darwin",
        "x86_64-pc-windows-msvc",
    ] {
        let fixture = CoverageFixture::new(target);
        let output = fixture.command().output().unwrap();
        assert_success(&output);
        assert!(output.stderr.is_empty());
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(stdout.contains(&format!("coverage policy satisfied for {target}")));
        assert!(stdout.contains("review=defensive-boundary"));
        assert!(stdout.contains("review=unclassified"));
        assert!(stdout.contains("instantiation gap"));
        let bytes = fs::read(fixture.report_path()).unwrap();
        assert!(bytes.ends_with(b"\n"));
        let report: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(report["schema_version"], 4);
        assert_eq!(report["target"], target);
        let intended = if target == "x86_64-pc-windows-msvc" {
            36
        } else {
            30
        };
        assert_eq!(
            report["intended_integration_definitions"],
            json!({"count": intended, "covered": intended})
        );
        let profiles = report["profiles"].as_array().unwrap();
        assert_eq!(profiles.len(), 3);
        for profile in profiles {
            let integration = profile["profile"] == "integration";
            let entries = intended + 7;
            let executed = intended + if integration { 2 } else { 5 };
            assert_eq!(
                profile["json_entries"],
                json!({"count": entries, "covered": executed})
            );
            assert_eq!(
                profile["external_json_entries"],
                json!({"count": 2, "covered": 1})
            );
            assert_eq!(profile["asymmetric_definition_groups"], 1);
            assert!(
                profile["definitions"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|definition| definition["ownership"] == "compiler-asymmetric")
            );
            let union = &profile["source_location_execution_union"];
            assert_eq!(union["informational_only"], true);
            assert_eq!(union["multi_topology_locations"], 1);
            if integration {
                assert_eq!(union["uncovered_groups_with_executed_location"], 1);
                assert!(
                    profile["definitions"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|definition| definition["ownership"] == "private-unit")
                );
                let reviewed = union["records"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|record| record["source"] == "src/stats.rs" && record["line"] == 20)
                    .unwrap();
                assert_eq!(reviewed["all_uncovered_topologies_unit_owned"], true);
                assert_eq!(
                    reviewed["reviewed_integration_gap"]["category"],
                    "defensive-boundary"
                );
            }
        }
    }
}

#[test]
fn coverage_command_preserves_reports_when_any_profile_is_rejected() {
    let fixture = CoverageFixture::new("x86_64-unknown-linux-gnu");
    assert_success(&fixture.command().output().unwrap());
    let previous = fs::read(fixture.report_path()).unwrap();
    for profile in ["combined", "unit", "integration"] {
        let path = fixture.directory.path().join(format!("{profile}.json"));
        let original = fs::read_to_string(&path).unwrap();
        let export: Value = serde_json::from_str(&original).unwrap();

        fs::remove_file(&path).unwrap();
        fixture.assert_rejected("fs2-dev:", &previous);
        fs::write(&path, "not JSON").unwrap();
        fixture.assert_rejected("fs2-dev:", &previous);

        for data in [
            json!([]),
            json!([export["data"][0].clone(), export["data"][0].clone()]),
        ] {
            let mut altered = export.clone();
            altered["data"] = data;
            fs::write(&path, altered.to_string()).unwrap();
            fixture.assert_rejected(
                &format!("{profile} coverage JSON must contain exactly one data set"),
                &previous,
            );
        }

        let mut altered = export.clone();
        altered["type"] = json!("not-a-coverage-export");
        fs::write(&path, altered.to_string()).unwrap();
        fixture.assert_rejected("unexpected coverage JSON type", &previous);

        let mut altered = export.clone();
        altered["data"][0]["functions"][0]["regions"]
            .as_array_mut()
            .unwrap()
            .push(json!([0]));
        fs::write(&path, altered.to_string()).unwrap();
        fixture.assert_rejected("malformed region", &previous);

        let mut altered = export.clone();
        altered["data"][0]["files"][0]["summary"]["instantiations"] =
            json!({"count": 0, "covered": 1});
        fs::write(&path, altered.to_string()).unwrap();
        fixture.assert_rejected("file instantiations", &previous);

        let mut altered = export.clone();
        altered["data"][0]["functions"][0]["count"] = json!(0);
        altered["data"][0]["functions"][0]["regions"][0][4] = json!(0);
        fs::write(&path, altered.to_string()).unwrap();
        fixture.assert_rejected("coverage regression", &previous);
        fs::write(&path, original).unwrap();
    }
    fs::write(
        fixture.directory.path().join("coverage.lcov"),
        "SF:src/synthetic_fixture.rs\nDA:1,invalid\nend_of_record\n",
    )
    .unwrap();
    fixture.assert_rejected("execution count is invalid", &previous);
}

#[test]
fn coverage_command_rejects_unowned_integration_residuals() {
    let fixture = CoverageFixture::new("x86_64-unknown-linux-gnu");
    assert_success(&fixture.command().output().unwrap());
    let previous = fs::read(fixture.report_path()).unwrap();
    let path = fixture.directory.path().join("integration.json");
    let mut export: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    export["data"][0]["functions"]
        .as_array_mut()
        .unwrap()
        .push(synthetic_function("unowned", "src/orphan.rs", 5, 1, 0));
    export["data"][0]["files"]
        .as_array_mut()
        .unwrap()
        .push(json!({
            "filename": "src/orphan.rs",
            "summary": {"instantiations": {"count": 1, "covered": 0}}
        }));
    let count = export["data"][0]["totals"]["instantiations"]["count"]
        .as_u64()
        .unwrap();
    export["data"][0]["totals"]["instantiations"]["count"] = json!(count + 1);
    fs::write(path, export.to_string()).unwrap();
    fixture.assert_rejected("lack unit ownership: [src/orphan.rs:5:1]", &previous);
}

#[test]
fn coverage_command_unions_lcov_records_and_rejects_malformed_lines() {
    let fixture = CoverageFixture::new("x86_64-unknown-linux-gnu");
    assert_success(&fixture.command().output().unwrap());
    let previous = fs::read(fixture.report_path()).unwrap();
    let path = fixture.directory.path().join("coverage.lcov");
    let mut lcov = fs::read_to_string(&path).unwrap();
    lcov.push_str("SF:src/synthetic_fixture.rs\nDA:1,0\nDA:1,2\nend_of_record\n");
    fs::write(&path, lcov).unwrap();
    assert_success(&fixture.command().output().unwrap());
    assert_eq!(fs::read(fixture.report_path()).unwrap(), previous);

    for (contents, expected) in [
        ("DA:1,1\n", "line data precedes a source record"),
        ("SF:src/fixture.rs\nDA:1\n", "execution count is missing"),
        ("SF:\n", "source path is empty"),
        (
            "SF:src/fixture.rs\nDA:invalid,1\n",
            "line number is invalid",
        ),
        ("TN:empty\n", "contains no physical source lines"),
        (
            "SF:src/fixture.rs\nDA:1,1\nend_of_record\nDA:2,1\n",
            "line data precedes a source record",
        ),
    ] {
        fs::write(&path, contents).unwrap();
        fixture.assert_rejected(expected, &previous);
    }
}

#[test]
fn coverage_command_reports_diagnostic_output_failures() {
    let fixture = CoverageFixture::new("x86_64-unknown-linux-gnu");
    fs::create_dir(fixture.report_path()).unwrap();
    let output = fixture.command().output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("fs2-dev:")
    );
    assert!(fixture.report_path().is_dir());
}

#[test]
#[should_panic(expected = "stdout:\nfixture stdout\nstderr:\nfixture stderr")]
fn failed_command_diagnostics_include_both_captured_streams() {
    let mut output = command().arg("unknown-command").output().unwrap();
    output.stdout = b"fixture stdout".to_vec();
    output.stderr = b"fixture stderr".to_vec();
    assert_success(&output);
}

#[test]
fn process_configuration_and_spawn_failures_are_reported_by_the_cli() {
    let directory = tempfile::tempdir().unwrap();
    let output = command()
        .arg("matrix")
        .env("CARGO", directory.path().join("missing-cargo"))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("read fs2-turbo package metadata failed: process spawn failed:"),
        "{stderr}"
    );

    for (value, expected) in [
        ("invalid", "must be an integer"),
        ("0", "outside the supported range"),
        ("86401", "outside the supported range"),
    ] {
        let output = command()
            .arg("matrix")
            .env("FS2_DEV_PROCESS_TIMEOUT_SECONDS", value)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(
            stderr.contains("FS2_DEV_PROCESS_TIMEOUT_SECONDS"),
            "{stderr}"
        );
        assert!(stderr.contains(expected), "{stderr}");
    }
}

#[cfg(windows)]
#[test]
fn native_cli_enforces_capture_authority_and_directory_backed_drive_identity() {
    let _serial = WINDOWS_AUTHORITY_TEST.lock().unwrap();

    let mapped_root = windows_fixture_root();
    let mapped = test_drive(mapped_root.path());
    let output = command()
        .arg("matrix")
        .env("CARGO", mapped_root.path().join("missing-cargo"))
        .env("USERPROFILE", &mapped.path)
        .env("LOCALAPPDATA", &mapped.path)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("process spawn failed"), "{stderr}");
    assert!(
        !stderr.contains("unable to bind capture temp ancestry"),
        "{stderr}"
    );
    drop(mapped);

    let private_root = windows_fixture_root();
    let private_cli = copied_cli(private_root.path());
    let capture = private_root.path().join(".fs2-secure-capture");
    let output = authority_command(&private_cli, private_root.path())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("process spawn failed"), "{stderr}");
    assert!(capture.is_dir());
    run_icacls(&capture, &["/grant", "*S-1-5-11:(RX)"]);
    let output = authority_command(&private_cli, private_root.path())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("capture private directory ACL is not limited"),
        "{stderr}"
    );

    let ancestor_root = windows_fixture_root();
    let ancestor_cli = copied_cli(ancestor_root.path());
    run_icacls(ancestor_root.path(), &["/grant", "*S-1-5-11:(W)"]);
    let output = authority_command(&ancestor_cli, ancestor_root.path())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("replacement or reparse mutation"),
        "{stderr}"
    );
}

#[test]
fn native_cargo_failures_preserve_output_and_timeout_cleanup() {
    let directory = tempfile::tempdir().unwrap();
    let executable = directory.path().join(format!(
        "native cargo fixture{}",
        std::env::consts::EXE_SUFFIX
    ));
    let rustc = std::path::Path::new(env!("CARGO"))
        .with_file_name(format!("rustc{}", std::env::consts::EXE_SUFFIX));
    let source =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/cargo_process.rs");
    let output = Command::new(rustc)
        .current_dir(directory.path())
        .args([
            "--edition=2021",
            "--crate-name=fs2_dev_cargo_fixture",
            "-Cdebuginfo=0",
            "-Copt-level=0",
        ])
        .arg(source)
        .arg("-o")
        .arg(&executable)
        .output()
        .unwrap();
    assert_success(&output);

    for (mode, expected) in [
        ("exit", "native exit 7"),
        ("wait", "process timed out after 3000 ms; reaped=true"),
    ] {
        let receipt = directory.path().join(format!("{mode}.pid"));
        let output = command()
            .arg("matrix")
            .env("CARGO", &executable)
            .env("FS2_DEV_CARGO_FIXTURE_MODE", mode)
            .env("FS2_DEV_CARGO_FIXTURE_RECEIPT", &receipt)
            .env("FS2_DEV_PROCESS_TIMEOUT_SECONDS", "3")
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(
            stderr.contains("read fs2-turbo package metadata failed:"),
            "{stderr}"
        );
        assert!(stderr.contains(expected), "{stderr}");
        assert!(
            stderr.contains("stdout:\nfixture stdout\nstderr:\nfixture stderr"),
            "{stderr}"
        );
        assert!(fs::read_to_string(receipt).unwrap().parse::<u32>().unwrap() > 0);
    }

    #[cfg(unix)]
    {
        let output = command()
            .arg("matrix")
            .env("CARGO", &executable)
            .env("FS2_DEV_CARGO_FIXTURE_MODE", "signal")
            .env("FS2_DEV_CARGO_FIXTURE_SIGNAL", libc::SIGTERM.to_string())
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(
            stderr.contains(&format!("process terminated: signal {}", libc::SIGTERM)),
            "{stderr}"
        );
        assert!(
            stderr.contains("stdout:\nfixture stdout\nstderr:\nfixture stderr"),
            "{stderr}"
        );

        let ready = directory.path().join("group-member.ready");
        let output = command()
            .arg("matrix")
            .env("CARGO", &executable)
            .env("FS2_DEV_CARGO_FIXTURE_MODE", "group-wait")
            .env("FS2_DEV_CARGO_FIXTURE_READY", &ready)
            .env("FS2_DEV_PROCESS_TIMEOUT_SECONDS", "3")
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(
            stderr.contains("process timed out after 3000 ms; reaped=true"),
            "{stderr}"
        );
        assert!(ready.exists());
    }
}

#[test]
fn compatibility_command_runs_the_frozen_legacy_and_current_consumers() {
    let output = command().arg("compatibility").output().unwrap();
    assert_success(&output);
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("v0.4 consumer sha256="));
    for subject in ["legacy", "current"] {
        for edition in ["2015", "2018", "2021", "2024"] {
            assert!(stdout.contains(&format!("run {subject} v0.4 consumer in edition {edition}")));
        }
    }
}

#[test]
fn help_is_successful_for_the_root_and_each_command() {
    let cases: &[&[&str]] = &[
        &["--help"],
        &["matrix", "--help"],
        &["coverage", "--help"],
        &["compatibility", "--help"],
    ];
    for arguments in cases {
        let output = command().args(*arguments).output().unwrap();
        assert_success(&output);
        let help = String::from_utf8(output.stdout).unwrap();
        assert!(help.contains("Usage:"));
        assert!(help.contains("fs2-dev"));
        assert!(output.stderr.is_empty());
    }
}

#[test]
fn invalid_cli_usage_returns_claps_usage_error() {
    let cases: &[&[&str]] = &[
        &[],
        &["unknown-command"],
        &["matrix", "--unknown-option"],
        &["coverage", "--target", "x86_64-unknown-linux-gnu"],
    ];
    for arguments in cases {
        let output = command().args(*arguments).output().unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(!output.stderr.is_empty());
    }
}

#[test]
fn coverage_validation_errors_have_a_failure_exit_and_context() {
    let directory = tempfile::tempdir().unwrap();
    let output = command()
        .current_dir(directory.path())
        .args([
            "coverage",
            "--target",
            "unknown-target",
            "--json",
            "combined.json",
            "--lcov",
            "coverage.lcov",
            "--unit-json",
            "unit.json",
            "--integration-json",
            "integration.json",
            "--diagnostics-json",
            "diagnostics.json",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let error = String::from_utf8(output.stderr).unwrap();
    assert!(error.contains("fs2-dev: no native coverage policy"));
    assert!(!directory.path().join("diagnostics.json").exists());
}

#[test]
fn matrix_command_uses_the_repository_root_and_supports_both_outputs() {
    let directory = tempfile::tempdir().unwrap();
    let output = command()
        .current_dir(directory.path())
        .arg("matrix")
        .output()
        .unwrap();
    assert_success(&output);
    let matrices: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(matrices["check"]["include"].as_array().unwrap().len() >= 2);
    assert_eq!(matrices["coverage"]["include"].as_array().unwrap().len(), 3);

    let path = directory.path().join("github output with spaces");
    fs::write(&path, "existing=value\n").unwrap();
    let output = command()
        .current_dir(directory.path())
        .args(["matrix", "--github-output"])
        .arg(&path)
        .output()
        .unwrap();
    assert_success(&output);
    assert!(output.stdout.is_empty());
    let contents = fs::read_to_string(path).unwrap();
    let lines = contents.lines().collect::<Vec<_>>();
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[0], "existing=value");
    assert_eq!(lines[2], "rust_version=1.88.0");
    let written: Value = serde_json::from_str(lines[1].strip_prefix("matrices=").unwrap()).unwrap();
    assert_eq!(written, matrices);
}
