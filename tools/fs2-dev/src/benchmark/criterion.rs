use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

use super::{common, statistics};
use crate::process::{self, ProcessRecord};
use crate::{Result, invalid_data};

const MAX_CRITERION_LOG_BYTES: u64 = 64 * 1024 * 1024;
const MAX_CRITERION_JSON_BYTES: u64 = 16 * 1024 * 1024;
const MAX_CRITERION_ARTIFACT_ENTRIES: usize = 65_536;
const MAX_CRITERION_ARTIFACT_DEPTH: usize = 64;
const MAX_CRITERION_METRICS: usize = 4_096;

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub(crate) struct CriterionSettings {
    pub(crate) sample_size: usize,
    pub(crate) warm_up_seconds: f64,
    pub(crate) measurement_seconds: f64,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(tag = "kind", content = "settings", rename_all = "kebab-case")]
pub(crate) enum CriterionMode {
    Prime,
    Measure(CriterionSettings),
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct Estimate {
    #[serde(serialize_with = "serialize_workload_id")]
    pub(crate) metric: String,
    pub(crate) median_ns: f64,
    pub(crate) mad_ns: f64,
    pub(crate) std_dev_ns: f64,
    pub(crate) ci_lower_ns: f64,
    pub(crate) ci_upper_ns: f64,
    pub(crate) sample_count: usize,
    pub(crate) outliers: usize,
    pub(crate) outlier_fraction: f64,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct FailureRecord {
    pub(crate) label: String,
    pub(crate) count: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) error: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct PrimeRecord {
    #[serde(serialize_with = "serialize_workload_id")]
    pub(crate) label: String,
    pub(crate) duration_ns: u128,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct CriterionRun {
    pub(crate) mode: CriterionMode,
    pub(crate) process: ProcessRecord,
    pub(crate) prime_observations: Vec<PrimeRecord>,
    pub(crate) estimates: Vec<Estimate>,
    pub(crate) failures: Vec<FailureRecord>,
    pub(crate) criterion_artifact: PathBuf,
    pub(crate) executable_sha256: String,
}

fn logical_workload_id(value: &str) -> String {
    format!("workload-{}", common::hash_bytes(value.as_bytes()))
}

fn serialize_workload_id<S>(value: &str, serializer: S) -> std::result::Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    serializer.serialize_str(&logical_workload_id(value))
}

pub(super) struct CriterionInvocation<'a> {
    pub(super) root: &'a Path,
    pub(super) executable: &'a common::RetainedExecutable,
    pub(super) benchmark: &'a str,
    pub(super) filter: Option<&'a str>,
    pub(super) mode: CriterionMode,
    pub(super) stats_fixture: &'a Path,
    pub(super) isolate_temp: bool,
    pub(super) run_root: &'a Path,
    pub(super) label: &'a str,
    pub(super) max_outlier_fraction: f64,
}

impl CriterionRun {
    pub(crate) fn valid(&self) -> bool {
        self.process.succeeded()
            && self.failures.is_empty()
            && workload_coverage_valid(self.mode, &self.prime_observations, &self.estimates)
    }

    pub(crate) fn cleanup_safe_to_continue(&self) -> bool {
        !self.process.may_still_be_running()
    }

    pub(crate) fn matches_priming(&self, priming: &Self) -> bool {
        matches!(self.mode, CriterionMode::Measure(_))
            && matches!(priming.mode, CriterionMode::Prime)
            && self.valid()
            && priming.valid()
            && prime_workloads(&self.prime_observations)
                == prime_workloads(&priming.prime_observations)
    }

    pub(crate) fn estimates_by_metric(&self) -> BTreeMap<String, f64> {
        self.estimates
            .iter()
            .map(|estimate| (estimate.metric.clone(), estimate.median_ns))
            .collect()
    }

