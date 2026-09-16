use std::collections::{BTreeMap, BTreeSet};
#[cfg(unix)]
use std::ffi::CString;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Serialize;
use serde_json::{Map, Value};
use sha2::{Digest as _, Sha256};

use crate::process;
use crate::{Result, invalid_data, lower_hex};

const REPORT_SCHEMA: &str = "fs2-turbo.mcdc-diagnostic.v1";
const RUST_MCDC_REVISION: &str = "e57cec60d416d49dc9d5bdb9b23ea14f20d4a49e";
const RUSTC_COMMIT: &str = "48a229ceaefd4985c50990b14116b6d856af0985";
const RUST_RELEASE_PREFIX: &str = "1.98.1";
const LLVM_VERSION: &str = "22.1.8";
const LLVM_EXPORT_TYPE: &str = "llvm.coverage.json.export";
const LLVM_EXPORT_VERSION: &str = "3.1.0";
const CARGO_LLVM_COV_VERSION: &str = "cargo-llvm-cov 0.8.7";
const MAX_EXPORT_BYTES: u64 = 64 * 1024 * 1024;
const MAX_MCDC_RECORDS: usize = 65_536;
const MAX_BRANCH_RECORDS: usize = 1_048_576;
const MAX_CONDITIONS_PER_DECISION: usize = 16;
const MAX_VECTORS_PER_DECISION: usize = 65_536;
const MAX_NAME_BYTES: usize = 4_096;

