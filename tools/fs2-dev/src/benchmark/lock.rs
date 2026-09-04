use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use clap::ArgMatches;
use serde::Serialize;

use super::arguments::{EvidenceMode, absolute, required_path};
use super::common;
use super::evidence::EnvironmentSnapshot;
use super::paired::{self, Comparison, Control, Measurement, RunRecord};
use crate::policy;
use crate::process::{self, ProcessRecord};
use crate::report;
use crate::{Result, invalid_data};

const METRIC: &str = "lock_unlock";
const UPSTREAM_V0_4_3: &str = "9a340454a8292df025de368fc4b310bb736f382f";
const UPSTREAM_REPORT: &str = "upstream-v0.4.3/report.json";

#[derive(Serialize)]
struct Method {
    name: &'static str,
    reason: &'static str,
    baseline_subject: &'static str,
    candidate_subject: &'static str,
    non_regression_margin: f64,
    confidence: f64,
    process_replicates: usize,
    sample_size: usize,
    warm_up_seconds: f64,
    measurement_seconds: f64,
    cooldown_seconds: f64,
    aa_control: bool,
    first_invocation_policy: &'static str,
    prime_timings_used: bool,
    inference: &'static str,
}

#[derive(Serialize)]
struct Artifacts {
    frozen_source_sha256: String,
    harness_source: PathBuf,
    harness_source_sha256: String,
    paired_core_source: PathBuf,
    paired_core_source_sha256: String,
    paired_protocol_source: PathBuf,
    paired_protocol_source_sha256: String,
    policy: PathBuf,
    policy_sha256: String,
    cargo_lock: PathBuf,
    cargo_lock_sha256: String,
    binary_sha256: String,
    logs: PathBuf,
}

#[derive(Serialize)]
struct LockReport<'a> {
    decision: &'static str,
    strict_configuration: bool,
    evidence_mode: &'a EvidenceMode,
    source_revision: &'a str,
    source_status: &'a str,
    fixture: &'a Path,
    environment: EnvironmentSnapshot,
    completed_environment: EnvironmentSnapshot,
    method: Method,
    #[serde(skip_serializing_if = "Option::is_none")]
    upstream_v0_4_3_report: Option<&'static str>,
    artifacts: Artifacts,
    build: &'a ProcessRecord,
    source_setup: &'a [ProcessRecord],
    runs: &'a [RunRecord],
    anomalies: &'a [String],
    ab: Comparison<'a>,
    aa_control: Control<'a>,
    records: &'a [Measurement],
    completed_unix_ms: u128,
}

#[derive(Serialize)]
struct SetupFailure<'a> {
    decision: &'static str,
    setup_error: &'a str,
    source_revision: &'a str,
    source_status: &'a str,
    fixture: &'a Path,
    environment: EnvironmentSnapshot,
    frozen_source_sha256: String,
    harness_source: PathBuf,
    harness_source_sha256: String,
    paired_core_source: PathBuf,
    paired_core_source_sha256: String,
    paired_protocol_source: PathBuf,
    paired_protocol_source_sha256: String,
    policy: PathBuf,
    policy_sha256: String,
    logs: PathBuf,
    build: &'a ProcessRecord,
    source_setup: &'a [ProcessRecord],
}

#[derive(Serialize)]
struct InvalidContext<'a> {
    decision: &'static str,
    source_revision: Option<String>,
    source_status: Option<String>,
    environment: Option<EnvironmentSnapshot>,
    harness_source_sha256: Option<String>,
    policy_sha256: Option<String>,
    output: &'a Path,
}

struct RunSpec<'a> {
    arguments: &'a ArgMatches,
    root: &'a Path,
    output: &'a Path,
    replicates: usize,
    sample_size: usize,
    warm_up: f64,
    measurement: f64,
    cooldown: f64,
    aa_control: bool,
    evidence_mode: EvidenceMode,
    max_outlier_fraction: f64,
    minimum_free_bytes: u64,
    margin: f64,
    confidence: f64,
}

fn paired_tools_feature_args() -> [String; 2] {
    ["--features".to_owned(), "paired-tools".to_owned()]
}