    pub(crate) fn workload_ids(&self) -> Option<BTreeSet<String>> {
        prime_workloads(&self.prime_observations)
            .map(|workloads| workloads.into_iter().map(str::to_owned).collect())
    }
}

fn prime_workloads(observations: &[PrimeRecord]) -> Option<BTreeSet<&str>> {
    let workloads = observations
        .iter()
        .map(|record| record.label.as_str())
        .collect::<BTreeSet<_>>();
    (!workloads.is_empty() && workloads.len() == observations.len()).then_some(workloads)
}

fn estimate_workloads(estimates: &[Estimate]) -> Option<BTreeSet<&str>> {
    let workloads = estimates
        .iter()
        .map(|estimate| estimate.metric.as_str())
        .collect::<BTreeSet<_>>();
    (!workloads.is_empty() && workloads.len() == estimates.len()).then_some(workloads)
}

fn workload_coverage_valid(
    mode: CriterionMode,
    observations: &[PrimeRecord],
    estimates: &[Estimate],
) -> bool {
    let Some(primes) = prime_workloads(observations) else {
        return false;
    };
    match mode {
        CriterionMode::Prime => estimates.is_empty(),
        CriterionMode::Measure(settings) => {
            estimate_workloads(estimates).is_some_and(|ids| ids == primes)
                && estimates
                    .iter()
                    .all(|estimate| estimate.sample_count == settings.sample_size)
        }
    }
}

#[derive(Deserialize)]
struct CriterionEstimates {
    median: EstimateValue,
    median_abs_dev: EstimateValue,
    std_dev: EstimateValue,
}

#[derive(Deserialize)]
struct EstimateValue {
    point_estimate: f64,
    confidence_interval: ConfidenceInterval,
}

#[derive(Deserialize)]
struct ConfidenceInterval {
    lower_bound: f64,
    upper_bound: f64,
}

#[derive(Deserialize)]
struct CriterionBenchmark {
    full_id: String,
    directory_name: String,
}

pub(crate) fn run(invocation: CriterionInvocation<'_>) -> Result<CriterionRun> {
    let CriterionInvocation {
        root,
        executable,
        benchmark,
        filter,
        mode,
        stats_fixture,
        isolate_temp,
        run_root,
        label,
        max_outlier_fraction,
    } = invocation;
    executable.validate()?;
    let criterion_root = run_root.join(format!("{label}-{benchmark}-criterion"));
    let stdout = run_root.join(format!("{label}-{benchmark}.stdout.log"));
    let stderr = run_root.join(format!("{label}-{benchmark}.stderr.log"));
    let protected_temp = isolate_temp
        .then(|| super::common::temporary_workspace(root, "fs2-criterion-", isolate_temp))
        .transpose()?;
    let mut command = std::process::Command::new(executable.path());
    command
        .current_dir(root)
        .env("CRITERION_HOME", &criterion_root)
        .env("FS2_BENCH_REPORT_ERRORS", "0");
    if let Some(protected_temp) = &protected_temp {
        bind_temp_environment(&mut command, protected_temp.path());
    }
    if let Some(filter) = filter.filter(|filter| !filter.is_empty()) {
        command.arg(filter);
    }
    match mode {
        CriterionMode::Prime => {
            command.arg("--test");
        }
        CriterionMode::Measure(settings) => {
            command.args([
                "--bench",
                "--sample-size",
                &settings.sample_size.to_string(),
                "--warm-up-time",
                &settings.warm_up_seconds.to_string(),
                "--measurement-time",
                &settings.measurement_seconds.to_string(),
            ]);
        }
    }
    bind_stats_fixture(&mut command, stats_fixture);
    let process = process::run_logged_attempt(
        &mut command,
        format!("run {label} {benchmark}"),
        &stdout,
        &stderr,
    );
    executable.validate()?;
    let mut failures = Vec::new();
    let stdout_text = read_benchmark_log(&stdout, "stdout", &mut failures);
    let stderr_text = read_benchmark_log(&stderr, "stderr", &mut failures);
    failures.extend(parse_failure_records(&stdout_text));
    failures.extend(parse_failure_records(&stderr_text));
    let mut prime_observations = parse_prime_records(&stdout_text, &mut failures);
    prime_observations.extend(parse_prime_records(&stderr_text, &mut failures));
    if !process.succeeded() {
        failures.push(FailureRecord {
            label: "benchmark_command".to_owned(),
            count: 1,
            error: Some(process.failure_description()),
        });
    }
    let estimates = match mode {
        CriterionMode::Measure(settings) => {
            match collect_estimates(&criterion_root, settings.sample_size, max_outlier_fraction) {
                Ok(estimates) => estimates,
                Err(_) => {
                    failures.push(FailureRecord {
                        label: "criterion_estimates".to_owned(),
                        count: 1,
                        error: Some("Criterion estimates failed validation".to_owned()),
                    });
                    Vec::new()
                }
            }
        }
        CriterionMode::Prime => Vec::new(),
    };
    Ok(CriterionRun {
        mode,
        process,
        prime_observations,
        estimates,
        failures,
        criterion_artifact: criterion_root,
        executable_sha256: executable.sha256().to_owned(),
    })
}

fn bind_stats_fixture(command: &mut std::process::Command, path: &Path) {
    // Every invocation overrides the inherited setting with its admitted fixture.
    command.env("FS2_BENCH_STATS_PATH", path);
}

#[cfg(windows)]
pub(super) fn windows_stats_workload(workload: &str) -> bool {
    workload.split('/').any(|component| {
        matches!(
            component,
            "free_space"
                | "free_space_file_fallback"
                | "available_space"
                | "available_space_file_fallback"
                | "total_space"
                | "allocation_granularity"
                | "stats_snapshot"
                | "prepared_stats"
                | "windows_root_stats"
        )
    })
}

fn bind_temp_environment(command: &mut std::process::Command, path: &Path) {
    command
        .env("TEMP", path)
        .env("TMP", path)
        .env("TMPDIR", path);
}

fn read_benchmark_log(
    path: &Path,
    stream: &'static str,
    failures: &mut Vec<FailureRecord>,
) -> String {
    match read_bounded_utf8(path, MAX_CRITERION_LOG_BYTES) {
        Ok(output) => output,
        Err(error) => {
            failures.push(FailureRecord {
                label: format!("benchmark_{stream}_log"),
                count: 1,
                error: Some(error.to_string()),
            });
            String::new()
        }
    }
}

fn read_bounded_utf8(path: &Path, max_bytes: u64) -> Result<String> {
    let file = fs::File::open(path)?;
    let mut bytes = Vec::new();
    file.take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len())? > max_bytes {
        return Err(invalid_data("benchmark input exceeds its size limit"));
    }
    String::from_utf8(bytes).map_err(|_| invalid_data("benchmark input is not valid UTF-8"))
}

