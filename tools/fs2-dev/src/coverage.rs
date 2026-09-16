use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::{Result, invalid_data};

const LLVM_COVERAGE_EXPORT: &str = "llvm.coverage.json.export";

// Reviewed integration-only residuals for Rust 1.98.1. These are private,
// platform-specific error or fallback paths covered through unit seams. Any
// source movement or changed residual requires an explicit policy review.
const WINDOWS_INTEGRATION_RESIDUAL: &[&str] = &[
    "src/stats.rs:20:1",
    "src/stats/counters.rs:70:5",
    "src/windows/allocation.rs:58:67",
    "src/windows/allocation.rs:64:57",
    "src/windows/allocation.rs:165:22",
    "src/windows/allocation.rs:332:55",
    "src/windows/allocation.rs:360:1",
    "src/windows/allocation.rs:378:22",
    "src/windows/overlapped.rs:43:5",
    "src/windows/stats/legacy.rs:8:1",
    "src/windows/stats/legacy.rs:12:1",
    "src/windows/stats/legacy.rs:33:1",
    "src/windows/stats/legacy.rs:36:12",
    "src/windows/stats/legacy.rs:37:12",
    "src/windows/stats/legacy.rs:41:1",
    "src/windows/stats/legacy.rs:54:1",
    "src/windows/stats/legacy.rs:73:1",
    "src/windows/stats/legacy.rs:87:1",
    "src/windows/stats/legacy.rs:109:1",
    "src/windows/stats/legacy.rs:128:1",
    "src/windows/stats/modern.rs:83:1",
    "src/windows/stats/modern.rs:100:1",
    "src/windows/stats/modern.rs:105:1",
    "src/windows/stats/modern.rs:143:1",
    "src/windows/stats/space.rs:128:1",
    "src/windows/stats/space.rs:378:1",
    "src/windows/stats/space.rs:388:1",
];

const LINUX_INTEGRATION_RESIDUAL: &[&str] = &[
    "src/allocation.rs:73:1",
    "src/allocation.rs:77:40",
    "src/allocation.rs:83:1",
    "src/stats.rs:20:1",
    "src/stats/validation.rs:65:24",
    "src/unix/allocation.rs:85:22",
    "src/unix/allocation.rs:90:1",
    "src/unix/stats.rs:172:34",
];

const MACOS_INTEGRATION_RESIDUAL: &[&str] = &[
    "src/stats.rs:20:1",
    "src/stats/validation.rs:65:24",
    "src/unix/allocation.rs:90:1",
];

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
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
struct FileTotals {
    instantiations: Metric,
}

#[derive(Debug, Deserialize)]
struct CoverageFile {
    filename: String,
    summary: FileTotals,
}

#[derive(Debug, Deserialize)]
struct FunctionCoverage {
    name: String,
    count: u64,
    filenames: Vec<String>,
    regions: Vec<Vec<u64>>,
}

#[derive(Debug, Deserialize)]
struct CoverageData {
    files: Vec<CoverageFile>,
    functions: Vec<FunctionCoverage>,
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

#[derive(Clone, Copy, Debug)]
struct IntegrationPolicy {
    uncovered_definition_groups: &'static [&'static str],
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct DefinitionKey {
    filename: String,
    line: u64,
    column: u64,
    regions: Vec<(u64, u64, u64, u64, u64)>,
}

#[derive(Debug, Eq, PartialEq, Serialize)]
struct FileInstantiationGap {
    filename: String,
    uncovered: u64,
    count: u64,
}

#[derive(Debug, Eq, PartialEq)]
struct InstantiationDiagnostics {
    entry_count: u64,
    executed_entries: u64,
    definition_groups: u64,
    covered_definition_groups: u64,
    asymmetric_definition_groups: u64,
    uncovered_definition_groups: Vec<String>,
    file_gaps: Vec<FileInstantiationGap>,
}

#[derive(Debug)]
struct DefinitionGroupState {
    entries: u64,
    covered_entries: u64,
    symbols: BTreeSet<String>,
}

#[derive(Serialize)]
struct CoverageDiagnosticsReport<'a> {
    schema_version: u32,
    target: &'a str,
    profiles: Vec<ProfileDiagnosticsReport<'a>>,
}