pub(crate) fn run(root: &Path, arguments: &ArgMatches) -> Result<()> {
    let output = absolute(root, required_path(arguments, "output")?);
    let compare_upstream = arguments.get_flag("compare-upstream-v0-4-3");
    let source_policy = root.join("benchmarks/measurement-policy.json");
    let (policy, policy_bytes) = policy::load_with_source(&source_policy)?;
    let settings = paired::settings(arguments, &policy)?;
    common::require_strict_windows_local_volume(
        root,
        "strict lock benchmark repository root",
        settings.evidence_mode.strict_configuration(),
    )?;
    common::require_strict_windows_local_volume(
        &output,
        "strict lock benchmark output",
        settings.evidence_mode.strict_configuration(),
    )?;
    let _destination_guard = super::output::preflight(
        root,
        &output,
        "output directory",
        settings.evidence_mode.strict_configuration(),
        policy.resources.minimum_free_bytes,
    )?;
    let output = _destination_guard.path().to_owned();
    let staged = super::output::StagedDirectory::new(
        root,
        &output,
        "fs2-lock-output-",
        settings.evidence_mode.strict_configuration(),
    )?;
    let staged_output = staged.path().to_owned();
    let policy_path = common::retain_bytes(
        &policy_bytes,
        &staged_output.join("artifacts/measurement-policy.json"),
    )?;
    let result = execute(RunSpec {
        arguments,
        root,
        output: &staged_output,
        replicates: settings.replicates,
        sample_size: settings.sample_size,
        warm_up: settings.warm_up,
        measurement: settings.measurement,
        cooldown: settings.cooldown,
        aa_control: settings.aa_control,
        evidence_mode: settings.evidence_mode,
        max_outlier_fraction: settings.max_outlier_fraction,
        minimum_free_bytes: policy.resources.minimum_free_bytes,
        margin: policy.non_inferiority_margin,
        confidence: policy.paired_process.confidence,
    });
    if let Err(error) = &result
        && !staged_output.join("report.json").exists()
    {
        report::write_invalid(
            &staged_output.join("report.json"),
            report::ReportKind::Lock,
            &error.to_string(),
            InvalidContext {
                decision: "invalid-execution",
                source_revision: common::resolve_ref(root, "HEAD").ok(),
                source_status: worktree_status(root).ok(),
                environment: EnvironmentSnapshot::capture(root).ok(),
                harness_source_sha256: common::hash_file(&root.join("benchmarks/paired_lock.rs"))
                    .ok(),
                policy_sha256: common::normalized_text_hash(&policy_path).ok(),
                output: &output,
            },
        )?;
    }
    let publication = staged.publish();
    match (result, publication) {
        (_, Err(error)) => Err(error),
        (result, Ok(())) => result,
    }?;
    if compare_upstream {
        run_upstream_comparison(root, arguments, &output)?;
    }
    Ok(())
}

fn run_upstream_comparison(root: &Path, arguments: &ArgMatches, output: &Path) -> Result<()> {
    let matches = upstream_ref_arguments(
        &output.join("upstream-v0.4.3"),
        arguments.get_flag("exploratory"),
    )?;
    super::stats::run_lock_refs(root, &matches)
}

fn upstream_ref_arguments(output: &Path, exploratory: bool) -> Result<ArgMatches> {
    let mut arguments = vec![
        OsString::from("bench"),
        OsString::from("lock-refs"),
        OsString::from("--baseline"),
        OsString::from(UPSTREAM_V0_4_3),
        OsString::from("--candidate"),
        OsString::from("HEAD"),
        OsString::from("--output"),
        output.as_os_str().to_owned(),
        OsString::from("--trust-selected-code"),
    ];
    if exploratory {
        arguments.push(OsString::from("--exploratory"));
    }
    let matches = super::command()
        .try_get_matches_from(arguments)
        .map_err(|error| invalid_data(format!("construct upstream v0.4.3 benchmark: {error}")))?;
    matches
        .subcommand_matches("lock-refs")
        .cloned()
        .ok_or_else(|| {
            invalid_data("upstream v0.4.3 benchmark arguments selected no lock-refs mode")
        })
}

