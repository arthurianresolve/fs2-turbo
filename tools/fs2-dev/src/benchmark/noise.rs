use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use clap::ArgMatches;
use serde::Serialize;
use sha2::{Digest as _, Sha256};

use super::{paired, statistics};
use crate::{Result, invalid_data, lower_hex};

const MAX_INPUT_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Debug, Serialize)]
pub(super) struct Noise {
    samples: usize,
    median_ratio: f64,
    relative_3mad_threshold: f64,
    relative_3mad_outliers: usize,
    outliers_within_one_percent: usize,
    excursions_over_one_percent: usize,
    excursions_over_two_percent: usize,
    excursion_p50: f64,
    excursion_p90: f64,
    excursion_p95: f64,
    excursion_p99: f64,
    excursion_max: f64,
}

pub(super) fn summarize(ratios: &[f64]) -> Result<Noise> {
    if ratios.is_empty()
        || ratios.len() > crate::policy::MAX_SAMPLE_SIZE as usize
        || ratios
            .iter()
            .any(|value| !value.is_finite() || *value <= 0.0)
    {
        return Err(invalid_data("invalid diagnostic ratio samples"));
    }
    let median = statistics::median(&mut ratios.to_vec())?;
    let mad = statistics::median_absolute_deviation(ratios)?;
    let threshold = if mad == 0.0 {
        f64::EPSILON * median.abs().max(1.0)
    } else {
        3.0 * mad
    };
    let mut excursions = ratios
        .iter()
        .map(|ratio| (ratio / median - 1.0).abs())
        .collect::<Vec<_>>();
    if !threshold.is_finite() || excursions.iter().any(|value| !value.is_finite()) {
        return Err(invalid_data("diagnostic ratio excursion overflow"));
    }
    let relative_3mad_outliers = ratios
        .iter()
        .filter(|ratio| (**ratio - median).abs() > threshold)
        .count();
    let outliers_within_one_percent = ratios
        .iter()
        .zip(&excursions)
        .filter(|(ratio, excursion)| (**ratio - median).abs() > threshold && **excursion <= 0.01)
        .count();
    excursions.sort_unstable_by(f64::total_cmp);
    let percentile = |fraction: f64| {
        let position = (excursions.len() - 1) as f64 * fraction;
        let lower = position.floor() as usize;
        let upper = position.ceil() as usize;
        excursions[lower] + (excursions[upper] - excursions[lower]) * position.fract()
    };
    Ok(Noise {
        samples: ratios.len(),
        median_ratio: median,
        relative_3mad_threshold: threshold / median,
        relative_3mad_outliers,
        outliers_within_one_percent,
        excursions_over_one_percent: excursions.iter().filter(|value| **value > 0.01).count(),
        excursions_over_two_percent: excursions.iter().filter(|value| **value > 0.02).count(),
        excursion_p50: percentile(0.50),
        excursion_p90: percentile(0.90),
        excursion_p95: percentile(0.95),
        excursion_p99: percentile(0.99),
        excursion_max: excursions[excursions.len() - 1],
    })
}

fn protocol(text: &str) -> Result<Vec<serde_json::Value>> {
    let mut lines = text.lines();
    if lines.next() != Some("fs2-paired-v3") {
        return Err(invalid_data("noise report requires paired v3 input"));
    }
    let header = lines
        .next()
        .ok_or_else(|| invalid_data("missing paired header"))?;
    if header.split('\t').count() != 15 || !header.ends_with("ratio_samples") {
        return Err(invalid_data("invalid paired noise header"));
    }
    let mut metrics = Vec::new();
    let mut seen = BTreeSet::new();
    for line in lines.filter(|line| !line.is_empty()) {
        let fields = line.split('\t').collect::<Vec<_>>();
        if fields.len() != 15 || metrics.len() >= 128 || !seen.insert(fields[0]) {
            return Err(invalid_data("invalid or duplicate noise metric row"));
        }
        let count: usize = fields[6].parse()?;
        if count == 0 || count > crate::policy::MAX_SAMPLE_SIZE as usize {
            return Err(invalid_data("noise sample count exceeds its limit"));
        }
        let mut ratios = Vec::with_capacity(count);
        for value in fields[14].split(',') {
            if ratios.len() == count {
                return Err(invalid_data("too many noise samples"));
            }
            ratios.push(value.parse::<f64>()?);
        }
        if ratios.len() != count {
            return Err(invalid_data("incomplete noise samples"));
        }
        metrics.push(serde_json::json!({
            "metric": fields[0],
            "noise": summarize(&ratios)?,
            "warm_up_failures": fields[9].parse::<u64>()?,
            "measurement_failures": fields[10].parse::<u64>()?,
            "prime_failures": fields[13].parse::<u64>()?,
        }));
    }
    if metrics.is_empty() {
        return Err(invalid_data("noise input contains no metric rows"));
    }
    Ok(metrics)
}

