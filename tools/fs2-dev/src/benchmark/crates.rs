use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use clap::ArgMatches;
use serde::Serialize;

use super::arguments::{
    CriterionProfile, EvidenceMode, absolute, criterion_evidence_mode, criterion_settings,
    require_exploratory, required_path, required_string,
};
use super::common;
use super::criterion::{self, CriterionInvocation, CriterionMode, CriterionRun, CriterionSettings};
use super::evidence::{DiskSnapshot, EnvironmentSnapshot};
use super::statistics;
use crate::policy;
use crate::policy::PairSubject;
use crate::report;
use crate::{Result, invalid_data};

#[derive(Serialize)]
struct PairResult {
    replicate: usize,
    pair: usize,
    order: [String; 2],
    #[serde(skip_serializing_if = "Option::is_none")]
    baseline: Option<CriterionRun>,
    #[serde(skip_serializing_if = "Option::is_none")]
    candidate: Option<CriterionRun>,
    evaluation: PairEvaluation,
}

#[derive(Serialize)]
#[serde(tag = "status", rename_all = "kebab-case")]
enum PairEvaluation {
    Valid { ratios: BTreeMap<String, f64> },
    Invalid { reason: &'static str },
}

impl PairResult {
    fn cleanup_safe_to_continue(&self) -> bool {
        self.baseline
            .as_ref()
            .is_none_or(CriterionRun::cleanup_safe_to_continue)
            && self
                .candidate
                .as_ref()
                .is_none_or(CriterionRun::cleanup_safe_to_continue)
    }
}

#[derive(Serialize)]
struct ReplicateResult {
    replicate: usize,
    pairs: Vec<PairResult>,
    ratios: BTreeMap<String, f64>,
    valid: bool,
}

#[derive(Serialize)]
struct CrateMetadata<'a> {
    baseline: &'a Path,
    candidate: &'a Path,
    baseline_commit: &'a str,
    candidate_commit: &'a str,
    baseline_package: &'a str,
    candidate_package: &'a str,
    baseline_tree_sha256: &'a str,
    candidate_tree_sha256: &'a str,
    workload_sha256: String,
    benchmark_harness_sha256: String,
    measurement_policy_sha256: String,
    benchmark: &'a str,
    filter: Option<&'a str>,
    pairs: usize,
    criterion: CriterionSettings,
    first_invocation_policy: &'static str,
    priming_estimates_used: bool,
    stats_fixture: &'a Path,
    non_inferiority_margin: f64,
    max_pair_spread: f64,
    evidence_mode: &'a EvidenceMode,
    allow_different_locks: bool,
    baseline_lock: Option<PathBuf>,
    baseline_lock_sha256: Option<String>,
    candidate_lock: Option<PathBuf>,
    candidate_lock_sha256: Option<String>,
}

#[derive(Serialize)]
struct CrateReport<'a> {
    decision_passed: bool,
    environment: EnvironmentSnapshot,
    #[serde(skip_serializing_if = "Option::is_none")]
    completed_environment: Option<EnvironmentSnapshot>,
    disk_snapshots: &'a [DiskSnapshot],
    metadata: CrateMetadata<'a>,
    setup: &'a [crate::process::ProcessRecord],
    priming: &'a [CriterionRun],
    anomalies: &'a [String],
    replicates: &'a [ReplicateResult],
    decisions: &'a [statistics::Decision],
}

#[derive(Serialize)]
struct CrateInvalidContext<'a> {
    decision_passed: bool,
    environment: Option<EnvironmentSnapshot>,
    baseline: &'a Path,
    candidate: &'a Path,
    baseline_package: &'a str,
    candidate_package: &'a str,
    benchmark: &'a str,
    filter: Option<&'a str>,
    measurement_policy_sha256: Option<String>,
}

struct CrateRunSpec<'a> {
    root: &'a Path,
    report_path: &'a Path,
    artifact_root: &'a Path,
    baseline_path: &'a Path,
    candidate_path: &'a Path,
    baseline_package: &'a str,
    candidate_package: &'a str,
    benchmark: &'a str,
    filter: Option<&'a str>,
    pairs: usize,
    settings: CriterionSettings,
    margin: f64,
    allow_different_locks: bool,
    explicitly_exploratory: bool,
    live_sources: bool,
    pair_order: &'a [PairSubject; 4],
    policy_path: &'a Path,
    benchmark_inputs: &'a Path,
    lockfile: &'a Path,
    evidence_mode: EvidenceMode,
    retain_targets: bool,
    minimum_free_bytes: u64,
    max_outlier_fraction: f64,
    max_pair_spread: f64,
}

