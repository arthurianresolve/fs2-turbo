use std::fs;
use std::path::Path;

use serde::Deserialize;

use crate::{Result, invalid_data};

const PAIRS_PER_BUILD_REPLICATE: usize = 4;
const MIN_GATING_PAIRS: u64 = 24;
const MIN_DRIFT_CORRECTED_BLOCKS: u64 = 8;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MeasurementPolicy {
    pub(crate) schema_version: u64,
    pub(crate) non_inferiority_margin: f64,
    pub(crate) criterion: CriterionPolicy,
    pub(crate) ref_to_ref: RefPolicy,
    pub(crate) cross_crate: CrossCratePolicy,
    pub(crate) paired_stats: PairedStatsPolicy,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CriterionPolicy {
    pub(crate) sample_size: u64,
    pub(crate) warm_up_seconds: f64,
    pub(crate) measurement_seconds: f64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RefPolicy {
    pub(crate) blocks: u64,
    pub(crate) minimum_blocks: u64,
    pub(crate) max_pair_spread: f64,
    pub(crate) pair_order: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CrossCratePolicy {
    pub(crate) pairs: u64,
    pub(crate) pair_order: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PairedStatsPolicy {
    pub(crate) confidence: f64,
    pub(crate) process_replicates: u64,
    pub(crate) cooldown_seconds: f64,
    pub(crate) aa_control: bool,
}

pub(crate) fn run(path: &Path) -> Result<()> {
    validate_path(path)?;
    println!("measurement policy valid: {}", path.display());
    Ok(())
}

pub(crate) fn load(path: &Path) -> Result<MeasurementPolicy> {
    let contents = fs::read_to_string(path)?;
    let policy: MeasurementPolicy = serde_json::from_str(&contents)?;
    validate(policy)
}

fn validate_path(path: &Path) -> Result<MeasurementPolicy> {
    load(path)
}

fn validate(policy: MeasurementPolicy) -> Result<MeasurementPolicy> {
    if policy.schema_version != 2 {
        return Err(invalid_data("measurement policy schema_version must be 2"));
    }
    fraction("non_inferiority_margin", policy.non_inferiority_margin)?;
    minimum("sample_size", policy.criterion.sample_size, 10)?;
    positive("warm_up_seconds", policy.criterion.warm_up_seconds)?;
    positive("measurement_seconds", policy.criterion.measurement_seconds)?;
    minimum(
        "minimum_blocks",
        policy.ref_to_ref.minimum_blocks,
        MIN_DRIFT_CORRECTED_BLOCKS,
    )?;
    minimum(
        "blocks",
        policy.ref_to_ref.blocks,
        policy.ref_to_ref.minimum_blocks,
    )?;
    fraction("max_pair_spread", policy.ref_to_ref.max_pair_spread)?;
    balanced_order("ref_to_ref", &policy.ref_to_ref.pair_order)?;
    minimum("pairs", policy.cross_crate.pairs, MIN_GATING_PAIRS)?;
    if !policy
        .cross_crate
        .pairs
        .is_multiple_of(PAIRS_PER_BUILD_REPLICATE as u64 * 2)
    {
        return Err(invalid_data(
            "cross_crate pairs must provide balanced build replicates",
        ));
    }
    balanced_order("cross_crate", &policy.cross_crate.pair_order)?;
    if !(0.5..1.0).contains(&policy.paired_stats.confidence) {
        return Err(invalid_data(
            "paired_stats confidence must be between 0.5 and 1",
        ));
    }
    minimum(
        "process_replicates",
        policy.paired_stats.process_replicates,
        1,
    )?;
    if !policy.paired_stats.cooldown_seconds.is_finite()
        || policy.paired_stats.cooldown_seconds < 0.0
    {
        return Err(invalid_data(
            "paired_stats cooldown_seconds must be finite and nonnegative",
        ));
    }
    let _ = policy.paired_stats.aa_control;
    Ok(policy)
}

fn minimum(name: &str, value: u64, minimum: u64) -> Result<()> {
    if value < minimum {
        Err(invalid_data(format!(
            "measurement policy field {name:?} must be an integer >= {minimum}"
        )))
    } else {
        Ok(())
    }
}

fn fraction(name: &str, value: f64) -> Result<()> {
    if value.is_finite() && (0.0..1.0).contains(&value) {
        Ok(())
    } else {
        Err(invalid_data(format!(
            "measurement policy field {name:?} must be at least 0 and less than 1"
        )))
    }
}

fn positive(name: &str, value: f64) -> Result<()> {
    if value.is_finite() && value > 0.0 {
        Ok(())
    } else {
        Err(invalid_data(format!(
            "measurement policy field {name:?} must be finite and positive"
        )))
    }
}

fn balanced_order(name: &str, order: &[String]) -> Result<()> {
    let a = order
        .iter()
        .filter(|subject| subject.as_str() == "A")
        .count();
    let b = order
        .iter()
        .filter(|subject| subject.as_str() == "B")
        .count();
    if order.len() == PAIRS_PER_BUILD_REPLICATE
        && a == PAIRS_PER_BUILD_REPLICATE / 2
        && b == PAIRS_PER_BUILD_REPLICATE / 2
    {
        Ok(())
    } else {
        Err(invalid_data(format!(
            "{name} pair_order must contain a balanced four-entry A/B order"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_policy() -> MeasurementPolicy {
        MeasurementPolicy {
            schema_version: 2,
            non_inferiority_margin: 0.02,
            criterion: CriterionPolicy {
                sample_size: 50,
                warm_up_seconds: 2.0,
                measurement_seconds: 5.0,
            },
            ref_to_ref: RefPolicy {
                blocks: 8,
                minimum_blocks: 8,
                max_pair_spread: 0.2,
                pair_order: vec!["A".into(), "B".into(), "B".into(), "A".into()],
            },
            cross_crate: CrossCratePolicy {
                pairs: 24,
                pair_order: vec!["A".into(), "B".into(), "A".into(), "B".into()],
            },
            paired_stats: PairedStatsPolicy {
                confidence: 0.95,
                process_replicates: 8,
                cooldown_seconds: 10.0,
                aa_control: true,
            },
        }
    }

    #[test]
    fn accepts_repository_policy() {
        validate_path(&crate::repository_root().join("benchmarks/measurement-policy.json"))
            .unwrap();
    }

    #[test]
    fn rejects_unbalanced_order() {
        let mut policy = valid_policy();
        policy.ref_to_ref.pair_order = vec!["A".into(); 4];
        assert!(validate(policy).is_err());
    }

    #[test]
    fn rejects_insufficient_cross_crate_pairs() {
        let mut policy = valid_policy();
        policy.cross_crate.pairs = 16;
        assert!(validate(policy).is_err());
    }
}
