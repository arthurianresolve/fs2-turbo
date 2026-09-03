use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use super::arguments::{EvidenceMode, absolute, required_path, required_string};
use super::common;
use super::evidence::EnvironmentSnapshot;
use super::paired::{self, Comparison, Control};
use super::stats_report::{
    SetupFailureReport, SetupProcesses, StatsArtifacts, StatsInvalidContext, StatsMethod,
    StatsProcesses, StatsReport,
};
use super::stats_source::{
    BASELINE_PACKAGE, CANDIDATE_PACKAGE, ManifestSpec, rename_package, write_manifest,
};
use crate::policy;
use crate::process;
use crate::report;
use crate::{Result, invalid_data};
use clap::ArgMatches;

#[path = "../../../../benchmarks/paired_stats_protocol.rs"]
mod stats_protocol;

const METRICS: [&str; 7] = stats_protocol::METRICS;

struct StatsRunSpec<'a> {
    root: &'a Path,
    repo: &'a Path,
    fixture: &'a Path,
    output: &'a Path,
    baseline_ref: &'a str,
    candidate_ref: &'a str,
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
    policy_path: &'a Path,
    harness_source: &'a Path,
    paired_core_source: &'a Path,
    paired_protocol_source: &'a Path,
    paired_stats_protocol_source: &'a Path,
}

pub(crate) fn run(root: &Path, arguments: &ArgMatches) -> Result<()> {
    let repo = arguments
        .get_one::<PathBuf>("repo")
        .cloned()
        .unwrap_or_else(|| root.to_owned());
    let fixture = arguments
        .get_one::<PathBuf>("fixture")
        .cloned()
        .unwrap_or_else(|| root.to_owned());
    let output_root = arguments
        .get_one::<PathBuf>("output-root")
        .cloned()
        .unwrap_or_else(|| root.to_owned());
    let explicit_output_root = arguments.contains_id("output-root");
    let output = required_path(arguments, "output")?;
    let source_policy = root.join("benchmarks/measurement-policy.json");
    let (policy, policy_bytes) = policy::load_with_source(&source_policy)?;
    let settings = paired::settings(arguments, &policy)?;
    let strict = settings.evidence_mode.strict_configuration();
    let repo = absolute(root, repo);
    let fixture = absolute(root, fixture);
    let output_root = absolute(root, output_root);
    let output = absolute(root, output);
    common::require_strict_windows_local_volume(&fixture, "strict paired-stats fixture", strict)?;
    common::require_strict_windows_local_volume(
        &output_root,
        "strict paired-stats output root",
        strict,
    )?;
    common::require_strict_windows_local_volume(&output, "strict paired-stats output", strict)?;
    let repo =
        common::retain_selected_repository(&repo, "strict paired-stats repository root", strict)?;
    let _fixture_guard = if strict {
        Some(common::retain_fixture_directory_ancestry(
            &fixture,
            "strict paired-stats fixture",
        )?)
    } else {
        None
    };
    let fixture = fixture.canonicalize()?;
    let baseline = required_string(arguments, "baseline")?;
    let candidate = required_string(arguments, "candidate")?;
    if output.strip_prefix(&output_root).is_err() {
        return Err(invalid_data(format!(
            "benchmark output must remain beneath trusted output root {}",
            output_root.display()
        )));
    }
    if explicit_output_root {
        super::output::prepare_output_root(&output_root, strict)?;
    }
    let _destination_guard = super::output::preflight(
        &output_root,
        &output,
        "output directory",
        strict,
        policy.resources.minimum_free_bytes,
    )?;
    let output = _destination_guard.path().to_owned();
    let staged =
        super::output::StagedDirectory::new(&output_root, &output, "fs2-stats-output-", strict)?;
    let staged_output = staged.path().to_owned();
    let policy_path = common::retain_bytes(
        &policy_bytes,
        &staged_output.join("artifacts/measurement-policy.json"),
    )?;
    let harness_source = common::retain_artifact(
        &root.join("benchmarks/paired_stats.rs"),
        &staged_output.join("artifacts/paired_stats.rs"),
    )?;
    let paired_core_source = common::retain_artifact(
        &root.join("benchmarks/paired.rs"),
        &staged_output.join("artifacts/paired.rs"),
    )?;
    let paired_protocol_source = common::retain_artifact(
        &root.join("benchmarks/paired_protocol.rs"),
        &staged_output.join("artifacts/paired_protocol.rs"),
    )?;
    let paired_stats_protocol_source = common::retain_artifact(
        &root.join("benchmarks/paired_stats_protocol.rs"),
        &staged_output.join("artifacts/paired_stats_protocol.rs"),
    )?;
    let result = execute(StatsRunSpec {
        root,
        repo: repo.path(),
        fixture: &fixture,
        output: &staged_output,
        baseline_ref: baseline,
        candidate_ref: candidate,
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
        policy_path: &policy_path,
        harness_source: &harness_source,
        paired_core_source: &paired_core_source,
        paired_protocol_source: &paired_protocol_source,
        paired_stats_protocol_source: &paired_stats_protocol_source,
    });
    if let Err(error) = &result
        && !staged_output.join("report.json").exists()
    {
        report::write_invalid(
            &staged_output.join("report.json"),
            report::ReportKind::Stats,
            &error.to_string(),
            StatsInvalidContext {
                decision: "invalid-execution",
                environment: EnvironmentSnapshot::capture(&fixture).ok(),
                baseline_ref: baseline,
                candidate_ref: candidate,
                baseline_source: common::resolve_ref(repo.path(), baseline).ok(),
                candidate_source: common::resolve_ref(repo.path(), candidate).ok(),
                fixture: &fixture,
                harness_source_sha256: common::hash_file(&harness_source).ok(),
                policy_sha256: common::normalized_text_hash(&policy_path).ok(),
            },
        )?;
    }
    let publication = staged.publish();
    match (result, publication) {
        (_, Err(error)) => Err(error),
        (result, Ok(())) => result,
    }
}

