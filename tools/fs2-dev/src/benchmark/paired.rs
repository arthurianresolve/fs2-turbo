use std::collections::BTreeSet;
use std::fs;
use std::io::Read;
use std::path::Path;
use std::process::Command;
use std::thread;
use std::time::Duration;

use clap::ArgMatches;
use serde::Serialize;

use super::arguments::{EvidenceMode, require_exploratory};
use super::common;
use super::statistics;
use crate::policy;
use crate::process::{self, ProcessRecord};
use crate::{Result, invalid_data};

#[path = "../../../../benchmarks/paired_protocol.rs"]
mod protocol;

use protocol::{HEADER, PROTOCOL};

const MAX_REPLICATES: usize = policy::MAX_PAIRED_REPLICATES as usize;
const MAX_SAMPLE_SIZE: usize = policy::MAX_SAMPLE_SIZE as usize;
const MAX_DURATION_SECONDS: f64 = policy::MAX_DURATION_SECONDS;
const MAX_COOLDOWN_SECONDS: f64 = policy::MAX_DURATION_SECONDS;
const MAX_PAIRED_PROTOCOL_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Clone, Debug, Serialize)]
pub(super) struct Measurement {
    pub(super) run: String,
    pub(super) mode: String,
    pub(super) metric: String,
    pub(super) baseline_ns: f64,
    pub(super) candidate_ns: f64,
    pub(super) ratio: f64,
    pub(super) aggregate_ratio: f64,
    pub(super) ratio_mad: f64,
    pub(super) samples: u64,
    pub(super) iterations: u64,
    pub(super) outliers: u64,
    pub(super) warm_up_failures: u64,
    pub(super) failures: u64,
    pub(super) prime_baseline_ns: u128,
    pub(super) prime_candidate_ns: u128,
    pub(super) prime_failures: u64,
    pub(super) ratio_samples: Vec<f64>,
}

#[derive(Debug, Serialize)]
pub(super) struct RunRecord {
    pub(super) run: String,
    pub(super) mode: String,
    pub(super) replicate: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) rotation: Option<usize>,
    pub(super) process: ProcessRecord,
}

#[derive(Debug, Serialize)]
pub(super) struct Summary {
    pub(super) metric: String,
    pub(super) ratios: Vec<f64>,
    pub(super) median_ratio: f64,
    pub(super) process_ratio_mad: f64,
    pub(super) exact_lower_ratio: f64,
    pub(super) exact_upper_ratio: f64,
    pub(super) confidence_requested: f64,
    pub(super) confidence_achieved: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) simultaneous_confidence_at_least: Option<f64>,
    pub(super) disposition: &'static str,
}

#[derive(Serialize)]
pub(super) struct Comparison<'a> {
    pub(super) passed: bool,
    pub(super) summary: &'a [Summary],
}

#[derive(Serialize)]
pub(super) struct Control<'a> {
    pub(super) enabled: bool,
    pub(super) passed: bool,
    pub(super) summary: &'a [Summary],
}

pub(super) struct Settings {
    pub(super) replicates: usize,
    pub(super) sample_size: usize,
    pub(super) warm_up: f64,
    pub(super) measurement: f64,
    pub(super) cooldown: f64,
    pub(super) aa_control: bool,
    pub(super) max_outlier_fraction: f64,
    pub(super) evidence_mode: EvidenceMode,
}

pub(super) struct JobOutput {
    pub(super) records: Vec<Measurement>,
    pub(super) process: ProcessRecord,
    pub(super) rotation: Option<usize>,
    pub(super) anomalies: Vec<String>,
}

pub(super) struct MeasurementRuns {
    pub(super) records: Vec<Measurement>,
    pub(super) runs: Vec<RunRecord>,
    pub(super) anomalies: Vec<String>,
}

pub(super) struct BinaryJobSpec<'a> {
    pub(super) working_directory: &'a Path,
    pub(super) fixture_argument: &'a Path,
    pub(super) binary: &'a Path,
    pub(super) logs: &'a Path,
    pub(super) metrics: &'a [&'a str],
    pub(super) replicates: usize,
    pub(super) sample_size: usize,
    pub(super) warm_up_ms: u64,
    pub(super) measurement_ms: u64,
    pub(super) cooldown: f64,
    pub(super) aa_control: bool,
    pub(super) max_outlier_fraction: f64,
    pub(super) minimum_free_bytes: u64,
    pub(super) rotation_count: Option<usize>,
    pub(super) diagnostic_samples: bool,
}