pub(crate) fn run(root: &Path, arguments: &ArgMatches) -> Result<()> {
    let baseline = absolute(root, required_path(arguments, "baseline")?);
    let candidate = absolute(root, required_path(arguments, "candidate")?);
    let baseline_package = required_string(arguments, "baseline-package")?;
    let candidate_package = required_string(arguments, "candidate-package")?;
    let benchmark = required_string(arguments, "bench")?;
    let filter = arguments.get_one::<String>("filter").map(String::as_str);
    let report_path = arguments
        .get_one::<PathBuf>("report")
        .cloned()
        .map(Ok)
        .unwrap_or_else(|| {
            common::default_output(root, "cross-crate").map(|path| path.with_extension("json"))
        })?;
    let report_path = absolute(root, report_path);
    let artifact_root = report_path.with_extension("artifacts");
    let source_policy = root.join("benchmarks/measurement-policy.json");
    let (policy, policy_bytes) = policy::load_with_source(&source_policy)?;
    let pairs = arguments
        .get_one::<usize>("pairs")
        .copied()
        .unwrap_or(usize::try_from(policy.cross_crate.pairs)?);
    if pairs < 24
        || pairs > usize::try_from(policy.cross_crate.maximum_pairs)?
        || !pairs.is_multiple_of(8)
    {
        return Err(invalid_data(
            "cross-crate pairs must be a bounded multiple of eight and at least 24",
        ));
    }
    let settings = criterion_settings(arguments, &policy)?;
    let margin = arguments
        .get_one::<f64>("non-inferiority-margin")
        .copied()
        .unwrap_or(policy.non_inferiority_margin);
    if !margin.is_finite() || !(0.0..1.0).contains(&margin) {
        return Err(invalid_data("invalid non-inferiority margin"));
    }
    let allow_different_locks = arguments.get_flag("allow-different-locks");
    let explicitly_exploratory = arguments.get_flag("exploratory");
    let mut evidence_mode = criterion_evidence_mode(
        explicitly_exploratory,
        settings,
        &policy,
        CriterionProfile::CrossCrate,
    )?;
    require_exploratory(
        &mut evidence_mode,
        explicitly_exploratory,
        pairs != usize::try_from(policy.cross_crate.pairs)?,
        "pair count differs from the measurement policy",
    )?;
    require_exploratory(
        &mut evidence_mode,
        explicitly_exploratory,
        margin != policy.non_inferiority_margin,
        "non-inferiority margin differs from the measurement policy",
    )?;
    common::require_strict_windows_local_volume(
        root,
        "strict cross-crate benchmark repository root",
        evidence_mode.strict_configuration(),
    )?;
    common::require_strict_windows_local_volume(
        &report_path,
        "strict cross-crate report destination",
        evidence_mode.strict_configuration(),
    )?;
    common::require_strict_windows_local_volume(
        &artifact_root,
        "strict cross-crate artifact destination",
        evidence_mode.strict_configuration(),
    )?;
    let _report_guard = super::output::preflight(
        root,
        &report_path,
        "report",
        evidence_mode.strict_configuration(),
        policy.resources.minimum_free_bytes,
    )?;
    let _artifact_guard = super::output::preflight(
        root,
        &artifact_root,
        "artifact directory",
        evidence_mode.strict_configuration(),
        policy.resources.minimum_free_bytes,
    )?;
    let report_path = _report_guard.path().to_owned();
    let artifact_root = _artifact_guard.path().to_owned();
    if arguments.get_one::<PathBuf>("target-root").is_some() {
        return Err(invalid_data(
            "--target-root is not accepted because Cargo cannot bind writes to a directory capability; use the private staged target and --retain-targets",
        ));
    }
    let staged = super::output::StagedBundle::new(
        root,
        "fs2-crates-output-",
        evidence_mode.strict_configuration(),
    )?;
    let staged_report = staged.path().join("report.json");
    let staged_artifact_root = staged.path().join("artifacts");
    fs::create_dir(&staged_artifact_root)?;
    let inputs = staged_artifact_root.join("inputs");
    fs::create_dir_all(&inputs)?;
    let policy_path = common::retain_bytes(&policy_bytes, &inputs.join("measurement-policy.json"))?;
    let benchmark_inputs = inputs.join("benchmarks");
    common::copy_tree(&root.join("benchmarks"), &benchmark_inputs)?;
    let lockfile = common::retain_artifact(&root.join("Cargo.lock"), &inputs.join("Cargo.lock"))?;
    let result = execute(CrateRunSpec {
        root,
        report_path: &staged_report,
        artifact_root: &staged_artifact_root,
        baseline_path: &baseline,
        candidate_path: &candidate,
        baseline_package,
        candidate_package,
        benchmark,
        filter,
        pairs,
        settings,
        margin,
        allow_different_locks,
        explicitly_exploratory,
        live_sources: explicitly_exploratory,
        pair_order: &policy.cross_crate.pair_order,
        policy_path: &policy_path,
        benchmark_inputs: &benchmark_inputs,
        lockfile: &lockfile,
        evidence_mode,
        retain_targets: arguments.get_flag("retain-targets"),
        minimum_free_bytes: policy.resources.minimum_free_bytes,
        max_outlier_fraction: policy.criterion.max_outlier_fraction,
        max_pair_spread: policy.ref_to_ref.max_pair_spread,
    });
    if let Err(error) = &result
        && !staged_report.exists()
    {
        report::write_invalid(
            &staged_report,
            report::ReportKind::CrossCrate,
            &error.to_string(),
            CrateInvalidContext {
                decision_passed: false,
                environment: EnvironmentSnapshot::capture(&staged_artifact_root).ok(),
                baseline: &baseline,
                candidate: &candidate,
                baseline_package,
                candidate_package,
                benchmark,
                filter,
                measurement_policy_sha256: common::normalized_text_hash(&policy_path).ok(),
            },
        )?;
    }
    let publication = staged.publish(
        &staged_report,
        &report_path,
        &staged_artifact_root,
        &artifact_root,
    );
    match (result, publication) {
        (_, Err(error)) => Err(error),
        (result, Ok(())) => result,
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum Subject {
    Baseline,
    Candidate,
}

impl Subject {
    const ALL: [Self; 2] = [Self::Baseline, Self::Candidate];

    const fn name(self) -> &'static str {
        match self {
            Self::Baseline => "baseline",
            Self::Candidate => "candidate",
        }
    }

    fn source<'a>(self, baseline: &'a Path, candidate: &'a Path) -> &'a Path {
        match self {
            Self::Baseline => baseline,
            Self::Candidate => candidate,
        }
    }

    fn package<'a>(self, baseline: &'a str, candidate: &'a str) -> &'a str {
        match self {
            Self::Baseline => baseline,
            Self::Candidate => candidate,
        }
    }

    fn features<'a>(self, baseline: &'a [String], candidate: &'a [String]) -> &'a [String] {
        match self {
            Self::Baseline => baseline,
            Self::Candidate => candidate,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum Slot {
    A,
    B,
}

impl Slot {
    const ALL: [Self; 2] = [Self::A, Self::B];

    const fn name(self) -> &'static str {
        match self {
            Self::A => "a",
            Self::B => "b",
        }
    }
}

#[derive(Clone, Copy)]
enum PairOrder {
    BaselineFirst,
    CandidateFirst,
}

impl PairOrder {
    const fn from_subject(value: PairSubject) -> Self {
        match value {
            PairSubject::A => Self::BaselineFirst,
            PairSubject::B => Self::CandidateFirst,
        }
    }

    const fn subjects(self) -> [Subject; 2] {
        match self {
            Self::BaselineFirst => [Subject::Baseline, Subject::Candidate],
            Self::CandidateFirst => [Subject::Candidate, Subject::Baseline],
        }
    }
}

struct Harness {
    manifest: PathBuf,
    target: PathBuf,
    executable: Option<common::RetainedExecutable>,
}

fn retain_lock_artifact(contents: Option<&[u8]>, path: PathBuf) -> Result<Option<PathBuf>> {
    let Some(contents) = contents else {
        return Ok(None);
    };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, contents)?;
    Ok(Some(path))
}

struct PairRunner<'a> {
    root: &'a Path,
    artifact: &'a Path,
    harnesses: &'a BTreeMap<(Subject, Slot), Harness>,
    priming: &'a BTreeMap<(Subject, Slot), CriterionRun>,
    benchmark: &'a str,
    filter: Option<&'a str>,
    settings: CriterionSettings,
    expected_metrics: &'a std::collections::BTreeSet<String>,
    stats_fixture: &'a Path,
    isolate_temp: bool,
    max_outlier_fraction: f64,
}

impl PairRunner<'_> {
    fn execute(&self, order: PairOrder, replicate: usize, local_pair: usize) -> Result<PairResult> {
        let baseline_slot = if local_pair < 2 { Slot::A } else { Slot::B };
        let candidate_slot = if local_pair < 2 { Slot::B } else { Slot::A };
        let mut observed = BTreeMap::new();
        for subject in order.subjects() {
            let slot = if subject == Subject::Baseline {
                baseline_slot
            } else {
                candidate_slot
            };
            let harness = self
                .harnesses
                .get(&(subject, slot))
                .ok_or_else(|| invalid_data("missing cross-crate benchmark harness"))?;
            let run = criterion::run(CriterionInvocation {
                root: self.root,
                executable: harness
                    .executable
                    .as_ref()
                    .ok_or_else(|| invalid_data("benchmark executable was not built"))?,
                benchmark: self.benchmark,
                filter: self.filter,
                mode: CriterionMode::Measure(self.settings),
                stats_fixture: self.stats_fixture,
                isolate_temp: self.isolate_temp,
                run_root: &self.artifact.join(format!("pair-{local_pair:02}")),
                label: subject.name(),
                max_outlier_fraction: self.max_outlier_fraction,
            })?;
            let cleanup_safe_to_continue = run.cleanup_safe_to_continue();
            let priming = self
                .priming
                .get(&(subject, slot))
                .ok_or_else(|| invalid_data("missing corresponding priming run"))?;
            let valid = run.matches_priming(priming);
            observed.insert(subject, (run, valid));
            if !cleanup_safe_to_continue {
                break;
            }
        }
        let baseline = observed.remove(&Subject::Baseline);
        let candidate = observed.remove(&Subject::Candidate);
        let cleanup_safe_to_continue = baseline
            .as_ref()
            .is_none_or(|(run, _)| run.cleanup_safe_to_continue())
            && candidate
                .as_ref()
                .is_none_or(|(run, _)| run.cleanup_safe_to_continue());
        let evaluation = if !cleanup_safe_to_continue {
            PairEvaluation::Invalid {
                reason: "process cleanup could not be verified",
            }
        } else if let (Some((baseline, baseline_valid)), Some((candidate, candidate_valid))) =
            (&baseline, &candidate)
        {
            if *baseline_valid && *candidate_valid {
                let baseline_estimates = baseline.estimates_by_metric();
                let candidate_estimates = candidate.estimates_by_metric();
                if baseline_estimates.keys().eq(candidate_estimates.keys()) {
                    let metrics: BTreeSet<_> = baseline_estimates.keys().cloned().collect();
                    if &metrics != self.expected_metrics {
                        PairEvaluation::Invalid {
                            reason: "benchmark metric set differs from priming",
                        }
                    } else {
                        PairEvaluation::Valid {
                            ratios: baseline_estimates
                                .into_iter()
                                .map(|(metric, baseline_ns)| {
                                    let ratio = candidate_estimates[&metric] / baseline_ns;
                                    (metric, ratio)
                                })
                                .collect(),
                        }
                    }
                } else {
                    PairEvaluation::Invalid {
                        reason: "baseline and candidate emitted different metrics",
                    }
                }
            } else {
                PairEvaluation::Invalid {
                    reason: "benchmark run was invalid or did not match its priming workload set",
                }
            }
        } else {
            PairEvaluation::Invalid {
                reason: "paired benchmark execution was incomplete",
            }
        };

        Ok(PairResult {
            replicate: replicate + 1,
            pair: replicate * 4 + local_pair + 1,
            order: order.subjects().map(|subject| subject.name().to_owned()),
            baseline: baseline.map(|(run, _)| run),
            candidate: candidate.map(|(run, _)| run),
            evaluation,
        })
    }
}

