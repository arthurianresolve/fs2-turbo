use std::path::Path;

use clap::ArgMatches;
use serde::Serialize;

use crate::{Result, invalid_data, report};

#[cfg(all(windows, target_pointer_width = "64"))]
mod windows;
#[cfg(all(windows, target_pointer_width = "64"))]
pub(super) use windows::AffinityGuard;

#[derive(Clone, Copy, Debug, Serialize)]
pub(super) struct Policy {
    pub(super) max_core_mean_busy_percent: f64,
    pub(super) max_core_sample_busy_percent: f64,
}

impl Policy {
    pub(super) fn from_arguments(arguments: &ArgMatches) -> Result<Option<Self>> {
        let mean = arguments
            .try_get_one::<f64>("idle-max-core-busy-percent")
            .ok()
            .flatten()
            .copied();
        let peak = arguments
            .try_get_one::<f64>("idle-max-sample-busy-percent")
            .ok()
            .flatten()
            .copied();
        match (mean, peak) {
            (None, None) => Ok(None),
            (Some(mean), Some(peak))
                if mean.is_finite()
                    && peak.is_finite()
                    && mean > 0.0
                    && mean <= peak
                    && peak <= 100.0 =>
            {
                Ok(Some(Self {
                    max_core_mean_busy_percent: mean,
                    max_core_sample_busy_percent: peak,
                }))
            }
            _ => Err(invalid_data(
                "host admission requires finite limits: 0 < mean <= sample <= 100 percent",
            )),
        }
    }
}

#[derive(Debug, Serialize)]
pub(super) struct Observation {
    pub(super) core_masks: Vec<u64>,
    pub(super) allowed_mask: u64,
    pub(super) logical_cpus: Vec<u32>,
    pub(super) busy_samples: Vec<Vec<f64>>,
    pub(super) interval_seconds: Vec<f64>,
    pub(super) ac_online: bool,
    pub(super) completed_unix_ms: u128,
}

#[derive(Debug, Serialize)]
struct CoreScore {
    mask: u64,
    mean_busy_percent: f64,
    maximum_sample_busy_percent: f64,
    admitted: bool,
}

#[derive(Debug, Serialize)]
struct Selection {
    cores: Vec<CoreScore>,
    cpu: Option<u32>,
}

fn select(observation: &Observation, policy: Policy) -> Result<Selection> {
    let mut coverage = 0;
    for &mask in &observation.core_masks {
        if mask == 0 || coverage & mask != 0 {
            return Err(invalid_data(
                "invalid or overlapping physical-core topology",
            ));
        }
        coverage |= mask;
    }
    let expected = (0..64)
        .filter(|cpu| coverage & (1u64 << cpu) != 0)
        .collect::<Vec<_>>();
    if coverage == 0
        || observation.allowed_mask == 0
        || observation.allowed_mask & !coverage != 0
        || observation.logical_cpus != expected
        || observation.busy_samples.len() != 30
        || observation.interval_seconds.len() != 30
        || observation
            .interval_seconds
            .iter()
            .any(|seconds| !seconds.is_finite() || !(0.9..=3.0).contains(seconds))
        || !observation.ac_online
        || observation.busy_samples.iter().any(|sample| {
            sample.len() != expected.len()
                || sample
                    .iter()
                    .any(|value| !value.is_finite() || !(0.0..=100.0).contains(value))
        })
    {
        return Err(invalid_data(
            "incomplete, delayed, or invalid host observations",
        ));
    }
    let mut cores = Vec::new();
    for &mask in &observation.core_masks {
        let sums = observation
            .busy_samples
            .iter()
            .map(|sample| {
                sample
                    .iter()
                    .zip(&expected)
                    .filter(|(_, cpu)| mask & (1u64 << **cpu) != 0)
                    .map(|(value, _)| value)
                    .sum::<f64>()
            })
            .collect::<Vec<_>>();
        let mean = sums.iter().sum::<f64>() / sums.len() as f64;
        let maximum = sums.iter().copied().fold(0.0, f64::max);
        cores.push(CoreScore {
            mask,
            mean_busy_percent: mean,
            maximum_sample_busy_percent: maximum,
            admitted: mask & observation.allowed_mask != 0
                && mean <= policy.max_core_mean_busy_percent
                && maximum <= policy.max_core_sample_busy_percent,
        });
    }
    let choice = cores
        .iter()
        .filter(|score| score.admitted)
        .min_by(|left, right| {
            left.mean_busy_percent
                .total_cmp(&right.mean_busy_percent)
                .then(
                    left.maximum_sample_busy_percent
                        .total_cmp(&right.maximum_sample_busy_percent),
                )
                .then(left.mask.cmp(&right.mask))
        });
    let cpu = choice.and_then(|choice| {
        expected
            .iter()
            .enumerate()
            .filter(|(_, cpu)| choice.mask & observation.allowed_mask & (1u64 << **cpu) != 0)
            .min_by(|(left, left_cpu), (right, right_cpu)| {
                let total = |index: usize| {
                    observation
                        .busy_samples
                        .iter()
                        .map(|row| row[index])
                        .sum::<f64>()
                };
                total(*left)
                    .total_cmp(&total(*right))
                    .then(left_cpu.cmp(right_cpu))
            })
            .map(|(_, cpu)| *cpu)
    });
    Ok(Selection { cores, cpu })
}