pub(super) struct GateDecision {
    pub(super) valid: bool,
    pub(super) decision: &'static str,
}

pub(super) fn settings(
    arguments: &ArgMatches,
    policy: &policy::MeasurementPolicy,
) -> Result<Settings> {
    let replicates = arguments
        .get_one::<usize>("replicates")
        .copied()
        .unwrap_or(usize::try_from(policy.paired_process.process_replicates)?);
    let sample_size = arguments
        .get_one::<usize>("sample-size")
        .copied()
        .unwrap_or(usize::try_from(policy.criterion.sample_size)?);
    let warm_up = arguments
        .get_one::<f64>("warm-up-seconds")
        .copied()
        .unwrap_or(policy.criterion.warm_up_seconds);
    let measurement = arguments
        .get_one::<f64>("measurement-seconds")
        .copied()
        .unwrap_or(policy.criterion.measurement_seconds);
    let cooldown = arguments
        .get_one::<f64>("cooldown-seconds")
        .copied()
        .unwrap_or(policy.paired_process.cooldown_seconds);
    let aa_control = policy.paired_process.aa_control && !arguments.get_flag("skip-aa-control");
    validate_settings(replicates, sample_size, warm_up, measurement, cooldown)?;
    validate_replicate_confidence(replicates, policy.paired_process.confidence, aa_control)?;
    let explicitly_exploratory = arguments.get_flag("exploratory");
    let mut evidence_mode = if explicitly_exploratory {
        EvidenceMode::exploratory("explicit --exploratory request")
    } else {
        EvidenceMode::strict()
    };
    require_exploratory(
        &mut evidence_mode,
        explicitly_exploratory,
        !policy.meets_strict_paired_profile(),
        "measurement policy does not meet the strict paired profile",
    )?;
    require_exploratory(
        &mut evidence_mode,
        explicitly_exploratory,
        replicates != usize::try_from(policy.paired_process.process_replicates)?,
        "replicate count differs from the measurement policy",
    )?;
    require_exploratory(
        &mut evidence_mode,
        explicitly_exploratory,
        sample_size != usize::try_from(policy.criterion.sample_size)?,
        "sample size differs from the measurement policy",
    )?;
    require_exploratory(
        &mut evidence_mode,
        explicitly_exploratory,
        warm_up != policy.criterion.warm_up_seconds,
        "warm-up duration differs from the measurement policy",
    )?;
    require_exploratory(
        &mut evidence_mode,
        explicitly_exploratory,
        measurement != policy.criterion.measurement_seconds,
        "measurement duration differs from the measurement policy",
    )?;
    require_exploratory(
        &mut evidence_mode,
        explicitly_exploratory,
        cooldown != policy.paired_process.cooldown_seconds,
        "cooldown differs from the measurement policy",
    )?;
    require_exploratory(
        &mut evidence_mode,
        explicitly_exploratory,
        aa_control != policy.paired_process.aa_control,
        "A/A control differs from the measurement policy",
    )?;
    Ok(Settings {
        replicates,
        sample_size,
        warm_up,
        measurement,
        cooldown,
        aa_control,
        max_outlier_fraction: policy.criterion.max_outlier_fraction,
        evidence_mode,
    })
}

fn validate_replicate_confidence(
    replicates: usize,
    confidence: f64,
    aa_control: bool,
) -> Result<()> {
    let confidence = if aa_control {
        (1.0 + confidence) / 2.0
    } else {
        confidence
    };
    statistics::exact_median_bounds(&vec![1.0; replicates], confidence)
        .map(|_| ())
        .map_err(|_| {
            invalid_data("replicate count is too small for the requested exact confidence")
        })
}

pub(super) fn duration_millis(seconds: f64) -> Result<u64> {
    let value = (seconds * 1000.0).round();
    if !value.is_finite() || !(1.0..=MAX_DURATION_SECONDS * 1000.0).contains(&value) {
        Err(invalid_data(
            "duration must be between one millisecond and one hour",
        ))
    } else {
        Ok(value as u64)
    }
}