#[derive(Serialize)]
struct DiagnosticReport {
    schema: &'static str,
    status: &'static str,
    release_gate: bool,
    ordinary_llvm_coverage_remains_authoritative: bool,
    source: SourceIdentity,
    rust_mcdc: RustMcdcIdentity,
    toolchain: ToolchainIdentity,
    transport: TransportSummary,
    supported_slice: SupportedSlice,
    explicit_non_claims: [&'static str; 8],
}

#[derive(Serialize)]
struct SourceIdentity {
    revision: String,
    tree: String,
    tracked_files_clean: bool,
    untracked_paths_observed: usize,
}

#[derive(Serialize)]
struct RustMcdcIdentity {
    revision: String,
    tree: String,
    checkout_clean: bool,
}

#[derive(Serialize)]
struct ToolchainIdentity {
    rustup_toolchain: String,
    release: String,
    commit_hash: String,
    host: String,
    llvm_version: String,
    rustc_sha256: String,
    cargo_llvm_cov_version: String,
    mcdc_flag_probe_passed: bool,
}

#[derive(Serialize)]
struct TransportSummary {
    export_type: String,
    export_version: String,
    byte_length: u64,
    sha256: String,
    file_count: usize,
    function_count: usize,
    branch_record_count: usize,
    decision_record_count: usize,
    condition_count: usize,
    covered_condition_pair_count: usize,
    executed_vector_count: usize,
    not_evaluated_condition_state_count: usize,
    llvm_condition_pair_coverage_complete: bool,
    semantic_census_complete: bool,
    independent_proof_policy_applied: bool,
}

#[derive(Serialize)]
struct SupportedSlice {
    decision_form: &'static str,
    async_functions: bool,
    generic_functions: bool,
    negation: bool,
    maximum_conditions_per_decision: usize,
    unsupported_constructs_are_coverage: bool,
}

struct RustcIdentity {
    release: String,
    commit_hash: String,
    host: String,
    llvm_version: String,
}

#[derive(Default)]
struct RecordStats {
    decisions: usize,
    conditions: usize,
    covered_pairs: usize,
    executed_vectors: usize,
    not_evaluated_states: usize,
}

pub(crate) fn run(
    repository_root: &Path,
    rust_mcdc_root: &Path,
    toolchain: &str,
    work_dir: &Path,
    report_path: &Path,
    minimum_free_bytes: u64,
) -> Result<()> {
    validate_toolchain_name(toolchain)?;
    let repository_root = fs::canonicalize(repository_root)?;
    let rust_mcdc_root = fs::canonicalize(rust_mcdc_root).map_err(|error| {
        invalid_data(format!(
            "cannot canonicalize Rust-MCDC root {}: {error}",
            rust_mcdc_root.display()
        ))
    })?;
    if !rust_mcdc_root.is_dir() {
        return Err(invalid_data("Rust-MCDC root is not a directory"));
    }

    let work_dir = prepare_new_directory(work_dir, &repository_root, &rust_mcdc_root)?;
    let report_path = prepare_new_file_path(
        report_path,
        "diagnostic report",
        &repository_root,
        &rust_mcdc_root,
    )?;
    if report_path.starts_with(&work_dir) {
        return Err(invalid_data(
            "diagnostic report must be retained outside the disposable work directory",
        ));
    }

    let free_bytes = available_space(&work_dir)?;
    if free_bytes < minimum_free_bytes {
        return Err(invalid_data(format!(
            "MC/DC work volume has {free_bytes} free bytes; at least {minimum_free_bytes} are required"
        )));
    }

    let source_status = git_status(&repository_root)?;
    let source_untracked = validate_source_status(&source_status)?;
    let source_revision = git_line(
        &repository_root,
        &["rev-parse", "HEAD"],
        "read source revision",
    )?;
    let source_tree = git_line(
        &repository_root,
        &["rev-parse", "HEAD^{tree}"],
        "read source tree",
    )?;

    let rust_mcdc_status = git_status(&rust_mcdc_root)?;
    if !rust_mcdc_status.is_empty() {
        return Err(invalid_data(
            "Rust-MCDC checkout must be completely clean, including untracked paths",
        ));
    }
    let rust_mcdc_revision = git_line(
        &rust_mcdc_root,
        &["rev-parse", "HEAD"],
        "read Rust-MCDC revision",
    )?;
    if rust_mcdc_revision != RUST_MCDC_REVISION {
        return Err(invalid_data(format!(
            "Rust-MCDC revision is {rust_mcdc_revision}; expected {RUST_MCDC_REVISION}"
        )));
    }
    let rust_mcdc_tree = git_line(
        &rust_mcdc_root,
        &["rev-parse", "HEAD^{tree}"],
        "read Rust-MCDC tree",
    )?;

    let selector = format!("+{toolchain}");
    let rustc_verbose = capture_utf8(
        Command::new("rustc").arg(&selector).arg("-vV"),
        "read MC/DC rustc identity",
    )?;
    let rustc_identity = parse_rustc_identity(&rustc_verbose)?;
    validate_rustc_identity(&rustc_identity)?;

    let sysroot = capture_text(
        Command::new("rustc")
            .arg(&selector)
            .args(["--print", "sysroot"]),
        "resolve MC/DC rustc sysroot",
    )?;
    let rustc_binary =
        PathBuf::from(sysroot)
            .join("bin")
            .join(if cfg!(windows) { "rustc.exe" } else { "rustc" });
    let rustc_sha256 = sha256_file(&rustc_binary)?;

    let cargo_llvm_cov_version = capture_text(
        Command::new("cargo")
            .arg(&selector)
            .args(["llvm-cov", "--version"]),
        "read cargo-llvm-cov version",
    )?;
    if cargo_llvm_cov_version != CARGO_LLVM_COV_VERSION {
        return Err(invalid_data(format!(
            "cargo-llvm-cov identity is {cargo_llvm_cov_version:?}; expected {CARGO_LLVM_COV_VERSION:?}"
        )));
    }

    let mut probe = Command::new("rustc");
    probe
        .arg(&selector)
        .args([
            "-C",
            "instrument-coverage",
            "-Z",
            "coverage-options=mcdc",
            "--print",
            "cfg",
        ])
        .env_remove("RUSTC_BOOTSTRAP")
        .env_remove("RUSTFLAGS")
        .env_remove("CARGO_ENCODED_RUSTFLAGS");
    process::capture(&mut probe, "probe patched rustc MC/DC support")?;

    let export_path = work_dir.join("fs2-turbo-mcdc.json");
    let target_dir = work_dir.join("target");
    let mut coverage = Command::new("cargo");
    coverage
        .current_dir(&repository_root)
        .arg(&selector)
        .args([
            "llvm-cov",
            "--mcdc",
            "--package",
            "fs2-turbo",
            "--lib",
            "--tests",
            "--locked",
            "--remap-path-prefix",
            "--json",
            "--output-path",
        ])
        .arg(&export_path)
        .args(["--", "--test-threads=1"])
        .env_remove("RUSTC_BOOTSTRAP")
        .env_remove("RUSTFLAGS")
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .env_remove("LLVM_PROFILE_FILE")
        .env("CARGO_INCREMENTAL", "0")
        .env("CARGO_TARGET_DIR", &target_dir)
        .env("CARGO_LLVM_COV_TARGET_DIR", &target_dir)
        .env("CARGO_LLVM_COV_BUILD_DIR", &target_dir)
        .env("LLVM_COV_FLAGS", "--skip-expansions");
    process::run(&mut coverage, "run fresh fs2-turbo MC/DC diagnostic")?;

    let export_bytes = read_bounded_regular_file(&export_path, MAX_EXPORT_BYTES)?;
    let transport = validate_export(&export_bytes)?;

    let final_source_status = git_status(&repository_root)?;
    let final_source_revision = git_line(
        &repository_root,
        &["rev-parse", "HEAD"],
        "revalidate source revision",
    )?;
    let final_source_tree = git_line(
        &repository_root,
        &["rev-parse", "HEAD^{tree}"],
        "revalidate source tree",
    )?;
    if final_source_status != source_status
        || final_source_revision != source_revision
        || final_source_tree != source_tree
    {
        return Err(invalid_data(
            "fs2-turbo source identity or worktree changed during the diagnostic",
        ));
    }
    if git_status(&rust_mcdc_root)? != rust_mcdc_status
        || git_line(
            &rust_mcdc_root,
            &["rev-parse", "HEAD"],
            "revalidate Rust-MCDC revision",
        )? != rust_mcdc_revision
        || git_line(
            &rust_mcdc_root,
            &["rev-parse", "HEAD^{tree}"],
            "revalidate Rust-MCDC tree",
        )? != rust_mcdc_tree
    {
        return Err(invalid_data(
            "Rust-MCDC identity or worktree changed during the diagnostic",
        ));
    }

    let report = DiagnosticReport {
        schema: REPORT_SCHEMA,
        status: "diagnostic_only",
        release_gate: false,
        ordinary_llvm_coverage_remains_authoritative: true,
        source: SourceIdentity {
            revision: source_revision,
            tree: source_tree,
            tracked_files_clean: true,
            untracked_paths_observed: source_untracked,
        },
        rust_mcdc: RustMcdcIdentity {
            revision: rust_mcdc_revision,
            tree: rust_mcdc_tree,
            checkout_clean: true,
        },
        toolchain: ToolchainIdentity {
            rustup_toolchain: toolchain.to_owned(),
            release: rustc_identity.release,
            commit_hash: rustc_identity.commit_hash,
            host: rustc_identity.host,
            llvm_version: rustc_identity.llvm_version,
            rustc_sha256,
            cargo_llvm_cov_version,
            mcdc_flag_probe_passed: true,
        },
        transport,
        supported_slice: SupportedSlice {
            decision_form: "root-expansion non-async non-generic free-function if with nested short-circuit Boolean leaves",
            async_functions: false,
            generic_functions: false,
            negation: false,
            maximum_conditions_per_decision: MAX_CONDITIONS_PER_DECISION,
            unsupported_constructs_are_coverage: false,
        },
        explicit_non_claims: [
            "not a release gate",
            "not 100 percent MC/DC evidence",
            "not a complete semantic denominator",
            "not an independent unique-cause or masking proof",
            "not source-to-object or production-binary equivalence",
            "not DO-178C compliance or certification credit",
            "not DO-330 qualification",
            "not a replacement for native line, region, function, or instantiation coverage",
        ],
    };
    write_new_report(&report_path, &report)?;
    println!(
        "wrote diagnostic-only MC/DC report to {}",
        report_path.display()
    );
    Ok(())
}

fn validate_toolchain_name(toolchain: &str) -> Result<()> {
    let mut bytes = toolchain.bytes();
    let Some(first) = bytes.next() else {
        return Err(invalid_data("toolchain name must not be empty"));
    };
    if !first.is_ascii_alphanumeric()
        || toolchain.len() > 128
        || !bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(invalid_data(
            "toolchain name contains unsupported characters",
        ));
    }
    Ok(())
}