#[derive(Serialize)]
struct ProfileDiagnosticsReport<'a> {
    profile: &'a str,
    llvm_instantiations: Metric,
    json_entries: Metric,
    source_definitions: Metric,
    asymmetric_definition_groups: u64,
    definitions: Vec<DefinitionDiagnosticsRecord>,
    file_gaps: &'a [FileInstantiationGap],
}

#[derive(Serialize)]
struct DefinitionDiagnosticsRecord {
    source: String,
    line: u64,
    column: u64,
    regions: Vec<(u64, u64, u64, u64, u64)>,
    entries: u64,
    covered_entries: u64,
    ownership: &'static str,
    symbols: Vec<String>,
}

pub(crate) fn run(
    target: &str,
    json_path: &Path,
    lcov_path: &Path,
    unit_json_path: &Path,
    integration_json_path: &Path,
    diagnostics_json_path: &Path,
) -> Result<()> {
    let policy = policy_for_target(target)?;
    let export = parse_json(&fs::read_to_string(json_path)?)?;
    let unit_export = parse_json(&fs::read_to_string(unit_json_path)?)?;
    let integration_export = parse_json(&fs::read_to_string(integration_json_path)?)?;
    let physical_lines = parse_lcov(&fs::read_to_string(lcov_path)?)?;
    let data = single_data(&export, "combined")?;
    let unit_data = single_data(&unit_export, "unit")?;
    let integration_data = single_data(&integration_export, "integration")?;

    validate(target, policy, &data.totals, physical_lines)?;
    let combined_diagnostics = instantiation_diagnostics(data)?;
    let unit_diagnostics = instantiation_diagnostics(unit_data)?;
    let integration_diagnostics = instantiation_diagnostics(integration_data)?;
    let combined_groups = definition_groups(data)?;
    let unit_groups = definition_groups(unit_data)?;
    let integration_groups = definition_groups(integration_data)?;
    validate_unit_instantiations(target, unit_data, &unit_diagnostics)?;
    validate_integration_instantiations(
        target,
        &unit_groups,
        &integration_groups,
        &integration_diagnostics,
    )?;
    write_diagnostics_report(
        diagnostics_json_path,
        target,
        [
            (
                "combined",
                data,
                &combined_groups,
                &combined_diagnostics,
                None,
            ),
            ("unit", unit_data, &unit_groups, &unit_diagnostics, None),
            (
                "integration",
                integration_data,
                &integration_groups,
                &integration_diagnostics,
                Some(&unit_groups),
            ),
        ],
    )?;
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
    print_instantiation_diagnostics(target, "combined", data, &combined_diagnostics);
    print_instantiation_diagnostics(target, "unit", unit_data, &unit_diagnostics);
    print_instantiation_diagnostics(
        target,
        "integration",
        integration_data,
        &integration_diagnostics,
    );
    Ok(())
}

fn single_data<'a>(export: &'a CoverageExport, profile: &str) -> Result<&'a CoverageData> {
    export
        .data
        .as_slice()
        .first()
        .filter(|_| export.data.len() == 1)
        .ok_or_else(|| {
            invalid_data(format!(
                "{profile} coverage JSON must contain exactly one data set"
            ))
        })
}

