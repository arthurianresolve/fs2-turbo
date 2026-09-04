use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use clap::ArgMatches;
use serde::Serialize;

use super::arguments::{
    CriterionProfile, EvidenceMode, absolute, criterion_evidence_mode, criterion_settings,
    require_exploratory, required_string,
};
use super::common;
use super::criterion::{self, CriterionInvocation, CriterionMode, CriterionRun, CriterionSettings};
use super::evidence::{DiskSnapshot, EnvironmentSnapshot};
use super::statistics;
use crate::policy;
use crate::policy::PairSubject;
use crate::report;
use crate::{Result, invalid_data};

#[derive(Clone, Debug, Serialize)]
struct Measurement {
    block: usize,
    position: usize,
    replicate: usize,
    subject: char,
    benchmark: String,
    metric: String,
    median_ns: f64,
    run: String,
}

#[derive(Clone, Debug, Serialize)]
struct PairRecord {
    benchmark: String,
    metric: String,
    block: usize,
    baseline_run: String,
    candidate_run: String,
    baseline_median_ns: f64,
    candidate_median_ns: f64,
    ratio: f64,
}

#[derive(Clone, Debug, Serialize)]
struct UnstableBlock {
    benchmark: String,
    metric: String,
    block: usize,
    ratios: [f64; 2],
    pair_spread: f64,
    max_pair_spread: f64,
}

struct PairedMeasurements {
    pairs: Vec<PairRecord>,
    unstable: Vec<UnstableBlock>,
    ratios: BTreeMap<String, Vec<f64>>,
}

#[derive(Serialize)]
struct SetupFailureReport<'a> {
    environment: EnvironmentSnapshot,
    repository: &'a Path,
    baseline_ref: &'a str,
    candidate_ref: &'a str,
    baseline_commit: &'a str,
    candidate_commit: &'a str,
    baseline_tree_sha256: String,
    candidate_tree_sha256: String,
    benchmark_harness_sha256: String,
    measurement_policy_sha256: String,
    benches: &'a [String],
    filter: Option<&'a str>,
    features: &'a [String],
    setup: &'a [crate::process::ProcessRecord],
}

#[derive(Serialize)]
struct RefMetadata<'a> {
    repository: &'a Path,
    baseline_ref: &'a str,
    candidate_ref: &'a str,
    baseline_commit: &'a str,
    candidate_commit: &'a str,
    baseline_tree_sha256: String,
    candidate_tree_sha256: String,
    benchmark_harness_sha256: String,
    measurement_policy_sha256: String,
    baseline_lock: PathBuf,
    baseline_lock_sha256: String,
    candidate_lock: PathBuf,
    candidate_lock_sha256: String,
    benches: &'a [String],
    filter: Option<&'a str>,
    features: &'a [String],
    blocks: usize,
    criterion: CriterionSettings,
    first_invocation_policy: &'static str,
    priming_estimates_used: bool,
    non_inferiority_margin: f64,
    max_pair_spread: f64,
    cooldown_seconds: f64,
    exploratory: bool,
    evidence_mode: &'a EvidenceMode,
}

#[derive(Serialize)]
struct RefReport<'a> {
    decision_passed: bool,
    environment: EnvironmentSnapshot,
    #[serde(skip_serializing_if = "Option::is_none")]
    completed_environment: Option<EnvironmentSnapshot>,
    disk_snapshots: &'a [DiskSnapshot],
    environment_drift: &'a [String],
    metadata: RefMetadata<'a>,
    setup: &'a [crate::process::ProcessRecord],
    priming: &'a [CriterionRun],
    runs: &'a [CriterionRun],
    pairs: &'a [PairRecord],
    unstable_blocks: &'a [UnstableBlock],
    decisions: &'a [statistics::Decision],
}

#[derive(Serialize)]
struct RefInvalidContext<'a> {
    decision_passed: bool,
    environment: Option<EnvironmentSnapshot>,
    repository: &'a Path,
    baseline_ref: &'a str,
    candidate_ref: &'a str,
    baseline_commit: Option<String>,
    candidate_commit: Option<String>,
    benchmark_harness_sha256: Option<String>,
    measurement_policy_sha256: Option<String>,
    benches: &'a [String],
    filter: Option<&'a str>,
    features: &'a [String],
}