fn prepare_new_directory(path: &Path, roots: &Path, rust_mcdc: &Path) -> Result<PathBuf> {
    let path = prepare_absent_path(path, "work directory", roots, rust_mcdc)?;
    fs::create_dir(&path)?;
    Ok(fs::canonicalize(path)?)
}

fn prepare_new_file_path(
    path: &Path,
    label: &str,
    roots: &Path,
    rust_mcdc: &Path,
) -> Result<PathBuf> {
    prepare_absent_path(path, label, roots, rust_mcdc)
}

fn prepare_absent_path(path: &Path, label: &str, root: &Path, rust_mcdc: &Path) -> Result<PathBuf> {
    if !path.is_absolute() {
        return Err(invalid_data(format!("{label} must be an absolute path")));
    }
    if path.exists() {
        return Err(invalid_data(format!("{label} must not already exist")));
    }
    let name = path
        .file_name()
        .ok_or_else(|| invalid_data(format!("{label} has no final component")))?;
    let parent = path
        .parent()
        .ok_or_else(|| invalid_data(format!("{label} has no parent")))?;
    let path = fs::canonicalize(parent)?.join(name);
    if path.starts_with(root) || path.starts_with(rust_mcdc) {
        return Err(invalid_data(format!(
            "{label} must be outside both source repositories"
        )));
    }
    Ok(path)
}

