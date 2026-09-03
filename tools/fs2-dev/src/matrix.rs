use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::process;
use crate::{Result, invalid_data};

const APPROVED_RUNNERS: [&str; 4] = [
    "macos-15-intel",
    "macos-latest",
    "ubuntu-latest",
    "windows-latest",
];
const APPROVED_TARGETS: [&str; 19] = [
    "aarch64-apple-darwin",
    "aarch64-linux-android",
    "aarch64-pc-windows-msvc",
    "aarch64-unknown-linux-gnu",
    "aarch64-unknown-linux-musl",
    "armv7-unknown-linux-uclibceabihf",
    "i686-linux-android",
    "i686-pc-windows-gnu",
    "i686-unknown-linux-gnu",
    "powerpc64-unknown-linux-gnu",
    "riscv64gc-unknown-linux-gnu",
    "x86_64-apple-darwin",
    "x86_64-pc-windows-gnu",
    "x86_64-pc-windows-msvc",
    "x86_64-unknown-freebsd",
    "x86_64-unknown-illumos",
    "x86_64-unknown-linux-gnu",
    "x86_64-unknown-netbsd",
    "x86_64-unknown-redox",
];
const EVIDENCE_LEVELS: [&str; 3] = ["runtime", "compile", "not-covered"];
const ALLOCATION_CAPABILITIES: [&str; 3] = ["physical-reservation", "unsupported", "unknown"];
const MATRIX_TARGET_EXPRESSION: &str = "${{ matrix.target }}";

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SupportRegistry {
    version: u64,
    evidence_levels: Vec<String>,
    targets: Vec<TargetSpec>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TargetSpec {
    target: String,
    platform: String,
    evidence: String,
    allocation: String,
    ci: Option<CiSpec>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CiSpec {
    job: String,
    runner: String,
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

pub(crate) fn run(root: &Path, github_output: Option<&Path>) -> Result<()> {
    let rust_version = package_rust_version(root)?;
    let registry = load_registry(&root.join("support-matrix.json"))?;
    validate_registry(&registry, &rust_version)?;
    let workflow = load_workflow(&root.join(".github/workflows/ci.yml"))?;
    validate_workflow(&registry, &workflow)?;
    let generated = matrices(&registry);
    if let Some(path) = github_output {
        write_github_output(path, &generated, &rust_version)?;
    } else {
        println!("{}", serde_json::to_string_pretty(&generated)?);
    }
    Ok(())
}

fn load_registry(path: &Path) -> Result<SupportRegistry> {
    Ok(serde_json::from_str(&fs::read_to_string(path)?)?)
}

fn load_workflow(path: &Path) -> Result<Value> {
    Ok(serde_yaml_ng::from_str(&fs::read_to_string(path)?)?)
}

fn package_rust_version(root: &Path) -> Result<String> {
    let mut command = process::cargo();
    command
        .current_dir(root)
        .args(["metadata", "--no-deps", "--format-version", "1", "--locked"]);
    let output = process::capture(&mut command, "read fs2 package metadata")?;
    let metadata: CargoMetadata = serde_json::from_slice(&output.stdout)?;
    metadata
        .packages
        .into_iter()
        .find(|package| package.name == "fs2")
        .and_then(|package| package.rust_version)
        .ok_or_else(|| invalid_data("cargo metadata did not provide fs2 rust-version"))
}

fn validate_registry(registry: &SupportRegistry, rust_version: &str) -> Result<()> {
    if registry.version != 5 {
        return Err(invalid_data("support matrix version must be 5"));
    }
    let levels = registry
        .evidence_levels
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    if levels != EVIDENCE_LEVELS.into_iter().collect() {
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
        if !APPROVED_TARGETS.contains(&entry.target.as_str()) {
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
        if !EVIDENCE_LEVELS.contains(&entry.evidence.as_str()) {
            return Err(invalid_data(format!(
                "unknown evidence level for {}",
                entry.target
            )));
        }
        if !ALLOCATION_CAPABILITIES.contains(&entry.allocation.as_str()) {
            return Err(invalid_data(format!(
                "unknown allocation capability for {}",
                entry.target
            )));
        }
        has_runtime |= entry.evidence == "runtime";
        if entry.evidence == "not-covered" {
            if entry.allocation != "unknown" || entry.ci.is_some() {
                return Err(invalid_data(format!(
                    "not-covered target {} must use unknown allocation and no CI",
                    entry.target
                )));
            }
            continue;
        }
        if entry.allocation == "unknown" {
            return Err(invalid_data(format!(
                "covered target {} must declare allocation capability",
                entry.target
            )));
        }
        let ci = entry
            .ci
            .as_ref()
            .ok_or_else(|| invalid_data(format!("CI metadata missing for {}", entry.target)))?;
        if !is_ci_job_name(&ci.job) {
            return Err(invalid_data(format!(
                "invalid CI job name for {}",
                entry.target
            )));
        }
        if !APPROVED_RUNNERS.contains(&ci.runner.as_str()) {
            return Err(invalid_data(format!(
                "unapproved runner for {}",
                entry.target
            )));
        }
        if ci.toolchains.is_empty() {
            return Err(invalid_data(format!(
                "toolchains missing for {}",
                entry.target
            )));
        }
        if entry.evidence == "runtime"
            && ci.toolchains != [rust_version.to_owned(), "stable".to_owned()]
        {
            return Err(invalid_data(format!(
                "runtime target {} must use Rust {rust_version} and stable",
                entry.target
            )));
        }
        if entry.evidence == "compile"
            && ci.toolchains != [rust_version.to_owned()]
            && ci.toolchains != ["nightly".to_owned()]
        {
            return Err(invalid_data(format!(
                "compile target {} must use Rust {rust_version} or nightly",
                entry.target
            )));
        }
        if ci.coverage && entry.evidence != "runtime" {
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
                os: ci.runner.clone(),
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
                        os: ci.runner.clone(),
                        target: entry.target.clone(),
                        toolchain: ci.toolchains[0].clone(),
                    })
                })
                .collect(),
        },
    );
    generated
}