struct RefRunSpec<'a> {
    root: &'a Path,
    output: &'a Path,
    baseline_ref: &'a str,
    candidate_ref: &'a str,
    benches: &'a [String],
    filter: Option<&'a str>,
    features: &'a [String],
    blocks: usize,
    settings: CriterionSettings,
    cooldown: f64,
    margin: f64,
    max_pair_spread: f64,
    position_replicates: usize,
    orders: &'a [[PairSubject; 4]; 2],
    minimum_free_bytes: u64,
    policy_path: &'a Path,
    benchmark_inputs: &'a Path,
    lockfile: &'a Path,
    evidence_mode: EvidenceMode,
    max_outlier_fraction: f64,
}

fn require_ref_policy_settings(
    evidence_mode: &mut EvidenceMode,
    explicitly_exploratory: bool,
    blocks: usize,
    policy_blocks: usize,
    cooldown: f64,
    policy_cooldown: f64,
) -> Result<()> {
    require_exploratory(
        evidence_mode,
        explicitly_exploratory,
        blocks != policy_blocks,
        "block count differs from the measurement policy",
    )?;
    require_exploratory(
        evidence_mode,
        explicitly_exploratory,
        cooldown != policy_cooldown,
        "cooldown differs from the measurement policy",
    )
}

pub(crate) fn run(root: &Path, arguments: &ArgMatches) -> Result<()> {
    let baseline_ref = required_string(arguments, "baseline")?;
    let candidate_ref = required_string(arguments, "candidate")?;
    let benches = arguments
        .get_many::<String>("bench")
        .map(|values| values.cloned().collect())
        .unwrap_or_else(|| vec!["fs2".into(), "fs2_legacy".into(), "fs_compat".into()]);
    let known = ["fs2", "fs2_legacy", "fs_compat"];
    if benches.iter().any(|bench| !known.contains(&bench.as_str())) {
        return Err(invalid_data("unknown Criterion benchmark name"));
    }
    let filter = arguments.get_one::<String>("filter").map(String::as_str);
    let features = vec![
        "--no-default-features".to_owned(),
        "--features".to_owned(),
        required_string(arguments, "features")?.to_owned(),
    ];
    let output = arguments
        .get_one::<PathBuf>("output")
        .cloned()
        .map(Ok)
        .unwrap_or_else(|| common::default_output(root, "refs"))?;
    let output = absolute(root, output);
    let source_policy = root.join("benchmarks/measurement-policy.json");
    let (policy, policy_bytes) = policy::load_with_source(&source_policy)?;
    let blocks = arguments
        .get_one::<usize>("blocks")
        .copied()
        .unwrap_or(usize::try_from(policy.ref_to_ref.blocks)?);
    if blocks < usize::try_from(policy.ref_to_ref.minimum_blocks)?
        || blocks > usize::try_from(policy.ref_to_ref.maximum_blocks)?
    {
        return Err(invalid_data(format!(
            "blocks must be between {} and {}",
            policy.ref_to_ref.minimum_blocks, policy.ref_to_ref.maximum_blocks
        )));
    }
    let settings = criterion_settings(arguments, &policy)?;
    let cooldown = arguments
        .get_one::<f64>("cooldown-seconds")
        .copied()
        .unwrap_or(policy.ref_to_ref.cooldown_seconds);
    if !cooldown.is_finite() || !(0.0..=policy::MAX_DURATION_SECONDS).contains(&cooldown) {
        return Err(invalid_data("cooldown is outside the supported range"));
    }
    let explicitly_exploratory = arguments.get_flag("exploratory");
    let mut evidence_mode = criterion_evidence_mode(
        explicitly_exploratory,
        settings,
        &policy,
        CriterionProfile::RefToRef,
    )?;
    require_ref_policy_settings(
        &mut evidence_mode,
        explicitly_exploratory,
        blocks,
        usize::try_from(policy.ref_to_ref.blocks)?,
        cooldown,
        policy.ref_to_ref.cooldown_seconds,
    )?;
    common::require_strict_windows_local_volume(
        root,
        "strict ref benchmark repository root",
        evidence_mode.strict_configuration(),
    )?;
    common::require_strict_windows_local_volume(
        &output,
        "strict ref benchmark output",
        evidence_mode.strict_configuration(),
    )?;
    let _destination_guard = super::output::preflight(
        root,
        &output,
        "output directory",
        evidence_mode.strict_configuration(),
        policy.resources.minimum_free_bytes,
    )?;
    let output = _destination_guard.path().to_owned();
    let staged = super::output::StagedDirectory::new(
        root,
        &output,
        "fs2-refs-output-",
        evidence_mode.strict_configuration(),
    )?;
    let staged_output = staged.path().to_owned();
    let inputs = staged_output.join("artifacts/inputs");
    fs::create_dir_all(&inputs)?;
    let policy_path = common::retain_bytes(&policy_bytes, &inputs.join("measurement-policy.json"))?;
    let benchmark_inputs = inputs.join("benchmarks");
    common::copy_tree(&root.join("benchmarks"), &benchmark_inputs)?;
    let lockfile = common::retain_artifact(&root.join("Cargo.lock"), &inputs.join("Cargo.lock"))?;
    let result = execute(RefRunSpec {
        root,
        output: &staged_output,
        baseline_ref,
        candidate_ref,
        benches: &benches,
        filter,
        features: &features,
        blocks,
        settings,
        cooldown,
        margin: policy.non_inferiority_margin,
        max_pair_spread: policy.ref_to_ref.max_pair_spread,
        position_replicates: usize::try_from(policy.ref_to_ref.position_replicates)?,
        orders: &policy.ref_to_ref.pair_orders,
        minimum_free_bytes: policy.resources.minimum_free_bytes,
        policy_path: &policy_path,
        benchmark_inputs: &benchmark_inputs,
        lockfile: &lockfile,
        evidence_mode,
        max_outlier_fraction: policy.criterion.max_outlier_fraction,
    });
    if let Err(error) = &result
        && !staged_output.join("report.json").exists()
    {
        report::write_invalid(
            &staged_output.join("report.json"),
            report::ReportKind::RefToRef,
            &error.to_string(),
            RefInvalidContext {
                decision_passed: false,
                environment: EnvironmentSnapshot::capture(&staged_output).ok(),
                repository: root,
                baseline_ref,
                candidate_ref,
                baseline_commit: common::resolve_ref(root, baseline_ref).ok(),
                candidate_commit: common::resolve_ref(root, candidate_ref).ok(),
                benchmark_harness_sha256: common::tree_digest(&root.join("benchmarks")).ok(),
                measurement_policy_sha256: common::normalized_text_hash(&policy_path).ok(),
                benches: &benches,
                filter,
                features: &features,
            },
        )?;
    }
    let publication = staged.publish();
    match (result, publication) {
        (_, Err(error)) => Err(error),
        (result, Ok(())) => result,
    }
}