fn git_status(root: &Path) -> Result<Vec<u8>> {
    let mut command = Command::new("git");
    command
        .args(["-c", "core.quotepath=false", "-C"])
        .arg(root)
        .args(["status", "--porcelain=v1", "-z", "--untracked-files=all"]);
    Ok(process::capture(&mut command, "read Git worktree status")?.stdout)
}

fn validate_source_status(status: &[u8]) -> Result<usize> {
    let mut untracked = 0;
    for entry in status
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
    {
        if entry.starts_with(b"?? ") {
            untracked += 1;
        } else {
            return Err(invalid_data(
                "fs2-turbo tracked files must be clean before MC/DC measurement",
            ));
        }
    }
    Ok(untracked)
}

fn git_line(root: &Path, arguments: &[&str], label: &str) -> Result<String> {
    let mut command = Command::new("git");
    command.arg("-C").arg(root).args(arguments);
    capture_text(&mut command, label)
}

fn capture_text(command: &mut Command, label: &str) -> Result<String> {
    let text = capture_utf8(command, label)?;
    let text = text.trim();
    if text.is_empty() || text.lines().count() != 1 {
        return Err(invalid_data(format!("{label} did not return one line")));
    }
    Ok(text.to_owned())
}

fn capture_utf8(command: &mut Command, label: &str) -> Result<String> {
    Ok(String::from_utf8(process::capture(command, label)?.stdout)?)
}

fn parse_rustc_identity(verbose: &str) -> Result<RustcIdentity> {
    let fields = verbose
        .lines()
        .filter_map(|line| line.split_once(':'))
        .map(|(key, value)| (key.trim(), value.trim()))
        .collect::<BTreeMap<_, _>>();
    let field = |name: &str| {
        fields
            .get(name)
            .filter(|value| !value.is_empty())
            .map(|value| (*value).to_owned())
            .ok_or_else(|| invalid_data(format!("rustc -vV omitted {name}")))
    };
    Ok(RustcIdentity {
        release: field("release")?,
        commit_hash: field("commit-hash")?,
        host: field("host")?,
        llvm_version: field("LLVM version")?,
    })
}