pub(super) fn validate_settings(
    replicates: usize,
    sample_size: usize,
    warm_up: f64,
    measurement: f64,
    cooldown: f64,
) -> Result<()> {
    if !(1..=MAX_REPLICATES).contains(&replicates)
        || !(10..=MAX_SAMPLE_SIZE).contains(&sample_size)
        || !(0.0..=MAX_DURATION_SECONDS).contains(&warm_up)
        || warm_up == 0.0
        || !(0.0..=MAX_DURATION_SECONDS).contains(&measurement)
        || measurement == 0.0
        || !(0.0..=MAX_COOLDOWN_SECONDS).contains(&cooldown)
        || !warm_up.is_finite()
        || !measurement.is_finite()
        || !cooldown.is_finite()
    {
        Err(invalid_data("invalid paired-process settings"))
    } else {
        Ok(())
    }
}

pub(super) fn parse_measurements(
    text: &str,
    run: &str,
    mode: &str,
    expected_metrics: &[&str],
    expected_samples: usize,
    max_outlier_fraction: f64,
) -> Result<Vec<Measurement>> {
    if expected_samples == 0 || expected_samples > MAX_SAMPLE_SIZE {
        return Err(invalid_data("invalid expected paired sample count"));
    }
    let mut lines = text.lines();
    if lines.next() != Some(PROTOCOL) || lines.next() != Some(HEADER) {
        return Err(invalid_data(format!("{run} emitted an unexpected header")));
    }
    let mut seen = BTreeSet::new();
    let mut records = Vec::new();
    for line in lines.filter(|line| !line.is_empty()) {
        let fields = line.splitn(16, '\t').collect::<Vec<_>>();
        if fields.len() != 15 || !expected_metrics.contains(&fields[0]) || !seen.insert(fields[0]) {
            return Err(invalid_data(format!(
                "{run} emitted an unexpected or duplicate metric"
            )));
        }
        let mut record = Measurement {
            run: run.to_owned(),
            mode: mode.to_owned(),
            metric: fields[0].to_owned(),
            baseline_ns: fields[1].parse()?,
            candidate_ns: fields[2].parse()?,
            ratio: fields[3].parse()?,
            aggregate_ratio: fields[4].parse()?,
            ratio_mad: fields[5].parse()?,
            samples: fields[6].parse()?,
            iterations: fields[7].parse()?,
            outliers: fields[8].parse()?,
            warm_up_failures: fields[9].parse()?,
            failures: fields[10].parse()?,
            prime_baseline_ns: fields[11].parse()?,
            prime_candidate_ns: fields[12].parse()?,
            prime_failures: fields[13].parse()?,
            ratio_samples: parse_ratio_samples(fields[14], expected_samples)?,
        };
        let mut ratio_samples = record.ratio_samples.clone();
        let recomputed_ratio = statistics::median(&mut ratio_samples)?;
        let recomputed_mad = statistics::median_absolute_deviation(&record.ratio_samples)?;
        let recomputed_outliers = if recomputed_mad == 0.0 {
            let tolerance = f64::EPSILON * recomputed_ratio.abs().max(1.0);
            record
                .ratio_samples
                .iter()
                .filter(|sample| (*sample - recomputed_ratio).abs() > tolerance)
                .count()
        } else {
            record
                .ratio_samples
                .iter()
                .filter(|sample| (*sample - recomputed_ratio).abs() > 3.0 * recomputed_mad)
                .count()
        };
        let expected_ratio = record.candidate_ns / record.baseline_ns;
        if !expected_ratio.is_finite() || expected_ratio <= 0.0 {
            return Err(invalid_data(format!(
                "{run} emitted an invalid derived timing ratio"
            )));
        }
        let ratio_tolerance = expected_ratio.abs().max(1.0) * 1.0e-6;
        let sample_tolerance = recomputed_ratio.abs().max(1.0) * 1.0e-6;
        if ![
            record.baseline_ns,
            record.candidate_ns,
            record.ratio,
            record.aggregate_ratio,
            record.ratio_mad,
        ]
        .iter()
        .all(|value| value.is_finite())
            || record.baseline_ns <= 0.0
            || record.candidate_ns <= 0.0
            || record.ratio <= 0.0
            || record.aggregate_ratio <= 0.0
            || record.ratio_mad < 0.0
            || record.samples != u64::try_from(expected_samples)?
            || record.ratio_samples.len() != expected_samples
            || record.iterations == 0
            || record.outliers > record.samples
            || record.outliers as f64 / record.samples as f64 > max_outlier_fraction
            || (record.aggregate_ratio - expected_ratio).abs() > ratio_tolerance
            || (record.ratio - recomputed_ratio).abs() > sample_tolerance
            || (record.ratio_mad - recomputed_mad).abs() > sample_tolerance
            || usize::try_from(record.outliers)? != recomputed_outliers
        {
            return Err(invalid_data(format!(
                "{run} emitted an invalid measurement"
            )));
        }
        record.ratio = recomputed_ratio;
        record.aggregate_ratio = expected_ratio;
        record.ratio_mad = recomputed_mad;
        record.outliers = u64::try_from(recomputed_outliers)?;
        records.push(record);
    }
    if seen.len() != expected_metrics.len() {
        return Err(invalid_data(format!(
            "{run} did not emit every expected metric"
        )));
    }
    Ok(records)
}

