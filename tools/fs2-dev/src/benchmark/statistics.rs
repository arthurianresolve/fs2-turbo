use std::collections::BTreeMap;

use serde::Serialize;

use crate::{Result, invalid_data};

const MIN_GATING_REPLICATES: usize = 6;

#[derive(Clone, Debug, Serialize)]
pub(crate) struct Decision {
    pub(crate) benchmark: String,
    pub(crate) median_ratio: f64,
    pub(crate) lower_bound: f64,
    pub(crate) upper_bound: f64,
    pub(crate) disposition: &'static str,
}

pub(crate) fn median(values: &mut [f64]) -> Result<f64> {
    if values.is_empty() || values.iter().any(|value| !value.is_finite()) {
        return Err(invalid_data("median requires finite observations"));
    }
    values.sort_unstable_by(f64::total_cmp);
    let middle = values.len() / 2;
    Ok(if values.len().is_multiple_of(2) {
        values[middle - 1] / 2.0 + values[middle] / 2.0
    } else {
        values[middle]
    })
}

pub(crate) fn median_absolute_deviation(values: &[f64]) -> Result<f64> {
    let mut center_values = values.to_vec();
    let center = median(&mut center_values)?;
    let mut deviations = values
        .iter()
        .map(|value| (value - center).abs())
        .collect::<Vec<_>>();
    median(&mut deviations)
}

pub(crate) fn exact_median_bounds(values: &[f64], confidence: f64) -> Result<(f64, f64, f64)> {
    if !(0.5..1.0).contains(&confidence)
        || values.is_empty()
        || values.iter().any(|value| !value.is_finite())
    {
        return Err(invalid_data(
            "invalid exact-median observations or confidence",
        ));
    }
    let count = values.len();
    let denominator = 2u128
        .checked_pow(u32::try_from(count)?)
        .ok_or_else(|| invalid_data("too many observations for exact median bounds"))?;
    let mut selected = None;
    for rank in 1..=count {
        let mut upper_tail = 0u128;
        for value in rank..=count {
            upper_tail = upper_tail
                .checked_add(binomial(count, value)?)
                .ok_or_else(|| invalid_data("exact median tail exceeds supported precision"))?;
        }
        let achieved = 1.0 - upper_tail as f64 / denominator as f64;
        if achieved >= confidence {
            selected = Some((rank, achieved));
            break;
        }
    }
    let (rank, achieved) = selected
        .ok_or_else(|| invalid_data("too few observations for requested median confidence"))?;
    let mut ordered = values.to_vec();
    ordered.sort_unstable_by(f64::total_cmp);
    Ok((ordered[count - rank], ordered[rank - 1], achieved))
}

pub(crate) fn evaluate(ratios: &BTreeMap<String, Vec<f64>>, margin: f64) -> Result<Vec<Decision>> {
    if !(0.0..1.0).contains(&margin) || ratios.is_empty() {
        return Err(invalid_data(
            "invalid non-inferiority margin or empty ratios",
        ));
    }
    let counts = ratios
        .values()
        .map(Vec::len)
        .collect::<std::collections::HashSet<_>>();
    if counts.len() != 1 || counts.iter().next().copied().unwrap_or(0) < MIN_GATING_REPLICATES {
        return Err(invalid_data(
            "at least six equal-sized independent replicates are required",
        ));
    }
    let limit = 1.0 + margin;
    ratios
        .iter()
        .map(|(benchmark, values)| {
            if values
                .iter()
                .any(|value| !value.is_finite() || *value <= 0.0)
            {
                return Err(invalid_data("benchmark ratios must be finite and positive"));
            }
            let mut sorted = values.clone();
            let median_ratio = median(&mut sorted)?;
            let (lower_bound, upper_bound, _) = exact_median_bounds(values, 0.95)?;
            let disposition = if upper_bound <= 1.0 {
                "pass"
            } else if upper_bound <= limit {
                "non-inferior"
            } else {
                "inconclusive-or-slower"
            };
            Ok(Decision {
                benchmark: benchmark.clone(),
                median_ratio,
                lower_bound,
                upper_bound,
                disposition,
            })
        })
        .collect()
}

pub(crate) fn geometric_mean(left: f64, right: f64) -> Result<f64> {
    if !left.is_finite() || !right.is_finite() || left <= 0.0 || right <= 0.0 {
        Err(invalid_data(
            "geometric mean requires finite positive ratios",
        ))
    } else {
        Ok(((left.ln() + right.ln()) / 2.0).exp())
    }
}

fn binomial(n: usize, k: usize) -> Result<u128> {
    if k > n {
        return Err(invalid_data("binomial selection exceeds population"));
    }
    let k = k.min(n - k);
    let mut result = 1u128;
    for index in 0..k {
        let mut numerator = (n - index) as u128;
        let mut denominator = (index + 1) as u128;
        let common = greatest_common_divisor(numerator, denominator);
        numerator /= common;
        denominator /= common;
        let common = greatest_common_divisor(result, denominator);
        result /= common;
        denominator /= common;
        if denominator != 1 {
            return Err(invalid_data("binomial coefficient is not integral"));
        }
        result = result
            .checked_mul(numerator)
            .ok_or_else(|| invalid_data("binomial coefficient exceeds supported precision"))?;
    }
    Ok(result)
}

fn greatest_common_divisor(mut left: u128, mut right: u128) -> u128 {
    while right != 0 {
        let remainder = left % right;
        left = right;
        right = remainder;
    }
    left
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_bounds_match_eight_replicate_policy() {
        let values = (1..=8).map(f64::from).collect::<Vec<_>>();
        let (lower, upper, achieved) = exact_median_bounds(&values, 0.95).unwrap();
        assert_eq!((lower, upper), (2.0, 7.0));
        assert!((achieved - 247.0 / 256.0).abs() < f64::EPSILON);
    }

    #[test]
    fn exact_binomial_distribution_supports_maximum_replicates() {
        let mut total = 0u128;
        for selected in 0..=127 {
            total = total.checked_add(binomial(127, selected).unwrap()).unwrap();
        }
        assert_eq!(total, 1u128 << 127);
    }

    #[test]
    fn non_inferiority_uses_the_exact_upper_bound() {
        let mut ratios = BTreeMap::new();
        ratios.insert(
            "operation".to_owned(),
            vec![0.995, 0.998, 1.001, 1.004, 1.007, 1.009],
        );
        assert_eq!(
            evaluate(&ratios, 0.01).unwrap()[0].disposition,
            "non-inferior"
        );
    }
}
