use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::ffi::OsStr;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::process;
use crate::{Result, invalid_data};

const MATRIX_TARGET_EXPRESSION: &str = "${{ matrix.target }}";
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

pub(crate) fn run(root: &Path, github_output: Option<&Path>) -> Result<()> {
    validate_xtask_alias(root)?;
    let rust_version = package_rust_version(root)?;
    let registry = load_registry(&root.join("support-matrix.json"))?;
    validate_registry(&registry, &rust_version)?;
    validate_workflow_directory(root, &registry)?;
    let generated = matrices(&registry);
    if let Some(path) = github_output {
        write_github_output(path, &generated, &rust_version)?;
    } else {
        println!("{}", serde_json::to_string_pretty(&generated)?);
    }
    Ok(())
}

fn validate_workflow_directory(root: &Path, registry: &SupportRegistry) -> Result<()> {
    let directory = root.join(".github/workflows");
    let mut entries = fs::read_dir(&directory)?.collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    let mut found_ci = false;
    let mut found_release_gates = false;

    for entry in entries {
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_symlink()
            || workflow_entry_is_windows_reparse_point(&path)?
            || !file_type.is_file()
        {
            return Err(invalid_data(format!(
                "workflow directory contains a link or non-file entry: {}",
                path.display()
            )));
        }
        if !matches!(
            path.extension(),
            Some(extension) if extension == OsStr::new("yml") || extension == OsStr::new("yaml")
        ) {
            return Err(invalid_data(format!(
                "workflow directory contains an unexpected file: {}",
                path.display()
            )));
        }
        let name = path
            .file_name()
            .and_then(OsStr::to_str)
            .ok_or_else(|| invalid_data("workflow file name is not valid Unicode"))?;
        let workflow = load_workflow(&path)?;
        match name {
            "ci.yml" => {
                validate_workflow(registry, &workflow)?;
                found_ci = true;
            }
            "release-gates.yml" => {
                validate_release_workflow(&workflow)?;
                found_release_gates = true;
            }
            "benchmark-results.yml" => validate_benchmark_publication_workflow(&workflow)?,
            _ => validate_workflow_policy(&workflow)?,
        }
    }

    if !found_ci || !found_release_gates {
        return Err(invalid_data(
            "workflow directory must contain ci.yml and release-gates.yml",
        ));
    }
    Ok(())
}

#[cfg(windows)]
fn workflow_entry_is_windows_reparse_point(path: &Path) -> Result<bool> {
    use std::os::windows::fs::MetadataExt as _;
    use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;

    Ok(fs::symlink_metadata(path)?.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0)
}