fn collect_estimates(
    root: &Path,
    expected_samples: usize,
    max_outlier_fraction: f64,
) -> Result<Vec<Estimate>> {
    let mut estimates = Vec::new();
    if !root.is_dir() {
        return Err(invalid_data(format!(
            "Criterion produced no output under {}",
            root.display()
        )));
    }
    for (index, entry) in WalkDir::new(root)
        .max_depth(MAX_CRITERION_ARTIFACT_DEPTH)
        .into_iter()
        .enumerate()
    {
        if index >= MAX_CRITERION_ARTIFACT_ENTRIES {
            return Err(invalid_data(
                "Criterion artifact tree exceeds its entry limit",
            ));
        }
        let entry = entry?;
        if entry.file_type().is_symlink() {
            return Err(invalid_data("Criterion artifact tree contains a link"));
        }
        if !entry.file_type().is_file()
            || entry.file_name() != OsStr::new("estimates.json")
            || entry.path().parent().and_then(Path::file_name) != Some(OsStr::new("new"))
        {
            continue;
        }
        if estimates.len() >= MAX_CRITERION_METRICS {
            return Err(invalid_data("Criterion emitted too many metrics"));
        }
        let data: CriterionEstimates =
            serde_json::from_str(&read_bounded_utf8(entry.path(), MAX_CRITERION_JSON_BYTES)?)?;
        let samples: CriterionSamples = serde_json::from_str(&read_bounded_utf8(
            &entry.path().with_file_name("sample.json"),
            MAX_CRITERION_JSON_BYTES,
        )?)?;
        let sample = sample_outliers(&samples, expected_samples)?;
        let metric_root = entry
            .path()
            .parent()
            .and_then(Path::parent)
            .ok_or_else(|| invalid_data("invalid Criterion estimate path"))?;
        let benchmark: CriterionBenchmark = serde_json::from_str(&read_bounded_utf8(
            &entry.path().with_file_name("benchmark.json"),
            MAX_CRITERION_JSON_BYTES,
        )?)?;
        if metric_root.file_name().and_then(OsStr::to_str)
            != Some(benchmark.directory_name.as_str())
            || benchmark.full_id.is_empty()
        {
            return Err(invalid_data("Criterion benchmark identity is inconsistent"));
        }
        let reported = Estimate {
            metric: benchmark.full_id.clone(),
            median_ns: data.median.point_estimate,
            mad_ns: data.median_abs_dev.point_estimate,
            std_dev_ns: data.std_dev.point_estimate,
            ci_lower_ns: data.median.confidence_interval.lower_bound,
            ci_upper_ns: data.median.confidence_interval.upper_bound,
            sample_count: sample.count,
            outliers: sample.outliers,
            outlier_fraction: sample.outlier_fraction,
        };
        validate_estimate(&reported)?;
        let estimate = Estimate {
            metric: benchmark.full_id,
            median_ns: sample.median_ns,
            mad_ns: sample.mad_ns,
            std_dev_ns: sample.std_dev_ns,
            ci_lower_ns: data.median.confidence_interval.lower_bound,
            ci_upper_ns: data.median.confidence_interval.upper_bound,
            sample_count: sample.count,
            outliers: sample.outliers,
            outlier_fraction: sample.outlier_fraction,
        };
        validate_estimate(&estimate)?;
        if estimate.outlier_fraction > max_outlier_fraction {
            return Err(invalid_data(format!(
                "Criterion outlier fraction for {} is {:.3}; maximum is {max_outlier_fraction:.3}",
                estimate.metric, estimate.outlier_fraction
            )));
        }
        estimates.push(estimate);
    }
    estimates.sort_by(|left, right| left.metric.cmp(&right.metric));
    if estimates.is_empty() {
        Err(invalid_data(format!(
            "Criterion produced no estimates under {}",
            root.display()
        )))
    } else {
        Ok(estimates)
    }
}