fn validate_rustc_identity(identity: &RustcIdentity) -> Result<()> {
    if !identity.release.starts_with(RUST_RELEASE_PREFIX)
        || identity.commit_hash != RUSTC_COMMIT
        || identity.llvm_version != LLVM_VERSION
    {
        return Err(invalid_data(format!(
            "MC/DC compiler identity mismatch: release {}, commit {}, LLVM {}",
            identity.release, identity.commit_hash, identity.llvm_version
        )));
    }
    Ok(())
}

fn read_bounded_regular_file(path: &Path, maximum: u64) -> Result<Vec<u8>> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file() || metadata.len() == 0 || metadata.len() > maximum {
        return Err(invalid_data(format!(
            "MC/DC export must be a nonempty regular file no larger than {maximum} bytes"
        )));
    }
    let capacity = usize::try_from(metadata.len())?;
    let mut bytes = Vec::with_capacity(capacity);
    File::open(path)?
        .take(maximum + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() != capacity {
        return Err(invalid_data("MC/DC export changed while it was read"));
    }
    Ok(bytes)
}

fn validate_export(bytes: &[u8]) -> Result<TransportSummary> {
    let root: Value = serde_json::from_slice(bytes)?;
    let root = object(&root, "root")?;
    require_exact_keys(root, &["data", "type", "version"], "root")?;
    let export_type = string_member(root, "type", "root")?;
    let export_version = string_member(root, "version", "root")?;
    if export_type != LLVM_EXPORT_TYPE || export_version != LLVM_EXPORT_VERSION {
        return Err(invalid_data(format!(
            "unsupported LLVM export {export_type:?} version {export_version:?}; expected {LLVM_EXPORT_TYPE:?} version {LLVM_EXPORT_VERSION:?}"
        )));
    }
    let data = array_member(root, "data", "root")?;
    if data.len() != 1 {
        return Err(invalid_data(
            "LLVM export must contain exactly one data entry",
        ));
    }
    let entry = object(&data[0], "data[0]")?;
    require_exact_keys(entry, &["files", "functions", "totals"], "data[0]")?;
    let files = array_member(entry, "files", "data[0]")?;
    let functions = array_member(entry, "functions", "data[0]")?;
    if files.is_empty() || functions.is_empty() {
        return Err(invalid_data("LLVM export has no files or functions"));
    }

    let mut filenames = BTreeSet::new();
    let mut file_records = BTreeMap::<String, usize>::new();
    let mut file_branches = 0usize;
    let mut stats = RecordStats::default();
    for (index, file) in files.iter().enumerate() {
        let context = format!("data[0].files[{index}]");
        let file = object(file, &context)?;
        require_exact_keys(
            file,
            &[
                "branches",
                "filename",
                "mcdc_records",
                "segments",
                "summary",
            ],
            &context,
        )?;
        let filename = string_member(file, "filename", &context)?;
        validate_name(filename, "LLVM filename")?;
        if !filenames.insert(filename.to_owned()) {
            return Err(invalid_data("LLVM filenames must be unique"));
        }
        file_branches = checked_add(
            file_branches,
            array_member(file, "branches", &context)?.len(),
            MAX_BRANCH_RECORDS,
            "branch records",
        )?;
        for record in array_member(file, "mcdc_records", &context)? {
            parse_record(record, &mut stats)?;
            let key = serde_json::to_string(record)?;
            *file_records.entry(key).or_default() += 1;
            if file_records.values().sum::<usize>() > MAX_MCDC_RECORDS {
                return Err(invalid_data("LLVM MC/DC record limit exceeded"));
            }
        }
    }
    if !filenames
        .iter()
        .any(|name| name.replace('\\', "/").ends_with("/src/lib.rs") || name == "src/lib.rs")
    {
        return Err(invalid_data(
            "LLVM export does not contain the fs2-turbo src/lib.rs source identity",
        ));
    }

    let mut function_records = BTreeMap::<String, usize>::new();
    let mut function_branches = 0usize;
    let mut function_identities = BTreeSet::new();
    for (index, function) in functions.iter().enumerate() {
        let context = format!("data[0].functions[{index}]");
        let function = object(function, &context)?;
        require_exact_keys(
            function,
            &[
                "branches",
                "count",
                "filenames",
                "mcdc_records",
                "name",
                "regions",
            ],
            &context,
        )?;
        let name = string_member(function, "name", &context)?;
        validate_name(name, "LLVM function name")?;
        let function_filenames = array_member(function, "filenames", &context)?;
        if function_filenames.is_empty() {
            return Err(invalid_data("LLVM function has no filenames"));
        }
        let mut identity = vec![name.to_owned()];
        for filename in function_filenames {
            let filename = filename
                .as_str()
                .ok_or_else(|| invalid_data("LLVM function filename is not a string"))?;
            validate_name(filename, "LLVM function filename")?;
            identity.push(filename.to_owned());
        }
        if !function_identities.insert(identity) {
            return Err(invalid_data("LLVM function identities must be unique"));
        }
        function_branches = checked_add(
            function_branches,
            array_member(function, "branches", &context)?.len(),
            MAX_BRANCH_RECORDS,
            "function branch records",
        )?;
        for record in array_member(function, "mcdc_records", &context)? {
            let key = serde_json::to_string(record)?;
            *function_records.entry(key).or_default() += 1;
        }
    }
    if file_records != function_records {
        return Err(invalid_data(
            "LLVM file and function MC/DC projections do not match",
        ));
    }
    if file_branches != function_branches {
        return Err(invalid_data(
            "LLVM file and function branch projections do not match",
        ));
    }
    if stats.decisions == 0 || stats.conditions == 0 {
        return Err(invalid_data("LLVM export contains no MC/DC decisions"));
    }

    Ok(TransportSummary {
        export_type: export_type.to_owned(),
        export_version: export_version.to_owned(),
        byte_length: u64::try_from(bytes.len())?,
        sha256: lower_hex(Sha256::digest(bytes)),
        file_count: files.len(),
        function_count: functions.len(),
        branch_record_count: file_branches,
        decision_record_count: stats.decisions,
        condition_count: stats.conditions,
        covered_condition_pair_count: stats.covered_pairs,
        executed_vector_count: stats.executed_vectors,
        not_evaluated_condition_state_count: stats.not_evaluated_states,
        llvm_condition_pair_coverage_complete: stats.covered_pairs == stats.conditions,
        semantic_census_complete: false,
        independent_proof_policy_applied: false,
    })
}