fn print_instantiation_diagnostics(
    target: &str,
    profile: &str,
    data: &CoverageData,
    diagnostics: &InstantiationDiagnostics,
) {
    println!(
        "{profile} instantiation structure for {target}: LLVM instantiations {}/{}, JSON entries {}/{}, source-definition groups {}/{}, asymmetric groups {}",
        data.totals.instantiations.covered,
        data.totals.instantiations.count,
        diagnostics.executed_entries,
        diagnostics.entry_count,
        diagnostics.covered_definition_groups,
        diagnostics.definition_groups,
        diagnostics.asymmetric_definition_groups,
    );
    for group in &diagnostics.uncovered_definition_groups {
        println!("{profile} source-definition gap for {target}: {group}");
    }
    for gap in &diagnostics.file_gaps {
        println!(
            "{profile} instantiation gap for {target}: {} has {}/{} uncovered",
            gap.filename, gap.uncovered, gap.count
        );
    }
}

fn instantiation_diagnostics(data: &CoverageData) -> Result<InstantiationDiagnostics> {
    let groups = definition_groups(data)?;
    let mut entry_count = 0_u64;
    let mut executed_entries = 0_u64;

    for function in &data.functions {
        entry_count = entry_count
            .checked_add(1)
            .ok_or_else(|| invalid_data("coverage function-entry count overflowed"))?;
        if function.count != 0 {
            executed_entries = executed_entries
                .checked_add(1)
                .ok_or_else(|| invalid_data("coverage executed-entry count overflowed"))?;
        }
    }

    let mut file_gaps = Vec::new();
    for file in &data.files {
        let gap = uncovered(file.summary.instantiations, "file instantiations")?;
        if gap != 0 {
            file_gaps.push(FileInstantiationGap {
                filename: file.filename.clone(),
                uncovered: gap,
                count: file.summary.instantiations.count,
            });
        }
    }
    file_gaps.sort_by(|left, right| {
        right
            .uncovered
            .cmp(&left.uncovered)
            .then_with(|| left.filename.cmp(&right.filename))
    });

    let uncovered_definition_groups = groups
        .iter()
        .filter(|(_, state)| state.covered_entries == 0)
        .map(|(key, _)| format!("{}:{}:{}", key.filename, key.line, key.column))
        .collect();

    Ok(InstantiationDiagnostics {
        entry_count,
        executed_entries,
        definition_groups: groups.len().try_into()?,
        covered_definition_groups: groups
            .values()
            .filter(|state| state.covered_entries != 0)
            .count()
            .try_into()?,
        asymmetric_definition_groups: groups
            .values()
            .filter(|state| state.covered_entries != 0 && state.covered_entries < state.entries)
            .count()
            .try_into()?,
        uncovered_definition_groups,
        file_gaps,
    })
}

fn definition_groups(data: &CoverageData) -> Result<BTreeMap<DefinitionKey, DefinitionGroupState>> {
    let mut groups = BTreeMap::<DefinitionKey, DefinitionGroupState>::new();
    for function in &data.functions {
        let Some(key) = definition_key(function)? else {
            continue;
        };
        let group = groups.entry(key).or_insert_with(|| DefinitionGroupState {
            entries: 0,
            covered_entries: 0,
            symbols: BTreeSet::new(),
        });
        group.entries = group
            .entries
            .checked_add(1)
            .ok_or_else(|| invalid_data("coverage definition-group count overflowed"))?;
        if function.count != 0 {
            group.covered_entries = group
                .covered_entries
                .checked_add(1)
                .ok_or_else(|| invalid_data("coverage covered-group count overflowed"))?;
        }
        group.symbols.insert(function.name.clone());
    }
    Ok(groups)
}