fn require_different_lockfiles_exploratory(
    evidence_mode: &mut EvidenceMode,
    explicitly_exploratory: bool,
    allow_different_locks: bool,
    lockfiles_differ: bool,
) -> Result<()> {
    require_exploratory(
        evidence_mode,
        explicitly_exploratory,
        allow_different_locks && lockfiles_differ,
        "baseline and candidate resolved different dependency lockfiles",
    )
}

fn execute(spec: CrateRunSpec<'_>) -> Result<()> {
    let CrateRunSpec {
        root,
        report_path,
        artifact_root,
        baseline_path,
        candidate_path,
        baseline_package,
        candidate_package,
        benchmark,
        filter,
        pairs,
        settings,
        margin,
        allow_different_locks,
        explicitly_exploratory,
        live_sources,
        pair_order,
        policy_path,
        benchmark_inputs,
        lockfile,
        evidence_mode,
        retain_targets,
        minimum_free_bytes,
        max_outlier_fraction,
        max_pair_spread,
    } = spec;
    let mut evidence_mode = evidence_mode;
    let strict = evidence_mode.strict_configuration();
    let (baseline_repository, baseline_commit) =
        common::repository_state(baseline_path, "baseline", strict)?;
    let (candidate_repository, candidate_commit) =
        common::repository_state(candidate_path, "candidate", strict)?;
    if baseline_repository.path() == candidate_repository.path() {
        return Err(invalid_data(
            "baseline and candidate must be different checkouts",
        ));
    }
    let baseline_features = common::subject_features(benchmark, baseline_package)?;
    let candidate_features = common::subject_features(benchmark, candidate_package)?;
    let temporary =
        common::temporary_workspace(root, "fs2-crates-", evidence_mode.strict_configuration())?;
    let stats_fixture = common::temporary_workspace(root, "fs2-crates-stats-", strict)?;
    let environment = EnvironmentSnapshot::capture(stats_fixture.path())?;
    if let Some(reason) = environment.strict_failure_reason() {
        if evidence_mode.strict_configuration() {
            return Err(invalid_data(reason));
        }
        evidence_mode.weaken(reason);
    }
    let frozen = temporary.path().join("frozen");
    fs::create_dir(&frozen)?;
    let baseline_source = frozen.join("baseline");
    let candidate_source = frozen.join("candidate");
    let source_setup = if !live_sources {
        let source_logs = artifact_root.join("source-setup");
        let mut setup = common::clone_revision(
            baseline_repository.path(),
            &baseline_source,
            &baseline_commit,
            &source_logs,
            "baseline-source",
        )?;
        setup.extend(common::clone_revision(
            candidate_repository.path(),
            &candidate_source,
            &candidate_commit,
            &source_logs,
            "candidate-source",
        )?);
        if !common::processes_succeeded(&setup) {
            report::write_setup_failure(
                report_path,
                report::ReportKind::CrossCrate,
                "unable to materialize recorded cross-crate revisions",
                &setup,
            )?;
            return Err(invalid_data(
                "cross-crate source setup failed; see retained logs",
            ));
        }
        setup
    } else {
        evidence_mode.weaken(
            "exploratory cross-crate sources use live checkout bytes rather than immutable commit materialization",
        );
        common::copy_tree(baseline_repository.path(), &baseline_source)?;
        common::copy_tree(candidate_repository.path(), &candidate_source)?;
        Vec::new()
    };
    let baseline_digest = common::tree_digest(&baseline_source)?;
    let candidate_digest = common::tree_digest(&candidate_source)?;
    if !live_sources {
        if common::resolve_ref(&baseline_source, "HEAD")? != baseline_commit
            || common::resolve_ref(&candidate_source, "HEAD")? != candidate_commit
        {
            return Err(invalid_data(
                "materialized cross-crate source does not match the recorded commit",
            ));
        }
    } else if baseline_digest != common::tree_digest(baseline_repository.path())?
        || candidate_digest != common::tree_digest(candidate_repository.path())?
    {
        return Err(invalid_data("frozen checkout differs from its source tree"));
    }
    let target_root = temporary.path().join("cargo-target");
    fs::create_dir_all(&target_root)?;
    let replicate_count = pairs / 4;
    let mut replicate_results = Vec::new();
    let mut ratios = BTreeMap::<String, Vec<f64>>::new();
    let mut disk_snapshots = Vec::with_capacity(replicate_count);
    let mut anomalies = Vec::new();

    let harness_root = temporary.path().join("prepared-harnesses");
    let setup_artifact = artifact_root.join("setup");
    let mut harnesses = BTreeMap::new();
    for subject in Subject::ALL {
        let source = subject.source(&baseline_source, &candidate_source);
        let package = subject.package(baseline_package, candidate_package);
        for slot in Slot::ALL {
            let name = format!("harness-{}-{}", subject.name(), slot.name());
            let manifest = common::prepare_harness(
                &harness_root,
                &name,
                source,
                package,
                benchmark_inputs,
                lockfile,
            )?;
            let target = target_root.join(format!("{}-{}", subject.name(), slot.name()));
            harnesses.insert(
                (subject, slot),
                Harness {
                    manifest,
                    target,
                    executable: None,
                },
            );
        }
    }

    let mut setup = source_setup;
    for subject in Subject::ALL {
        let harness = &harnesses[&(subject, Slot::A)];
        let record = common::generate_lockfile(
            root,
            &harness.manifest,
            &harness.target,
            &setup_artifact,
            subject.name(),
        )?;
        let lock_succeeded = record.succeeded();
        setup.push(record);
        if lock_succeeded {
            let source = harness
                .manifest
                .parent()
                .ok_or_else(|| invalid_data("benchmark manifest has no parent"))?
                .join("Cargo.lock");
            let destination = harnesses[&(subject, Slot::B)]
                .manifest
                .parent()
                .ok_or_else(|| invalid_data("benchmark manifest has no parent"))?
                .join("Cargo.lock");
            if let Err(error) = fs::copy(source, destination) {
                anomalies.push(format!(
                    "unable to reuse {} dependency lockfile: {error}",
                    subject.name()
                ));
            }
        } else {
            anomalies.push(format!("{} lockfile generation failed", subject.name()));
        }
    }

    let locks = harnesses
        .iter()
        .filter_map(|(key, harness)| {
            let parent = harness.manifest.parent()?;
            fs::read(parent.join("Cargo.lock"))
                .ok()
                .map(|lock| (*key, lock))
        })
        .collect::<BTreeMap<_, _>>();
    for subject in Subject::ALL {
        match (
            locks.get(&(subject, Slot::A)),
            locks.get(&(subject, Slot::B)),
        ) {
            (Some(left), Some(right)) if left == right => {}
            (Some(_), Some(_)) => anomalies.push(format!(
                "{} slots resolved different dependency locks",
                subject.name()
            )),
            _ => anomalies.push(format!("{} dependency lockfile is missing", subject.name())),
        }
    }
    let baseline_lock = locks.get(&(Subject::Baseline, Slot::A)).cloned();
    let candidate_lock = locks.get(&(Subject::Candidate, Slot::A)).cloned();
    let retained_baseline_lock = retain_lock_artifact(
        baseline_lock.as_deref(),
        artifact_root.join("locks/baseline.Cargo.lock"),
    )?;
    let retained_candidate_lock = retain_lock_artifact(
        candidate_lock.as_deref(),
        artifact_root.join("locks/candidate.Cargo.lock"),
    )?;
    let lockfiles_differ = match (&baseline_lock, &candidate_lock) {
        (Some(baseline), Some(candidate)) => baseline != candidate,
        _ => false,
    };
    require_different_lockfiles_exploratory(
        &mut evidence_mode,
        explicitly_exploratory,
        allow_different_locks,
        lockfiles_differ,
    )?;
    if lockfiles_differ && !allow_different_locks {
        anomalies.push("baseline and candidate resolved different dependency lockfiles".to_owned());
    }

    for ((subject, _), harness) in &harnesses {
        common::validate_path_dependencies(
            root,
            root,
            &harness.manifest,
            subject.features(&baseline_features, &candidate_features),
            &[subject.source(&baseline_source, &candidate_source)],
        )?;
    }

    for ((subject, slot), harness) in &mut harnesses {
        let record = common::prebuild(
            root,
            &harness.manifest,
            &harness.target,
            benchmark,
            subject.features(&baseline_features, &candidate_features),
            &setup_artifact,
            &format!("{}-{}", subject.name(), slot.name()),
        )?;
        if record.succeeded() {
            match common::retained_cargo_executable(&record.stdout, benchmark, &harness.target) {
                Ok(executable) => harness.executable = Some(executable),
                Err(error) => anomalies.push(error.to_string()),
            }
        }
        setup.push(record);
    }
    if !common::processes_succeeded(&setup) {
        anomalies.push("cross-crate benchmark setup failed".to_owned());
    }

    let mut priming = BTreeMap::new();
    let mut benchmark_cleanup_safe = true;
    if anomalies.is_empty() {
        for ((subject, slot), harness) in &harnesses {
            let run = criterion::run(CriterionInvocation {
                root,
                executable: harness
                    .executable
                    .as_ref()
                    .ok_or_else(|| invalid_data("benchmark executable was not built"))?,
                benchmark,
                filter,
                mode: CriterionMode::Prime,
                stats_fixture: stats_fixture.path(),
                isolate_temp: evidence_mode.strict,
                run_root: &artifact_root.join("priming"),
                label: &format!("{}-{}", subject.name(), slot.name()),
                max_outlier_fraction,
            })?;
            let cleanup_safe_to_continue = run.cleanup_safe_to_continue();
            priming.insert((*subject, *slot), run);
            if !cleanup_safe_to_continue {
                benchmark_cleanup_safe = false;
                anomalies.push(format!(
                    "{}-{} priming process cleanup could not be verified; remaining benchmark processes were aborted",
                    subject.name(),
                    slot.name()
                ));
                break;
            }
        }
    }
    let expected_metrics = priming
        .values()
        .next()
        .and_then(CriterionRun::workload_ids)
        .unwrap_or_default();
    if expected_metrics.is_empty()
        || priming
            .values()
            .any(|run| !run.valid() || run.workload_ids().as_ref() != Some(&expected_metrics))
    {
        anomalies.push("priming did not establish one exact workload set".to_owned());
    }

    #[cfg(windows)]
    if expected_metrics
        .iter()
        .any(|workload| criterion::windows_stats_workload(workload))
    {
        if evidence_mode.strict_configuration() {
            return Err(invalid_data(
                "Windows filesystem-stat workloads require bench stats in strict mode",
            ));
        }
        evidence_mode
            .weaken("Windows filesystem-stat workloads require the same-process bench stats mode");
    }

    let priming_valid = anomalies.is_empty();
    for replicate in 0..replicate_count {
        if !priming_valid {
            break;
        }
        let artifact = artifact_root.join(format!("replicate-{replicate:03}"));
        if let Err(error) = common::ensure_disk_headroom(artifact_root, minimum_free_bytes) {
            anomalies.push(format!("replicate {}: {error}", replicate + 1));
            replicate_results.push(ReplicateResult {
                replicate: replicate + 1,
                pairs: Vec::new(),
                ratios: BTreeMap::new(),
                valid: false,
            });
            break;
        }

        let runner = PairRunner {
            root,
            artifact: &artifact,
            harnesses: &harnesses,
            priming: &priming,
            benchmark,
            filter,
            settings,
            expected_metrics: &expected_metrics,
            stats_fixture: stats_fixture.path(),
            isolate_temp: evidence_mode.strict,
            max_outlier_fraction,
        };
        let mut pair_results = Vec::new();
        let mut replicate_ratios = BTreeMap::<String, Vec<f64>>::new();
        let mut valid = true;
        let mut replicate_cleanup_safe = true;
        for (local_pair, &subject) in pair_order.iter().enumerate() {
            let order = PairOrder::from_subject(subject);
            let pair = runner.execute(order, replicate, local_pair)?;
            let pair_cleanup_safe = pair.cleanup_safe_to_continue();
            match &pair.evaluation {
                PairEvaluation::Valid {
                    ratios: pair_ratios,
                } => {
                    for (metric, ratio) in pair_ratios {
                        replicate_ratios
                            .entry(metric.clone())
                            .or_default()
                            .push(*ratio);
                    }
                }
                PairEvaluation::Invalid { .. } => {
                    valid = false;
                    anomalies.push(format!(
                        "replicate {} pair {} was invalid",
                        replicate + 1,
                        local_pair + 1
                    ));
                }
            }
            pair_results.push(pair);
            if !pair_cleanup_safe {
                valid = false;
                replicate_cleanup_safe = false;
                benchmark_cleanup_safe = false;
                anomalies.push(format!(
                    "replicate {} pair {} process cleanup could not be verified; remaining pairs and replicates were aborted",
                    replicate + 1,
                    local_pair + 1
                ));
                break;
            }
        }
        let mut summary = BTreeMap::new();
        if valid {
            for metric in &expected_metrics {
                let Some(mut values) = replicate_ratios.remove(metric) else {
                    valid = false;
                    anomalies.push(format!(
                        "replicate {} omitted primed metric {metric}",
                        replicate + 1
                    ));
                    continue;
                };
                if values.len() != 4 {
                    valid = false;
                    anomalies.push(format!(
                        "replicate {} produced {} pairs for {metric}; expected 4",
                        replicate + 1,
                        values.len()
                    ));
                    continue;
                }
                let minimum = values.iter().copied().fold(f64::INFINITY, f64::min);
                let maximum = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
                let spread = maximum / minimum - 1.0;
                if spread > max_pair_spread {
                    valid = false;
                    anomalies.push(format!(
                        "replicate {} pair spread for {metric} was {spread:.6}; maximum is {max_pair_spread:.6}",
                        replicate + 1
                    ));
                    continue;
                }
                let value = statistics::median(&mut values)?;
                summary.insert(metric.clone(), value);
                ratios.entry(metric.clone()).or_default().push(value);
            }
        }
        disk_snapshots.push(DiskSnapshot::capture(artifact_root)?);
        replicate_results.push(ReplicateResult {
            replicate: replicate + 1,
            pairs: pair_results,
            ratios: summary,
            valid,
        });
        if !replicate_cleanup_safe {
            break;
        }
    }

    if retain_targets {
        let retained = artifact_root.join("targets");
        if retained.exists() {
            anomalies.push("retained target destination already exists".to_owned());
        } else if let Err(error) = fs::rename(&target_root, &retained) {
            anomalies.push(format!(
                "unable to retain private benchmark targets: {error}"
            ));
        }
    } else {
        let targets = harnesses
            .values()
            .map(|harness| harness.target.clone())
            .collect::<BTreeSet<_>>();
        for target in targets {
            if target.exists()
                && let Err(error) = fs::remove_dir_all(&target)
            {
                anomalies.push(format!(
                    "unable to remove benchmark target {}: {error}",
                    target.display()
                ));
            }
        }
    }
    if common::tree_digest(&baseline_source)? != baseline_digest
        || common::tree_digest(&candidate_source)? != candidate_digest
    {
        anomalies.push("frozen cross-crate source changed during execution".to_owned());
    }
    let completed_environment = if benchmark_cleanup_safe {
        Some(EnvironmentSnapshot::capture(stats_fixture.path())?)
    } else {
        None
    };
    if let Some(completed_environment) = &completed_environment {
        anomalies.extend(environment.drift_reasons(completed_environment));
    }
    let all_valid = anomalies.is_empty()
        && replicate_results.len() == replicate_count
        && replicate_results.iter().all(|replicate| replicate.valid);
    let decisions = if all_valid {
        statistics::evaluate(&ratios, margin)?
    } else {
        Vec::new()
    };
    let performance_passed = !decisions.is_empty()
        && decisions
            .iter()
            .all(|decision| decision.disposition != "inconclusive-or-slower");
    let passed = evidence_mode.strict && performance_passed;
    let priming_records = priming.into_values().collect::<Vec<_>>();
    report::write_json(
        report_path,
        &report::ReportEnvelope::new(
            report::ReportKind::CrossCrate,
            if all_valid { "completed" } else { "invalid" },
            all_valid,
            CrateReport {
                decision_passed: passed,
                environment,
                completed_environment,
                disk_snapshots: &disk_snapshots,
                metadata: CrateMetadata {
                    baseline: baseline_repository.path(),
                    candidate: candidate_repository.path(),
                    baseline_commit: &baseline_commit,
                    candidate_commit: &candidate_commit,
                    baseline_package,
                    candidate_package,
                    baseline_tree_sha256: &baseline_digest,
                    candidate_tree_sha256: &candidate_digest,
                    workload_sha256: common::hash_file(
                        &benchmark_inputs
                            .join("benches")
                            .join(format!("{benchmark}.rs")),
                    )?,
                    benchmark_harness_sha256: common::publication_tree_digest(benchmark_inputs)?,
                    measurement_policy_sha256: common::normalized_text_hash(policy_path)?,
                    benchmark,
                    filter,
                    pairs,
                    criterion: settings,
                    first_invocation_policy: "separate cold-start process plus one in-process prime before Criterion warm-up",
                    priming_estimates_used: false,
                    stats_fixture: stats_fixture.path(),
                    non_inferiority_margin: margin,
                    max_pair_spread,
                    evidence_mode: &evidence_mode,
                    allow_different_locks,
                    baseline_lock: retained_baseline_lock.clone(),
                    baseline_lock_sha256: baseline_lock.as_deref().map(common::hash_bytes),
                    candidate_lock: retained_candidate_lock.clone(),
                    candidate_lock_sha256: candidate_lock.as_deref().map(common::hash_bytes),
                },
                setup: &setup,
                priming: &priming_records,
                anomalies: &anomalies,
                replicates: &replicate_results,
                decisions: &decisions,
            },
        ),
    )?;
    println!("report={}", report_path.display());
    if !all_valid {
        return Err(invalid_data("cross-crate measurement is invalid"));
    }
    if evidence_mode.strict && !performance_passed {
        return Err(invalid_data(
            "at least one workload did not prove non-inferiority",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allow_different_locks_requires_explicit_exploratory_only_for_a_mismatch() {
        let mut matching = EvidenceMode::strict();
        require_different_lockfiles_exploratory(&mut matching, false, true, false).unwrap();
        assert!(matching.strict_configuration());

        let mut strict = EvidenceMode::strict();
        assert!(require_different_lockfiles_exploratory(&mut strict, false, true, true).is_err());

        let mut exploratory = EvidenceMode::exploratory("explicit --exploratory request");
        require_different_lockfiles_exploratory(&mut exploratory, true, true, true).unwrap();
        assert!(exploratory.reasons.iter().any(|reason| {
            reason == "baseline and candidate resolved different dependency lockfiles"
        }));
    }
}