fn parse_ratio_samples(encoded: &str, expected_samples: usize) -> Result<Vec<f64>> {
    if encoded.is_empty() {
        return Err(invalid_data("ratio samples are empty"));
    }
    let mut samples = Vec::with_capacity(expected_samples);
    for encoded_sample in encoded.split(',') {
        if samples.len() == expected_samples {
            return Err(invalid_data(
                "ratio sample count exceeds the expected count",
            ));
        }
        let sample = encoded_sample
            .parse::<f64>()
            .map_err(|_| invalid_data("ratio sample is not a number"))?;
        if !sample.is_finite() || sample <= 0.0 {
            return Err(invalid_data("ratio sample must be finite and positive"));
        }
        samples.push(sample);
    }
    Ok(samples)
}

pub(super) fn summarize(
    records: &[Measurement],
    mode: &str,
    metrics: &[&str],
    replicates: usize,
    confidence: f64,
    margin: f64,
) -> Result<(Vec<Summary>, bool)> {
    let limit = 1.0 + margin;
    let lower_limit = 1.0 / limit;
    let mut result = Vec::new();
    let mut passed = true;
    for metric in metrics {
        let ratios = records
            .iter()
            .filter(|record| record.mode == mode && record.metric == *metric)
            .map(|record| record.ratio)
            .collect::<Vec<_>>();
        if ratios.len() != replicates {
            return Err(invalid_data(format!(
                "{mode} {metric} has {} replicates; expected {replicates}",
                ratios.len()
            )));
        }
        let bound_confidence = if mode == "aa" {
            (1.0 + confidence) / 2.0
        } else {
            confidence
        };
        let (lower, upper, one_sided_achieved) =
            statistics::exact_median_bounds(&ratios, bound_confidence)?;
        let achieved = if mode == "aa" {
            (2.0 * one_sided_achieved - 1.0).max(0.0)
        } else {
            one_sided_achieved
        };
        let metric_passed = if mode == "aa" {
            lower >= lower_limit && upper <= limit
        } else {
            upper <= limit
        };
        passed &= metric_passed;
        let mut median_values = ratios.clone();
        result.push(Summary {
            metric: (*metric).to_owned(),
            ratios: ratios.clone(),
            median_ratio: statistics::median(&mut median_values)?,
            process_ratio_mad: statistics::median_absolute_deviation(&ratios)?,
            exact_lower_ratio: lower,
            exact_upper_ratio: upper,
            confidence_requested: confidence,
            confidence_achieved: achieved,
            simultaneous_confidence_at_least: (mode == "aa").then_some(achieved),
            disposition: if mode == "aa" {
                if metric_passed { "balanced" } else { "biased" }
            } else if metric_passed {
                "non-inferior"
            } else {
                "regression"
            },
        });
    }
    Ok((result, passed))
}

