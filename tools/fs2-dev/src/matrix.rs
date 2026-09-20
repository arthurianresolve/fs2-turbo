use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::ffi::OsStr;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::process;
use crate::{Result, invalid_data};

const MATRIX_TARGET_EXPRESSION: &str = "${{ matrix.target }}";
const PRIMARY_COVERAGE_TOOLCHAIN: &str = "1.98.1";
const REVIEWED_PACKAGE_LIST_COMMAND: &str =
    "cargo package --locked --list > \"$RUNNER_TEMP/package-files.txt\"";

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd)]
#[serde(rename_all = "kebab-case")]
enum EvidenceLevel {
    Runtime,
    Compile,
    NotCovered,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
enum AllocationCapability {
    PhysicalReservation,
    Unsupported,
    Unknown,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
enum Runner {
    #[serde(rename = "macos-15-intel")]
    MacOsIntel,
    #[serde(rename = "macos-latest")]
    MacOs,
    #[serde(rename = "ubuntu-latest")]
    Ubuntu,
    #[serde(rename = "windows-latest")]
    Windows,
}

impl Runner {
    const fn coverage_as_str(self) -> &'static str {
        match self {
            Self::MacOsIntel => "macos-15-intel",
            Self::MacOs => "macos-26",
            Self::Ubuntu => "ubuntu-24.04",
            Self::Windows => "windows-2025-vs2026",
        }
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::MacOsIntel => "macos-15-intel",
            Self::MacOs => "macos-latest",
            Self::Ubuntu => "ubuntu-latest",
            Self::Windows => "windows-latest",
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SupportRegistry {
    version: u64,
    coverage_toolchain: String,
    evidence_levels: Vec<EvidenceLevel>,
    targets: Vec<TargetSpec>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TargetSpec {
    target: String,
    platform: String,
    evidence: EvidenceLevel,
    allocation: AllocationCapability,
    ci: Option<CiSpec>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CiSpec {
    job: String,
    runner: Runner,
    toolchains: Vec<String>,
    #[serde(default)]
    coverage: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct Matrix {
    include: Vec<MatrixEntry>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct MatrixEntry {
    os: String,
    target: String,
    toolchain: String,
}

#[derive(Deserialize)]
struct CargoMetadata {
    packages: Vec<CargoPackage>,
}

#[derive(Deserialize)]
struct CargoPackage {
    name: String,
    rust_version: Option<String>,
}

#[derive(Clone, Copy)]
enum WorkflowFile {
    Ci,
    ReleaseGates,
    Policy,
}

pub(crate) fn run(root: &Path, github_output: Option<&Path>) -> Result<()> {
    validate_xtask_alias(root)
        .and_then(|()| package_rust_version(root))
        .and_then(|rust_version| {
            load_registry(&root.join("support-matrix.json")).and_then(|registry| {
                validate_registry(&registry, &rust_version)
                    .and_then(|()| validate_workflow_directory(root, &registry))
                    .and_then(|()| {
                        let generated = matrices(&registry);
                        if let Some(path) = github_output {
                            write_github_output(path, &generated, &rust_version)
                        } else {
                            let rendered = serde_json::to_string_pretty(&generated)
                                .expect("support matrices contain only JSON-serializable values");
                            println!("{rendered}");
                            Ok(())
                        }
                    })
            })
        })
}

fn validate_workflow_directory(root: &Path, registry: &SupportRegistry) -> Result<()> {
    let directory = root.join(".github/workflows");
    fs::read_dir(&directory)
        .map_err(crate::DynError::from)
        .and_then(|entries| {
            entries
                .collect::<io::Result<Vec<_>>>()
                .map_err(crate::DynError::from)
        })
        .and_then(|mut entries| {
            entries.sort_by_key(|entry| entry.file_name());
            entries
                .into_iter()
                .try_fold((false, false), |(found_ci, found_release_gates), entry| {
                    validate_workflow_entry(registry, entry).map(|kind| match kind {
                        WorkflowFile::Ci => (true, found_release_gates),
                        WorkflowFile::ReleaseGates => (found_ci, true),
                        WorkflowFile::Policy => (found_ci, found_release_gates),
                    })
                })
                .and_then(|(found_ci, found_release_gates)| {
                    if found_ci && found_release_gates {
                        Ok(())
                    } else {
                        Err(invalid_data(
                            "workflow directory must contain ci.yml and release-gates.yml",
                        ))
                    }
                })
        })
}

fn validate_workflow_entry(
    registry: &SupportRegistry,
    entry: fs::DirEntry,
) -> Result<WorkflowFile> {
    let path = entry.path();
    entry
        .file_type()
        .map_err(crate::DynError::from)
        .and_then(|file_type| {
            workflow_entry_is_windows_reparse_point(&path).and_then(|reparse_point| {
                if file_type.is_symlink() || reparse_point || !file_type.is_file() {
                    return Err(invalid_data(format!(
                        "workflow directory contains a link or non-file entry: {}",
                        path.display()
                    )));
                }
                if !matches!(
                    path.extension(),
                    Some(extension)
                        if extension == OsStr::new("yml") || extension == OsStr::new("yaml")
                ) {
                    return Err(invalid_data(format!(
                        "workflow directory contains an unexpected file: {}",
                        path.display()
                    )));
                }
                workflow_file_name(&path).and_then(|name| {
                    load_workflow(&path).and_then(|workflow| match name {
                        "ci.yml" => {
                            validate_workflow(registry, &workflow).map(|()| WorkflowFile::Ci)
                        }
                        "release-gates.yml" => validate_release_workflow(&workflow)
                            .map(|()| WorkflowFile::ReleaseGates),
                        _ => validate_workflow_policy(&workflow).map(|_| WorkflowFile::Policy),
                    })
                })
            })
        })
}

#[cfg(windows)]
fn workflow_entry_is_windows_reparse_point(path: &Path) -> Result<bool> {
    use std::os::windows::fs::MetadataExt as _;
    use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;

    fs::symlink_metadata(path)
        .map(|metadata| metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0)
        .map_err(crate::DynError::from)
}

#[cfg(not(windows))]
fn workflow_entry_is_windows_reparse_point(_path: &Path) -> Result<bool> {
    Ok(false)
}

fn workflow_file_name(path: &Path) -> Result<&str> {
    let Some(name) = path.file_name().and_then(OsStr::to_str) else {
        return Err(invalid_data("workflow file name is not valid Unicode"));
    };
    Ok(name)
}

fn load_registry(path: &Path) -> Result<SupportRegistry> {
    fs::read_to_string(path)
        .map_err(crate::DynError::from)
        .and_then(|contents| serde_json::from_str(&contents).map_err(crate::DynError::from))
}

fn load_workflow(path: &Path) -> Result<Value> {
    fs::read_to_string(path)
        .map_err(crate::DynError::from)
        .and_then(|contents| serde_yaml_ng::from_str(&contents).map_err(crate::DynError::from))
}

fn package_rust_version(root: &Path) -> Result<String> {
    let mut command = process::cargo();
    command
        .current_dir(root)
        .args(["metadata", "--no-deps", "--format-version", "1", "--locked"]);
    let output = process::capture(&mut command, "read fs2-turbo package metadata")?;
    rust_version_from_metadata(&output.stdout)
}

fn rust_version_from_metadata(bytes: &[u8]) -> Result<String> {
    let metadata: CargoMetadata = serde_json::from_slice(bytes)?;
    let Some(version) = metadata
        .packages
        .into_iter()
        .find(|package| package.name == "fs2-turbo")
        .and_then(|package| package.rust_version)
    else {
        return Err(invalid_data(
            "cargo metadata did not provide fs2-turbo rust-version",
        ));
    };
    Ok(version)
}

fn validate_registry(registry: &SupportRegistry, rust_version: &str) -> Result<()> {
    if registry.version != 6 {
        return Err(invalid_data("support matrix version must be 6"));
    }
    if registry.coverage_toolchain != PRIMARY_COVERAGE_TOOLCHAIN {
        return Err(invalid_data(format!(
            "coverage toolchain must be pinned to Rust {PRIMARY_COVERAGE_TOOLCHAIN}"
        )));
    }
    let levels = registry
        .evidence_levels
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();
    if levels
        != [
            EvidenceLevel::Runtime,
            EvidenceLevel::Compile,
            EvidenceLevel::NotCovered,
        ]
        .into_iter()
        .collect()
    {
        return Err(invalid_data(
            "evidence_levels must contain runtime, compile, and not-covered",
        ));
    }
    if registry.targets.is_empty() {
        return Err(invalid_data("targets must be a non-empty list"));
    }

    let mut targets = HashSet::new();
    let mut has_runtime = false;
    let mut has_coverage = false;
    for entry in &registry.targets {
        if !is_target_triple(&entry.target) {
            return Err(invalid_data(format!(
                "target is not approved: {:?}",
                entry.target
            )));
        }
        if !targets.insert(entry.target.as_str()) {
            return Err(invalid_data(format!(
                "target must be unique: {:?}",
                entry.target
            )));
        }
        if entry.platform.is_empty() {
            return Err(invalid_data(format!(
                "platform must be non-empty for {}",
                entry.target
            )));
        }
        has_runtime |= entry.evidence == EvidenceLevel::Runtime;
        if entry.evidence == EvidenceLevel::NotCovered {
            if entry.allocation != AllocationCapability::Unknown || entry.ci.is_some() {
                return Err(invalid_data(format!(
                    "not-covered target {} must use unknown allocation and no CI",
                    entry.target
                )));
            }
            continue;
        }
        if entry.allocation == AllocationCapability::Unknown {
            return Err(invalid_data(format!(
                "covered target {} must declare allocation capability",
                entry.target
            )));
        }
        let Some(ci) = entry.ci.as_ref() else {
            return Err(invalid_data(format!(
                "CI metadata missing for {}",
                entry.target
            )));
        };
        if !is_ci_job_name(&ci.job) {
            return Err(invalid_data(format!(
                "invalid CI job name for {}",
                entry.target
            )));
        }
        if ci.toolchains.is_empty() {
            return Err(invalid_data(format!(
                "toolchains missing for {}",
                entry.target
            )));
        }
        if entry.evidence == EvidenceLevel::Runtime
            && ci.toolchains != [rust_version.to_owned(), "stable".to_owned()]
        {
            return Err(invalid_data(format!(
                "runtime target {} must use Rust {rust_version} and stable",
                entry.target
            )));
        }
        if entry.evidence == EvidenceLevel::Runtime
            && expected_runtime_runner(&entry.target) != Some(ci.runner)
        {
            return Err(invalid_data(format!(
                "runtime target {} uses the wrong native runner",
                entry.target
            )));
        }
        if entry.evidence == EvidenceLevel::Compile
            && ci.toolchains != [rust_version.to_owned()]
            && ci.toolchains != ["nightly".to_owned()]
        {
            return Err(invalid_data(format!(
                "compile target {} must use Rust {rust_version} or nightly",
                entry.target
            )));
        }
        if ci.coverage && entry.evidence != EvidenceLevel::Runtime {
            return Err(invalid_data(format!(
                "compile target {} cannot provide native coverage",
                entry.target
            )));
        }
        has_coverage |= ci.coverage;
    }
    if !has_runtime {
        return Err(invalid_data("at least one runtime target is required"));
    }
    if !has_coverage {
        return Err(invalid_data(
            "at least one native coverage target is required",
        ));
    }
    Ok(())
}

fn matrices(registry: &SupportRegistry) -> BTreeMap<String, Matrix> {
    let mut generated = BTreeMap::<String, Matrix>::new();
    for entry in &registry.targets {
        let Some(ci) = &entry.ci else { continue };
        let matrix = generated.entry(ci.job.clone()).or_insert_with(|| Matrix {
            include: Vec::new(),
        });
        for toolchain in &ci.toolchains {
            matrix.include.push(MatrixEntry {
                os: ci.runner.as_str().to_owned(),
                target: entry.target.clone(),
                toolchain: toolchain.clone(),
            });
        }
    }
    generated.insert(
        "coverage".to_owned(),
        Matrix {
            include: registry
                .targets
                .iter()
                .filter_map(|entry| {
                    let ci = entry.ci.as_ref()?;
                    ci.coverage.then(|| MatrixEntry {
                        os: ci.runner.coverage_as_str().to_owned(),
                        target: entry.target.clone(),
                        toolchain: registry.coverage_toolchain.clone(),
                    })
                })
                .collect(),
        },
    );
    generated
}

fn validate_workflow(registry: &SupportRegistry, workflow: &Value) -> Result<()> {
    let jobs = validate_workflow_policy(workflow)?;
    let generated = matrices(registry);
    let declared = generated.keys().map(String::as_str).collect::<HashSet<_>>();

    for (job_name, job) in jobs {
        let job = job.as_object().expect("workflow policy validated each job");
        let configured = job
            .get("strategy")
            .and_then(Value::as_object)
            .and_then(|strategy| strategy.get("matrix"));
        match configured {
            Some(Value::String(expression)) if expression.contains("fromJSON") => {
                return Err(invalid_data(format!(
                    "workflow must not consume a runtime-generated matrix: {expression}"
                )));
            }
            Some(configured) if declared.contains(job_name.as_str()) => {
                let expected = serde_json::to_value(&generated[job_name])
                    .expect("support matrices contain only JSON-serializable values");
                if configured != &expected {
                    return Err(invalid_data(format!(
                        "workflow job {job_name} literal matrix drifted from support data"
                    )));
                }
            }
            None if declared.contains(job_name.as_str()) => {
                return Err(invalid_data(format!(
                    "workflow job {job_name} must define a literal support matrix"
                )));
            }
            _ => {}
        }
    }
    let missing = declared
        .into_iter()
        .filter(|job| !jobs.contains_key(*job))
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        return Err(invalid_data(format!(
            "workflow support jobs are missing: {missing:?}"
        )));
    }

    let Some(triggers) = workflow.get("on").and_then(Value::as_object) else {
        return Err(invalid_data("workflow must define triggers"));
    };
    if !triggers.contains_key("workflow_dispatch") {
        return Err(invalid_data("workflow must retain a manual trigger"));
    }
    if triggers.get("schedule") != Some(&serde_json::json!([{ "cron": "17 1 1 * *" }])) {
        return Err(invalid_data("workflow must retain the monthly canary"));
    }
    Ok(())
}

fn validate_workflow_policy(workflow: &Value) -> Result<&serde_json::Map<String, Value>> {
    let Some(permissions) = workflow.get("permissions").and_then(Value::as_object) else {
        return Err(invalid_data(
            "workflow must declare top-level token permissions",
        ));
    };
    if permissions.len() != 1 || permissions.get("contents").and_then(Value::as_str) != Some("read")
    {
        return Err(invalid_data(
            "workflow token permissions must be exactly contents: read",
        ));
    }
    let Some(jobs) = workflow.get("jobs").and_then(Value::as_object) else {
        return Err(invalid_data("workflow must define a jobs object"));
    };
    jobs.iter()
        .try_for_each(|(job_name, job)| validate_workflow_job(job_name, job))
        .map(|()| jobs)
}

fn validate_workflow_job(job_name: &str, job: &Value) -> Result<()> {
    let Some(job) = job.as_object() else {
        return Err(invalid_data(format!(
            "workflow job {job_name} must be an object"
        )));
    };
    if job.contains_key("permissions") {
        return Err(invalid_data(format!(
            "workflow job {job_name} may not override token permissions"
        )));
    }
    let action_result = job
        .get("uses")
        .and_then(Value::as_str)
        .map_or(Ok(()), validate_action);
    action_result.and_then(|()| {
        let Some(steps) = job.get("steps") else {
            return Ok(());
        };
        let Some(steps) = steps.as_array() else {
            return Err(invalid_data(format!(
                "workflow job {job_name} steps must be a list"
            )));
        };
        steps
            .iter()
            .try_for_each(|step| validate_workflow_step(job_name, step))
    })
}

fn validate_workflow_step(job_name: &str, step: &Value) -> Result<()> {
    let Some(step) = step.as_object() else {
        return Err(invalid_data(format!(
            "workflow job {job_name} contains an invalid step"
        )));
    };
    let action_result = match step.get("uses").and_then(Value::as_str) {
        Some(action) => validate_action(action).and_then(|()| {
            if action_repository(action) == Some("actions/checkout") {
                validate_checkout_credentials(job_name, step)
            } else {
                Ok(())
            }
        }),
        None => Ok(()),
    };
    action_result.and_then(|()| {
        let Some(command) = step.get("run").and_then(Value::as_str) else {
            return Ok(());
        };
        if has_unquoted_matrix_target(command) {
            return Err(invalid_data(format!(
                "workflow job {job_name} uses an unquoted matrix target"
            )));
        }
        validate_locked_cargo(job_name, command)
    })
}

fn validate_checkout_credentials(
    job_name: &str,
    step: &serde_json::Map<String, Value>,
) -> Result<()> {
    let Some(inputs) = step.get("with").and_then(Value::as_object) else {
        return Err(invalid_data(format!(
            "workflow checkout in {job_name} must disable credential persistence"
        )));
    };
    if !matches!(inputs.get("persist-credentials"), Some(Value::Bool(false))) {
        return Err(invalid_data(format!(
            "workflow checkout in {job_name} must set persist-credentials to boolean false"
        )));
    }
    Ok(())
}

fn validate_release_workflow(workflow: &Value) -> Result<()> {
    validate_workflow_policy(workflow).and_then(|jobs| {
        let Some(triggers) = workflow.get("on").and_then(Value::as_object) else {
            return Err(invalid_data("release workflow must define triggers"));
        };
        for trigger in ["push", "pull_request", "workflow_dispatch"] {
            if !triggers.contains_key(trigger) {
                return Err(invalid_data(format!(
                    "release workflow must retain the {trigger} trigger"
                )));
            }
        }
        for job in ["toolchains", "package", "dependencies"] {
            if !jobs.contains_key(job) {
                return Err(invalid_data(format!(
                    "release workflow must retain the {job} job"
                )));
            }
        }
        Ok(())
    })
}

fn validate_action(action: &str) -> Result<()> {
    if pinned_action(action) {
        Ok(())
    } else if action.starts_with("./") {
        Err(invalid_data(format!(
            "local workflow action is not recursively policy-validated: {action}"
        )))
    } else {
        Err(invalid_data(format!(
            "workflow action is not pinned to a commit: {action}"
        )))
    }
}

fn action_repository(action: &str) -> Option<&str> {
    action.rsplit_once('@').map(|(repository, _)| repository)
}

fn validate_locked_cargo(job_name: &str, command: &str) -> Result<()> {
    for source_line in command.lines() {
        let line = command_before_comment(source_line)?.trim();
        if line.is_empty() {
            continue;
        }
        if line.contains("$(") || line.contains('`') {
            return Err(invalid_data(format!(
                "workflow command substitution is not auditable in {job_name}: {line}"
            )));
        }
        if command_position_is_dynamic(line) {
            return Err(invalid_data(format!(
                "workflow command-position expansion is not auditable in {job_name}: {line}"
            )));
        }
        let words = line.split_ascii_whitespace().collect::<Vec<_>>();
        if words.first().copied() != Some("cargo") {
            if mentions_cargo_executable(line) {
                return Err(invalid_data(format!(
                    "workflow Cargo invocation is not a direct, auditable command in {job_name}: {line}"
                )));
            }
            continue;
        }
        if !direct_cargo_command_is_auditable(line) {
            return Err(invalid_data(format!(
                "workflow Cargo command contains unsupported shell syntax in {job_name}: {line}"
            )));
        }
        let mut arguments = words[1..].iter().copied();
        let mut subcommand = arguments.next().unwrap_or_default();
        if subcommand.starts_with('+') {
            subcommand = arguments.next().unwrap_or_default();
        }
        let exempt = matches!(subcommand, "audit" | "deny" | "fmt" | "xtask");
        let locked = words[1..].contains(&"--locked");
        if !exempt && !locked {
            return Err(invalid_data(format!(
                "workflow cargo command is not locked in {job_name}: {line}"
            )));
        }
    }
    Ok(())
}

fn command_before_comment(line: &str) -> Result<&str> {
    let mut quote = None;
    for (index, character) in line.char_indices() {
        match quote {
            Some(expected) if character == expected => quote = None,
            Some(_) => {}
            None if matches!(character, '\'' | '"') => quote = Some(character),
            None if character == '#' => return Ok(&line[..index]),
            None => {}
        }
    }
    if quote.is_some() {
        Err(invalid_data(
            "workflow command contains an unterminated quote",
        ))
    } else {
        Ok(line)
    }
}

fn command_position_is_dynamic(line: &str) -> bool {
    let words = line.split_ascii_whitespace().collect::<Vec<_>>();
    let mut index = 0usize;
    while index < words.len() && shell_assignment(words[index]) {
        index += 1;
    }
    loop {
        let Some(word) = words.get(index).copied() else {
            return false;
        };
        if word.contains('$') || word.starts_with(['\'', '"']) {
            return true;
        }
        if !matches!(word, "env" | "command" | "exec") {
            return false;
        }
        index += 1;
        while index < words.len()
            && (words[index].starts_with('-') || shell_assignment(words[index]))
        {
            index += 1;
        }
    }
}

fn shell_assignment(word: &str) -> bool {
    word.split_once('=').is_some_and(|(name, _)| {
        !name.is_empty()
            && name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    })
}

fn direct_cargo_command_is_auditable(line: &str) -> bool {
    if line == REVIEWED_PACKAGE_LIST_COMMAND {
        return true;
    }
    let normalized = line
        .replace(MATRIX_TARGET_EXPRESSION, "")
        .replace("\"$GITHUB_OUTPUT\"", "");
    !normalized.chars().any(|character| {
        matches!(
            character,
            '\\' | ';' | '|' | '&' | '<' | '>' | '`' | '$' | '(' | ')'
        )
    })
}

fn cargo_executable(token: &str) -> bool {
    token.rsplit(['/', '\\']).next().is_some_and(|name| {
        name.eq_ignore_ascii_case("cargo")
            || name.eq_ignore_ascii_case("cargo.exe")
            || name.eq_ignore_ascii_case("cargo.cmd")
    })
}

fn shell_word_skeleton(line: &str) -> String {
    let mut skeleton = String::with_capacity(line.len());
    let mut characters = line.chars().peekable();
    while let Some(character) = characters.next() {
        match character {
            '\\' => {
                if let Some(escaped) = characters.next() {
                    skeleton.push(escaped);
                }
            }
            '\'' | '"' => {}
            '$' if characters.peek() == Some(&'{') => {
                characters.next();
                let mut expansion = String::new();
                for expanded in characters.by_ref() {
                    if expanded == '}' {
                        break;
                    }
                    expansion.push(expanded);
                }
                if expansion.to_ascii_lowercase().contains("cargo") {
                    skeleton.push_str("cargo");
                }
            }
            _ => skeleton.push(character),
        }
    }
    skeleton
}

fn mentions_cargo_executable(line: &str) -> bool {
    let skeleton = shell_word_skeleton(line);
    skeleton
        .split(|character: char| {
            character.is_ascii_whitespace()
                || matches!(character, '=' | ';' | '|' | '&' | '(' | ')')
        })
        .any(|token| {
            let normalized = token.trim_matches(['$', '{', '}']).to_ascii_lowercase();
            [
                cargo_executable(token),
                ["cargo", "env:cargo"].contains(&normalized.as_str()),
            ]
            .contains(&true)
        })
}

fn validate_xtask_alias(root: &Path) -> Result<()> {
    let configuration = fs::read_to_string(root.join(".cargo/config.toml"))?;
    let expected = "xtask = \"run --locked --package fs2-dev --\"";
    if configuration.lines().any(|line| line.trim() == expected) {
        Ok(())
    } else {
        Err(invalid_data(
            "the cargo xtask alias must invoke fs2-dev with --locked",
        ))
    }
}

fn is_target_triple(value: &str) -> bool {
    value.is_ascii()
        && value.split('-').count() >= 3
        && !value.starts_with('-')
        && !value.ends_with('-')
        && value.bytes().all(|byte| {
            [
                byte.is_ascii_lowercase(),
                byte.is_ascii_digit(),
                b"-_".contains(&byte),
            ]
            .contains(&true)
        })
}

fn pinned_action(action: &str) -> bool {
    if matches!(
        action,
        "actions/download-artifact@3e5f45b2cfb9172054b4087a40e8e0b5a5461e7c"
            | "codecov/codecov-action@fb8b3582c8e4def4969c97caa2f19720cb33a72f"
    ) {
        return true;
    }
    action
        .rsplit_once('@')
        .is_some_and(|(repository, revision)| {
            matches!(
                repository,
                "actions/checkout"
                    | "actions/upload-artifact"
                    | "dtolnay/rust-toolchain"
                    | "taiki-e/install-action"
            ) && revision.len() == 40
                && revision.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
}

fn expected_runtime_runner(target: &str) -> Option<Runner> {
    if target.contains("-pc-windows-") {
        Some(Runner::Windows)
    } else if target == "x86_64-apple-darwin" {
        Some(Runner::MacOsIntel)
    } else if target == "aarch64-apple-darwin" {
        Some(Runner::MacOs)
    } else if target.contains("-unknown-linux-") {
        Some(Runner::Ubuntu)
    } else {
        None
    }
}

fn is_ci_job_name(value: &str) -> bool {
    value.is_ascii()
        && value
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_alphanumeric())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

fn has_unquoted_matrix_target(command: &str) -> bool {
    command
        .match_indices(MATRIX_TARGET_EXPRESSION)
        .any(|(start, _)| {
            let end = start + MATRIX_TARGET_EXPRESSION.len();
            let token_start = command[..start]
                .rfind(char::is_whitespace)
                .map_or(0, |index| index + 1);
            let token_end = command[end..]
                .find(char::is_whitespace)
                .map_or(command.len(), |index| end + index);
            let token = &command[token_start..token_end];
            !((token.starts_with('"') && token.ends_with('"'))
                || (token.starts_with('\'') && token.ends_with('\'')))
        })
}

fn write_github_output(
    path: &Path,
    generated: &BTreeMap<String, Matrix>,
    rust_version: &str,
) -> Result<()> {
    let mut output = OpenOptions::new().create(true).append(true).open(path)?;
    let generated = serde_json::to_string(generated)
        .expect("support matrices contain only JSON-serializable values");
    writeln!(output, "matrices={generated}")
        .and_then(|()| writeln!(output, "rust_version={rust_version}"))
        .map_err(crate::DynError::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejection_guards_preserve_error_kinds_and_context() {
        let assert_invalid = |error: crate::DynError, expected: &str| {
            assert_eq!(
                error.downcast_ref::<std::io::Error>().unwrap().kind(),
                std::io::ErrorKind::InvalidData
            );
            assert_eq!(error.to_string(), expected);
        };
        assert_invalid(
            workflow_file_name(Path::new("")).unwrap_err(),
            "workflow file name is not valid Unicode",
        );
        assert_invalid(
            rust_version_from_metadata(
                br#"{"packages":[{"name":"fs2-turbo"},{"name":"fs2-turbo","rust_version":"1.88.0"}]}"#,
            )
            .unwrap_err(),
            "cargo metadata did not provide fs2-turbo rust-version",
        );

        let registry: SupportRegistry = serde_json::from_value(fixture_registry_value()).unwrap();
        let mut missing_ci = registry.clone();
        missing_ci.targets[0].ci = None;
        assert_invalid(
            validate_registry(&missing_ci, "1.88.0").unwrap_err(),
            "CI metadata missing for x86_64-unknown-linux-gnu",
        );
        let mut workflow = fixture_workflow(&registry);
        workflow["on"] = Value::Null;
        assert_invalid(
            validate_workflow(&registry, &workflow).unwrap_err(),
            "workflow must define triggers",
        );

        for (workflow, expected) in [
            (
                serde_json::json!({}),
                "workflow must declare top-level token permissions",
            ),
            (
                serde_json::json!({"permissions":{"contents":"read"}}),
                "workflow must define a jobs object",
            ),
            (
                serde_json::json!({"permissions":{"contents":"read"},"jobs":{"guard":null}}),
                "workflow job guard must be an object",
            ),
            (
                serde_json::json!({"permissions":{"contents":"read"},"jobs":{"guard":{"steps":null}}}),
                "workflow job guard steps must be a list",
            ),
            (
                serde_json::json!({"permissions":{"contents":"read"},"jobs":{"guard":{"steps":[null]}}}),
                "workflow job guard contains an invalid step",
            ),
        ] {
            assert_invalid(validate_workflow_policy(&workflow).unwrap_err(), expected);
        }

        assert_invalid(
            validate_checkout_credentials("guard", &serde_json::Map::new()).unwrap_err(),
            "workflow checkout in guard must disable credential persistence",
        );
        let release = serde_json::json!({"permissions":{"contents":"read"},"jobs":{}});
        assert_invalid(
            validate_release_workflow(&release).unwrap_err(),
            "release workflow must define triggers",
        );
    }

    #[test]
    fn matrix_entrypoint_preserves_output_when_repository_validation_fails() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("github output with spaces");
        fs::write(&path, "existing=value\n").unwrap();
        run(crate::repository_root(), Some(&path)).unwrap();
        let previous = fs::read_to_string(&path).unwrap();
        let lines = previous.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0], "existing=value");
        assert_eq!(lines[2], "rust_version=1.88.0");
        let generated: Value =
            serde_json::from_str(lines[1].strip_prefix("matrices=").unwrap()).unwrap();
        assert_eq!(
            generated,
            serde_json::to_value(matrices(&repository_registry())).unwrap()
        );
        run(crate::repository_root(), None).unwrap();

        assert!(run(directory.path(), Some(&path)).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), previous);
        let error = package_rust_version(directory.path())
            .unwrap_err()
            .to_string();
        assert!(error.contains("read fs2-turbo package metadata"), "{error}");
        assert_eq!(fs::read_to_string(&path).unwrap(), previous);
    }

    #[test]
    fn workflow_policy_rejects_missing_and_nonobject_jobs() {
        let mut missing = minimal_policy_workflow();
        missing.as_object_mut().unwrap().remove("jobs");
        assert!(
            validate_workflow_policy(&missing)
                .unwrap_err()
                .to_string()
                .contains("jobs object")
        );
        for jobs in [
            Value::Null,
            serde_json::json!([]),
            serde_json::json!({"test": null}),
        ] {
            let workflow = serde_json::json!({
                "permissions": {"contents": "read"},
                "jobs": jobs
            });
            assert!(validate_workflow_policy(&workflow).is_err());
        }
    }

    #[test]
    fn release_workflow_rejects_missing_and_nonobject_triggers() {
        let workflow = serde_json::json!({
            "permissions": {"contents": "read"},
            "jobs": {"toolchains": {}, "package": {}, "dependencies": {}}
        });
        assert!(
            validate_release_workflow(&workflow)
                .unwrap_err()
                .to_string()
                .contains("must define triggers")
        );
        for triggers in [
            Value::Null,
            serde_json::json!([]),
            serde_json::json!("push"),
        ] {
            let mut invalid = workflow.clone();
            invalid["on"] = triggers;
            assert!(
                validate_release_workflow(&invalid)
                    .unwrap_err()
                    .to_string()
                    .contains("must define triggers")
            );
        }
    }

    #[test]
    fn shell_policy_handles_comments_and_rejects_unterminated_quotes() {
        for command in [
            "",
            " \n\t",
            "# comment",
            "cargo fmt # comment",
            "echo 'literal # sign'",
        ] {
            validate_locked_cargo("fixture", command).unwrap();
        }
        for command in ["cargo test --locked '", "echo \"unterminated"] {
            assert!(
                validate_locked_cargo("fixture", command)
                    .unwrap_err()
                    .to_string()
                    .contains("unterminated quote")
            );
        }
    }

    #[test]
    fn metadata_requires_the_library_package_and_explicit_msrv() {
        for bytes in [
            br#"{"packages":[]}"#.as_slice(),
            br#"{"packages":[{"name":"other","rust_version":"1.88.0"}]}"#.as_slice(),
            br#"{"packages":[{"name":"fs2-turbo","rust_version":null}]}"#.as_slice(),
        ] {
            assert!(
                rust_version_from_metadata(bytes)
                    .unwrap_err()
                    .to_string()
                    .contains("rust-version")
            );
        }
        assert!(rust_version_from_metadata(b"not JSON").is_err());
        assert_eq!(
            rust_version_from_metadata(
                br#"{"packages":[{"name":"fs2-turbo","rust_version":"1.88.0"}]}"#
            )
            .unwrap(),
            "1.88.0"
        );
    }

    #[test]
    fn workflow_directory_rejects_missing_gates_and_nonfiles() {
        let registry: SupportRegistry = serde_json::from_value(fixture_registry_value()).unwrap();
        let temporary = tempfile::tempdir().unwrap();
        let directory = temporary.path().join(".github/workflows");
        fs::create_dir_all(&directory).unwrap();
        assert!(
            validate_workflow_directory(temporary.path(), &registry)
                .unwrap_err()
                .to_string()
                .contains("must contain")
        );
        fs::create_dir(directory.join("unexpected.yml")).unwrap();
        assert!(
            validate_workflow_directory(temporary.path(), &registry)
                .unwrap_err()
                .to_string()
                .contains("non-file")
        );
    }

    #[test]
    fn workflow_names_must_be_present_and_unicode() {
        assert_eq!(workflow_file_name(Path::new("ci.yml")).unwrap(), "ci.yml");
        assert!(workflow_file_name(Path::new("")).is_err());
        #[cfg(windows)]
        let invalid = {
            use std::os::windows::ffi::OsStringExt as _;
            std::ffi::OsString::from_wide(&[0xd800, u16::from(b'.'), u16::from(b'y')])
        };
        #[cfg(unix)]
        let invalid = {
            use std::os::unix::ffi::OsStringExt as _;
            std::ffi::OsString::from_vec(vec![0xff, b'.', b'y'])
        };
        assert!(workflow_file_name(Path::new(&invalid)).is_err());
    }

    #[test]
    fn shell_boundaries_remain_fail_closed_for_partial_and_escaped_words() {
        for line in ["", "NAME=value", "env -i NAME=value command -- exec"] {
            assert!(!command_position_is_dynamic(line), "{line}");
        }
        for line in [
            "$CARGO",
            "'cargo'",
            "env -i NAME=value command -- exec $CARGO",
        ] {
            assert!(command_position_is_dynamic(line), "{line}");
        }
        assert_eq!(shell_word_skeleton("ca\\rgo"), "cargo");
        assert_eq!(shell_word_skeleton("\\"), "");
        assert_eq!(shell_word_skeleton("${CARGO}"), "cargo");
        assert_eq!(shell_word_skeleton("${OTHER}"), "");
        assert_eq!(shell_word_skeleton("${CARGO"), "cargo");
        for executable in ["cargo", "cargo.exe", "CARGO.CMD", "C:\\bin\\cargo.cmd"] {
            assert!(cargo_executable(executable), "{executable}");
        }
        assert!(!shell_assignment("=value"));
        assert!(!shell_assignment("A-B=value"));
        assert!(has_unquoted_matrix_target(MATRIX_TARGET_EXPRESSION));
        assert!(!has_unquoted_matrix_target(&format!(
            "\"{MATRIX_TARGET_EXPRESSION}\""
        )));
        assert!(!has_unquoted_matrix_target(&format!(
            "'{MATRIX_TARGET_EXPRESSION}'"
        )));
        for line in ["cargo", "cargo +stable"] {
            assert!(validate_locked_cargo("fixture", line).is_err());
        }
    }

    fn fixture_registry_value() -> Value {
        serde_json::json!({
            "version": 6,
            "coverage_toolchain": "1.98.1",
            "evidence_levels": ["runtime", "compile", "not-covered"],
            "targets": [
                {
                    "target": "x86_64-unknown-linux-gnu", "platform": "Linux",
                    "evidence": "runtime", "allocation": "physical-reservation",
                    "ci": {
                        "job": "check", "runner": "ubuntu-latest",
                        "toolchains": ["1.88.0", "stable"], "coverage": true
                    }
                },
                {
                    "target": "aarch64-unknown-linux-gnu", "platform": "Linux ARM64",
                    "evidence": "compile", "allocation": "physical-reservation",
                    "ci": {
                        "job": "cross_check", "runner": "ubuntu-latest",
                        "toolchains": ["1.88.0"]
                    }
                },
                {
                    "target": "i686-apple-darwin", "platform": "Legacy macOS",
                    "evidence": "not-covered", "allocation": "unknown", "ci": null
                }
            ]
        })
    }

    #[test]
    fn registry_enforces_each_evidence_and_toolchain_requirement() {
        let original = fixture_registry_value();
        let registry = serde_json::from_value(original.clone()).unwrap();
        validate_registry(&registry, "1.88.0").unwrap();
        let cases = [
            ("/version", serde_json::json!(0)),
            ("/coverage_toolchain", serde_json::json!("stable")),
            (
                "/evidence_levels",
                serde_json::json!(["runtime", "compile"]),
            ),
            ("/targets", serde_json::json!([])),
            ("/targets/0/platform", serde_json::json!("")),
            ("/targets/0/allocation", serde_json::json!("unknown")),
            ("/targets/0/ci", serde_json::Value::Null),
            ("/targets/0/ci/job", serde_json::json!("invalid-job")),
            ("/targets/0/ci/toolchains", serde_json::json!([])),
            ("/targets/0/ci/toolchains", serde_json::json!(["stable"])),
            ("/targets/0/ci/runner", serde_json::json!("windows-latest")),
            ("/targets/1/ci/toolchains", serde_json::json!(["stable"])),
            (
                "/targets/2/allocation",
                serde_json::json!("physical-reservation"),
            ),
            ("/targets/2/ci", original["targets"][0]["ci"].clone()),
        ];
        for (pointer, replacement) in cases {
            let mut altered = original.clone();
            *altered.pointer_mut(pointer).unwrap() = replacement;
            let registry = serde_json::from_value(altered).unwrap();
            assert!(validate_registry(&registry, "1.88.0").is_err(), "{pointer}");
        }
        let mut registry: SupportRegistry = serde_json::from_value(original).unwrap();
        registry.targets[1].ci.as_mut().unwrap().toolchains = vec!["nightly".to_owned()];
        validate_registry(&registry, "1.88.0").unwrap();
        registry.targets[1].ci.as_mut().unwrap().coverage = true;
        assert!(validate_registry(&registry, "1.88.0").is_err());
        registry.targets[1].ci.as_mut().unwrap().coverage = false;
        registry.targets[0].ci.as_mut().unwrap().coverage = false;
        let error = validate_registry(&registry, "1.88.0")
            .unwrap_err()
            .to_string();
        assert!(error.contains("at least one native coverage target"));
        registry.targets.remove(0);
        let error = validate_registry(&registry, "1.88.0")
            .unwrap_err()
            .to_string();
        assert!(error.contains("at least one runtime target"));
    }

    fn fixture_workflow(registry: &SupportRegistry) -> Value {
        let mut workflow = minimal_policy_workflow();
        workflow["on"] = serde_json::json!({
            "workflow_dispatch": {},
            "schedule": [{"cron": "17 1 1 * *"}]
        });
        for (name, matrix) in matrices(registry) {
            workflow["jobs"].as_object_mut().unwrap().insert(
                name,
                serde_json::json!({"strategy": {"matrix": matrix}, "steps": []}),
            );
        }
        workflow
    }

    #[test]
    fn workflow_requires_literal_matrices_complete_jobs_and_canary_triggers() {
        let registry = serde_json::from_value(fixture_registry_value()).unwrap();
        let original = fixture_workflow(&registry);
        validate_workflow(&registry, &original).unwrap();
        for (pointer, replacement) in [
            ("/jobs", serde_json::json!({})),
            ("/jobs/check", serde_json::Value::Null),
            ("/jobs/check/strategy", serde_json::Value::Null),
            (
                "/jobs/check/strategy/matrix",
                serde_json::json!("${{ fromJSON(needs.matrix.outputs.value) }}"),
            ),
            (
                "/jobs/check/strategy/matrix",
                serde_json::json!({"include": []}),
            ),
            ("/on", serde_json::Value::Null),
            (
                "/on",
                serde_json::json!({"schedule": [{"cron": "17 1 1 * *"}]}),
            ),
            ("/on/schedule", serde_json::json!([])),
        ] {
            let mut workflow = original.clone();
            *workflow.pointer_mut(pointer).unwrap() = replacement;
            assert!(
                validate_workflow(&registry, &workflow).is_err(),
                "{pointer}"
            );
        }
    }

    #[test]
    fn workflow_policy_rejects_malformed_and_untrusted_steps() {
        let original = minimal_policy_workflow();
        for (pointer, replacement) in [
            ("/jobs/test/steps", serde_json::json!(0)),
            ("/jobs/test/steps", serde_json::json!([null])),
            ("/jobs/test/steps/0/with", serde_json::Value::Null),
            (
                "/jobs/test/steps/0",
                serde_json::json!({"run": "cargo check --locked --target ${{ matrix.target }}"}),
            ),
            (
                "/jobs/test",
                serde_json::json!({"uses": "untrusted/reusable@main"}),
            ),
        ] {
            let mut workflow = original.clone();
            *workflow.pointer_mut(pointer).unwrap() = replacement;
            assert!(validate_workflow_policy(&workflow).is_err(), "{pointer}");
        }
        let workflow = serde_json::json!({
            "permissions": {"contents": "read"},
            "jobs": {"empty": {}}
        });
        validate_workflow_policy(&workflow).unwrap();
    }

    #[test]
    fn release_workflow_requires_every_trigger_and_gate_job() {
        let original = serde_json::json!({
            "permissions": {"contents": "read"},
            "on": {"push": {}, "pull_request": {}, "workflow_dispatch": {}},
            "jobs": {"toolchains": {}, "package": {}, "dependencies": {}}
        });
        validate_release_workflow(&original).unwrap();
        for trigger in ["push", "pull_request", "workflow_dispatch"] {
            let mut workflow = original.clone();
            workflow["on"].as_object_mut().unwrap().remove(trigger);
            let error = validate_release_workflow(&workflow)
                .unwrap_err()
                .to_string();
            assert!(error.contains(trigger));
        }
        for job in ["toolchains", "package", "dependencies"] {
            let mut workflow = original.clone();
            workflow["jobs"].as_object_mut().unwrap().remove(job);
            let error = validate_release_workflow(&workflow)
                .unwrap_err()
                .to_string();
            assert!(error.contains(job));
        }
    }

    #[test]
    fn xtask_alias_must_remain_locked() {
        let directory = tempfile::tempdir().unwrap();
        assert!(validate_xtask_alias(directory.path()).is_err());
        let cargo = directory.path().join(".cargo");
        fs::create_dir(&cargo).unwrap();
        let path = cargo.join("config.toml");
        fs::write(
            &path,
            "[alias]\n  xtask = \"run --locked --package fs2-dev --\"\n",
        )
        .unwrap();
        validate_xtask_alias(directory.path()).unwrap();
        fs::write(&path, "[alias]\nxtask = \"run --package fs2-dev --\"\n").unwrap();
        assert!(validate_xtask_alias(directory.path()).is_err());
    }

    #[test]
    fn github_output_appends_parseable_matrices_and_preserves_prior_values() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("github-output");
        let registry = serde_json::from_value(fixture_registry_value()).unwrap();
        let generated = matrices(&registry);
        fs::write(&path, "existing=value\n").unwrap();
        write_github_output(&path, &generated, "1.88.0").unwrap();
        let contents = fs::read_to_string(&path).unwrap();
        let lines = contents.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0], "existing=value");
        assert_eq!(lines[2], "rust_version=1.88.0");
        let actual: Value =
            serde_json::from_str(lines[1].strip_prefix("matrices=").unwrap()).unwrap();
        assert_eq!(actual, serde_json::to_value(&generated).unwrap());
        assert!(write_github_output(directory.path(), &generated, "1.88.0").is_err());
    }

