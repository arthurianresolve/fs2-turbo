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
use super::{host, noise};
use crate::policy;
use crate::process;
use crate::report;
use crate::{Result, invalid_data};
use clap::ArgMatches;

#[path = "../../../../benchmarks/paired_stats_protocol.rs"]
mod stats_protocol;

#[path = "../../../../benchmarks/paired_common_protocol.rs"]
mod common_protocol;

#[path = "../../../../benchmarks/paired_duplicate_protocol.rs"]
mod duplicate_protocol;

#[path = "../../../../benchmarks/paired_single_duplicate_protocol.rs"]
mod single_duplicate_protocol;

#[path = "../../../../benchmarks/paired_file_create_delete_protocol.rs"]
mod file_create_delete_protocol;

const METRICS: [&str; 7] = stats_protocol::METRICS;
const COMMON_METRICS: [&str; 5] = stats_protocol::COMMON_METRICS;
const LOCK_METRICS: [&str; 1] = ["lock_unlock"];

#[derive(Clone, Copy)]
struct PairedProfile {
    id: &'static str,
    policy_source: &'static str,
    report_kind: report::ReportKind,
    harness_source: &'static str,
    protocol_source: &'static str,
    package_name: &'static str,
    metrics: &'static [&'static str],
    operations_per_timed_interval: u64,
    diagnostic_samples: bool,
    prepared_queries: bool,
    include_tempfile: bool,
    rotate_workloads: bool,
    output_prefix: &'static str,
    method_name: &'static str,
    method_reason: &'static str,
}

const FULL_STATS: PairedProfile = PairedProfile {
    id: "filesystem-stats-full",
    diagnostic_samples: false,
    policy_source: "benchmarks/measurement-policy.json",
    report_kind: report::ReportKind::Stats,
    harness_source: "benchmarks/paired_stats.rs",
    protocol_source: "benchmarks/paired_stats_protocol.rs",
    package_name: "fs2-paired-stats",
    metrics: &METRICS,
    operations_per_timed_interval: 1,
    prepared_queries: true,
    include_tempfile: false,
    rotate_workloads: true,
    output_prefix: "fs2-stats-output-",
    method_name: "same-process alternating paired filesystem-stat measurement",
    method_reason: "separate-process ABBA cannot cancel abrupt between-process Windows filesystem state changes",
};

const COMMON_V04_STATS: PairedProfile = PairedProfile {
    id: "filesystem-stats-v0.4-common",
    metrics: &COMMON_METRICS,
    prepared_queries: false,
    ..FULL_STATS
};

const EXACT_REF_LOCK: PairedProfile = PairedProfile {
    id: "lock-exact-refs",
    diagnostic_samples: false,
    policy_source: "benchmarks/measurement-policy.json",
    report_kind: report::ReportKind::Lock,
    harness_source: "benchmarks/paired_lock_refs.rs",
    protocol_source: "benchmarks/paired_lock_protocol.rs",
    package_name: "fs2-paired-lock-refs",
    metrics: &LOCK_METRICS,
    operations_per_timed_interval: 1,
    prepared_queries: false,
    include_tempfile: true,
    rotate_workloads: false,
    output_prefix: "fs2-lock-refs-output-",
    method_name: "same-process alternating exact-ref lock measurement",
    method_reason: "compare the v0.4-compatible lock sequence from immutable baseline and candidate revisions in one process",
};

const COMMON_API: PairedProfile = PairedProfile {
    id: "common-api-v0.4",
    diagnostic_samples: false,
    policy_source: "benchmarks/measurement-policy.json",
    report_kind: report::ReportKind::Common,
    harness_source: "benchmarks/paired_common.rs",
    protocol_source: "benchmarks/paired_common_protocol.rs",
    package_name: "fs2-paired-common",
    metrics: common_protocol::METRICS,
    operations_per_timed_interval: 1,
    prepared_queries: false,
    include_tempfile: true,
    rotate_workloads: true,
    output_prefix: "fs2-common-output-",
    method_name: "same-process alternating exact-ref common API measurement",
    method_reason: "compare the common API under one dependency lockfile with adjacent ABBA/BAAB operations and A/A controls",
};