pub(super) fn gate_decision(
    anomalies_empty: bool,
    aa_control: bool,
    aa_passed: bool,
    strict: bool,
    ab_passed: bool,
) -> GateDecision {
    if strict && !aa_control {
        return GateDecision {
            valid: false,
            decision: "invalid-strict-configuration",
        };
    }
    let valid = anomalies_empty && (!aa_control || aa_passed);
    let decision = if !anomalies_empty {
        "invalid"
    } else if aa_control && !aa_passed {
        "invalid-aa-control"
    } else if !strict {
        if ab_passed {
            "exploratory-non-inferior"
        } else {
            "exploratory-regression"
        }
    } else if ab_passed {
        "strict-non-regression-pass"
    } else {
        "regression"
    };
    GateDecision { valid, decision }
}

pub(super) fn run_jobs<F>(
    replicates: usize,
    aa_control: bool,
    cooldown: f64,
    mut run_job: F,
) -> Result<MeasurementRuns>
where
    F: FnMut(&str, usize, &str) -> JobOutput,
{
    let total_jobs = replicates
        .checked_mul(if aa_control { 2 } else { 1 })
        .ok_or_else(|| invalid_data("paired-process job count overflowed"))?;
    let mut completed = 0usize;
    let mut records = Vec::new();
    let mut runs = Vec::new();
    let mut anomalies = Vec::new();
    for replicate in 0..replicates {
        let modes: &[&str] = if aa_control && !replicate.is_multiple_of(2) {
            &["aa", "ab"]
        } else if aa_control {
            &["ab", "aa"]
        } else {
            &["ab"]
        };
        for mode in modes {
            let run_name = format!("{mode}-run{:02}", replicate + 1);
            let mut output = run_job(mode, replicate, &run_name);
            let failures = output.records.iter().fold(0u64, |total, record| {
                total.saturating_add(
                    record
                        .warm_up_failures
                        .saturating_add(record.failures)
                        .saturating_add(record.prime_failures),
                )
            });
            if failures > 0 {
                output
                    .anomalies
                    .push(format!("{run_name} reported {failures} operation failures"));
            }
            if !output.process.succeeded() {
                output.anomalies.push(format!(
                    "{run_name}: {}",
                    output.process.failure_description()
                ));
            }
            let unsafe_to_continue = output.process.may_still_be_running();
            records.append(&mut output.records);
            anomalies.append(&mut output.anomalies);
            if unsafe_to_continue {
                anomalies.push(format!(
                    "{run_name}: process cleanup was incomplete; remaining runs were aborted"
                ));
            }
            runs.push(RunRecord {
                run: run_name,
                mode: (*mode).to_owned(),
                replicate: replicate + 1,
                rotation: output.rotation,
                process: output.process,
            });
            completed += 1;
            if unsafe_to_continue {
                return Ok(MeasurementRuns {
                    records,
                    runs,
                    anomalies,
                });
            }
            if completed < total_jobs && cooldown > 0.0 {
                thread::sleep(Duration::from_secs_f64(cooldown));
            }
        }
    }
    Ok(MeasurementRuns {
        records,
        runs,
        anomalies,
    })
}

fn binary_command(spec: &BinaryJobSpec<'_>, mode: &str, rotation: Option<usize>) -> Command {
    let mut command = Command::new(spec.binary);
    command
        .env(
            "FS2_PAIRED_DIAGNOSTIC_SAMPLES",
            if spec.diagnostic_samples { "1" } else { "0" },
        )
        .current_dir(spec.working_directory)
        .arg(spec.fixture_argument)
        .arg(mode)
        .arg(spec.sample_size.to_string())
        .arg(spec.warm_up_ms.to_string())
        .arg(spec.measurement_ms.to_string());
    if let Some(rotation) = rotation {
        command.arg(rotation.to_string());
    }
    command
}