pub(super) fn input(path: &Path) -> serde_json::Value {
    match paired::read_bounded_utf8(path, MAX_INPUT_BYTES) {
        Ok(text) => {
            let parsed = protocol(&text);
            serde_json::json!({
                "source_sha256": lower_hex(Sha256::digest(text.as_bytes())),
                "metrics": parsed.as_ref().ok(),
                "error": parsed.as_ref().err().map(ToString::to_string),
            })
        }
        Err(_) => serde_json::json!({"error": "input unavailable, oversized, or not UTF-8"}),
    }
}

pub(super) fn envelope(inputs: Vec<serde_json::Value>) -> serde_json::Value {
    serde_json::json!({
        "schema_version": 1,
        "kind": "paired-noise-diagnostics",
        "performance_evidence": false,
        "method": "Absolute ratio/within-process-median minus one; linear interpolation at (n-1)*p. All samples, including rejected blocks, are retained. Fractions, not percent units. These are sample-average ratio excursions, not individual-call latency percentiles or confidence intervals.",
        "limitations": "This sidecar does not validate the canonical timing summaries, completeness of a comparison, or its acceptance gates. Only the canonical report can decide validity; diagnostic counts never relax that decision.",
        "inputs": inputs,
    })
}

pub(crate) fn run(arguments: &ArgMatches) -> Result<()> {
    let inputs = arguments
        .get_many::<PathBuf>("samples")
        .expect("required by clap")
        .map(|path| input(path))
        .collect::<Vec<_>>();
    let failed = inputs.iter().any(|input| !input["error"].is_null());
    serde_json::to_writer_pretty(std::io::stdout().lock(), &envelope(inputs))?;
    if failed {
        return Err(invalid_data("one or more noise inputs were invalid"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noise_retains_large_excursions_and_uses_interpolated_quantiles() {
        let noise = summarize(&[1.0, 1.0, 1.0, 1.0, 1.4]).unwrap();
        assert_eq!(noise.samples, 5);
        assert_eq!(noise.relative_3mad_outliers, 1);
        assert_eq!(noise.excursions_over_two_percent, 1);
        assert!((noise.excursion_p95 - 0.32).abs() < 1e-12);
        assert!((noise.excursion_max - 0.4).abs() < 1e-12);
        let small = summarize(&[1.0, 1.0, 1.0, 1.005]).unwrap();
        assert_eq!(small.outliers_within_one_percent, 1);
        assert_eq!(small.excursions_over_one_percent, 0);
        assert!(summarize(&[]).is_err());
        assert!(summarize(&[f64::NAN]).is_err());
        assert!(summarize(&[0.0]).is_err());
    }

    #[test]
    fn noise_protocol_checks_counts_without_reclassifying_acceptance() {
        let header = format!("{}ratio_samples", "column\t".repeat(14));
        let text = format!(
            "fs2-paired-v3\n{header}\nmetric\t1\t1\t1\t1\t0\t4\t32\t1\t0\t0\t1\t1\t0\t1,1,1,1.4\n"
        );
        assert_eq!(protocol(&text).unwrap().len(), 1);
        assert!(protocol(&text.replace("1,1,1,1.4", "1,1,1")).is_err());
        assert!(protocol(&text.replace("1,1,1,1.4", "1,1,1,NaN")).is_err());
        assert_eq!(envelope(Vec::new())["performance_evidence"], false);
    }
}