struct RefEvaluation {
    paired: PairedMeasurements,
    decisions: Vec<statistics::Decision>,
    passed: bool,
    valid: bool,
}

fn evaluate_measurements(
    measurements: &[Measurement],
    blocks: usize,
    max_pair_spread: f64,
    margin: f64,
    execution_valid: bool,
    position_replicates: usize,
) -> Result<RefEvaluation> {
    let paired = if execution_valid {
        pair_measurements(measurements, blocks, position_replicates, max_pair_spread)?
    } else {
        PairedMeasurements {
            pairs: Vec::new(),
            unstable: Vec::new(),
            ratios: BTreeMap::new(),
        }
    };
    let decisions = if execution_valid && paired.unstable.is_empty() {
        statistics::evaluate(&paired.ratios, margin)?
    } else {
        Vec::new()
    };
    let passed = !decisions.is_empty()
        && decisions
            .iter()
            .all(|decision| decision.disposition != "inconclusive-or-slower");

    Ok(RefEvaluation {
        valid: execution_valid && paired.unstable.is_empty(),
        paired,
        decisions,
        passed,
    })
}

fn execute(spec: RefRunSpec<'_>) -> Result<()> {
    let RefRunSpec {
        root,
        output,
        baseline_ref,
        candidate_ref,
        benches,
        filter,
        features,
        blocks,
        settings,
        cooldown,
        margin,
        max_pair_spread,
        position_replicates,
        orders,
        minimum_free_bytes,
        policy_path,
        benchmark_inputs,
        lockfile,
        evidence_mode,
        max_outlier_fraction,
    } = spec;
    let mut evidence_mode = evidence_mode;
    let baseline_commit = common::resolve_ref(root, baseline_ref)?;
    let candidate_commit = common::resolve_ref(root, candidate_ref)?;
    if evidence_mode.strict && baseline_commit == candidate_commit {
        return Err(invalid_data(
            "strict ref A/B requires different baseline and candidate revisions",
        ));
    }
    let temporary =
        common::temporary_workspace(root, "fs2-refs-", evidence_mode.strict_configuration())?;
    let environment = EnvironmentSnapshot::capture(output)?;
    if let Some(reason) = environment.strict_failure_reason() {
        if evidence_mode.strict_configuration() {
            return Err(invalid_data(reason));
        }
        evidence_mode.weaken(reason);
    }
    let mut disk_snapshots = Vec::with_capacity(blocks * benches.len());
    let baseline_source = temporary.path().join("baseline-source");
    let candidate_source = temporary.path().join("candidate-source");
    let setup_logs = output.join("setup");
    let mut setup = common::clone_revision(
        root,
        &baseline_source,
        &baseline_commit,
        &setup_logs,
        "baseline",
    )?;
    setup.extend(common::clone_revision(
        root,
        &candidate_source,
        &candidate_commit,
        &setup_logs,
        "candidate",
    )?);
    if !common::processes_succeeded(&setup) {
        report::write_setup_failure(
            &output.join("report.json"),
            report::ReportKind::RefToRef,
            "unable to materialize benchmark revisions",
            &setup,
        )?;
        return Err(invalid_data(
            "benchmark source setup failed; see retained logs",
        ));
    }
    let baseline_digest = common::tree_digest(&baseline_source)?;
    let candidate_digest = common::tree_digest(&candidate_source)?;
    let baseline_package = common::subject_package_name(&baseline_source)?;
    let candidate_package = common::subject_package_name(&candidate_source)?;

    let harness_root = temporary.path().join("harnesses");
    let baseline_manifest = common::prepare_harness(
        &harness_root,
        "baseline",
        &baseline_source,
        &baseline_package,
        benchmark_inputs,
        lockfile,
    )?;
    let candidate_manifest = common::prepare_harness(
        &harness_root,
        "candidate",
        &candidate_source,
        &candidate_package,
        benchmark_inputs,
        lockfile,
    )?;
    let baseline_target = temporary.path().join("baseline-target");
    let candidate_target = temporary.path().join("candidate-target");
    setup.push(common::generate_lockfile(
        root,
        &baseline_manifest,
        &baseline_target,
        &setup_logs,
        "baseline",
    )?);
    setup.push(common::generate_lockfile(
        root,
        &candidate_manifest,
        &candidate_target,
        &setup_logs,
        "candidate",
    )?);
    if !common::processes_succeeded(&setup) {
        report::write_setup_failure(
            &output.join("report.json"),
            report::ReportKind::RefToRef,
            "benchmark lockfile generation failed",
            &setup,
        )?;
        return Err(invalid_data("benchmark setup failed; see retained logs"));
    }
    let baseline_lock = baseline_manifest
        .parent()
        .ok_or_else(|| invalid_data("baseline manifest has no parent"))?
        .join("Cargo.lock");
    let candidate_lock = candidate_manifest
        .parent()
        .ok_or_else(|| invalid_data("candidate manifest has no parent"))?
        .join("Cargo.lock");
    let retained_baseline_lock = common::retain_artifact(
        &baseline_lock,
        &output.join("artifacts/locks/baseline.Cargo.lock"),
    )?;
    let retained_candidate_lock = common::retain_artifact(
        &candidate_lock,
        &output.join("artifacts/locks/candidate.Cargo.lock"),
    )?;
    if fs::read(&baseline_lock)? != fs::read(&candidate_lock)? {
        if evidence_mode.strict_configuration() {
            return Err(invalid_data(
                "strict ref A/B requires identical resolved dependency lockfiles",
            ));
        }
        evidence_mode.weaken("baseline and candidate resolved different dependency lockfiles");
    }
    common::validate_path_dependencies(
        root,
        root,
        &baseline_manifest,
        features,
        &[&baseline_source],
    )?;
    common::validate_path_dependencies(
        root,
        root,
        &candidate_manifest,
        features,
        &[&candidate_source],
    )?;
    let mut executables = BTreeMap::<(char, String), common::RetainedExecutable>::new();
    for bench in benches {
        let baseline_build = common::prebuild(
            root,
            &baseline_manifest,
            &baseline_target,
            bench,
            features,
            &setup_logs,
            "baseline",
        )?;
        if baseline_build.succeeded() {
            executables.insert(
                ('A', bench.clone()),
                common::retained_cargo_executable(&baseline_build.stdout, bench, &baseline_target)?,
            );
        }
        setup.push(baseline_build);
        let candidate_build = common::prebuild(
            root,
            &candidate_manifest,
            &candidate_target,
            bench,
            features,
            &setup_logs,
            "candidate",
        )?;
        if candidate_build.succeeded() {
            executables.insert(
                ('B', bench.clone()),
                common::retained_cargo_executable(
                    &candidate_build.stdout,
                    bench,
                    &candidate_target,
                )?,
            );
        }
        setup.push(candidate_build);
    }
    if !common::processes_succeeded(&setup) {
        report::write_json(
            &output.join("report.json"),
            &report::ReportEnvelope::new(
                report::ReportKind::RefToRef,
                "setup-failure",
                false,
                SetupFailureReport {
                    environment,
                    repository: root,
                    baseline_ref,
                    candidate_ref,
                    baseline_commit: &baseline_commit,
                    candidate_commit: &candidate_commit,
                    baseline_tree_sha256: baseline_digest.clone(),
                    candidate_tree_sha256: candidate_digest.clone(),
                    benchmark_harness_sha256: common::publication_tree_digest(benchmark_inputs)?,
                    measurement_policy_sha256: common::normalized_text_hash(policy_path)?,
                    benches,
                    filter,
                    features,
                    setup: &setup,
                },
            ),
        )?;
        return Err(invalid_data("benchmark setup failed; see retained logs"));
    }

    let fixture = output.join("stats-fixture");
    fs::create_dir(&fixture)?;
    let mut priming = Vec::new();
    let mut priming_index = BTreeMap::new();
    let mut benchmark_cleanup_safe = true;
    'priming: for subject in ['A', 'B'] {
        for bench in benches {
            let label = format!("prime-{subject}");
            let run = criterion::run(CriterionInvocation {
                root,
                executable: &executables[&(subject, bench.clone())],
                benchmark: bench,
                filter,
                mode: CriterionMode::Prime,
                stats_fixture: &fixture,
                isolate_temp: evidence_mode.strict,
                run_root: &output.join("priming"),
                label: &label,
                max_outlier_fraction,
            })?;
            let cleanup_safe_to_continue = run.cleanup_safe_to_continue();
            priming_index.insert((subject, bench.clone()), priming.len());
            priming.push(run);
            if !cleanup_safe_to_continue {
                benchmark_cleanup_safe = false;
                break 'priming;
            }
        }
    }
    let priming_valid = benchmark_cleanup_safe && priming.iter().all(CriterionRun::valid);
    #[cfg(windows)]
    let windows_stats_detected = priming.iter().any(|run| {
        run.workload_ids().is_some_and(|workloads| {
            workloads
                .iter()
                .any(|workload| criterion::windows_stats_workload(workload))
        })
    });
    #[cfg(not(windows))]
    let windows_stats_detected = false;
    if windows_stats_detected && benchmark_cleanup_safe {
        if evidence_mode.strict_configuration() {
            return Err(invalid_data(
                "Windows filesystem-stat workloads require bench stats in strict mode",
            ));
        }
        evidence_mode
            .weaken("Windows filesystem-stat workloads require the same-process bench stats mode");
    }
    if cooldown > 0.0 {
        thread::sleep(Duration::from_secs_f64(cooldown));
    }

    let mut runs = Vec::new();
    let mut measurements = Vec::new();
    let mut exact_priming_valid = priming_valid && !windows_stats_detected;
    let total_runs = blocks
        .checked_mul(position_replicates)
        .and_then(|runs| runs.checked_mul(orders[0].len()))
        .and_then(|runs| runs.checked_mul(benches.len()))
        .ok_or_else(|| invalid_data("A/B run count overflowed"))?;
    let mut completed_runs = 0usize;
    'measurements: for bench in benches {
        if !exact_priming_valid {
            break;
        }
        for block in 1..=blocks {
            common::ensure_disk_headroom(output, minimum_free_bytes)?;
            let order = &orders[(block - 1) % orders.len()];
            for replicate in 0..position_replicates {
                for (position, subject) in order.iter().copied().enumerate() {
                    let subject_id = subject.as_char();
                    let subject_name = subject.as_str();
                    let run_name = format!(
                        "{bench}-block{block:02}-replicate{:02}-{:02}-{subject_name}",
                        replicate + 1,
                        position + 1
                    );
                    let run = criterion::run(CriterionInvocation {
                        root,
                        executable: &executables[&(subject_id, bench.clone())],
                        benchmark: bench,
                        filter,
                        mode: CriterionMode::Measure(settings),
                        stats_fixture: &fixture,
                        isolate_temp: evidence_mode.strict,
                        run_root: &output.join(&run_name),
                        label: subject_name,
                        max_outlier_fraction,
                    })?;
                    let cleanup_safe_to_continue = run.cleanup_safe_to_continue();
                    let prime = priming_index
                        .get(&(subject_id, bench.clone()))
                        .and_then(|index| priming.get(*index))
                        .ok_or_else(|| invalid_data("missing corresponding priming run"))?;
                    let run_valid = run.matches_priming(prime);
                    exact_priming_valid &= run_valid;
                    if run_valid {
                        measurements.extend(run.estimates.iter().map(|estimate| Measurement {
                            block,
                            position,
                            replicate,
                            subject: subject_id,
                            benchmark: bench.clone(),
                            metric: estimate.metric.clone(),
                            median_ns: estimate.median_ns,
                            run: run_name.clone(),
                        }));
                    }
                    runs.push(run);
                    if !cleanup_safe_to_continue {
                        benchmark_cleanup_safe = false;
                        break 'measurements;
                    }
                    if !run_valid {
                        break 'measurements;
                    }
                    completed_runs += 1;
                    if completed_runs < total_runs && cooldown > 0.0 {
                        thread::sleep(Duration::from_secs_f64(cooldown));
                    }
                }
            }
            disk_snapshots.push(DiskSnapshot::capture(output)?);
        }
    }

    let source_unchanged = common::tree_digest(&baseline_source)? == baseline_digest
        && common::tree_digest(&candidate_source)? == candidate_digest;
    let completed_environment = if benchmark_cleanup_safe {
        Some(EnvironmentSnapshot::capture(output)?)
    } else {
        None
    };
    let mut environment_drift = completed_environment
        .as_ref()
        .map_or_else(Vec::new, |completed| environment.drift_reasons(completed));
    if !benchmark_cleanup_safe {
        environment_drift.push(
            "process cleanup could not be verified; remaining benchmark processes and post-run executable probes were aborted"
                .to_owned(),
        );
    }
    if !source_unchanged {
        environment_drift.push("frozen ref benchmark source changed during execution".to_owned());
    }
    if windows_stats_detected {
        environment_drift.push(
            "Windows filesystem-stat workloads must use the same-process paired-stats mode"
                .to_owned(),
        );
    }
    let evaluation = evaluate_measurements(
        &measurements,
        blocks,
        max_pair_spread,
        margin,
        exact_priming_valid && environment_drift.is_empty(),
        position_replicates,
    )?;
    let metadata = RefMetadata {
        repository: root,
        baseline_ref,
        candidate_ref,
        baseline_commit: &baseline_commit,
        candidate_commit: &candidate_commit,
        baseline_tree_sha256: baseline_digest,
        candidate_tree_sha256: candidate_digest,
        benchmark_harness_sha256: common::publication_tree_digest(benchmark_inputs)?,
        measurement_policy_sha256: common::normalized_text_hash(policy_path)?,
        baseline_lock: retained_baseline_lock.clone(),
        baseline_lock_sha256: common::hash_file(&retained_baseline_lock)?,
        candidate_lock: retained_candidate_lock.clone(),
        candidate_lock_sha256: common::hash_file(&retained_candidate_lock)?,
        benches,
        filter,
        features,
        blocks,
        criterion: settings,
        first_invocation_policy: "separate cold-start process plus one in-process prime before Criterion warm-up; each logical position selects the median of fixed process replicates",
        priming_estimates_used: false,
        non_inferiority_margin: margin,
        max_pair_spread,
        cooldown_seconds: cooldown,
        exploratory: !evidence_mode.strict,
        evidence_mode: &evidence_mode,
    };
    report::write_json(
        &output.join("report.json"),
        &report::ReportEnvelope::new(
            report::ReportKind::RefToRef,
            if evaluation.valid {
                "completed"
            } else {
                "invalid"
            },
            evaluation.valid,
            RefReport {
                decision_passed: evidence_mode.strict && evaluation.passed,
                environment,
                completed_environment,
                disk_snapshots: &disk_snapshots,
                environment_drift: &environment_drift,
                metadata,
                setup: &setup,
                priming: &priming,
                runs: &runs,
                pairs: &evaluation.paired.pairs,
                unstable_blocks: &evaluation.paired.unstable,
                decisions: &evaluation.decisions,
            },
        ),
    )?;
    println!("A/B report: {}", output.join("report.json").display());
    if !evaluation.valid {
        return Err(invalid_data("A/B measurement is invalid; see report"));
    }
    if evidence_mode.strict && !evaluation.passed {
        return Err(invalid_data("A/B non-regression gate failed"));
    }
    Ok(())
}