const DUPLICATE_BATCHED: PairedProfile = PairedProfile {
    id: "duplicate-batch64",
    policy_source: "benchmarks/duplicate-measurement-policy.json",
    report_kind: report::ReportKind::Common,
    harness_source: "benchmarks/paired_duplicate.rs",
    protocol_source: "benchmarks/paired_duplicate_protocol.rs",
    package_name: "fs2-paired-duplicate",
    metrics: duplicate_protocol::METRICS,
    operations_per_timed_interval: duplicate_protocol::OPERATIONS_PER_TIMED_INTERVAL,
    output_prefix: "fs2-duplicate-output-",
    method_name: "same-process batched exact-ref duplicate measurement",
    method_reason: "amortize timer overhead across 64 immediate duplicate-and-drop operations with balanced ordering and A/A controls",
    ..EXACT_REF_LOCK
};

const DUPLICATE_SINGLE: PairedProfile = PairedProfile {
    id: "duplicate-single-call",
    policy_source: "benchmarks/duplicate-measurement-policy.json",
    protocol_source: "benchmarks/paired_single_duplicate_protocol.rs",
    metrics: single_duplicate_protocol::METRICS,
    output_prefix: "fs2-duplicate-single-output-",
    method_name: "original common-API single-call duplicate measurement",
    method_reason: "reuse the original duplicate timed body without batching or unrelated measured workloads",
    ..COMMON_API
};

const FILE_CREATE_DELETE: PairedProfile = PairedProfile {
    id: "file-create-delete-single-workload",
    protocol_source: "benchmarks/paired_file_create_delete_protocol.rs",
    metrics: file_create_delete_protocol::METRICS,
    output_prefix: "fs2-file-create-delete-output-",
    method_name: "original common-API file-create-delete measurement",
    method_reason: "reuse the original timed body and common policy without unrelated measured workloads",
    ..COMMON_API
};

struct StatsRunSpec<'a> {
    profile: PairedProfile,
    idle_policy: Option<host::Policy>,
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
    aa_margin: f64,
    confidence: f64,
    policy_path: &'a Path,
    harness_source: &'a Path,
    paired_core_source: &'a Path,
    paired_protocol_source: &'a Path,
    paired_stats_protocol_source: &'a Path,
}

pub(crate) fn run(root: &Path, arguments: &ArgMatches) -> Result<()> {
    let profile = if arguments.get_flag("common-v0-4") {
        COMMON_V04_STATS
    } else {
        FULL_STATS
    };
    run_profile(root, arguments, profile)
}

pub(crate) fn run_lock_refs(root: &Path, arguments: &ArgMatches) -> Result<()> {
    run_profile(root, arguments, EXACT_REF_LOCK)
}

pub(crate) fn run_common_refs(root: &Path, arguments: &ArgMatches) -> Result<()> {
    run_profile(root, arguments, COMMON_API)
}

pub(crate) fn run_file_create_delete_refs(root: &Path, arguments: &ArgMatches) -> Result<()> {
    run_profile(root, arguments, FILE_CREATE_DELETE)
}

pub(crate) fn run_duplicate_refs(root: &Path, arguments: &ArgMatches) -> Result<()> {
    run_duplicate_profile(root, arguments, DUPLICATE_BATCHED)
}

pub(crate) fn run_duplicate_single_refs(root: &Path, arguments: &ArgMatches) -> Result<()> {
    run_duplicate_profile(root, arguments, DUPLICATE_SINGLE)
}

fn run_duplicate_profile(
    root: &Path,
    arguments: &ArgMatches,
    profile: PairedProfile,
) -> Result<()> {
    let profile = if arguments.get_flag("diagnostic-trace")
        || arguments.get_flag("diagnostic-samples")
    {
        if !arguments.get_flag("exploratory") {
            return Err(invalid_data(
                "diagnostic instrumentation requires --exploratory",
            ));
        }
        PairedProfile {
            method_name: "diagnostic duplicate sample-window capture",
            method_reason: if arguments.get_flag("diagnostic-trace") {
                "operator-declared external tracing with sample-window capture; diagnostic only"
            } else {
                "sample-window capture without a claim that external tracing was active; diagnostic only"
            },
            diagnostic_samples: true,
            ..profile
        }
    } else {
        profile
    };
    run_profile(root, arguments, profile)
}