fn validate_workflow(registry: &SupportRegistry, workflow: &Value) -> Result<()> {
    let jobs = workflow
        .get("jobs")
        .and_then(Value::as_object)
        .ok_or_else(|| invalid_data("workflow must define a jobs object"))?;
    let generated = matrices(registry);
    let declared = generated.keys().map(String::as_str).collect::<HashSet<_>>();

    for (job_name, job) in jobs {
        let job = job
            .as_object()
            .ok_or_else(|| invalid_data(format!("workflow job {job_name} must be an object")))?;
        if let Some(steps) = job.get("steps") {
            let steps = steps.as_array().ok_or_else(|| {
                invalid_data(format!("workflow job {job_name} steps must be a list"))
            })?;
            for step in steps {
                let step = step.as_object().ok_or_else(|| {
                    invalid_data(format!("workflow job {job_name} contains an invalid step"))
                })?;
                if let Some(action) = step.get("uses").and_then(Value::as_str)
                    && !action.starts_with("./")
                    && !pinned_action(action)
                {
                    return Err(invalid_data(format!(
                        "workflow action is not pinned to a commit: {action}"
                    )));
                }
                if let Some(command) = step.get("run").and_then(Value::as_str) {
                    if has_unquoted_matrix_target(command) {
                        return Err(invalid_data(format!(
                            "workflow job {job_name} uses an unquoted matrix target"
                        )));
                    }
                    validate_locked_cargo(job_name, command)?;
                }
            }
        }

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
                let expected = serde_json::to_value(&generated[job_name])?;
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

    let triggers = workflow
        .get("on")
        .and_then(Value::as_object)
        .ok_or_else(|| invalid_data("workflow must define triggers"))?;
    if !triggers.contains_key("workflow_dispatch") {
        return Err(invalid_data("workflow must retain a manual trigger"));
    }
    if triggers.get("schedule") != Some(&serde_json::json!([{ "cron": "17 1 1 * *" }])) {
        return Err(invalid_data("workflow must retain the monthly canary"));
    }
    Ok(())
}

fn validate_locked_cargo(job_name: &str, command: &str) -> Result<()> {
    let command = command.trim_start();
    if !command.starts_with("cargo ")
        || command.starts_with("cargo fmt ")
        || command.starts_with("cargo xtask ")
        || command.contains("--locked")
    {
        Ok(())
    } else {
        Err(invalid_data(format!(
            "workflow cargo command is not locked in {job_name}: {command}"
        )))
    }
}

fn pinned_action(action: &str) -> bool {
    action.rsplit_once('@').is_some_and(|(_, revision)| {
        revision.len() == 40 && revision.bytes().all(|byte| byte.is_ascii_hexdigit())
    })
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
    let mut in_quotes = false;
    let mut escaped = false;
    let mut index = 0;
    while index < command.len() {
        if command[index..].starts_with(MATRIX_TARGET_EXPRESSION) {
            if !in_quotes {
                return true;
            }
            index += MATRIX_TARGET_EXPRESSION.len();
            escaped = false;
            continue;
        }
        let byte = command.as_bytes()[index];
        if byte == b'"' && !escaped {
            in_quotes = !in_quotes;
        }
        escaped = byte == b'\\' && !escaped;
        index += 1;
    }
    false
}

fn write_github_output(
    path: &Path,
    generated: &BTreeMap<String, Matrix>,
    rust_version: &str,
) -> Result<()> {
    let mut output = OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(output, "matrices={}", serde_json::to_string(generated)?)?;
    writeln!(output, "rust_version={rust_version}")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repository_registry() -> SupportRegistry {
        load_registry(&crate::repository_root().join("support-matrix.json")).unwrap()
    }

    #[test]
    fn repository_registry_and_workflow_agree() {
        let registry = repository_registry();
        validate_registry(&registry, "1.88").unwrap();
        let workflow =
            load_workflow(&crate::repository_root().join(".github/workflows/ci.yml")).unwrap();
        validate_workflow(&registry, &workflow).unwrap();
    }

    #[test]
    fn rejects_duplicate_or_unapproved_targets() {
        let mut registry = repository_registry();
        registry.targets[1].target = registry.targets[0].target.clone();
        assert!(validate_registry(&registry, "1.88").is_err());
        registry.targets[1].target = "$(echo injected)".to_owned();
        assert!(validate_registry(&registry, "1.88").is_err());
    }

    #[test]
    fn rejects_mutable_actions_and_unquoted_targets() {
        assert!(!pinned_action("actions/checkout@v4"));
        assert!(pinned_action(
            "actions/checkout@34e114876b0b11c390a56381ad16ebd13914f8d5"
        ));
        assert!(has_unquoted_matrix_target(
            "cargo check --target ${{ matrix.target }}"
        ));
        assert!(!has_unquoted_matrix_target(
            "cargo check --target \"${{ matrix.target }}\""
        ));
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
    }
}