pub(super) fn run_binary_jobs(spec: BinaryJobSpec<'_>) -> Result<MeasurementRuns> {
    if spec.rotation_count == Some(0) {
        return Err(invalid_data("workload rotation count must be positive"));
    }
    run_jobs(
        spec.replicates,
        spec.aa_control,
        spec.cooldown,
        |mode, replicate, run_name| {
            let stdout = spec.logs.join(format!("{run_name}.stdout.tsv"));
            let stderr = spec.logs.join(format!("{run_name}.stderr.log"));
            let rotation = spec.rotation_count.map(|count| replicate % count);
            let mut command = binary_command(&spec, mode, rotation);

            let mut anomalies = Vec::new();
            let process = if let Err(error) =
                common::ensure_disk_headroom(spec.logs, spec.minimum_free_bytes)
            {
                anomalies.push(error.to_string());
                ProcessRecord::skipped(
                    &command,
                    format!("run {run_name}"),
                    stdout.clone(),
                    stderr.clone(),
                    "insufficient benchmark disk headroom",
                )
            } else {
                process::run_logged_attempt(
                    &mut command,
                    format!("run {run_name}"),
                    &stdout,
                    &stderr,
                )
            };
            let (records, mut parse_anomalies) = read_job_output(
                &stdout,
                run_name,
                mode,
                spec.metrics,
                spec.sample_size,
                spec.max_outlier_fraction,
            );
            anomalies.append(&mut parse_anomalies);
            JobOutput {
                records,
                process,
                rotation,
                anomalies,
            }
        },
    )
}

pub(super) fn read_job_output(
    stdout: &Path,
    run: &str,
    mode: &str,
    metrics: &[&str],
    expected_samples: usize,
    max_outlier_fraction: f64,
) -> (Vec<Measurement>, Vec<String>) {
    match read_bounded_utf8(stdout, MAX_PAIRED_PROTOCOL_BYTES) {
        Ok(output) => match parse_measurements(
            &output,
            run,
            mode,
            metrics,
            expected_samples,
            max_outlier_fraction,
        ) {
            Ok(records) => (records, Vec::new()),
            Err(error) => (Vec::new(), vec![error.to_string()]),
        },
        Err(error) => (
            Vec::new(),
            vec![format!(
                "{run}: unable to read {}: {error}",
                stdout.display()
            )],
        ),
    }
}

pub(super) fn open_regular_input(path: &Path) -> Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;

        // Reject a FIFO without waiting for a writer before checking its type.
        options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_FLAG_OPEN_REPARSE_POINT, SECURITY_IDENTIFICATION,
        };

        options
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .security_qos_flags(SECURITY_IDENTIFICATION);
    }
    let file = options.open(path)?;
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle as _;
        use windows_sys::Win32::Storage::FileSystem::{FILE_TYPE_DISK, GetFileType};

        // SAFETY: file owns this live handle for the duration of the query.
        if unsafe { GetFileType(file.as_raw_handle()) } != FILE_TYPE_DISK {
            return Err(invalid_data("benchmark input must be a regular disk file"));
        }
    }
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(invalid_data("benchmark input must be a regular file"));
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;
        use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;

        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(invalid_data("benchmark input must not be a reparse point"));
        }
    }
    Ok(file)
}