fn parse_record(record: &Value, stats: &mut RecordStats) -> Result<()> {
    let record = record
        .as_array()
        .ok_or_else(|| invalid_data("LLVM MC/DC record is not an array"))?;
    if record.len() != 11 {
        return Err(invalid_data("LLVM MC/DC record does not have 11 fields"));
    }
    for value in &record[..9] {
        value
            .as_u64()
            .ok_or_else(|| invalid_data("LLVM MC/DC scalar field is not an unsigned integer"))?;
    }
    let pairs = record[9]
        .as_array()
        .ok_or_else(|| invalid_data("LLVM MC/DC condition-pair field is not an array"))?;
    if pairs.is_empty() || pairs.len() > MAX_CONDITIONS_PER_DECISION {
        return Err(invalid_data(
            "LLVM MC/DC condition count is outside the supported slice",
        ));
    }
    let covered = pairs
        .iter()
        .map(|value| {
            value
                .as_bool()
                .ok_or_else(|| invalid_data("LLVM MC/DC condition-pair value is not Boolean"))
        })
        .collect::<Result<Vec<_>>>()?;
    let vectors = record[10]
        .as_array()
        .ok_or_else(|| invalid_data("LLVM MC/DC test-vector field is not an array"))?;
    if vectors.len() > MAX_VECTORS_PER_DECISION {
        return Err(invalid_data("LLVM MC/DC test-vector limit exceeded"));
    }
    for vector in vectors {
        let vector = object(vector, "MC/DC test vector")?;
        require_exact_keys(
            vector,
            &["conditions", "executed", "result"],
            "MC/DC test vector",
        )?;
        if vector.get("executed").and_then(Value::as_bool) != Some(true) {
            return Err(invalid_data(
                "diagnostic export must contain executed MC/DC vectors only",
            ));
        }
        if vector.get("result").and_then(Value::as_bool).is_none() {
            return Err(invalid_data("executed MC/DC vector result is not Boolean"));
        }
        let states = array_member(vector, "conditions", "MC/DC test vector")?;
        if states.len() != pairs.len() {
            return Err(invalid_data(
                "MC/DC test-vector condition count does not match its decision",
            ));
        }
        for state in states {
            match state {
                Value::Bool(_) => {}
                Value::Null => stats.not_evaluated_states += 1,
                _ => {
                    return Err(invalid_data(
                        "MC/DC condition state is not true, false, or not_evaluated",
                    ));
                }
            }
        }
        stats.executed_vectors += 1;
    }
    stats.decisions += 1;
    stats.conditions = stats
        .conditions
        .checked_add(pairs.len())
        .ok_or_else(|| invalid_data("MC/DC condition count overflowed"))?;
    stats.covered_pairs = stats
        .covered_pairs
        .checked_add(covered.into_iter().filter(|value| *value).count())
        .ok_or_else(|| invalid_data("MC/DC covered-pair count overflowed"))?;
    Ok(())
}