fn assess(policy: Policy) -> (serde_json::Value, Option<u32>) {
    #[cfg(all(windows, target_pointer_width = "64"))]
    let observation = windows::observe();
    #[cfg(not(all(windows, target_pointer_width = "64")))]
    let observation: Result<Observation> = Err(invalid_data(
        "native idle-host admission is available only on 64-bit Windows with one processor group",
    ));
    let selection = observation
        .as_ref()
        .map_err(|error| error.to_string())
        .and_then(|data| select(data, policy).map_err(|error| error.to_string()));
    let cpu = selection.as_ref().ok().and_then(|selection| selection.cpu);
    let value = serde_json::json!({
        "schema_version": 1,
        "kind": "host-admission",
        "performance_evidence": false,
        "selection_passed": cpu.is_some(),
        "policy": policy,
        "settle_seconds": 60,
        "observations": 30,
        "interval_seconds": 1,
        "method": "One fixed observation window; sum all SMT-sibling busy percentages per physical core. Require both mean and every sample limit, then select minimum mean, maximum sample, mask, and sibling mean/CPU. Respect inherited allowed affinity. No benchmark timings participate and there are no replacement windows.",
        "limitations": "Admission observes pre-run load only. It does not establish thermal stability, frequency stability, future isolation, or a statistical noise bound. The recorder is closed before timing. Missing counters, AC power, or supported topology fail closed.",
        "observation": observation.as_ref().ok(),
        "selection": selection.as_ref().ok(),
        "error": selection.as_ref().err(),
    });
    (value, cpu)
}

pub(super) fn admit(policy: Policy, path: &Path) -> Result<AffinityGuard> {
    let (value, cpu) = assess(policy);
    report::write_json(path, &value)?;
    let cpu =
        cpu.ok_or_else(|| invalid_data("idle-host admission refused; retain host-admission.json"))?;
    AffinityGuard::pin(cpu)
}

pub(crate) fn run(arguments: &ArgMatches) -> Result<()> {
    let policy = Policy::from_arguments(arguments)?
        .ok_or_else(|| invalid_data("explicit host-admission limits are required"))?;
    let (value, cpu) = assess(policy);
    serde_json::to_writer_pretty(std::io::stdout().lock(), &value)?;
    if cpu.is_none() {
        return Err(invalid_data(
            "idle-host admission refused; no benchmark was started",
        ));
    }
    Ok(())
}

#[cfg(not(all(windows, target_pointer_width = "64")))]
pub(super) struct AffinityGuard;

#[cfg(not(all(windows, target_pointer_width = "64")))]
impl AffinityGuard {
    fn pin(_cpu: u32) -> Result<Self> {
        Err(invalid_data("native host affinity is unavailable"))
    }

    pub(super) fn restore(&mut self) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> Policy {
        Policy {
            max_core_mean_busy_percent: 5.0,
            max_core_sample_busy_percent: 20.0,
        }
    }

    fn observation() -> Observation {
        Observation {
            core_masks: vec![3, 12],
            allowed_mask: 15,
            logical_cpus: vec![0, 1, 2, 3],
            busy_samples: vec![vec![1.0, 30.0, 1.0, 2.0]; 30],
            interval_seconds: vec![1.0; 30],
            ac_online: true,
            completed_unix_ms: 0,
        }
    }

    #[test]
    fn admission_uses_siblings_and_never_picks_a_merely_least_busy_core() {
        let mut data = observation();
        assert_eq!(select(&data, policy()).unwrap().cpu, Some(2));
        data.busy_samples = vec![vec![5.0; 4]; 30];
        assert!(select(&data, policy()).unwrap().cpu.is_none());
        data = observation();
        data.busy_samples[10][2] = 25.0;
        assert!(select(&data, policy()).unwrap().cpu.is_none());
        data = observation();
        data.allowed_mask = 8;
        assert_eq!(select(&data, policy()).unwrap().cpu, Some(3));
    }

    #[test]
    fn admission_rejects_missing_nonfinite_delayed_and_overlapping_data() {
        let mut data = observation();
        data.busy_samples.pop();
        assert!(select(&data, policy()).is_err());
        data = observation();
        data.busy_samples[0][0] = f64::NAN;
        assert!(select(&data, policy()).is_err());
        data = observation();
        data.interval_seconds[0] = 4.0;
        assert!(select(&data, policy()).is_err());
        data = observation();
        data.core_masks.push(3);
        assert!(select(&data, policy()).is_err());
        data = observation();
        data.ac_online = false;
        assert!(select(&data, policy()).is_err());
    }
}