fn execute(spec: RunSpec<'_>) -> Result<()> {
    let RunSpec {
        arguments,
        root,
        output,
        replicates,
        sample_size,
        warm_up,
        measurement,
        cooldown,
        aa_control,
        evidence_mode,
        max_outlier_fraction,
        minimum_free_bytes,
        margin,
        confidence,
    } = spec;
    let source_root = root.canonicalize()?;
    let source_revision = common::resolve_ref(&source_root, "HEAD")?;
    let source_status = worktree_status(&source_root)?;
    let copy_live_source = !source_status.trim().is_empty();
    let mut evidence_mode = evidence_mode;
    if copy_live_source {
        if evidence_mode.strict_configuration() {
            return Err(invalid_data(
                "strict lock benchmarks require a clean source worktree",
            ));
        }
        evidence_mode.weaken("lock benchmark source worktree is dirty");
    }
    require_live_source_copy_trust(copy_live_source, arguments)?;
    let temporary =
        common::temporary_workspace(root, "fs2-lock-", evidence_mode.strict_configuration())?;
    let fixture = temporary.path();
    let frozen_source = temporary.path().join("source");
    let logs = output.join("logs");
    let source_setup = if copy_live_source {
        common::copy_tree(&source_root, &frozen_source)?;
        Vec::new()
    } else {
        common::clone_revision(
            &source_root,
            &frozen_source,
            &source_revision,
            &logs,
            "lock-source",
        )?
    };
    if !common::processes_succeeded(&source_setup) {
        report::write_setup_failure(
            &output.join("report.json"),
            report::ReportKind::Lock,
            "unable to materialize the lock benchmark source",
            &source_setup,
        )?;
        return Err(invalid_data("paired-lock source setup failed"));
    }
    let frozen_source_digest = common::tree_digest(&frozen_source)?;
    let environment = EnvironmentSnapshot::capture(fixture)?;
    if let Some(reason) = environment.strict_failure_reason() {
        if evidence_mode.strict_configuration() {
            return Err(invalid_data(reason));
        }
        evidence_mode.weaken(reason);
    }
    let strict = evidence_mode.strict_configuration();
    let artifact_root = output.join("artifacts");
    let retained_harness = common::retain_artifact(
        &frozen_source.join("benchmarks/paired_lock.rs"),
        &artifact_root.join("paired_lock.rs"),
    )?;
    let retained_core = common::retain_artifact(
        &frozen_source.join("benchmarks/paired.rs"),
        &artifact_root.join("paired.rs"),
    )?;
    let retained_protocol = common::retain_artifact(
        &frozen_source.join("benchmarks/paired_protocol.rs"),
        &artifact_root.join("paired_protocol.rs"),
    )?;
    let retained_policy = output.join("artifacts/measurement-policy.json");
    let retained_lock = common::retain_artifact(
        &frozen_source.join("Cargo.lock"),
        &artifact_root.join("Cargo.lock"),
    )?;
    let manifest = frozen_source.join("benchmarks/Cargo.toml");
    let target = temporary.path().join("target");
    let paired_tools_features = paired_tools_feature_args();
    common::validate_path_dependencies(
        root,
        temporary.path(),
        &manifest,
        &paired_tools_features,
        &[&frozen_source],
    )?;
    let cargo_directory =
        common::isolated_cargo_working_directory(&frozen_source, temporary.path())?;
    let mut build = process::cargo();
    build
        .current_dir(cargo_directory.path())
        .args([
            "build",
            "--release",
            "--locked",
            "--offline",
            "--message-format=json-render-diagnostics",
            "--manifest-path",
        ])
        .arg(&manifest)
        .args(&paired_tools_features)
        .args(["--bin", "fs2-paired-lock", "--target-dir"])
        .arg(&target);
    let build_record = process::run_logged_attempt(
        &mut build,
        "build paired-lock harness",
        &logs.join("cargo-build.stdout.jsonl"),
        &logs.join("cargo-build.stderr.log"),
    );
    let binary_result = if build_record.succeeded() {
        common::cargo_executable(&build_record.stdout, "fs2-paired-lock")
    } else {
        Err(invalid_data(build_record.failure_description()))
    };
    if let Err(error) = &binary_result {
        report::write_json(
            &output.join("report.json"),
            &report::ReportEnvelope::new(
                report::ReportKind::Lock,
                "setup-failure",
                false,
                SetupFailure {
                    decision: "setup-failure",
                    setup_error: &error.to_string(),
                    source_revision: &source_revision,
                    source_status: &source_status,
                    fixture,
                    environment,
                    frozen_source_sha256: frozen_source_digest.clone(),
                    harness_source: retained_harness.clone(),
                    harness_source_sha256: common::hash_file(&retained_harness)?,
                    paired_core_source: retained_core.clone(),
                    paired_core_source_sha256: common::hash_file(&retained_core)?,
                    paired_protocol_source: retained_protocol.clone(),
                    paired_protocol_source_sha256: common::hash_file(&retained_protocol)?,
                    policy: retained_policy.clone(),
                    policy_sha256: common::normalized_text_hash(&retained_policy)?,
                    logs: logs.clone(),
                    build: &build_record,
                    source_setup: &source_setup,
                },
            ),
        )?;
        return Err(invalid_data("paired-lock setup failed"));
    }
    let binary = binary_result?;

    let measurement_runs = paired::run_binary_jobs(paired::BinaryJobSpec {
        working_directory: &frozen_source,
        fixture_argument: fixture,
        binary: &binary,
        logs: &logs,
        metrics: &[METRIC],
        replicates,
        sample_size,
        warm_up_ms: paired::duration_millis(warm_up)?,
        measurement_ms: paired::duration_millis(measurement)?,
        cooldown,
        aa_control,
        max_outlier_fraction,
        minimum_free_bytes,
        rotation_count: None,
        diagnostic_samples: false,
    })?;
    let paired::MeasurementRuns {
        records,
        runs,
        mut anomalies,
    } = measurement_runs;

    if common::tree_digest(&frozen_source)? != frozen_source_digest {
        anomalies.push("frozen lock benchmark source changed during execution".to_owned());
    }
    let completed_environment = EnvironmentSnapshot::capture(fixture)?;
    anomalies.extend(environment.drift_reasons(&completed_environment));

    let (ab_summary, ab_passed) = if anomalies.is_empty() {
        paired::summarize(&records, "ab", &[METRIC], replicates, confidence, margin)?
    } else {
        (Vec::new(), false)
    };
    let (aa_summary, aa_passed) = if anomalies.is_empty() && aa_control {
        paired::summarize(&records, "aa", &[METRIC], replicates, confidence, margin)?
    } else {
        (Vec::new(), !aa_control)
    };
    let gate = paired::gate_decision(
        anomalies.is_empty(),
        aa_control,
        aa_passed,
        strict,
        ab_passed,
    );
    report::write_json(
        &output.join("report.json"),
        &report::ReportEnvelope::new(
            report::ReportKind::Lock,
            if gate.valid { "completed" } else { "invalid" },
            gate.valid,
            LockReport {
                decision: gate.decision,
                strict_configuration: strict,
                evidence_mode: &evidence_mode,
                source_revision: &source_revision,
                source_status: &source_status,
                fixture,
                environment,
                completed_environment,
                method: Method {
                    name: "same-process alternating paired lock measurement",
                    reason: "adjacent alternating operations cancel process-level scheduler and power-state drift",
                    baseline_subject: "FileExt::fs2_lock_exclusive/FileExt::fs2_unlock",
                    candidate_subject: "FileExt::lock_exclusive/FileExt::unlock",
                    non_regression_margin: margin,
                    confidence,
                    process_replicates: replicates,
                    sample_size,
                    warm_up_seconds: warm_up,
                    measurement_seconds: measurement,
                    cooldown_seconds: cooldown,
                    aa_control,
                    first_invocation_policy: "one explicitly reported pair before warm-up",
                    prime_timings_used: false,
                    inference: "exact distribution-free one-sided A/B and simultaneous two-sided A/A median bounds",
                },
                upstream_v0_4_3_report: arguments
                    .get_flag("compare-upstream-v0-4-3")
                    .then_some(UPSTREAM_REPORT),
                artifacts: Artifacts {
                    frozen_source_sha256: frozen_source_digest,
                    harness_source: retained_harness.clone(),
                    harness_source_sha256: common::hash_file(&retained_harness)?,
                    paired_core_source: retained_core.clone(),
                    paired_core_source_sha256: common::hash_file(&retained_core)?,
                    paired_protocol_source: retained_protocol.clone(),
                    paired_protocol_source_sha256: common::hash_file(&retained_protocol)?,
                    policy: retained_policy.clone(),
                    policy_sha256: common::normalized_text_hash(&retained_policy)?,
                    cargo_lock: retained_lock.clone(),
                    cargo_lock_sha256: common::hash_file(&retained_lock)?,
                    binary_sha256: common::hash_file(&binary)?,
                    logs: logs.clone(),
                },
                build: &build_record,
                source_setup: &source_setup,
                runs: &runs,
                anomalies: &anomalies,
                ab: Comparison {
                    passed: ab_passed,
                    summary: &ab_summary,
                },
                aa_control: Control {
                    enabled: aa_control,
                    passed: aa_passed,
                    summary: &aa_summary,
                },
                records: &records,
                completed_unix_ms: SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis(),
            },
        ),
    )?;
    println!("report: {}", output.join("report.json").display());
    println!("decision: {}", gate.decision);
    if !gate.valid {
        return Err(invalid_data("paired-lock evidence is invalid"));
    }
    if evidence_mode.strict_configuration() && !ab_passed {
        return Err(invalid_data("paired-lock non-regression gate failed"));
    }
    Ok(())
}