fn object<'a>(value: &'a Value, context: &str) -> Result<&'a Map<String, Value>> {
    value
        .as_object()
        .ok_or_else(|| invalid_data(format!("{context} is not an object")))
}

fn require_exact_keys(map: &Map<String, Value>, expected: &[&str], context: &str) -> Result<()> {
    let actual = map.keys().map(String::as_str).collect::<BTreeSet<_>>();
    let expected = expected.iter().copied().collect::<BTreeSet<_>>();
    if actual != expected {
        return Err(invalid_data(format!(
            "{context} has unexpected object members"
        )));
    }
    Ok(())
}

fn string_member<'a>(map: &'a Map<String, Value>, name: &str, context: &str) -> Result<&'a str> {
    map.get(name)
        .and_then(Value::as_str)
        .ok_or_else(|| invalid_data(format!("{context}.{name} is not a string")))
}

fn array_member<'a>(
    map: &'a Map<String, Value>,
    name: &str,
    context: &str,
) -> Result<&'a Vec<Value>> {
    map.get(name)
        .and_then(Value::as_array)
        .ok_or_else(|| invalid_data(format!("{context}.{name} is not an array")))
}

fn validate_name(name: &str, label: &str) -> Result<()> {
    if name.is_empty() || name.len() > MAX_NAME_BYTES || name.chars().any(char::is_control) {
        return Err(invalid_data(format!(
            "{label} is empty, oversized, or contains controls"
        )));
    }
    Ok(())
}

fn checked_add(current: usize, add: usize, maximum: usize, label: &str) -> Result<usize> {
    let value = current
        .checked_add(add)
        .ok_or_else(|| invalid_data(format!("{label} count overflowed")))?;
    if value > maximum {
        return Err(invalid_data(format!("{label} limit exceeded")));
    }
    Ok(value)
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut file = File::open(path).map_err(|error| {
        invalid_data(format!(
            "cannot open rustc binary {}: {error}",
            path.display()
        ))
    })?;
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(lower_hex(digest.finalize()))
}

fn write_new_report(path: &Path, report: &DiagnosticReport) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(report)?;
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(&bytes)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    Ok(())
}

#[cfg(unix)]
fn widen_filesystem_counter<T: Into<u64>>(value: T) -> u64 {
    value.into()
}