    #[test]
    fn runner_names_and_target_mapping_are_explicit() {
        for (runner, normal, coverage) in [
            (Runner::MacOsIntel, "macos-15-intel", "macos-15-intel"),
            (Runner::MacOs, "macos-latest", "macos-26"),
            (Runner::Ubuntu, "ubuntu-latest", "ubuntu-24.04"),
            (Runner::Windows, "windows-latest", "windows-2025-vs2026"),
        ] {
            assert_eq!(runner.as_str(), normal);
            assert_eq!(runner.coverage_as_str(), coverage);
        }
        assert_eq!(expected_runtime_runner("unknown-target-os"), None);
    }

    fn repository_registry() -> SupportRegistry {
        load_registry(&crate::repository_root().join("support-matrix.json")).unwrap()
    }

    #[test]
    fn repository_registry_and_workflow_agree() {
        let registry = repository_registry();
        validate_registry(&registry, "1.88.0").unwrap();
        validate_workflow_directory(crate::repository_root(), &registry).unwrap();
        let workflow =
            load_workflow(&crate::repository_root().join(".github/workflows/ci.yml")).unwrap();
        validate_workflow(&registry, &workflow).unwrap();
        let release_gates =
            load_workflow(&crate::repository_root().join(".github/workflows/release-gates.yml"))
                .unwrap();
        validate_release_workflow(&release_gates).unwrap();
    }