fn worktree_status(root: &Path) -> Result<String> {
    let output = common::git_output(
        root,
        [
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--ignored=matching",
        ],
        "capture benchmark source status",
    )?;
    let mut status = Vec::new();
    for record in output.stdout.split(|byte| *byte == 0) {
        if common::repository_state_record_is_dirty(record) {
            status.extend_from_slice(record);
            status.push(b'\n');
        }
    }
    Ok(String::from_utf8(status)?)
}

fn require_live_source_copy_trust(copy_live_source: bool, arguments: &ArgMatches) -> Result<()> {
    if copy_live_source {
        super::require_selected_code_trust(arguments)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::process::Command;

    use super::*;

    fn run_git(root: &Path, arguments: &[&str]) {
        let status = Command::new("git")
            .current_dir(root)
            .args(arguments)
            .status()
            .expect("git should execute");
        assert!(status.success(), "git {arguments:?} failed");
    }

    fn lock_arguments(trust_selected_code: bool) -> ArgMatches {
        let mut arguments = vec!["bench", "lock", "--output", "out"];
        if trust_selected_code {
            arguments.push("--trust-selected-code");
        }
        let matches = super::super::command()
            .try_get_matches_from(arguments)
            .expect("lock arguments should parse");
        matches
            .subcommand_matches("lock")
            .expect("lock subcommand should be selected")
            .clone()
    }

    #[test]
    fn live_source_copy_requires_explicit_trust() {
        let untrusted = lock_arguments(false);
        let trusted = lock_arguments(true);

        assert!(require_live_source_copy_trust(false, &untrusted).is_ok());
        assert!(require_live_source_copy_trust(true, &untrusted).is_err());
        assert!(require_live_source_copy_trust(true, &trusted).is_ok());
    }

    #[test]
    fn lock_dependency_validation_uses_exact_paired_tools_features() {
        assert_eq!(
            paired_tools_feature_args(),
            ["--features".to_owned(), "paired-tools".to_owned()]
        );
    }

    #[test]
    fn worktree_status_does_not_execute_configured_fsmonitor() {
        let repo = tempfile::tempdir().unwrap();
        run_git(repo.path(), &["init", "--quiet"]);
        fs::write(repo.path().join("tracked"), "tracked\n").unwrap();
        let hook = repo.path().join("fsmonitor-test-hook");
        fs::write(&hook, "#!/bin/sh\nprintf invoked > fsmonitor-was-invoked\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
        }
        run_git(repo.path(), &["add", "tracked", "fsmonitor-test-hook"]);
        run_git(
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
        run_git(
            repo.path(),
            &["config", "core.fsmonitor", "./fsmonitor-test-hook"],
        );
        run_git(repo.path(), &["config", "core.untrackedCache", "true"]);

        let status = worktree_status(repo.path());

        assert!(status.is_ok(), "{status:?}");
        assert!(!repo.path().join("fsmonitor-was-invoked").exists());
    }

    #[test]
    fn upstream_comparison_uses_the_exact_v0_4_3_ref_and_lock_workload() {
        let matches = upstream_ref_arguments(Path::new("out/upstream-v0.4.3"), false).unwrap();

        assert_eq!(
            matches.get_one::<String>("baseline").map(String::as_str),
            Some(UPSTREAM_V0_4_3)
        );
        assert_eq!(
            matches.get_one::<String>("candidate").map(String::as_str),
            Some("HEAD")
        );
        assert_eq!(
            matches.get_one::<PathBuf>("output"),
            Some(&PathBuf::from("out/upstream-v0.4.3"))
        );
    }
}