#[derive(Deserialize)]
struct CriterionSamples {
    iters: Vec<f64>,
    times: Vec<f64>,
}

struct SampleSummary {
    count: usize,
    outliers: usize,
    outlier_fraction: f64,
    median_ns: f64,
    mad_ns: f64,
    std_dev_ns: f64,
}

fn sample_outliers(samples: &CriterionSamples, expected_samples: usize) -> Result<SampleSummary> {
    if samples.iters.is_empty()
        || samples.iters.len() != expected_samples
        || samples.times.len() != expected_samples
    {
        return Err(invalid_data(
            "Criterion sample arrays do not match the expected sample count",
        ));
    }
    let values = samples
        .times
        .iter()
        .zip(&samples.iters)
        .map(|(time, iterations)| {
            if !time.is_finite() || !iterations.is_finite() || *time <= 0.0 || *iterations <= 0.0 {
                Err(invalid_data("Criterion sample is not finite and positive"))
            } else {
                let value = time / iterations;
                (value.is_finite() && value > 0.0)
                    .then_some(value)
                    .ok_or_else(|| {
                        invalid_data("Criterion derived sample is not finite and positive")
                    })
            }
        })
        .collect::<Result<Vec<_>>>()?;
    let mut sorted = values.clone();
    let sample_median = statistics::median(&mut sorted)?;
    let mut deviations = values
        .iter()
        .map(|value| (value - sample_median).abs())
        .collect::<Vec<_>>();
    let mad = statistics::median(&mut deviations)?;
    let outliers = if mad == 0.0 {
        let tolerance = f64::EPSILON * sample_median.abs().max(1.0);
        values
            .iter()
            .filter(|value| (*value - sample_median).abs() > tolerance)
            .count()
    } else {
        values
            .iter()
            .filter(|value| (*value - sample_median).abs() > 3.0 * mad)
            .count()
    };
    let fraction = outliers as f64 / values.len() as f64;
    let mean = values.iter().sum::<f64>() / values.len() as f64;
    let variance = values
        .iter()
        .map(|value| {
            let deviation = value - mean;
            deviation * deviation
        })
        .sum::<f64>()
        / values.len() as f64;
    let std_dev_ns = variance.sqrt();
    if !mean.is_finite() || !std_dev_ns.is_finite() {
        return Err(invalid_data("Criterion sample statistics are not finite"));
    }
    Ok(SampleSummary {
        count: values.len(),
        outliers,
        outlier_fraction: fraction,
        median_ns: sample_median,
        mad_ns: mad,
        std_dev_ns,
    })
}