    #[test]
    fn rejects_duplicate_or_unapproved_targets() {
        let mut registry = repository_registry();
        registry.targets[1].target = registry.targets[0].target.clone();
        assert!(validate_registry(&registry, "1.88.0").is_err());
        registry.targets[1].target = "$(echo injected)".to_owned();
        assert!(validate_registry(&registry, "1.88.0").is_err());
    }

    #[test]
    fn rejects_mutable_actions_and_unquoted_targets() {
        assert!(!pinned_action("actions/checkout@v4"));
        assert!(pinned_action(
            "actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1"
        ));
        assert!(!pinned_action(
            "untrusted/example@3d3c42e5aac5ba805825da76410c181273ba90b1"
        ));
        assert!(has_unquoted_matrix_target(
            "cargo check --target ${{ matrix.target }}"
        ));
        assert!(!has_unquoted_matrix_target(
            "cargo check --target \"${{ matrix.target }}\""
        ));
        assert!(validate_locked_cargo("test", "echo preparing\ncargo test").is_err());
        assert!(validate_locked_cargo("test", r"cargo test --locked ; c\argo update").is_err());
        assert!(validate_locked_cargo("test", r#"c'a'rgo update"#).is_err());
        assert!(validate_locked_cargo("test", r#"c${EMPTY}argo update"#).is_err());
        assert!(validate_locked_cargo("test", r#"cargo test --locked "$(printf cargo)""#).is_err());
        assert!(
            validate_locked_cargo(
                "test",
                r#"cargo xtask matrix --github-output "$GITHUB_OUTPUT""#
            )
            .is_ok()
        );
        assert!(validate_locked_cargo("test", REVIEWED_PACKAGE_LIST_COMMAND).is_ok());
        assert!(validate_locked_cargo("test", "cargo check --locked && cargo test").is_err());
        assert!(validate_locked_cargo("test", "cargo.exe test").is_err());
        assert!(validate_locked_cargo("test", "cargo\ttest").is_err());
        assert!(validate_locked_cargo("test", "/opt/rust/bin/cargo test").is_err());
        assert!(validate_locked_cargo("test", "$CARGO test --locked").is_err());
        assert!(validate_locked_cargo("test", "$env:CARGO test --locked").is_err());
        assert!(validate_locked_cargo("test", "cargo test # --locked").is_err());
        assert!(validate_locked_cargo("test", "cargo test --locked # reviewed").is_ok());
        assert!(validate_locked_cargo("test", "${CARGO:-cargo} test --locked").is_err());
        assert!(validate_locked_cargo("test", "${CARGO-cargo} test --locked").is_err());
        assert!(validate_locked_cargo("test", "${CARGO} test --locked").is_err());
        assert!(validate_locked_cargo("test", r#""${CARGO:-cargo}" test"#).is_err());
        assert!(validate_locked_cargo("test", r#"MODE=ci "${TOOL}" test"#).is_err());
        assert!(validate_locked_cargo("test", r#"env "${TOOL}" test"#).is_err());
        assert!(validate_locked_cargo("test", "cargo.cmd test").is_err());
        assert!(
            validate_locked_cargo("test", "cargo check --locked && cargo test --locked").is_err()
        );
    }

    #[test]
    fn coverage_actions_require_reviewed_revisions() {
        for (repository, revision) in [
            (
                "actions/download-artifact",
                "3e5f45b2cfb9172054b4087a40e8e0b5a5461e7c",
            ),
            (
                "codecov/codecov-action",
                "fb8b3582c8e4def4969c97caa2f19720cb33a72f",
            ),
        ] {
            validate_action(&format!("{repository}@{revision}")).unwrap();
            for unreviewed in ["main", "v7", "0000000000000000000000000000000000000000"] {
                assert!(validate_action(&format!("{repository}@{unreviewed}")).is_err());
            }
        }
    }

    fn minimal_policy_workflow() -> Value {
        serde_json::json!({
            "permissions": { "contents": "read" },
            "jobs": {
                "test": {
                    "steps": [{
                        "uses": "actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1",
                        "with": { "persist-credentials": false }
                    }]
                }
            }
        })
    }

    #[test]
    fn workflow_policy_binds_token_permissions() {
        let workflow = minimal_policy_workflow();
        validate_workflow_policy(&workflow).unwrap();

        let mut missing = minimal_policy_workflow();
        missing.as_object_mut().unwrap().remove("permissions");
        assert!(validate_workflow_policy(&missing).is_err());

        let mut writable = minimal_policy_workflow();
        writable["permissions"]["contents"] = serde_json::json!("write");
        assert!(validate_workflow_policy(&writable).is_err());

        let mut job_override = minimal_policy_workflow();
        job_override["jobs"]["test"]["permissions"] = serde_json::json!({ "contents": "write" });
        assert!(validate_workflow_policy(&job_override).is_err());
        job_override["jobs"]["test"]["permissions"] = serde_json::json!({ "contents": "read" });
        assert!(validate_workflow_policy(&job_override).is_err());
    }

    #[test]
    fn workflow_policy_requires_nonpersistent_checkout_credentials() {
        let mut missing = minimal_policy_workflow();
        missing["jobs"]["test"]["steps"][0]["with"]
            .as_object_mut()
            .unwrap()
            .remove("persist-credentials");
        assert!(validate_workflow_policy(&missing).is_err());

        let mut string_false = minimal_policy_workflow();
        string_false["jobs"]["test"]["steps"][0]["with"]["persist-credentials"] =
            serde_json::json!("false");
        assert!(validate_workflow_policy(&string_false).is_err());
    }

    #[test]
    fn workflow_directory_applies_policy_to_every_yaml_file() {
        let repository = crate::repository_root();
        let temporary = tempfile::tempdir().unwrap();
        let directory = temporary.path().join(".github/workflows");
        fs::create_dir_all(&directory).unwrap();
        for name in ["ci.yml", "release-gates.yml"] {
            fs::copy(
                repository.join(".github/workflows").join(name),
                directory.join(name),
            )
            .unwrap();
        }
        let registry = repository_registry();
        let custom = directory.join("custom.yaml");
        fs::write(
            &custom,
            serde_yaml_ng::to_string(&minimal_policy_workflow()).unwrap(),
        )
        .unwrap();
        assert!(validate_workflow_directory(temporary.path(), &registry).is_ok());

        let mut invalid = minimal_policy_workflow();
        invalid["permissions"]["contents"] = serde_json::json!("write");
        for name in ["untrusted.yml", "untrusted.yaml"] {
            let path = directory.join(name);
            fs::write(&path, serde_yaml_ng::to_string(&invalid).unwrap()).unwrap();
            assert!(validate_workflow_directory(temporary.path(), &registry).is_err());
            fs::remove_file(path).unwrap();
        }

        fs::write(
            &custom,
            serde_yaml_ng::to_string(&minimal_policy_workflow()).unwrap(),
        )
        .unwrap();
        fs::write(directory.join("README.md"), "not a workflow\n").unwrap();
        assert!(validate_workflow_directory(temporary.path(), &registry).is_err());
    }

    #[test]
    fn rejects_unvalidated_local_actions() {
        assert!(validate_action("./.github/actions/local").is_err());
    }

    #[test]
    fn generates_every_declared_matrix() {
        let registry = repository_registry();
        let generated = matrices(&registry);
        assert!(generated.contains_key("check"));
        assert!(generated.contains_key("cross_check"));
        assert!(generated.contains_key("mingw"));
        assert!(generated.contains_key("uclibc"));
        assert_eq!(generated["coverage"].include.len(), 3);
        assert!(
            generated["coverage"]
                .include
                .iter()
                .all(|entry| entry.toolchain == PRIMARY_COVERAGE_TOOLCHAIN)
        );
    }
}
