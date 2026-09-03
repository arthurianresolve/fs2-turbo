use std::fs;
use std::path::Path;

use serde::Deserialize;

use crate::{Result, invalid_data};

const PAIRS_PER_BUILD_REPLICATE: usize = 4;
const MIN_GATING_PAIRS: u64 = 24;
const MIN_DRIFT_CORRECTED_BLOCKS: u64 = 8;
const MAX_DRIFT_CORRECTED_BLOCKS: u64 = 64;
const MAX_GATING_PAIRS: u64 = 128;
const STRICT_PAIRED_REPLICATES: u64 = 8;
const STRICT_PAIRED_CONFIDENCE: f64 = 0.95;
const STRICT_SAMPLE_SIZE: u64 = 50;
const STRICT_WARM_UP_SECONDS: f64 = 2.0;
const STRICT_MEASUREMENT_SECONDS: f64 = 5.0;
const STRICT_COOLDOWN_SECONDS: f64 = 10.0;
const STRICT_NON_INFERIORITY_MARGIN: f64 = 0.02;
const STRICT_MAX_OUTLIER_FRACTION: f64 = 0.30;
const STRICT_MAX_PAIR_SPREAD: f64 = 0.20;
const STRICT_REF_POSITION_REPLICATES: u64 = 3;
pub(crate) const MAX_PAIRED_REPLICATES: u64 = 127;
pub(crate) const MAX_SAMPLE_SIZE: u64 = 10_000;
pub(crate) const MAX_DURATION_SECONDS: f64 = 3_600.0;
pub(crate) const MIN_BENCHMARK_FREE_BYTES: u64 = 8 * 1024 * 1024 * 1024;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MeasurementPolicy {
    pub(crate) schema_version: u64,
    pub(crate) non_inferiority_margin: f64,
    pub(crate) resources: ResourcePolicy,
    pub(crate) criterion: CriterionPolicy,
    pub(crate) ref_to_ref: RefPolicy,
    pub(crate) cross_crate: CrossCratePolicy,
    pub(crate) paired_process: PairedProcessPolicy,
}

impl MeasurementPolicy {
    pub(crate) fn meets_strict_criterion_profile(&self) -> bool {
        self.non_inferiority_margin <= STRICT_NON_INFERIORITY_MARGIN
            && self.criterion.sample_size >= STRICT_SAMPLE_SIZE
            && self.criterion.warm_up_seconds >= STRICT_WARM_UP_SECONDS
            && self.criterion.measurement_seconds >= STRICT_MEASUREMENT_SECONDS
            && self.criterion.max_outlier_fraction <= STRICT_MAX_OUTLIER_FRACTION
    }

    pub(crate) fn meets_strict_ref_profile(&self) -> bool {
        self.meets_strict_criterion_profile()
            && self.ref_to_ref.blocks >= MIN_DRIFT_CORRECTED_BLOCKS
            && self.ref_to_ref.position_replicates >= STRICT_REF_POSITION_REPLICATES
            && self.ref_to_ref.max_pair_spread <= STRICT_MAX_PAIR_SPREAD
            && self.ref_to_ref.cooldown_seconds >= STRICT_COOLDOWN_SECONDS
    }

    pub(crate) fn meets_strict_cross_crate_profile(&self) -> bool {
        self.meets_strict_criterion_profile()
            && self.cross_crate.pairs >= MIN_GATING_PAIRS
            && self.ref_to_ref.max_pair_spread <= STRICT_MAX_PAIR_SPREAD
    }