#[cfg(not(windows))]
fn workflow_entry_is_windows_reparse_point(_path: &Path) -> Result<bool> {
    Ok(false)
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
                        os: ci.runner.as_str().to_owned(),
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
    validate_workflow_policy(workflow)?;
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

fn validate_workflow_policy(workflow: &Value) -> Result<()> {
    validate_workflow_policy_with_pages(workflow, false)
}

fn validate_benchmark_publication_workflow(workflow: &Value) -> Result<()> {
    let jobs = workflow["jobs"]
        .as_object()
        .ok_or_else(|| invalid_data("benchmark publisher must define jobs"))?;
    if jobs.len() != 2 || !jobs.contains_key("render") || !jobs.contains_key("deploy") {
        return Err(invalid_data(
            "benchmark publisher must isolate render and deploy",
        ));
    }
    let render = &workflow["jobs"]["render"];
    let deploy = &workflow["jobs"]["deploy"];
    let condition = render["if"]
        .as_str()
        .map(|value| value.split_whitespace().collect::<Vec<_>>().join(" "));
    let expected_condition = concat!(
        "github.repository == 'arthurianresolve/fs2-turbo' && ",
        "github.event.workflow_run.conclusion == 'success' && ",
        "github.event.workflow_run.head_repository.full_name == github.repository && ",
        "(github.event.workflow_run.head_branch == 'dev' || ",
        "github.event.workflow_run.head_branch == github.event.repository.default_branch) && ",
        "(github.event.workflow_run.event == 'push' || ",
        "github.event.workflow_run.event == 'workflow_dispatch')"
    );
    if workflow["on"]
        != serde_json::json!({
            "workflow_run": {
                "workflows": ["Windows benchmark pilot"],
                "types": ["completed"]
            }
        })
        || condition.as_deref() != Some(expected_condition)
        || render["permissions"] != serde_json::json!({ "contents": "read", "actions": "read" })
        || render["outputs"]["ready"] != "${{ steps.render.outputs.ready }}"
        || deploy["permissions"] != serde_json::json!({ "pages": "write", "id-token": "write" })
        || deploy["needs"] != "render"
        || deploy["if"] != "needs.render.outputs.ready == 'true'"
        || deploy["environment"]["name"] != "github-pages"
    {
        return Err(invalid_data(
            "benchmark publisher permissions, source gates, or deployment dependency drifted",
        ));
    }
    let deploy_object = deploy
        .as_object()
        .ok_or_else(|| invalid_data("benchmark deployment must be an object"))?;
    if deploy_object.keys().any(|key| {
        !matches!(
            key.as_str(),
            "needs"
                | "if"
                | "runs-on"
                | "timeout-minutes"
                | "permissions"
                | "environment"
                | "steps"
        )
    }) {
        return Err(invalid_data(
            "benchmark deployment has unreviewed configuration",
        ));
    }
    let steps = deploy["steps"]
        .as_array()
        .ok_or_else(|| invalid_data("benchmark deployment must define one action"))?;
    if steps.len() != 1 {
        return Err(invalid_data("benchmark deployment must define one action"));
    }
    let step = steps[0]
        .as_object()
        .ok_or_else(|| invalid_data("benchmark deployment step must be an object"))?;
    if step
        .keys()
        .any(|key| !matches!(key.as_str(), "name" | "id" | "uses"))
        || step.get("id").and_then(Value::as_str) != Some("deployment")
        || step
            .get("uses")
            .and_then(Value::as_str)
            .and_then(action_repository)
            != Some("actions/deploy-pages")
    {
        return Err(invalid_data(
            "benchmark deployment may only run the pinned Pages deployment action",
        ));
    }
    validate_workflow_policy_with_pages(workflow, true)
}

fn validate_publication_action(action: &str) -> Result<()> {
    let pinned_pages_action = action
        .rsplit_once('@')
        .is_some_and(|(repository, revision)| {
            matches!(
                repository,
                "actions/download-artifact"
                    | "actions/upload-pages-artifact"
                    | "actions/deploy-pages"
            ) && revision.len() == 40
                && revision.bytes().all(|byte| byte.is_ascii_hexdigit())
        });
    if pinned_pages_action {
        Ok(())
    } else {
        validate_action(action)
    }
}

fn validate_workflow_policy_with_pages(workflow: &Value, pages: bool) -> Result<()> {
    let permissions = workflow
        .get("permissions")
        .and_then(Value::as_object)
        .ok_or_else(|| invalid_data("workflow must declare top-level token permissions"))?;
    if permissions.len() != 1 || permissions.get("contents").and_then(Value::as_str) != Some("read")
    {
        return Err(invalid_data(
            "workflow token permissions must be exactly contents: read",
        ));
    }
    let jobs = workflow
        .get("jobs")
        .and_then(Value::as_object)
        .ok_or_else(|| invalid_data("workflow must define a jobs object"))?;
    for (job_name, job) in jobs {
        let job = job
            .as_object()
            .ok_or_else(|| invalid_data(format!("workflow job {job_name} must be an object")))?;
        if let Some(permissions) = job.get("permissions") {
            let expected = match (pages, job_name.as_str()) {
                (true, "render") => Some(serde_json::json!({
                    "contents": "read", "actions": "read"
                })),
                (true, "deploy") => Some(serde_json::json!({
                    "pages": "write", "id-token": "write"
                })),
                _ => None,
            };
            if expected.as_ref() != Some(permissions) {
                return Err(invalid_data(format!(
                    "workflow job {job_name} may not override token permissions"
                )));
            }
        }
        if let Some(action) = job.get("uses").and_then(Value::as_str) {
            validate_action(action)?;
        }
        let Some(steps) = job.get("steps") else {
            continue;
        };
        let steps = steps
            .as_array()
            .ok_or_else(|| invalid_data(format!("workflow job {job_name} steps must be a list")))?;
        for step in steps {
            let step = step.as_object().ok_or_else(|| {
                invalid_data(format!("workflow job {job_name} contains an invalid step"))
            })?;
            if let Some(action) = step.get("uses").and_then(Value::as_str) {
                if pages {
                    validate_publication_action(action)?;
                } else {
                    validate_action(action)?;
                }
                if action_repository(action) == Some("actions/checkout") {
                    validate_checkout_credentials(job_name, step)?;
                }
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
    Ok(())
}

fn validate_checkout_credentials(
    job_name: &str,
    step: &serde_json::Map<String, Value>,
) -> Result<()> {
    let inputs = step.get("with").and_then(Value::as_object).ok_or_else(|| {
        invalid_data(format!(
            "workflow checkout in {job_name} must disable credential persistence"
        ))
    })?;
    if !matches!(inputs.get("persist-credentials"), Some(Value::Bool(false))) {
        return Err(invalid_data(format!(
            "workflow checkout in {job_name} must set persist-credentials to boolean false"
        )));
    }
    Ok(())
}

fn validate_release_workflow(workflow: &Value) -> Result<()> {
    validate_workflow_policy(workflow)?;
    let triggers = workflow
        .get("on")
        .and_then(Value::as_object)
        .ok_or_else(|| invalid_data("release workflow must define triggers"))?;
    for trigger in ["push", "pull_request", "workflow_dispatch"] {
        if !triggers.contains_key(trigger) {
            return Err(invalid_data(format!(
                "release workflow must retain the {trigger} trigger"
            )));
        }
    }
    let jobs = workflow
        .get("jobs")
        .and_then(Value::as_object)
        .ok_or_else(|| invalid_data("release workflow must define jobs"))?;
    for job in ["toolchains", "package", "dependencies"] {
        if !jobs.contains_key(job) {
            return Err(invalid_data(format!(
                "release workflow must retain the {job} job"
            )));
        }
    }
    Ok(())
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
            cargo_executable(token) || matches!(normalized.as_str(), "cargo" | "env:cargo")
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
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
        })
}

fn pinned_action(action: &str) -> bool {
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
    fn benchmark_publication_policy_preserves_privilege_boundaries() {
        let workflow = load_workflow(
            &crate::repository_root().join(".github/workflows/benchmark-results.yml"),
        )
        .unwrap();
        validate_benchmark_publication_workflow(&workflow).unwrap();
        assert!(validate_workflow_policy(&workflow).is_err());

        let mut write = workflow.clone();
        write["permissions"]["contents"] = serde_json::json!("write");
        assert!(validate_benchmark_publication_workflow(&write).is_err());

        let mut render_write = workflow.clone();
        render_write["jobs"]["render"]["permissions"]["contents"] = serde_json::json!("write");
        assert!(validate_benchmark_publication_workflow(&render_write).is_err());

        let mut deploy_contents = workflow.clone();
        deploy_contents["jobs"]["deploy"]["permissions"]["contents"] = serde_json::json!("write");
        assert!(validate_benchmark_publication_workflow(&deploy_contents).is_err());

        let mut shell = workflow.clone();
        shell["jobs"]["deploy"]["steps"][0]["run"] = serde_json::json!("echo unreviewed");
        assert!(validate_benchmark_publication_workflow(&shell).is_err());

        let mut extra_job = workflow.clone();
        extra_job["jobs"]["extra"] = serde_json::json!({});
        assert!(validate_benchmark_publication_workflow(&extra_job).is_err());

        let mut mutable = workflow;
        mutable["jobs"]["deploy"]["steps"][0]["uses"] =
            serde_json::json!("actions/deploy-pages@v5");
        assert!(validate_benchmark_publication_workflow(&mutable).is_err());
    }

    #[test]
    fn benchmark_publication_policy_binds_success_and_readiness() {
        let workflow = load_workflow(
            &crate::repository_root().join(".github/workflows/benchmark-results.yml"),
        )
        .unwrap();
        for pointer in [
            "/jobs/render/if",
            "/jobs/render/outputs/ready",
            "/jobs/deploy/if",
            "/jobs/deploy/needs",
            "/jobs/deploy/environment/name",
        ] {
            let mut changed = workflow.clone();
            *changed.pointer_mut(pointer).unwrap() = serde_json::json!("true");
            assert!(validate_benchmark_publication_workflow(&changed).is_err());
        }
        let mut trigger = workflow;
        trigger["on"] = serde_json::json!({ "pull_request": {} });
        assert!(validate_benchmark_publication_workflow(&trigger).is_err());
        assert!(
            validate_action("actions/deploy-pages@368f82528645a54fb793d4d04e342629a3f51346")
                .is_err()
        );
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
            "actions/checkout@34e114876b0b11c390a56381ad16ebd13914f8d5"
        ));
        assert!(!pinned_action(
            "untrusted/example@34e114876b0b11c390a56381ad16ebd13914f8d5"
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

    fn minimal_policy_workflow() -> Value {
        serde_json::json!({
            "permissions": { "contents": "read" },
            "jobs": {
                "test": {
                    "steps": [{
                        "uses": "actions/checkout@34e114876b0b11c390a56381ad16ebd13914f8d5",
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
    }
}