pub(super) fn read_bounded_utf8(path: &Path, max_bytes: u64) -> Result<String> {
    let file = open_regular_input(path)?;
    let mut bytes = Vec::new();
    file.take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len())? > max_bytes {
        return Err(invalid_data(
            "paired benchmark output exceeds its size limit",
        ));
    }
    String::from_utf8(bytes).map_err(|_| invalid_data("paired benchmark output is not valid UTF-8"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_paired_child_receives_the_explicit_fixture() {
        let fixture = Path::new("admitted fixture with spaces");
        let mut spec = BinaryJobSpec {
            working_directory: Path::new("source"),
            fixture_argument: fixture,
            binary: Path::new("paired-harness"),
            logs: Path::new("logs"),
            metrics: &["metric"],
            replicates: 8,
            sample_size: 50,
            warm_up_ms: 2_000,
            measurement_ms: 5_000,
            cooldown: 10.0,
            aa_control: true,
            max_outlier_fraction: 0.2,
            minimum_free_bytes: 0,
            rotation_count: None,
            diagnostic_samples: false,
        };
        for mode in ["ab", "aa"] {
            for rotation in [None, Some(3)] {
                let command = binary_command(&spec, mode, rotation);
                let arguments = command.get_args().collect::<Vec<_>>();
                assert_eq!(arguments[0], fixture.as_os_str());
                assert_eq!(arguments[1], mode);
                assert_eq!(arguments[2..5], ["50", "2000", "5000"]);
                assert_eq!(command.get_current_dir(), Some(spec.working_directory));
                if rotation.is_some() {
                    assert_eq!(arguments[5], "3");
                    assert_eq!(arguments.len(), 6);
                } else {
                    assert_eq!(arguments.len(), 5);
                }
            }
        }
        for (enabled, value) in [(false, "0"), (true, "1")] {
            spec.diagnostic_samples = enabled;
            let command = binary_command(&spec, "ab", None);
            assert!(command.get_envs().any(|(key, actual)| {
                key == "FS2_PAIRED_DIAGNOSTIC_SAMPLES" && actual == Some(value.as_ref())
            }));
        }
    }

    #[test]
    fn strict_decisions_require_the_aa_control() {
        let decision = gate_decision(true, false, true, true, true);
        assert!(!decision.valid);
        assert_eq!(decision.decision, "invalid-strict-configuration");
    }

    #[test]
    fn rejects_unbounded_settings() {
        assert!(validate_settings(MAX_REPLICATES + 1, 50, 2.0, 5.0, 10.0).is_err());
        assert!(validate_settings(8, MAX_SAMPLE_SIZE + 1, 2.0, 5.0, 10.0).is_err());
        assert!(duration_millis(MAX_DURATION_SECONDS + 1.0).is_err());
        assert!(validate_replicate_confidence(1, 0.95, false).is_err());
        assert!(validate_replicate_confidence(5, 0.95, false).is_ok());
        assert!(validate_replicate_confidence(5, 0.95, true).is_err());
        assert!(validate_replicate_confidence(6, 0.95, true).is_ok());
    }

    #[test]
    fn parses_complete_measurements_in_any_order() {
        let ratios = ["1.1"; 50].join(",");
        let text = format!(
            "{PROTOCOL}\n{HEADER}\nsecond\t10\t11\t1.1\t1.1\t0\t50\t5\t0\t0\t0\t12\t13\t0\t{ratios}\nfirst\t10\t11\t1.1\t1.1\t0\t50\t5\t0\t0\t0\t12\t13\t0\t{ratios}\n"
        );
        assert_eq!(
            parse_measurements(&text, "ab-run01", "ab", &["first", "second"], 50, 0.5)
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn rejects_summary_statistics_that_disagree_with_raw_samples() {
        let ratios = ["1.1"; 50].join(",");
        let text = format!(
            "{PROTOCOL}\n{HEADER}\nmetric\t10\t11\t1.2\t1.1\t0\t50\t5\t0\t0\t0\t12\t13\t0\t{ratios}\n"
        );
        assert!(parse_measurements(&text, "ab-run01", "ab", &["metric"], 50, 0.5).is_err());
    }

    #[test]
    fn accepted_measurements_use_parent_recomputed_summaries() {
        let ratios = ["1.1"; 50].join(",");
        let text = format!(
            "{PROTOCOL}\n{HEADER}\nmetric\t10\t11\t1.1000005\t1.1000005\t0.0000005\t50\t5\t0\t0\t0\t12\t13\t0\t{ratios}\n"
        );
        let records = parse_measurements(&text, "ab-run01", "ab", &["metric"], 50, 0.5).unwrap();
        assert_eq!(records[0].ratio, 1.1);
        assert_eq!(records[0].aggregate_ratio, 1.1);
        assert_eq!(records[0].ratio_mad, 0.0);
        assert_eq!(records[0].outliers, 0);
    }

    #[test]
    fn rejects_overflowed_derived_timing_ratio() {
        let text = format!(
            "{PROTOCOL}\n{HEADER}\nmetric\t1e-308\t1e308\t1\t1\t0\t1\t1\t0\t0\t0\t1\t1\t0\t1\n"
        );
        assert!(parse_measurements(&text, "ab-run01", "ab", &["metric"], 1, 0.5).is_err());
    }

    #[test]
    fn rejects_ratio_samples_beyond_the_expected_count() {
        assert!(parse_ratio_samples("1,1,1", 2).is_err());
    }

    #[test]
    fn paired_output_is_read_with_an_explicit_byte_limit() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("stdout");
        fs::write(&path, b"12345").unwrap();

        assert!(read_bounded_utf8(&path, 4).is_err());
        assert_eq!(read_bounded_utf8(&path, 5).unwrap(), "12345");
    }

    #[test]
    fn regular_inputs_preserve_hard_links_and_reject_invalid_text_and_directories() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("input");
        let link = directory.path().join("hard-link");
        fs::write(&path, b"12345").unwrap();
        fs::hard_link(&path, &link).unwrap();
        assert_eq!(read_bounded_utf8(&link, 5).unwrap(), "12345");
        assert!(read_bounded_utf8(directory.path(), 5).is_err());
        fs::write(&path, [0xff]).unwrap();
        assert!(read_bounded_utf8(&path, 5).is_err());
        fs::write(&path, []).unwrap();
        assert_eq!(read_bounded_utf8(&path, 0).unwrap(), "");
    }

    #[cfg(unix)]
    #[test]
    fn regular_inputs_reject_final_symlinks_and_fifos_without_waiting() {
        use std::os::unix::ffi::OsStrExt as _;
        use std::os::unix::fs::symlink;
        use std::sync::mpsc;

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("input");
        let link = directory.path().join("link");
        let fifo = directory.path().join("fifo");
        fs::write(&path, b"ordinary").unwrap();
        symlink(&path, &link).unwrap();
        assert!(read_bounded_utf8(&link, 16).is_err());
        let fifo_path = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
        // SAFETY: fifo_path is NUL-terminated and lives through this call.
        let result = unsafe { libc::mkfifo(fifo_path.as_ptr(), 0o600) };
        assert_eq!(result, 0, "{}", std::io::Error::last_os_error());
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let _ = sender.send(read_bounded_utf8(&fifo, 16).is_err());
        });
        assert!(receiver.recv_timeout(Duration::from_secs(5)).unwrap());
    }

    #[cfg(windows)]
    #[test]
    fn regular_inputs_reject_windows_character_devices() {
        assert!(read_bounded_utf8(Path::new("NUL"), 16).is_err());
    }

    #[test]
    fn aa_summary_uses_simultaneous_two_sided_confidence() {
        let ratios = [0.99, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.01];
        let records = ratios
            .into_iter()
            .map(|ratio| Measurement {
                run: "aa".to_owned(),
                mode: "aa".to_owned(),
                metric: "metric".to_owned(),
                baseline_ns: 10.0,
                candidate_ns: 10.0 * ratio,
                ratio,
                aggregate_ratio: ratio,
                ratio_mad: 0.0,
                samples: 50,
                iterations: 1,
                outliers: 0,
                warm_up_failures: 0,
                failures: 0,
                prime_baseline_ns: 1,
                prime_candidate_ns: 1,
                prime_failures: 0,
                ratio_samples: vec![ratio; 50],
            })
            .collect::<Vec<_>>();
        let (summary, passed) = summarize(&records, "aa", &["metric"], 8, 0.95, 0.02).unwrap();
        assert!(passed);
        assert!(summary[0].confidence_achieved >= 0.95);
        assert_eq!(
            summary[0].simultaneous_confidence_at_least,
            Some(summary[0].confidence_achieved)
        );
    }

    #[test]
    fn tighter_aa_margin_rejects_bias_without_tightening_the_ab_gate() {
        for (ratio, aa_expected) in [(0.985, false), (0.995, true), (1.005, true), (1.015, false)] {
            for mode in ["ab", "aa"] {
                let ratios = vec![ratio; 50];
                let encoded = ratios
                    .iter()
                    .map(f64::to_string)
                    .collect::<Vec<_>>()
                    .join(",");
                let text = format!(
                    "{PROTOCOL}\n{HEADER}\nmetric\t1000\t{}\t{ratio}\t{ratio}\t0\t50\t32\t0\t0\t0\t1\t1\t0\t{encoded}\n",
                    ratio * 1000.0,
                );
                let records = (0..16)
                    .flat_map(|replicate| {
                        parse_measurements(
                            &text,
                            &format!("{mode}-{replicate}"),
                            mode,
                            &["metric"],
                            50,
                            0.30,
                        )
                        .unwrap()
                    })
                    .collect::<Vec<_>>();
                let margin = if mode == "aa" { 0.01 } else { 0.02 };
                let (summary, passed) =
                    summarize(&records, mode, &["metric"], 16, 0.95, margin).unwrap();
                assert_eq!(passed, mode == "ab" || aa_expected);
                assert_eq!(summary[0].ratios.len(), 16);
                assert!(summary[0].confidence_achieved >= 0.95);
            }
        }
    }
}