fn run_profile(root: &Path, arguments: &ArgMatches, profile: PairedProfile) -> Result<()> {
    if profile.diagnostic_samples && !arguments.get_flag("exploratory") {
        return Err(invalid_data(
            "diagnostic samples cannot be strict performance evidence",
        ));
    }
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
    let source_policy = root.join(profile.policy_source);
    let (policy, policy_bytes) = policy::load_with_source(&source_policy)?;
    let settings = paired::settings(arguments, &policy)?;
    let strict = settings.evidence_mode.strict_configuration();
    let idle_policy = host::Policy::from_arguments(arguments)?;
    if cfg!(windows) && strict && profile.id.starts_with("duplicate") && idle_policy.is_none() {
        return Err(invalid_data(
            "strict Windows duplicate comparisons require explicit --idle-max-core-busy-percent and --idle-max-sample-busy-percent limits",
        ));
    }
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
        super::output::StagedDirectory::new(&output_root, &output, profile.output_prefix, strict)?;
    let staged_output = staged.path().to_owned();
    let policy_path = common::retain_bytes(
        &policy_bytes,
        &staged_output.join("artifacts/measurement-policy.json"),
    )?;
    let harness_source = common::retain_artifact(
        &root.join(profile.harness_source),
        &staged_output.join("artifacts/paired_harness.rs"),
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
        &root.join(profile.protocol_source),
        &staged_output.join("artifacts/workload_protocol.rs"),
    )?;
    let result = execute(StatsRunSpec {
        profile,
        idle_policy,
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
        aa_margin: policy.aa_equivalence_margin(),
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
            profile.report_kind,
            &error.to_string(),
            StatsInvalidContext {
                decision: "invalid-execution",
                profile: profile.id,
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
        profile,
        idle_policy,
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
        aa_margin,
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
            profile.report_kind,
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
        package_name: profile.package_name,
        include_tempfile: profile.include_tempfile,
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
    if !profile.prepared_queries {
        build.arg("--no-default-features");
    }
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
        common::cargo_executable(&build_record.stdout, profile.package_name)
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
                profile.report_kind,
                "setup-failure",
                false,
                SetupFailureReport {
                    decision: "setup-failure",
                    profile: profile.id,
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
    let admission_path = artifact_root.join("host-admission.json");
    let mut admission = idle_policy
        .map(|policy| host::admit(policy, &admission_path))
        .transpose()?;
    let environment = if admission.is_some() {
        EnvironmentSnapshot::capture(fixture)?
    } else {
        environment
    };
    let measurement_runs = paired::run_binary_jobs(paired::BinaryJobSpec {
        working_directory: if evidence_mode.strict_configuration() {
            project.as_path()
        } else {
            repo
        },
        fixture_argument: fixture,
        binary: &retained_binary,
        logs: &logs,
        metrics: profile.metrics,
        replicates,
        sample_size,
        warm_up_ms,
        measurement_ms,
        cooldown,
        aa_control,
        max_outlier_fraction,
        minimum_free_bytes,
        rotation_count: profile
            .rotate_workloads
            .then_some(profile.metrics.len() - usize::from(profile.prepared_queries)),
        diagnostic_samples: profile.diagnostic_samples,
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
    if let Some(guard) = &mut admission {
        guard.restore()?;
    }
    let noise_path = artifact_root.join("noise.json");
    let noise_inputs = runs
        .iter()
        .map(|run| {
            let mut input = noise::input(&logs.join(format!("{}.stdout.tsv", run.run)));
            input["run"] = serde_json::json!(run.run);
            input["mode"] = serde_json::json!(run.mode);
            input
        })
        .collect();
    report::write_json(&noise_path, &noise::envelope(noise_inputs))?;

    let (ab_summary, ab_passed) = if anomalies.is_empty() {
        paired::summarize(
            &records,
            "ab",
            profile.metrics,
            replicates,
            confidence,
            margin,
        )?
    } else {
        (Vec::new(), false)
    };
    let (aa_summary, aa_passed) = if anomalies.is_empty() && aa_control {
        paired::summarize(
            &records,
            "aa",
            profile.metrics,
            replicates,
            confidence,
            aa_margin,
        )?
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
            profile.report_kind,
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
                    profile: profile.id,
                    name: profile.method_name,
                    reason: profile.method_reason,
                    operations_per_timed_interval: profile.operations_per_timed_interval,
                    diagnostic_samples: profile.diagnostic_samples,
                    host_admission_sha256: idle_policy
                        .map(|_| common::hash_file(&admission_path))
                        .transpose()?,
                    noise_report_sha256: common::hash_file(&noise_path)?,
                    non_regression_margin: margin,
                    aa_equivalence_margin: aa_margin,
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
                    baseline_repository_sha256: common::publication_tree_digest(&baseline_source)?,
                    candidate_repository: candidate_source.clone(),
                    candidate_repository_sha256: common::publication_tree_digest(
                        &candidate_source,
                    )?,
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