#[cfg(unix)]
fn available_space(path: &Path) -> Result<u64> {
    use std::os::unix::ffi::OsStrExt as _;

    let path = CString::new(path.as_os_str().as_bytes())?;
    let mut statistics = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    let result = unsafe {
        // SAFETY: `path` is NUL-terminated and `statistics` is writable output storage.
        libc::statvfs(path.as_ptr(), statistics.as_mut_ptr())
    };
    if result != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let statistics = unsafe {
        // SAFETY: a successful statvfs call initialized the output structure.
        statistics.assume_init()
    };
    widen_filesystem_counter(statistics.f_bavail)
        .checked_mul(widen_filesystem_counter(statistics.f_frsize))
        .ok_or_else(|| invalid_data("available-space calculation overflowed"))
}

#[cfg(windows)]
fn available_space(path: &Path) -> Result<u64> {
    use std::os::windows::ffi::OsStrExt as _;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;

    let path = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let mut available = 0u64;
    let result = unsafe {
        // SAFETY: `path` is NUL-terminated and `available` is writable output storage.
        GetDiskFreeSpaceExW(
            path.as_ptr(),
            &mut available,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if result == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(available)
}

#[cfg(not(any(unix, windows)))]
fn available_space(_path: &Path) -> Result<u64> {
    Err(invalid_data(
        "MC/DC diagnostics support free-space admission on Unix and Windows only",
    ))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn sample_export() -> Value {
        let record = json!([
            1, 1, 1, 20, 1, 1, 0, 0, 4,
            [true, false],
            [{"conditions": [true, null], "executed": true, "result": false}]
        ]);
        json!({
            "type": LLVM_EXPORT_TYPE,
            "version": LLVM_EXPORT_VERSION,
            "data": [{
                "files": [{
                    "branches": [],
                    "filename": "workspace/src/lib.rs",
                    "mcdc_records": [record.clone()],
                    "segments": [],
                    "summary": {}
                }],
                "functions": [{
                    "branches": [],
                    "count": 1,
                    "filenames": ["workspace/src/lib.rs"],
                    "mcdc_records": [record],
                    "name": "sample",
                    "regions": []
                }],
                "totals": {}
            }]
        })
    }

    #[test]
    fn accepts_pinned_transport_and_preserves_tri_state_counts() {
        let bytes = serde_json::to_vec(&sample_export()).unwrap();
        let summary = validate_export(&bytes).unwrap();
        assert_eq!(summary.decision_record_count, 1);
        assert_eq!(summary.condition_count, 2);
        assert_eq!(summary.covered_condition_pair_count, 1);
        assert_eq!(summary.executed_vector_count, 1);
        assert_eq!(summary.not_evaluated_condition_state_count, 1);
        assert!(!summary.llvm_condition_pair_coverage_complete);
        assert!(!summary.semantic_census_complete);
        assert!(!summary.independent_proof_policy_applied);
    }

    #[test]
    fn rejects_legacy_export_version() {
        let mut export = sample_export();
        export["version"] = json!("3.0.1");
        assert!(validate_export(&serde_json::to_vec(&export).unwrap()).is_err());
    }

    #[test]
    fn rejects_disagreeing_file_and_function_projections() {
        let mut export = sample_export();
        export["data"][0]["functions"][0]["mcdc_records"][0][0] = json!(2);
        assert!(validate_export(&serde_json::to_vec(&export).unwrap()).is_err());
    }

    #[test]
    fn parses_and_checks_exact_compiler_identity() {
        let identity = parse_rustc_identity(
            "rustc 1.98.1 (48a229cea 2026-09-01)\n\
             binary: rustc\n\
             commit-hash: 48a229ceaefd4985c50990b14116b6d856af0985\n\
             host: x86_64-pc-windows-msvc\n\
             release: 1.98.1\n\
             LLVM version: 22.1.8\n",
        )
        .unwrap();
        validate_rustc_identity(&identity).unwrap();
    }

    #[test]
    fn source_status_allows_only_untracked_paths() {
        assert_eq!(
            validate_source_status(b"?? artifacts/report.json\0").unwrap(),
            1
        );
        assert!(validate_source_status(b" M src/lib.rs\0").is_err());
    }
}