    pub(crate) fn meets_strict_paired_profile(&self) -> bool {
        self.non_inferiority_margin <= STRICT_NON_INFERIORITY_MARGIN
            && self.criterion.sample_size >= STRICT_SAMPLE_SIZE
            && self.criterion.warm_up_seconds >= STRICT_WARM_UP_SECONDS
            && self.criterion.measurement_seconds >= STRICT_MEASUREMENT_SECONDS
            && self.criterion.max_outlier_fraction <= STRICT_MAX_OUTLIER_FRACTION
            && self.paired_process.confidence >= STRICT_PAIRED_CONFIDENCE
            && self.paired_process.process_replicates >= STRICT_PAIRED_REPLICATES
            && self.paired_process.cooldown_seconds >= STRICT_COOLDOWN_SECONDS
            && self.paired_process.aa_control
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CriterionPolicy {
    pub(crate) sample_size: u64,
    pub(crate) warm_up_seconds: f64,
    pub(crate) measurement_seconds: f64,
    pub(crate) max_outlier_fraction: f64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ResourcePolicy {
    pub(crate) minimum_free_bytes: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
pub(crate) enum PairSubject {
    A,
    B,
}

impl PairSubject {
    pub(crate) const fn as_char(self) -> char {
        match self {
            Self::A => 'A',
            Self::B => 'B',
        }
    }

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::A => "A",
            Self::B => "B",
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RefPolicy {
    pub(crate) blocks: u64,
    pub(crate) minimum_blocks: u64,
    pub(crate) maximum_blocks: u64,
    pub(crate) position_replicates: u64,
    pub(crate) max_pair_spread: f64,
    pub(crate) cooldown_seconds: f64,
    pub(crate) pair_orders: [[PairSubject; PAIRS_PER_BUILD_REPLICATE]; 2],
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CrossCratePolicy {
    pub(crate) pairs: u64,
    pub(crate) maximum_pairs: u64,
    pub(crate) pair_order: [PairSubject; PAIRS_PER_BUILD_REPLICATE],
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PairedProcessPolicy {
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
    load_with_source(path).map(|(policy, _)| policy)
}

pub(crate) fn load_with_source(path: &Path) -> Result<(MeasurementPolicy, Vec<u8>)> {
    let contents = fs::read(path)?;
    let policy: MeasurementPolicy = serde_json::from_slice(&contents)?;
    Ok((validate(policy)?, contents))
}

fn validate_path(path: &Path) -> Result<MeasurementPolicy> {
    load(path)
}

fn validate(policy: MeasurementPolicy) -> Result<MeasurementPolicy> {
    if policy.schema_version != 6 {
        return Err(invalid_data("measurement policy schema_version must be 6"));
    }
    fraction("non_inferiority_margin", policy.non_inferiority_margin)?;
    minimum("sample_size", policy.criterion.sample_size, 10)?;
    maximum("sample_size", policy.criterion.sample_size, MAX_SAMPLE_SIZE)?;
    fraction(
        "criterion.max_outlier_fraction",
        policy.criterion.max_outlier_fraction,
    )?;
    positive("warm_up_seconds", policy.criterion.warm_up_seconds)?;
    positive("measurement_seconds", policy.criterion.measurement_seconds)?;
    maximum_float(
        "warm_up_seconds",
        policy.criterion.warm_up_seconds,
        MAX_DURATION_SECONDS,
    )?;
    maximum_float(
        "measurement_seconds",
        policy.criterion.measurement_seconds,
        MAX_DURATION_SECONDS,
    )?;
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
    maximum(
        "maximum_blocks",
        policy.ref_to_ref.maximum_blocks,
        MAX_DRIFT_CORRECTED_BLOCKS,
    )?;
    maximum(
        "blocks",
        policy.ref_to_ref.blocks,
        policy.ref_to_ref.maximum_blocks,
    )?;
    minimum(
        "ref_to_ref.position_replicates",
        policy.ref_to_ref.position_replicates,
        1,
    )?;
    maximum(
        "ref_to_ref.position_replicates",
        policy.ref_to_ref.position_replicates,
        MAX_PAIRED_REPLICATES,
    )?;
    if policy.ref_to_ref.position_replicates.is_multiple_of(2) {
        return Err(invalid_data(
            "ref_to_ref.position_replicates must be odd so its median is an observed process",
        ));
    }
    fraction("max_pair_spread", policy.ref_to_ref.max_pair_spread)?;
    positive(
        "ref_to_ref.cooldown_seconds",
        policy.ref_to_ref.cooldown_seconds,
    )?;
    maximum_float(
        "ref_to_ref.cooldown_seconds",
        policy.ref_to_ref.cooldown_seconds,
        MAX_DURATION_SECONDS,
    )?;
    exact_order(
        "ref_to_ref.pair_orders[0]",
        policy.ref_to_ref.pair_orders[0],
        [
            PairSubject::A,
            PairSubject::B,
            PairSubject::B,
            PairSubject::A,
        ],
    )?;
    exact_order(
        "ref_to_ref.pair_orders[1]",
        policy.ref_to_ref.pair_orders[1],
        [
            PairSubject::B,
            PairSubject::A,
            PairSubject::A,
            PairSubject::B,
        ],
    )?;
    minimum("pairs", policy.cross_crate.pairs, MIN_GATING_PAIRS)?;
    maximum(
        "maximum_pairs",
        policy.cross_crate.maximum_pairs,
        MAX_GATING_PAIRS,
    )?;
    maximum(
        "pairs",
        policy.cross_crate.pairs,
        policy.cross_crate.maximum_pairs,
    )?;
    minimum(
        "minimum_free_bytes",
        policy.resources.minimum_free_bytes,
        MIN_BENCHMARK_FREE_BYTES,
    )?;
    if !policy
        .cross_crate
        .pairs
        .is_multiple_of(PAIRS_PER_BUILD_REPLICATE as u64 * 2)
    {
        return Err(invalid_data(
            "cross_crate pairs must provide balanced build replicates",
        ));
    }
    exact_order(
        "cross_crate",
        policy.cross_crate.pair_order,
        [
            PairSubject::A,
            PairSubject::B,
            PairSubject::A,
            PairSubject::B,
        ],
    )?;
    if !(0.5..1.0).contains(&policy.paired_process.confidence) {
        return Err(invalid_data(
            "paired_process confidence must be between 0.5 and 1",
        ));
    }
    minimum(
        "process_replicates",
        policy.paired_process.process_replicates,
        1,
    )?;
    maximum(
        "process_replicates",
        policy.paired_process.process_replicates,
        MAX_PAIRED_REPLICATES,
    )?;
    if !policy.paired_process.cooldown_seconds.is_finite()
        || policy.paired_process.cooldown_seconds < 0.0
    {
        return Err(invalid_data(
            "paired_process cooldown_seconds must be finite and nonnegative",
        ));
    }
    maximum_float(
        "paired_process.cooldown_seconds",
        policy.paired_process.cooldown_seconds,
        MAX_DURATION_SECONDS,
    )?;
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

fn maximum(name: &str, value: u64, maximum: u64) -> Result<()> {
    if value > maximum {
        Err(invalid_data(format!(
            "measurement policy field {name:?} must be an integer <= {maximum}"
        )))
    } else {
        Ok(())
    }
}

fn maximum_float(name: &str, value: f64, maximum: f64) -> Result<()> {
    if value <= maximum {
        Ok(())
    } else {
        Err(invalid_data(format!(
            "measurement policy field {name:?} must be <= {maximum}"
        )))
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

fn exact_order(
    name: &str,
    order: [PairSubject; PAIRS_PER_BUILD_REPLICATE],
    expected: [PairSubject; PAIRS_PER_BUILD_REPLICATE],
) -> Result<()> {
    if order == expected {
        Ok(())
    } else {
        Err(invalid_data(format!(
            "{name} pair_order must match the declared four-entry schedule"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_policy() -> MeasurementPolicy {
        MeasurementPolicy {
            schema_version: 6,
            non_inferiority_margin: 0.02,
            resources: ResourcePolicy {
                minimum_free_bytes: MIN_BENCHMARK_FREE_BYTES,
            },
            criterion: CriterionPolicy {
                sample_size: 50,
                warm_up_seconds: 2.0,
                measurement_seconds: 5.0,
                max_outlier_fraction: 0.30,
            },
            ref_to_ref: RefPolicy {
                blocks: 8,
                minimum_blocks: 8,
                maximum_blocks: 64,
                position_replicates: 3,
                max_pair_spread: 0.2,
                cooldown_seconds: 10.0,
                pair_orders: [
                    [
                        PairSubject::A,
                        PairSubject::B,
                        PairSubject::B,
                        PairSubject::A,
                    ],
                    [
                        PairSubject::B,
                        PairSubject::A,
                        PairSubject::A,
                        PairSubject::B,
                    ],
                ],
            },
            cross_crate: CrossCratePolicy {
                pairs: 24,
                maximum_pairs: 128,
                pair_order: [
                    PairSubject::A,
                    PairSubject::B,
                    PairSubject::A,
                    PairSubject::B,
                ],
            },
            paired_process: PairedProcessPolicy {
                confidence: 0.95,
                process_replicates: 8,
                cooldown_seconds: 10.0,
                aa_control: true,
            },
        }
    }

    #[test]
    fn accepts_repository_policy() {
        let policy =
            validate_path(&crate::repository_root().join("benchmarks/measurement-policy.json"))
                .unwrap();
        assert!(policy.meets_strict_paired_profile());
    }

    #[test]
    fn rejects_unbalanced_order() {
        let mut policy = valid_policy();
        policy.ref_to_ref.pair_orders[0] = [PairSubject::A; 4];
        assert!(validate(policy).is_err());
    }

    #[test]
    fn rejects_even_ref_position_replicates() {
        let mut policy = valid_policy();
        policy.ref_to_ref.position_replicates = 2;
        assert!(validate(policy).is_err());
    }

    #[test]
    fn rejects_insufficient_cross_crate_pairs() {
        let mut policy = valid_policy();
        policy.cross_crate.pairs = 16;
        assert!(validate(policy).is_err());
    }

    #[test]
    fn weak_paired_profiles_are_exploratory() {
        let mut policy = valid_policy();
        policy.paired_process.aa_control = false;
        assert!(!policy.meets_strict_paired_profile());

        let mut policy = valid_policy();
        policy.paired_process.process_replicates = 7;
        assert!(!policy.meets_strict_paired_profile());

        let mut policy = valid_policy();
        policy.paired_process.confidence = 0.90;
        assert!(!policy.meets_strict_paired_profile());
    }

    #[test]
    fn weak_criterion_profiles_are_exploratory() {
        let mut policy = valid_policy();
        policy.non_inferiority_margin = 0.03;
        assert!(!policy.meets_strict_ref_profile());
        assert!(!policy.meets_strict_cross_crate_profile());

        let mut policy = valid_policy();
        policy.ref_to_ref.blocks = 7;
        assert!(!policy.meets_strict_ref_profile());

        let mut policy = valid_policy();
        policy.ref_to_ref.position_replicates = 1;
        assert!(!policy.meets_strict_ref_profile());

        let mut policy = valid_policy();
        policy.cross_crate.pairs = 16;
        assert!(!policy.meets_strict_cross_crate_profile());

        let mut policy = valid_policy();
        policy.ref_to_ref.max_pair_spread = STRICT_MAX_PAIR_SPREAD;
        assert!(policy.meets_strict_cross_crate_profile());
        policy.ref_to_ref.max_pair_spread = f64::from_bits(STRICT_MAX_PAIR_SPREAD.to_bits() + 1);
        assert!(!policy.meets_strict_cross_crate_profile());
    }
}