fn validate_estimate(estimate: &Estimate) -> Result<()> {
    if ![
        estimate.median_ns,
        estimate.mad_ns,
        estimate.std_dev_ns,
        estimate.ci_lower_ns,
        estimate.ci_upper_ns,
    ]
    .iter()
    .all(|value| value.is_finite())
        || estimate.median_ns <= 0.0
        || estimate.mad_ns < 0.0
        || estimate.std_dev_ns < 0.0
        || estimate.ci_lower_ns <= 0.0
        || estimate.ci_lower_ns > estimate.ci_upper_ns
        || estimate.median_ns < estimate.ci_lower_ns
        || estimate.median_ns > estimate.ci_upper_ns
    {
        Err(invalid_data(format!(
            "Criterion emitted invalid estimates for {}",
            estimate.metric
        )))
    } else {
        Ok(())
    }
}

fn parse_failure_records(output: &str) -> Vec<FailureRecord> {
    const PREFIX: &str = "[fs2-bench] FS2_BENCH_FAILURE\t";
    let mut records = Vec::new();
    for line in output.lines() {
        let Some(value) = line.strip_prefix(PREFIX) else {
            continue;
        };
        let parsed = value
            .rsplit_once('\t')
            .filter(|(label, _)| !label.is_empty())
            .and_then(|(label, count)| count.parse::<u64>().ok().map(|count| (label, count)));
        match parsed {
            Some((label, count)) if count > 0 => match unescape_label(label) {
                Ok(label) => records.push(FailureRecord {
                    label: logical_workload_id(&label),
                    count,
                    error: None,
                }),
                Err(_) => records.push(FailureRecord {
                    label: "malformed_failure_record".to_owned(),
                    count: 1,
                    error: Some("failure record contains an invalid escape".to_owned()),
                }),
            },
            Some(_) => records.push(FailureRecord {
                label: "malformed_failure_record".to_owned(),
                count: 1,
                error: Some("failure record has a zero count".to_owned()),
            }),
            None => records.push(FailureRecord {
                label: "malformed_failure_record".to_owned(),
                count: 1,
                error: Some("failure record is malformed".to_owned()),
            }),
        }
    }
    records
}