fn execute(spec: StatsRunSpec<'_>) -> Result<()> {
    let StatsRunSpec {
        root,
        repo,
        fixture,
        output,
        baseline_ref,
        candidate_ref,
        replicates,
        sample_size,
        warm_up,
        measurement,
        cooldown,
        aa_control,
        mut evidence_mode,
        max_outlier_fraction,
        minimum_free_bytes,
        margin,
        confidence,
        policy_path,
        harness_source,
        paired_core_source,
        paired_protocol_source,
        paired_stats_protocol_source,
    } = spec;
    let baseline_revision = common::resolve_ref(repo, baseline_ref)?;
    let candidate_revision = common::resolve_ref(repo, candidate_ref)?;
    if evidence_mode.strict_configuration() && baseline_revision == candidate_revision {
        return Err(invalid_data(
            "strict paired-stats A/B requires different baseline and candidate revisions",
        ));
    }
    let environment = EnvironmentSnapshot::capture(fixture)?;
    if let Some(reason) = environment.strict_failure_reason() {
        if evidence_mode.strict_configuration() {
            return Err(invalid_data(reason));
        }
        evidence_mode.weaken(reason);
    }
    let temporary =
        common::temporary_workspace(root, "fs2-stats-", evidence_mode.strict_configuration())?;
    let logs = output.join("logs");
    let artifact_root = output.join("artifacts");
    let baseline_source = artifact_root.join("sources/baseline");
    let candidate_source = artifact_root.join("sources/candidate");
    let mut source_setup = common::clone_revision(
        repo,
        &baseline_source,
        &baseline_revision,
        &logs,
        "baseline",
    )?;
    source_setup.extend(common::clone_revision(
        repo,
        &candidate_source,
        &candidate_revision,
        &logs,
        "candidate",
    )?);
    if !common::processes_succeeded(&source_setup) {
        report::write_setup_failure(
            &output.join("report.json"),
            report::ReportKind::Stats,
            "unable to materialize isolated benchmark sources",
            &source_setup,
        )?;
        return Err(invalid_data("paired-stats source setup failed"));
    }
    // Validate both selected trees before the first mutation. In particular,
    // committed links and Windows reparse points must not redirect Cargo.toml.
    common::tree_digest(&baseline_source)?;
    common::tree_digest(&candidate_source)?;
    rename_package(&baseline_source, BASELINE_PACKAGE)?;
    rename_package(&candidate_source, CANDIDATE_PACKAGE)?;
    let project = artifact_root.join("build");
    write_manifest(ManifestSpec {
        project: &project,
        harness_source,
        paired_core_source,
        paired_protocol_source,
        paired_stats_protocol_source,
        baseline_source: &baseline_source,
        candidate_source: &candidate_source,
    })?;
    let manifest = project.join("Cargo.toml");
    let target = temporary.path().join("target");
    let cargo_working_directory = manifest
        .ancestors()
        .last()
        .filter(|path| path.has_root())
        .ok_or_else(|| invalid_data("paired-stats manifest has no filesystem root"))?;
    let cargo_directory =
        common::isolated_cargo_working_directory(cargo_working_directory, temporary.path())?;
    let baseline_source_digest = common::tree_digest(&baseline_source)?;
    let candidate_source_digest = common::tree_digest(&candidate_source)?;
    let mut lock = process::cargo();
    // Resolve only from the local registry cache so setup is reproducible and
    // independent of network availability.
    // Cargo discovers repository-local configuration from its working
    // directory, not from --manifest-path. Start at the filesystem root so
    // mutable selected-source configuration cannot alter the frozen build.
    lock.current_dir(cargo_directory.path())
        .args(["generate-lockfile", "--manifest-path"])
        .arg(&manifest)
        .arg("--offline");
    let lock_record = process::run_logged_attempt(
        &mut lock,
        "generate paired-stats lockfile",
        &logs.join("cargo-lock.stdout.log"),
        &logs.join("cargo-lock.stderr.log"),
    );
    if lock_record.succeeded() && evidence_mode.strict {
        common::validate_path_dependencies(
            cargo_working_directory,
            temporary.path(),
            &manifest,
            &[],
            &[&baseline_source, &candidate_source],
        )?;
    }
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
        .args(["--target-dir"])
        .arg(&target);
    let build_record = if lock_record.succeeded() {
        process::run_logged_attempt(
            &mut build,
            "build paired-stats harness",
            &logs.join("cargo-build.stdout.jsonl"),
            &logs.join("cargo-build.stderr.log"),
        )
    } else {
        process::ProcessRecord::skipped(
            &build,
            "build paired-stats harness",
            logs.join("cargo-build.stdout.log"),
            logs.join("cargo-build.stderr.log"),
            "skipped after lockfile failure",
        )
    };
    let binary_result = if build_record.succeeded() {
        common::cargo_executable(&build_record.stdout, "fs2-paired-stats")
    } else {
        Err(invalid_data(build_record.failure_description()))
    };
    if !common::processes_succeeded(&source_setup)
        || !lock_record.succeeded()
        || !build_record.succeeded()
        || binary_result.is_err()
    {
        report::write_json(
            &output.join("report.json"),
            &report::ReportEnvelope::new(
                report::ReportKind::Stats,
                "setup-failure",
                false,
                SetupFailureReport {
                    decision: "setup-failure",
                    environment,
                    baseline_source: &baseline_revision,
                    candidate_source: &candidate_revision,
                    fixture,
                    harness_source: harness_source.to_owned(),
                    harness_source_sha256: common::hash_file(harness_source)?,
                    paired_core_source: paired_core_source.to_owned(),
                    paired_core_source_sha256: common::hash_file(paired_core_source)?,
                    paired_protocol_source: paired_protocol_source.to_owned(),
                    paired_protocol_source_sha256: common::hash_file(paired_protocol_source)?,
                    paired_stats_protocol_source: paired_stats_protocol_source.to_owned(),
                    paired_stats_protocol_source_sha256: common::hash_file(
                        paired_stats_protocol_source,
                    )?,
                    manifest: manifest.clone(),
                    manifest_sha256: common::hash_file(&manifest)?,
                    policy: policy_path,
                    policy_sha256: common::normalized_text_hash(policy_path)?,
                    logs: &logs,
                    processes: SetupProcesses {
                        source: &source_setup,
                        lock: &lock_record,
                        build: &build_record,
                    },
                },
            ),
        )?;
        return Err(invalid_data("paired-stats setup failed"));
    }
    let binary = binary_result?;
    let binary_name = binary
        .file_name()
        .ok_or_else(|| invalid_data("paired-stats binary has no file name"))?;
    let retained_binary =
        common::retain_artifact(&binary, &artifact_root.join("binary").join(binary_name))?;
    let retained_harness = harness_source.to_owned();
    let retained_core = paired_core_source.to_owned();
    let retained_protocol = paired_protocol_source.to_owned();
    let retained_stats_protocol = paired_stats_protocol_source.to_owned();
    let retained_policy = policy_path.to_owned();
    let retained_manifest = project.join("Cargo.toml");
    let retained_lock = project.join("Cargo.lock");
    let project_digest = common::tree_digest(&project)?;

    let warm_up_ms = paired::duration_millis(warm_up)?;
    let measurement_ms = paired::duration_millis(measurement)?;
    let measurement_runs = paired::run_binary_jobs(paired::BinaryJobSpec {
        working_directory: if evidence_mode.strict_configuration() {
            project.as_path()
        } else {
            repo
        },
        fixture_argument: fixture,
        binary: &retained_binary,
        logs: &logs,
        metrics: &METRICS,
        replicates,
        sample_size,
        warm_up_ms,
        measurement_ms,
        cooldown,
        aa_control,
        max_outlier_fraction,
        minimum_free_bytes,
        rotation_count: Some(METRICS.len() - 1),
    })?;
    let paired::MeasurementRuns {
        records,
        runs,
        mut anomalies,
    } = measurement_runs;

    if common::tree_digest(&baseline_source)? != baseline_source_digest
        || common::tree_digest(&candidate_source)? != candidate_source_digest
        || common::tree_digest(&project)? != project_digest
    {
        anomalies.push("paired-stats source or build project changed during execution".to_owned());
    }
    let completed_environment = EnvironmentSnapshot::capture(fixture)?;
    anomalies.extend(environment.drift_reasons(&completed_environment));

    let (ab_summary, ab_passed) = if anomalies.is_empty() {
        paired::summarize(&records, "ab", &METRICS, replicates, confidence, margin)?
    } else {
        (Vec::new(), false)
    };
    let (aa_summary, aa_passed) = if anomalies.is_empty() && aa_control {
        paired::summarize(&records, "aa", &METRICS, replicates, confidence, margin)?
    } else {
        (Vec::new(), !aa_control)
    };
    let gate = paired::gate_decision(
        anomalies.is_empty(),
        aa_control,
        aa_passed,
        evidence_mode.strict_configuration(),
        ab_passed,
    );
    report::write_json(
        &output.join("report.json"),
        &report::ReportEnvelope::new(
            report::ReportKind::Stats,
            if gate.valid { "completed" } else { "invalid" },
            gate.valid,
            StatsReport {
                decision: gate.decision,
                strict_configuration: evidence_mode.strict_configuration(),
                evidence_mode: &evidence_mode,
                baseline_source: &baseline_revision,
                candidate_source: &candidate_revision,
                environment,
                completed_environment,
                fixture,
                method: StatsMethod {
                    name: "same-process alternating paired filesystem-stat measurement",
                    reason: "separate-process ABBA cannot cancel abrupt between-process Windows filesystem state changes",
                    non_regression_margin: margin,
                    confidence,
                    process_replicates: replicates,
                    sample_size,
                    warm_up_seconds: warm_up,
                    measurement_seconds: measurement,
                    cooldown_seconds: cooldown,
                    aa_control,
                    first_invocation_policy: "one explicitly reported pair per workload before warm-up",
                    prime_timings_used: false,
                    inference: "exact distribution-free one-sided A/B and simultaneous two-sided A/A median bounds",
                    source_identity: "retained detached checkouts use benchmark-only package names to keep path dependencies lockfile-distinct",
                },
                artifacts: StatsArtifacts {
                    harness_source: retained_harness.clone(),
                    harness_source_sha256: common::hash_file(&retained_harness)?,
                    paired_core_source: retained_core.clone(),
                    paired_core_source_sha256: common::hash_file(&retained_core)?,
                    paired_protocol_source: retained_protocol.clone(),
                    paired_protocol_source_sha256: common::hash_file(&retained_protocol)?,
                    paired_stats_protocol_source: retained_stats_protocol.clone(),
                    paired_stats_protocol_source_sha256: common::hash_file(
                        &retained_stats_protocol,
                    )?,
                    policy: &retained_policy,
                    policy_sha256: common::normalized_text_hash(&retained_policy)?,
                    manifest: retained_manifest.clone(),
                    manifest_sha256: common::hash_file(&retained_manifest)?,
                    baseline_repository: baseline_source.clone(),
                    baseline_repository_sha256: baseline_source_digest,
                    candidate_repository: candidate_source.clone(),
                    candidate_repository_sha256: candidate_source_digest,
                    cargo_lock: retained_lock.clone(),
                    cargo_lock_sha256: common::hash_file(&retained_lock)?,
                    binary: retained_binary.clone(),
                    binary_sha256: common::hash_file(&retained_binary)?,
                    logs: &logs,
                },
                processes: StatsProcesses {
                    source: &source_setup,
                    lock: &lock_record,
                    build: &build_record,
                    runs: &runs,
                },
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
        return Err(invalid_data("paired-stats evidence is invalid"));
    }
    if evidence_mode.strict_configuration() && !ab_passed {
        return Err(invalid_data("paired-stats non-regression gate failed"));
    }
    Ok(())
}
