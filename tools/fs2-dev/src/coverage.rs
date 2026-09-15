use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde::Deserialize;

use crate::{Result, invalid_data};

const LLVM_COVERAGE_EXPORT: &str = "llvm.coverage.json.export";

#[derive(Clone, Copy, Debug, Deserialize)]
struct Metric {
    count: u64,
    covered: u64,
}

#[derive(Debug, Deserialize)]
struct Totals {
    functions: Metric,
    instantiations: Metric,
    lines: Metric,
    regions: Metric,
}

#[derive(Debug, Deserialize)]
struct CoverageData {
    totals: Totals,
}

#[derive(Debug, Deserialize)]
struct CoverageExport {
    data: Vec<CoverageData>,
    #[serde(rename = "type")]
    kind: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PhysicalLines {
    count: u64,
    covered: u64,
}

#[derive(Clone, Copy, Debug)]
struct CoveragePolicy {
    minimum_unique_lines: u64,
    minimum_aggregate_lines: u64,
    minimum_regions: u64,
    minimum_functions: u64,
    maximum_uncovered_regions: u64,
}

pub(crate) fn run(target: &str, json_path: &Path, lcov_path: &Path) -> Result<()> {
    let policy = policy_for_target(target)?;
    let export = parse_json(&fs::read_to_string(json_path)?)?;
    let physical_lines = parse_lcov(&fs::read_to_string(lcov_path)?)?;
    let data = export
        .data
        .as_slice()
        .first()
        .filter(|_| export.data.len() == 1)
        .ok_or_else(|| invalid_data("coverage JSON must contain exactly one data set"))?;

    validate(target, policy, &data.totals, physical_lines)?;
    println!(
        "coverage policy satisfied for {target}: unique lines {}/{}, LLVM lines {}/{}, regions {}/{}, functions {}/{}, instantiations {}/{} (diagnostic)",
        physical_lines.covered,
        physical_lines.count,
        data.totals.lines.covered,
        data.totals.lines.count,
        data.totals.regions.covered,
        data.totals.regions.count,
        data.totals.functions.covered,
        data.totals.functions.count,
        data.totals.instantiations.covered,
        data.totals.instantiations.count,
    );
    Ok(())
}

fn parse_json(contents: &str) -> Result<CoverageExport> {
    let export: CoverageExport = serde_json::from_str(contents)?;
    if export.kind != LLVM_COVERAGE_EXPORT {
        return Err(invalid_data(format!(
            "unexpected coverage JSON type: {:?}",
            export.kind
        )));
    }
    Ok(export)
}

fn parse_lcov(contents: &str) -> Result<PhysicalLines> {
    let mut source = None::<String>;
    let mut lines = BTreeMap::<(String, u64), u64>::new();

    for (index, line) in contents.lines().enumerate() {
        if let Some(path) = line.strip_prefix("SF:") {
            if path.is_empty() {
                return Err(invalid_data(format!(
                    "LCOV source path is empty on line {}",
                    index + 1
                )));
            }
            source = Some(path.to_owned());
        } else if let Some(record) = line.strip_prefix("DA:") {
            let source = source.as_ref().ok_or_else(|| {
                invalid_data(format!(
                    "LCOV line data precedes a source record on line {}",
                    index + 1
                ))
            })?;
            let mut fields = record.split(',');
            let line_number = parse_lcov_number(fields.next(), index, "line number")?;
            let execution_count = parse_lcov_number(fields.next(), index, "execution count")?;
            let key = (source.clone(), line_number);
            lines
                .entry(key)
                .and_modify(|count| *count = (*count).max(execution_count))
                .or_insert(execution_count);
        } else if line == "end_of_record" {
            source = None;
        }
    }

    if lines.is_empty() {
        return Err(invalid_data(
            "LCOV report contains no physical source lines",
        ));
    }
    Ok(PhysicalLines {
        count: lines.len().try_into()?,
        covered: lines
            .values()
            .filter(|count| **count != 0)
            .count()
            .try_into()?,
    })
}

fn parse_lcov_number(field: Option<&str>, index: usize, name: &str) -> Result<u64> {
    field
        .ok_or_else(|| invalid_data(format!("LCOV {name} is missing on line {}", index + 1)))?
        .parse::<u64>()
        .map_err(|error| {
            invalid_data(format!(
                "LCOV {name} is invalid on line {}: {error}",
                index + 1
            ))
        })
}

fn policy_for_target(target: &str) -> Result<CoveragePolicy> {
    let policy = match target {
        "x86_64-pc-windows-msvc" => CoveragePolicy {
            minimum_unique_lines: 1_248,
            minimum_aggregate_lines: 1_268,
            minimum_regions: 1_672,
            minimum_functions: 176,
            // Deterministically forcing CreateEventW to fail would require
            // process-wide API interception or resource exhaustion.
            maximum_uncovered_regions: 5,
        },
        "x86_64-unknown-linux-gnu" => CoveragePolicy {
            minimum_unique_lines: 560,
            minimum_aggregate_lines: 572,
            minimum_regions: 837,
            minimum_functions: 104,
            maximum_uncovered_regions: 0,
        },
        "aarch64-apple-darwin" => CoveragePolicy {
            minimum_unique_lines: 526,
            minimum_aggregate_lines: 538,
            minimum_regions: 763,
            minimum_functions: 99,
            maximum_uncovered_regions: 0,
        },
        _ => {
            return Err(invalid_data(format!(
                "no native coverage policy exists for target {target:?}"
            )));
        }
    };
    Ok(policy)
}

fn validate(
    target: &str,
    policy: CoveragePolicy,
    totals: &Totals,
    physical_lines: PhysicalLines,
) -> Result<()> {
    require_minimum(
        target,
        "unique physical lines",
        physical_lines.count,
        policy.minimum_unique_lines,
    )?;
    require_complete(target, "unique physical lines", physical_lines)?;
    require_minimum(
        target,
        "LLVM aggregate lines",
        totals.lines.count,
        policy.minimum_aggregate_lines,
    )?;
    require_complete(target, "LLVM aggregate lines", metric_lines(totals.lines)?)?;
    require_minimum(
        target,
        "regions",
        totals.regions.count,
        policy.minimum_regions,
    )?;
    let uncovered_regions = uncovered(totals.regions, "regions")?;
    if uncovered_regions > policy.maximum_uncovered_regions {
        return Err(invalid_data(format!(
            "coverage regression for {target}: regions have {uncovered_regions} uncovered, maximum is {}",
            policy.maximum_uncovered_regions
        )));
    }
    require_minimum(
        target,
        "functions",
        totals.functions.count,
        policy.minimum_functions,
    )?;
    require_complete(target, "functions", metric_lines(totals.functions)?)?;
    uncovered(totals.instantiations, "instantiations")?;
    Ok(())
}

fn metric_lines(metric: Metric) -> Result<PhysicalLines> {
    uncovered(metric, "coverage metric")?;
    Ok(PhysicalLines {
        count: metric.count,
        covered: metric.covered,
    })
}

fn uncovered(metric: Metric, name: &str) -> Result<u64> {
    metric.count.checked_sub(metric.covered).ok_or_else(|| {
        invalid_data(format!(
            "coverage {name} reports more covered entries than total entries"
        ))
    })
}

fn require_minimum(target: &str, name: &str, actual: u64, minimum: u64) -> Result<()> {
    if actual < minimum {
        return Err(invalid_data(format!(
            "coverage report for {target} has only {actual} {name}; expected at least {minimum}"
        )));
    }
    Ok(())
}

fn require_complete(target: &str, name: &str, lines: PhysicalLines) -> Result<()> {
    if lines.covered != lines.count {
        return Err(invalid_data(format!(
            "coverage report for {target} has incomplete {name}: {}/{}",
            lines.covered, lines.count
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lcov_uses_the_union_of_physical_source_lines() {
        let lines = parse_lcov(
            "SF:src/lib.rs\nDA:7,0\nDA:8,0\nend_of_record\nSF:src/lib.rs\nDA:7,3\nend_of_record\n",
        )
        .unwrap();
        assert_eq!(
            lines,
            PhysicalLines {
                count: 2,
                covered: 1
            }
        );
    }

    #[test]
    fn rejects_coverage_regressions() {
        let policy = CoveragePolicy {
            minimum_unique_lines: 2,
            minimum_aggregate_lines: 2,
            minimum_regions: 3,
            minimum_functions: 1,
            maximum_uncovered_regions: 1,
        };
        let totals = Totals {
            functions: Metric {
                count: 1,
                covered: 1,
            },
            instantiations: Metric {
                count: 2,
                covered: 1,
            },
            lines: Metric {
                count: 2,
                covered: 2,
            },
            regions: Metric {
                count: 3,
                covered: 1,
            },
        };
        assert!(
            validate(
                "test-target",
                policy,
                &totals,
                PhysicalLines {
                    count: 2,
                    covered: 2
                }
            )
            .is_err()
        );
    }
}