fn pair_measurements(
    measurements: &[Measurement],
    blocks: usize,
    position_replicates: usize,
    max_pair_spread: f64,
) -> Result<PairedMeasurements> {
    if position_replicates == 0 || position_replicates.is_multiple_of(2) {
        return Err(invalid_data("position replicates must be positive and odd"));
    }
    let keys = measurements
        .iter()
        .map(|item| (item.benchmark.clone(), item.metric.clone()))
        .collect::<BTreeSet<_>>();
    let mut index = BTreeMap::new();
    for measurement in measurements {
        let key = (
            measurement.benchmark.as_str(),
            measurement.metric.as_str(),
            measurement.block,
            measurement.replicate,
            measurement.position,
        );
        if index.insert(key, measurement).is_some() {
            return Err(invalid_data("duplicate benchmark measurement"));
        }
    }
    let mut pairs = Vec::new();
    let mut unstable = Vec::new();
    let mut block_ratios = BTreeMap::<String, Vec<f64>>::new();
    for (benchmark, metric) in keys {
        for block in 1..=blocks {
            let position_measurement = |position: usize| -> Result<&Measurement> {
                let mut values = (0..position_replicates)
                    .map(|replicate| {
                        index
                            .get(&(
                                benchmark.as_str(),
                                metric.as_str(),
                                block,
                                replicate,
                                position,
                            ))
                            .copied()
                            .ok_or_else(|| invalid_data("missing replicated estimate"))
                    })
                    .collect::<Result<Vec<_>>>()?;
                let subject = values[0].subject;
                if values.iter().any(|value| value.subject != subject) {
                    return Err(invalid_data(
                        "replicated position changed benchmark subject",
                    ));
                }
                values.sort_unstable_by(|left, right| {
                    left.median_ns
                        .total_cmp(&right.median_ns)
                        .then_with(|| left.run.cmp(&right.run))
                });
                Ok(values[values.len() / 2])
            };
            let directional = [(0, 1), (2, 3)];
            let mut ratios = [0.0; 2];
            for (index, (first, second)) in directional.into_iter().enumerate() {
                let first = position_measurement(first)?;
                let second = position_measurement(second)?;
                let (baseline, candidate) = match (first.subject, second.subject) {
                    ('A', 'B') => (first, second),
                    ('B', 'A') => (second, first),
                    _ => {
                        return Err(invalid_data(
                            "directional pair must contain one baseline and one candidate",
                        ));
                    }
                };
                ratios[index] = candidate.median_ns / baseline.median_ns;
                pairs.push(PairRecord {
                    benchmark: benchmark.clone(),
                    metric: metric.clone(),
                    block,
                    baseline_run: baseline.run.clone(),
                    candidate_run: candidate.run.clone(),
                    baseline_median_ns: baseline.median_ns,
                    candidate_median_ns: candidate.median_ns,
                    ratio: ratios[index],
                });
            }
            let spread = ratios[0].max(ratios[1]) / ratios[0].min(ratios[1]) - 1.0;
            if spread > max_pair_spread {
                unstable.push(UnstableBlock {
                    benchmark: benchmark.clone(),
                    metric: metric.clone(),
                    block,
                    ratios,
                    pair_spread: spread,
                    max_pair_spread,
                });
            }
            block_ratios
                .entry(format!("{benchmark}::{metric}"))
                .or_default()
                .push(statistics::geometric_mean(ratios[0], ratios[1])?);
        }
    }
    Ok(PairedMeasurements {
        pairs,
        unstable,
        ratios: block_ratios,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn measurement(
        block: usize,
        position: usize,
        replicate: usize,
        subject: char,
        median_ns: f64,
    ) -> Measurement {
        Measurement {
            block,
            position,
            replicate,
            subject,
            benchmark: "bench".to_owned(),
            metric: "metric".to_owned(),
            median_ns,
            run: format!("block-{block}-position-{position}-replicate-{replicate}"),
        }
    }

    #[test]
    fn pairing_uses_observed_position_medians_for_both_orders() {
        let mut measurements = Vec::new();
        let blocks = [
            (1, ['A', 'B', 'B', 'A'], [100.0, 110.0, 120.0, 100.0]),
            (2, ['B', 'A', 'A', 'B'], [90.0, 100.0, 100.0, 95.0]),
        ];
        for (block, order, medians) in blocks {
            for (position, subject) in order.into_iter().enumerate() {
                for (replicate, scale) in [0.5, 1.0, 2.0].into_iter().enumerate() {
                    measurements.push(measurement(
                        block,
                        position,
                        replicate,
                        subject,
                        medians[position] * scale,
                    ));
                }
            }
        }

        let paired = pair_measurements(&measurements, 2, 3, 0.2).unwrap();
        assert!(paired.unstable.is_empty());
        assert_eq!(paired.pairs.len(), 4);
        assert!(
            paired
                .pairs
                .iter()
                .all(|pair| pair.baseline_run.ends_with("replicate-1"))
        );
        assert!(
            paired
                .pairs
                .iter()
                .all(|pair| pair.candidate_run.ends_with("replicate-1"))
        );
        let ratios = &paired.ratios["bench::metric"];
        assert!((ratios[0] - (1.1_f64 * 1.2).sqrt()).abs() < f64::EPSILON);
        assert!((ratios[1] - (0.9_f64 * 0.95).sqrt()).abs() < f64::EPSILON);
    }

    #[test]
    fn non_policy_blocks_require_explicit_exploratory() {
        let mut strict = EvidenceMode::strict();
        assert!(require_ref_policy_settings(&mut strict, false, 7, 8, 10.0, 10.0).is_err());

        let mut exploratory = EvidenceMode::exploratory("explicit --exploratory request");
        require_ref_policy_settings(&mut exploratory, true, 7, 8, 10.0, 10.0).unwrap();
        assert!(
            exploratory
                .reasons
                .iter()
                .any(|reason| reason == "block count differs from the measurement policy")
        );
    }

    #[test]
    fn non_policy_cooldown_requires_explicit_exploratory() {
        let mut strict = EvidenceMode::strict();
        assert!(require_ref_policy_settings(&mut strict, false, 8, 8, 9.0, 10.0).is_err());

        let mut exploratory = EvidenceMode::exploratory("explicit --exploratory request");
        require_ref_policy_settings(&mut exploratory, true, 8, 8, 9.0, 10.0).unwrap();
        assert!(
            exploratory
                .reasons
                .iter()
                .any(|reason| reason == "cooldown differs from the measurement policy")
        );
    }
}