fn parse_prime_records(output: &str, failures: &mut Vec<FailureRecord>) -> Vec<PrimeRecord> {
    const PREFIX: &str = "[fs2-bench] FS2_BENCH_PRIME\t";
    let mut records = Vec::new();
    for line in output.lines() {
        let Some(value) = line.strip_prefix(PREFIX) else {
            continue;
        };
        let parsed = value
            .rsplit_once('\t')
            .filter(|(label, _)| !label.is_empty())
            .and_then(|(label, duration)| {
                duration
                    .parse::<u128>()
                    .ok()
                    .map(|duration| (label, duration))
            });
        match parsed {
            Some((label, duration_ns)) => match unescape_label(label) {
                Ok(label) => records.push(PrimeRecord { label, duration_ns }),
                Err(_) => failures.push(FailureRecord {
                    label: "malformed_prime_record".to_owned(),
                    count: 1,
                    error: Some("prime record contains an invalid escape".to_owned()),
                }),
            },
            None => failures.push(FailureRecord {
                label: "malformed_prime_record".to_owned(),
                count: 1,
                error: Some("prime record is malformed".to_owned()),
            }),
        }
    }
    records
}

fn unescape_label(value: &str) -> Result<String> {
    let mut result = String::new();
    let mut characters = value.chars();
    while let Some(character) = characters.next() {
        if character == '\\' {
            match characters.next() {
                Some('\\') => result.push('\\'),
                Some('t') => result.push('\t'),
                Some('r') => result.push('\r'),
                Some('n') => result.push('\n'),
                Some(other) => {
                    return Err(invalid_data(format!(
                        "unknown benchmark-label escape: \\{other}"
                    )));
                }
                None => return Err(invalid_data("trailing benchmark-label escape")),
            }
        } else {
            result.push(character);
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prime(label: &str) -> PrimeRecord {
        PrimeRecord {
            label: label.to_owned(),
            duration_ns: 1,
        }
    }

    fn estimate(metric: &str) -> Estimate {
        Estimate {
            metric: metric.to_owned(),
            median_ns: 1.0,
            mad_ns: 0.0,
            std_dev_ns: 0.0,
            ci_lower_ns: 1.0,
            ci_upper_ns: 1.0,
            sample_count: 50,
            outliers: 0,
            outlier_fraction: 0.0,
        }
    }

    #[test]
    fn parses_and_unescapes_failure_records() {
        let records = parse_failure_records("[fs2-bench] FS2_BENCH_FAILURE\ta\\tb\\n\\\\c\t3\n");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].label, logical_workload_id("a\tb\n\\c"));
        assert_eq!(records[0].count, 3);
    }

    #[test]
    fn malformed_failure_records_remain_observable() {
        let records = parse_failure_records(
            "[fs2-bench] FS2_BENCH_FAILURE\tmissing-count\n[fs2-bench] FS2_BENCH_FAILURE\tlabel\tnot-a-count\n",
        );
        assert_eq!(records.len(), 2);
        assert!(
            records
                .iter()
                .all(|record| record.label == "malformed_failure_record")
        );
    }

    #[test]
    fn parses_prime_records_and_rejects_malformed_records() {
        let mut failures = Vec::new();
        let records = parse_prime_records(
            "[fs2-bench] FS2_BENCH_PRIME\ta\\tb\t17\n[fs2-bench] FS2_BENCH_PRIME\tbroken\n",
            &mut failures,
        );
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].label, "a\tb");
        assert_eq!(records[0].duration_ns, 17);
        assert_eq!(failures.len(), 1);
    }

    #[test]
    fn workload_coverage_requires_exact_unique_ids() {
        let settings = CriterionSettings {
            sample_size: 50,
            warm_up_seconds: 2.0,
            measurement_seconds: 5.0,
        };
        let primes = [prime("first"), prime("second")];
        let estimates = [estimate("first"), estimate("second")];
        assert!(workload_coverage_valid(
            CriterionMode::Measure(settings),
            &primes,
            &estimates
        ));
        assert!(!workload_coverage_valid(
            CriterionMode::Measure(settings),
            &primes,
            &[estimate("first"), estimate("other")]
        ));
        assert!(!workload_coverage_valid(
            CriterionMode::Measure(settings),
            &[prime("first"), prime("first")],
            &estimates
        ));
        let mut incomplete = [estimate("first"), estimate("second")];
        incomplete[0].sample_count = 49;
        assert!(!workload_coverage_valid(
            CriterionMode::Measure(settings),
            &primes,
            &incomplete
        ));
    }

    #[test]
    fn criterion_outliers_are_computed_from_structured_samples() {
        let samples = CriterionSamples {
            iters: vec![1.0; 5],
            times: vec![10.0, 10.0, 10.0, 10.0, 100.0],
        };
        let summary = sample_outliers(&samples, 5).unwrap();
        assert_eq!(summary.count, 5);
        assert_eq!(summary.outliers, 1);
        assert_eq!(summary.outlier_fraction, 0.2);
        assert_eq!(summary.median_ns, 10.0);
        assert_eq!(summary.mad_ns, 0.0);
    }

    #[test]
    fn criterion_derived_samples_must_remain_positive() {
        let samples = CriterionSamples {
            iters: vec![f64::MAX],
            times: vec![f64::MIN_POSITIVE],
        };
        assert!(sample_outliers(&samples, 1).is_err());
    }

    #[test]
    fn criterion_inputs_are_read_with_an_explicit_byte_limit() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("input");
        fs::write(&path, b"12345").unwrap();

        assert!(read_bounded_utf8(&path, 4).is_err());
        assert_eq!(read_bounded_utf8(&path, 5).unwrap(), "12345");
    }

    #[test]
    fn child_controlled_report_text_is_normalized() {
        let secret = "credential-sentinel";
        let prime_json = serde_json::to_string(&prime(secret)).unwrap();
        let estimate_json = serde_json::to_string(&estimate(secret)).unwrap();
        let failures = parse_failure_records(&format!(
            "[fs2-bench] FS2_BENCH_FAILURE\t{secret}\t1\n\
             [fs2-bench] FS2_BENCH_FAILURE\t{secret}\n"
        ));
        let failures_json = serde_json::to_string(&failures).unwrap();

        assert!(!prime_json.contains(secret));
        assert!(!estimate_json.contains(secret));
        assert!(!failures_json.contains(secret));
        assert!(prime_json.contains("workload-"));
        assert!(estimate_json.contains("workload-"));
        assert!(failures_json.contains("workload-"));
        assert!(failures_json.contains("failure record is malformed"));
    }

    #[test]
    fn criterion_samples_must_match_the_expected_count() {
        let samples = CriterionSamples {
            iters: vec![1.0; 2],
            times: vec![1.0; 2],
        };

        assert!(sample_outliers(&samples, 1).is_err());
    }

    #[test]
    fn admitted_stats_fixture_overrides_untrusted_environment() {
        let directory = tempfile::tempdir().unwrap();
        for inherited in [r"\\server\share\fixture", "mutable/fixture"] {
            let mut command = std::process::Command::new("unused");
            command.env("FS2_BENCH_STATS_PATH", inherited);
            bind_stats_fixture(&mut command, directory.path());
            let value = command
                .get_envs()
                .find(|(name, _)| *name == OsStr::new("FS2_BENCH_STATS_PATH"))
                .and_then(|(_, value)| value);
            assert_eq!(value, Some(directory.path().as_os_str()));
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_statistics_classification_covers_shared_harnesses() {
        for metric in [
            "free_space",
            "free_space_file_fallback",
            "available_space",
            "available_space_file_fallback",
            "total_space",
            "allocation_granularity",
            "stats_snapshot",
            "prepared_stats",
            "windows_root_stats",
        ] {
            assert!(windows_stats_workload(&format!("subject/{metric}/case")));
        }
        assert!(!windows_stats_workload("subject/file_create_delete"));
    }

    #[test]
    fn strict_fixture_root_binds_every_supported_temp_variable() {
        let directory = tempfile::tempdir().unwrap();
        let mut command = std::process::Command::new("unused");
        bind_temp_environment(&mut command, directory.path());

        for variable in ["TEMP", "TMP", "TMPDIR"] {
            let value = command
                .get_envs()
                .find(|(name, _)| *name == OsStr::new(variable))
                .and_then(|(_, value)| value);
            assert_eq!(value, Some(directory.path().as_os_str()));
        }
    }
}