fn definition_key(function: &FunctionCoverage) -> Result<Option<DefinitionKey>> {
    let Some(filename) = function.filenames.first() else {
        return Ok(None);
    };
    let Some(start) = function
        .regions
        .iter()
        .find(|region| region.get(7) == Some(&0))
    else {
        return Ok(None);
    };
    let (Some(&line), Some(&column)) = (start.first(), start.get(1)) else {
        return Ok(None);
    };
    let regions = function
        .regions
        .iter()
        .map(|region| {
            let values = (
                region.first(),
                region.get(1),
                region.get(2),
                region.get(3),
                region.get(7),
            );
            match values {
                (
                    Some(&start_line),
                    Some(&start_column),
                    Some(&end_line),
                    Some(&end_column),
                    Some(&kind),
                ) => Ok((start_line, start_column, end_line, end_column, kind)),
                _ => Err(invalid_data(
                    "coverage function contains a malformed region",
                )),
            }
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(Some(DefinitionKey {
        filename: normalize_source_path(filename),
        line,
        column,
        regions,
    }))
}

fn normalize_source_path(filename: &str) -> String {
    let normalized = filename.replace('\\', "/");
    normalized
        .rfind("/src/")
        .map_or(normalized.clone(), |index| {
            normalized[index + 1..].to_owned()
        })
}

fn validate_unit_instantiations(
    target: &str,
    data: &CoverageData,
    diagnostics: &InstantiationDiagnostics,
) -> Result<()> {
    require_complete(
        target,
        "unit LLVM instantiations",
        metric_lines(data.totals.instantiations)?,
    )?;
    if diagnostics.executed_entries != diagnostics.entry_count {
        return Err(invalid_data(format!(
            "coverage regression for {target}: unit JSON entries are {}/{}",
            diagnostics.executed_entries, diagnostics.entry_count
        )));
    }
    if diagnostics.covered_definition_groups != diagnostics.definition_groups {
        return Err(invalid_data(format!(
            "coverage regression for {target}: unit source-definition groups are {}/{}",
            diagnostics.covered_definition_groups, diagnostics.definition_groups
        )));
    }
    Ok(())
}

fn validate_integration_instantiations(
    target: &str,
    unit_groups: &BTreeMap<DefinitionKey, DefinitionGroupState>,
    integration_groups: &BTreeMap<DefinitionKey, DefinitionGroupState>,
    diagnostics: &InstantiationDiagnostics,
) -> Result<()> {
    let policy = integration_policy_for_target(target)?;
    let actual = diagnostics
        .uncovered_definition_groups
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let expected = policy
        .uncovered_definition_groups
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();
    if actual != expected
        || diagnostics.uncovered_definition_groups.len() != policy.uncovered_definition_groups.len()
    {
        let unexpected = actual.difference(&expected).copied().collect::<Vec<_>>();
        let resolved = expected.difference(&actual).copied().collect::<Vec<_>>();
        return Err(invalid_data(format!(
            "coverage integration residual changed for {target}; unexpected [{}], resolved [{}]",
            unexpected.join(", "),
            resolved.join(", ")
        )));
    }

    let unowned = integration_groups
        .iter()
        .filter(|(_, state)| state.covered_entries == 0)
        .filter(|(key, _)| {
            unit_groups
                .get(*key)
                .is_none_or(|state| state.covered_entries == 0)
        })
        .map(|(key, _)| format!("{}:{}:{}", key.filename, key.line, key.column))
        .collect::<Vec<_>>();
    if !unowned.is_empty() {
        return Err(invalid_data(format!(
            "coverage integration residuals for {target} lack unit ownership: [{}]",
            unowned.join(", ")
        )));
    }
    Ok(())
}

type ProfileEvidence<'a> = (
    &'a str,
    &'a CoverageData,
    &'a BTreeMap<DefinitionKey, DefinitionGroupState>,
    &'a InstantiationDiagnostics,
    Option<&'a BTreeMap<DefinitionKey, DefinitionGroupState>>,
);

fn write_diagnostics_report<const N: usize>(
    path: &Path,
    target: &str,
    profiles: [ProfileEvidence<'_>; N],
) -> Result<()> {
    let profiles = profiles
        .into_iter()
        .map(|(profile, data, groups, diagnostics, unit_groups)| {
            let definitions = groups
                .iter()
                .map(|(key, state)| {
                    let ownership = if state.covered_entries == 0 {
                        if unit_groups
                            .and_then(|groups| groups.get(key))
                            .is_some_and(|unit| unit.covered_entries != 0)
                        {
                            "private-unit"
                        } else {
                            "unowned"
                        }
                    } else if state.covered_entries < state.entries {
                        "compiler-asymmetric"
                    } else {
                        "covered"
                    };
                    DefinitionDiagnosticsRecord {
                        source: key.filename.clone(),
                        line: key.line,
                        column: key.column,
                        regions: key.regions.clone(),
                        entries: state.entries,
                        covered_entries: state.covered_entries,
                        ownership,
                        symbols: state.symbols.iter().cloned().collect(),
                    }
                })
                .collect();
            ProfileDiagnosticsReport {
                profile,
                llvm_instantiations: data.totals.instantiations,
                json_entries: Metric {
                    count: diagnostics.entry_count,
                    covered: diagnostics.executed_entries,
                },
                source_definitions: Metric {
                    count: diagnostics.definition_groups,
                    covered: diagnostics.covered_definition_groups,
                },
                asymmetric_definition_groups: diagnostics.asymmetric_definition_groups,
                definitions,
                file_gaps: &diagnostics.file_gaps,
            }
        })
        .collect();
    let report = CoverageDiagnosticsReport {
        schema_version: 1,
        target,
        profiles,
    };
    let mut encoded = serde_json::to_vec_pretty(&report)?;
    encoded.push(b'\n');
    fs::write(path, encoded)?;
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
            maximum_uncovered_regions: 0,
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

fn integration_policy_for_target(target: &str) -> Result<IntegrationPolicy> {
    let policy = match target {
        "x86_64-pc-windows-msvc" => IntegrationPolicy {
            uncovered_definition_groups: WINDOWS_INTEGRATION_RESIDUAL,
        },
        "x86_64-unknown-linux-gnu" => IntegrationPolicy {
            uncovered_definition_groups: LINUX_INTEGRATION_RESIDUAL,
        },
        "aarch64-apple-darwin" => IntegrationPolicy {
            uncovered_definition_groups: MACOS_INTEGRATION_RESIDUAL,
        },
        _ => {
            return Err(invalid_data(format!(
                "no native integration coverage policy exists for target {target:?}"
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

    #[test]
    fn distinguishes_raw_entries_from_source_definition_groups() {
        let complete = Metric {
            count: 1,
            covered: 1,
        };
        let data = CoverageData {
            files: vec![CoverageFile {
                filename: "src/lib.rs".to_owned(),
                summary: FileTotals {
                    instantiations: Metric {
                        count: 3,
                        covered: 1,
                    },
                },
            }],
            functions: vec![
                FunctionCoverage {
                    name: "first-uncovered-instance".to_owned(),
                    count: 0,
                    filenames: vec!["src/lib.rs".to_owned()],
                    regions: vec![vec![7, 1, 7, 8, 0, 0, 0, 0]],
                },
                FunctionCoverage {
                    name: "covered-instance".to_owned(),
                    count: 1,
                    filenames: vec!["src/lib.rs".to_owned()],
                    regions: vec![vec![7, 1, 7, 8, 0, 0, 0, 0]],
                },
                FunctionCoverage {
                    name: "uncovered-definition".to_owned(),
                    count: 0,
                    filenames: vec!["src/lib.rs".to_owned()],
                    regions: vec![vec![11, 1, 11, 8, 0, 0, 0, 0]],
                },
            ],
            totals: Totals {
                functions: complete,
                instantiations: Metric {
                    count: 3,
                    covered: 1,
                },
                lines: complete,
                regions: complete,
            },
        };

        assert_eq!(
            instantiation_diagnostics(&data).unwrap(),
            InstantiationDiagnostics {
                entry_count: 3,
                executed_entries: 1,
                definition_groups: 2,
                covered_definition_groups: 1,
                asymmetric_definition_groups: 1,
                uncovered_definition_groups: vec!["src/lib.rs:11:1".to_owned()],
                file_gaps: vec![FileInstantiationGap {
                    filename: "src/lib.rs".to_owned(),
                    uncovered: 2,
                    count: 3,
                }],
            }
        );
    }
}
