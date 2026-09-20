use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::{Result, invalid_data};

mod source_locations;

const LLVM_COVERAGE_EXPORT: &str = "llvm.coverage.json.export";

#[derive(Clone, Copy, Debug)]
struct IntendedIntegrationDefinition {
    api: &'static str,
    source: &'static str,
}

const WINDOWS_INTEGRATION_DEFINITIONS: &[IntendedIntegrationDefinition] = &[
    IntendedIntegrationDefinition {
        api: "windows::stats::modern::io_error_from_hresult",
        source: "src/windows/stats/modern.rs:83:1",
    },
    IntendedIntegrationDefinition {
        api: "windows::stats::modern::hresult_from_win32",
        source: "src/windows/stats/modern.rs:100:1",
    },
    IntendedIntegrationDefinition {
        api: "windows::stats::modern::modern_statvfs_unavailable",
        source: "src/windows/stats/modern.rs:105:1",
    },
    IntendedIntegrationDefinition {
        api: "windows::stats::space::space_after_exact_root",
        source: "src/windows/stats/space.rs:128:1",
    },
    IntendedIntegrationDefinition {
        api: "windows::stats::space::exact_root_value",
        source: "src/windows/stats/space.rs:378:1",
    },
    IntendedIntegrationDefinition {
        api: "windows::stats::space::is_volume_resolution_error",
        source: "src/windows/stats/space.rs:388:1",
    },
];

// These externally reachable definitions form the stable integration
// contract. Compiler-created monomorphizations remain diagnostic because their
// number and linkage names vary by toolchain even when this contract does not.
const INTENDED_INTEGRATION_DEFINITIONS: &[IntendedIntegrationDefinition] = &[
    IntendedIntegrationDefinition {
        api: "FileExt::fs2_lock_shared",
        source: "src/lib.rs:140:5",
    },
    IntendedIntegrationDefinition {
        api: "FileExt::fs2_lock_exclusive",
        source: "src/lib.rs:147:5",
    },
    IntendedIntegrationDefinition {
        api: "FileExt::fs2_try_lock_shared",
        source: "src/lib.rs:154:5",
    },
    IntendedIntegrationDefinition {
        api: "FileExt::fs2_try_lock_exclusive",
        source: "src/lib.rs:161:5",
    },
    IntendedIntegrationDefinition {
        api: "FileExt::fs2_unlock",
        source: "src/lib.rs:167:5",
    },
    IntendedIntegrationDefinition {
        api: "FileExt for File::duplicate",
        source: "src/lib.rs:193:5",
    },
    IntendedIntegrationDefinition {
        api: "FileExt for File::allocated_size",
        source: "src/lib.rs:197:5",
    },
    IntendedIntegrationDefinition {
        api: "FileExt for File::allocate",
        source: "src/lib.rs:201:5",
    },
    IntendedIntegrationDefinition {
        api: "FileExt for File::fs2_lock_shared",
        source: "src/lib.rs:205:5",
    },
    IntendedIntegrationDefinition {
        api: "FileExt for File::fs2_lock_exclusive",
        source: "src/lib.rs:209:5",
    },
    IntendedIntegrationDefinition {
        api: "FileExt for File::fs2_try_lock_shared",
        source: "src/lib.rs:213:5",
    },
    IntendedIntegrationDefinition {
        api: "FileExt for File::fs2_try_lock_exclusive",
        source: "src/lib.rs:217:5",
    },
    IntendedIntegrationDefinition {
        api: "FileExt for File::fs2_unlock",
        source: "src/lib.rs:221:5",
    },
    IntendedIntegrationDefinition {
        api: "FileExt for File::lock_shared",
        source: "src/lib.rs:225:5",
    },
    IntendedIntegrationDefinition {
        api: "FileExt for File::lock_exclusive",
        source: "src/lib.rs:229:5",
    },
    IntendedIntegrationDefinition {
        api: "FileExt for File::try_lock_shared",
        source: "src/lib.rs:233:5",
    },
    IntendedIntegrationDefinition {
        api: "FileExt for File::try_lock_exclusive",
        source: "src/lib.rs:237:5",
    },
    IntendedIntegrationDefinition {
        api: "FileExt for File::unlock",
        source: "src/lib.rs:241:5",
    },
    IntendedIntegrationDefinition {
        api: "lock_contended_error",
        source: "src/lib.rs:248:1",
    },
    IntendedIntegrationDefinition {
        api: "statvfs",
        source: "src/stats.rs:33:1",
    },
    IntendedIntegrationDefinition {
        api: "free_space",
        source: "src/stats.rs:41:1",
    },
    IntendedIntegrationDefinition {
        api: "available_space",
        source: "src/stats.rs:49:1",
    },
    IntendedIntegrationDefinition {
        api: "total_space",
        source: "src/stats.rs:57:1",
    },
    IntendedIntegrationDefinition {
        api: "allocation_granularity",
        source: "src/stats.rs:65:1",
    },
    IntendedIntegrationDefinition {
        api: "FsStatsQuery::new",
        source: "src/stats/query.rs:42:5",
    },
    IntendedIntegrationDefinition {
        api: "FsStatsQuery::snapshot",
        source: "src/stats/query.rs:63:5",
    },
    IntendedIntegrationDefinition {
        api: "FsStats::free_space",
        source: "src/stats/snapshot.rs:42:5",
    },
    IntendedIntegrationDefinition {
        api: "FsStats::available_space",
        source: "src/stats/snapshot.rs:48:5",
    },
    IntendedIntegrationDefinition {
        api: "FsStats::total_space",
        source: "src/stats/snapshot.rs:57:5",
    },
    IntendedIntegrationDefinition {
        api: "FsStats::allocation_granularity",
        source: "src/stats/snapshot.rs:67:5",
    },
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
    intended_definitions: &'static [IntendedIntegrationDefinition],
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
    workspace_entry_count: u64,
    executed_workspace_entries: u64,
    external_entry_count: u64,
    executed_external_entries: u64,
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
    intended_integration_definitions: Metric,
    profiles: Vec<ProfileDiagnosticsReport<'a>>,
}

#[derive(Serialize)]
struct ProfileDiagnosticsReport<'a> {
    profile: &'a str,
    llvm_instantiations: Metric,
    json_entries: Metric,
    workspace_json_entries: Metric,
    external_json_entries: Metric,
    source_definitions: Metric,
    source_location_execution_union: source_locations::SourceLocationExecutionUnion,
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

struct CoverageInputs {
    combined: CoverageExport,
    unit: CoverageExport,
    integration: CoverageExport,
    physical_lines: PhysicalLines,
}

fn read_coverage_export(path: &Path) -> Result<CoverageExport> {
    fs::read_to_string(path)
        .map_err(Into::into)
        .and_then(|contents| parse_json(&contents))
}

fn read_physical_lines(path: &Path) -> Result<PhysicalLines> {
    fs::read_to_string(path)
        .map_err(Into::into)
        .and_then(|contents| parse_lcov(&contents))
}

fn load_coverage_inputs(
    json_path: &Path,
    unit_json_path: &Path,
    integration_json_path: &Path,
    lcov_path: &Path,
) -> Result<CoverageInputs> {
    read_coverage_export(json_path)
        .and_then(|combined| read_coverage_export(unit_json_path).map(|unit| (combined, unit)))
        .and_then(|(combined, unit)| {
            read_coverage_export(integration_json_path)
                .map(|integration| (combined, unit, integration))
        })
        .and_then(|(combined, unit, integration)| {
            read_physical_lines(lcov_path).map(|physical_lines| CoverageInputs {
                combined,
                unit,
                integration,
                physical_lines,
            })
        })
}

pub(crate) fn run(
    target: &str,
    json_path: &Path,
    lcov_path: &Path,
    unit_json_path: &Path,
    integration_json_path: &Path,
    diagnostics_json_path: &Path,
) -> Result<()> {
    policy_for_target(target).and_then(|policy| {
        load_coverage_inputs(
            json_path,
            unit_json_path,
            integration_json_path,
            lcov_path,
        )
        .and_then(|inputs| {
            let data = single_data(&inputs.combined, "combined")?;
            let unit_data = single_data(&inputs.unit, "unit")?;
            let integration_data = single_data(&inputs.integration, "integration")?;

            validate(target, policy, &data.totals, inputs.physical_lines).and_then(|()| {
                let (combined_diagnostics, combined_groups) =
                    instantiation_diagnostics(data)?;
                let (unit_diagnostics, unit_groups) = instantiation_diagnostics(unit_data)?;
                let (integration_diagnostics, integration_groups) =
                    instantiation_diagnostics(integration_data)?;
                validate_source_definition_completeness(
                    target,
                    "combined",
                    &combined_diagnostics,
                )?;
                validate_source_definition_completeness(target, "unit", &unit_diagnostics)?;
                let intended_integration_definitions = validate_integration_instantiations(
                    target,
                    &unit_groups,
                    &integration_groups,
                )?;
                write_diagnostics_report(
                    diagnostics_json_path,
                    target,
                    intended_integration_definitions,
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
                    inputs.physical_lines.covered,
                    inputs.physical_lines.count,
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
                println!(
                    "intended integration definitions for {target}: {}/{}",
                    intended_integration_definitions.covered,
                    intended_integration_definitions.count
                );
                Ok(())
            })
        })
    })
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
        "{profile} instantiation structure for {target}: LLVM instantiations {}/{}, JSON entries {}/{}, workspace JSON entries {}/{}, external/compiler JSON entries {}/{}, source-definition groups {}/{}, asymmetric groups {}",
        data.totals.instantiations.covered,
        data.totals.instantiations.count,
        diagnostics.executed_entries,
        diagnostics.entry_count,
        diagnostics.executed_workspace_entries,
        diagnostics.workspace_entry_count,
        diagnostics.executed_external_entries,
        diagnostics.external_entry_count,
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

fn instantiation_diagnostics(
    data: &CoverageData,
) -> Result<(
    InstantiationDiagnostics,
    BTreeMap<DefinitionKey, DefinitionGroupState>,
)> {
    let groups = definition_groups(data)?;
    // Counts are bounded by the allocated functions vector, not by input counters.
    const { assert!(usize::BITS <= u64::BITS) };
    let entry_count = data.functions.len() as u64;
    let mut executed_entries = 0_u64;
    let mut workspace_entry_count = 0_u64;
    let mut executed_workspace_entries = 0_u64;
    let mut external_entry_count = 0_u64;
    let mut executed_external_entries = 0_u64;

    for function in &data.functions {
        let workspace_owned = function
            .filenames
            .first()
            .is_some_and(|filename| normalize_source_path(filename).is_some());
        if workspace_owned {
            workspace_entry_count += 1;
        } else {
            external_entry_count += 1;
        }
        if function.count != 0 {
            executed_entries += 1;
            if workspace_owned {
                executed_workspace_entries += 1;
            } else {
                executed_external_entries += 1;
            }
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
        .map(|(key, _)| definition_id(key))
        .collect();

    let diagnostics = InstantiationDiagnostics {
        entry_count,
        executed_entries,
        workspace_entry_count,
        executed_workspace_entries,
        external_entry_count,
        executed_external_entries,
        definition_groups: groups.len() as u64,
        covered_definition_groups: groups
            .values()
            .filter(|state| state.covered_entries != 0)
            .count() as u64,
        asymmetric_definition_groups: groups
            .values()
            .filter(|state| state.covered_entries != 0 && state.covered_entries < state.entries)
            .count() as u64,
        uncovered_definition_groups,
        file_gaps,
    };
    Ok((diagnostics, groups))
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
        // A group's counts cannot exceed the number of allocated function entries.
        const { assert!(usize::BITS <= u64::BITS) };
        group.entries += 1;
        if function.count != 0 {
            group.covered_entries += 1;
        }
        group.symbols.insert(function.name.clone());
    }
    Ok(groups)
}

fn definition_key(function: &FunctionCoverage) -> Result<Option<DefinitionKey>> {
    let Some(filename) = function.filenames.first() else {
        return Ok(None);
    };
    let Some(filename) = normalize_source_path(filename) else {
        return Ok(None);
    };
    let Some(start) = function
        .regions
        .iter()
        .find(|region| region.get(7) == Some(&0))
    else {
        return Ok(None);
    };
    // Finding element 7 above establishes the bounds for both coordinates.
    let (line, column) = (start[0], start[1]);
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
        filename,
        line,
        column,
        regions,
    }))
}

fn normalize_source_path(filename: &str) -> Option<String> {
    let normalized = filename.replace('\\', "/");
    let relative = normalized.strip_prefix("./").unwrap_or(&normalized);
    (relative.starts_with("src/") || relative.starts_with("tests/")).then(|| relative.to_owned())
}

fn definition_id(key: &DefinitionKey) -> String {
    format!("{}:{}:{}", key.filename, key.line, key.column)
}

fn validate_source_definition_completeness(
    target: &str,
    profile: &str,
    diagnostics: &InstantiationDiagnostics,
) -> Result<()> {
    if diagnostics.covered_definition_groups != diagnostics.definition_groups {
        return Err(invalid_data(format!(
            "coverage regression for {target}: {profile} source-definition groups are {}/{}",
            diagnostics.covered_definition_groups, diagnostics.definition_groups
        )));
    }
    Ok(())
}

fn validate_integration_instantiations(
    target: &str,
    unit_groups: &BTreeMap<DefinitionKey, DefinitionGroupState>,
    integration_groups: &BTreeMap<DefinitionKey, DefinitionGroupState>,
) -> Result<Metric> {
    let policy = integration_policy_for_target(target)?;
    let mut intended = validate_intended_integration_definitions(
        target,
        policy.intended_definitions,
        integration_groups,
    )?;
    if target == "x86_64-pc-windows-msvc" {
        let windows = validate_intended_integration_definitions(
            target,
            WINDOWS_INTEGRATION_DEFINITIONS,
            integration_groups,
        )?;
        intended.count += windows.count;
        intended.covered += windows.covered;
    }

    let unowned = integration_groups
        .iter()
        .filter(|(_, state)| state.covered_entries == 0)
        .filter(|(key, _)| {
            unit_groups
                .get(*key)
                .is_none_or(|state| state.covered_entries == 0)
        })
        .map(|(key, _)| definition_id(key))
        .collect::<Vec<_>>();
    if !unowned.is_empty() {
        return Err(invalid_data(format!(
            "coverage integration residuals for {target} lack unit ownership: [{}]",
            unowned.join(", ")
        )));
    }
    Ok(intended)
}

fn validate_intended_integration_definitions(
    target: &str,
    intended: &[IntendedIntegrationDefinition],
    integration_groups: &BTreeMap<DefinitionKey, DefinitionGroupState>,
) -> Result<Metric> {
    let mut api_names = BTreeSet::new();
    let mut sources = BTreeSet::new();
    let mut missing = Vec::new();
    let mut uncovered = Vec::new();

    for definition in intended {
        if !api_names.insert(definition.api) || !sources.insert(definition.source) {
            return Err(invalid_data(format!(
                "duplicate intended integration definition for {target}: {} ({})",
                definition.api, definition.source
            )));
        }
        let matches = integration_groups
            .iter()
            .filter(|(key, _)| definition_id(key) == definition.source)
            .map(|(_, state)| state)
            .collect::<Vec<_>>();

        if matches.is_empty() {
            missing.push(format!("{} ({})", definition.api, definition.source));
        } else if matches.iter().all(|state| state.covered_entries == 0) {
            uncovered.push(format!("{} ({})", definition.api, definition.source));
        }
    }

    if !missing.is_empty() || !uncovered.is_empty() {
        return Err(invalid_data(format!(
            "intended integration coverage regression for {target}; missing [{}], uncovered [{}]",
            missing.join(", "),
            uncovered.join(", ")
        )));
    }

    const { assert!(usize::BITS <= u64::BITS) };
    let count = intended.len() as u64;
    Ok(Metric {
        count,
        covered: count,
    })
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
    intended_integration_definitions: Metric,
    profiles: [ProfileEvidence<'_>; N],
) -> Result<()> {
    let profiles = profiles
        .into_iter()
        .map(|(profile, data, groups, diagnostics, unit_groups)| {
            let source_location_execution_union =
                source_locations::summarize(target, profile, groups, unit_groups);
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
                workspace_json_entries: Metric {
                    count: diagnostics.workspace_entry_count,
                    covered: diagnostics.executed_workspace_entries,
                },
                external_json_entries: Metric {
                    count: diagnostics.external_entry_count,
                    covered: diagnostics.executed_external_entries,
                },
                source_definitions: Metric {
                    count: diagnostics.definition_groups,
                    covered: diagnostics.covered_definition_groups,
                },
                source_location_execution_union,
                asymmetric_definition_groups: diagnostics.asymmetric_definition_groups,
                definitions,
                file_gaps: &diagnostics.file_gaps,
            }
        })
        .collect::<Vec<_>>();
    let report = CoverageDiagnosticsReport {
        schema_version: 4,
        target,
        intended_integration_definitions,
        profiles,
    };
    let mut encoded = serde_json::to_vec_pretty(&report)
        .expect("coverage diagnostics contain only JSON-serializable values");
    encoded.push(b'\n');
    fs::write(path, encoded)?;
    for profile in &report.profiles {
        profile
            .source_location_execution_union
            .print(target, profile.profile);
    }
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
    const { assert!(usize::BITS <= u64::BITS) };
    Ok(PhysicalLines {
        count: lines.len() as u64,
        covered: lines.values().filter(|count| **count != 0).count() as u64,
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
        "x86_64-pc-windows-msvc" | "x86_64-unknown-linux-gnu" | "aarch64-apple-darwin" => {
            IntegrationPolicy {
                intended_definitions: INTENDED_INTEGRATION_DEFINITIONS,
            }
        }
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
    fn integration_profiles_propagate_missing_intended_definition_errors() {
        assert!(
            validate_integration_instantiations(
                "unsupported-target",
                &BTreeMap::new(),
                &BTreeMap::new(),
            )
            .is_err()
        );
        for target in [
            "x86_64-unknown-linux-gnu",
            "x86_64-pc-windows-msvc",
            "aarch64-apple-darwin",
        ] {
            assert!(
                validate_integration_instantiations(target, &BTreeMap::new(), &BTreeMap::new())
                    .is_err()
            );
        }
    }

    #[test]
    fn three_profile_reports_preserve_unit_owned_integration_residuals() {
        let target = "x86_64-unknown-linux-gnu";
        let directory = tempfile::tempdir().unwrap();
        let combined = directory.path().join("combined.json");
        let unit = directory.path().join("unit.json");
        let integration = directory.path().join("integration.json");
        let lcov = directory.path().join("coverage.lcov");
        let report_path = directory.path().join("diagnostics.json");

        // Synthetic exports test report ownership, not measured library coverage.
        for (path, hits) in [(&combined, 1), (&unit, 1), (&integration, 0)] {
            let mut export = fixture_export(target);
            export["data"][0]["functions"]
                .as_array_mut()
                .unwrap()
                .push(serde_json::json!({
                    "name": "private-fixture",
                    "count": hits,
                    "filenames": ["src/private_fixture.rs"],
                    "regions": [[999, 1, 999, 2, hits, 0, 0, 0]]
                }));
            fs::write(path, export.to_string()).unwrap();
        }
        let mut lines = String::from("SF:src/synthetic_fixture.rs\n");
        for line in 1..=policy_for_target(target).unwrap().minimum_unique_lines {
            use std::fmt::Write as _;
            writeln!(lines, "DA:{line},1").unwrap();
        }
        lines.push_str("end_of_record\n");
        fs::write(&lcov, lines).unwrap();
        run(target, &combined, &lcov, &unit, &integration, &report_path).unwrap();

        let report: serde_json::Value =
            serde_json::from_slice(&fs::read(report_path).unwrap()).unwrap();
        let profiles = report["profiles"].as_array().unwrap();
        assert_eq!(profiles.len(), 3);
        for profile in profiles {
            let residual = profile["definitions"]
                .as_array()
                .unwrap()
                .iter()
                .find(|entry| entry["source"] == "src/private_fixture.rs")
                .unwrap();
            let integration = profile["profile"] == "integration";
            assert_eq!(
                residual["ownership"],
                if integration {
                    "private-unit"
                } else {
                    "covered"
                }
            );
            assert_eq!(residual["covered_entries"], u64::from(!integration));
        }
    }

    #[test]
    fn diagnostics_preserve_external_execution_and_every_ownership_state() {
        let target = "x86_64-unknown-linux-gnu";
        let mut fixture = fixture_export(target);
        fixture["data"][0]["functions"] = serde_json::json!([
            {"name":"unowned","count":0,"filenames":["src/fixture.rs"],"regions":[[3,1,4,2,0,0,0,0]]},
            {"name":"partial-zero","count":0,"filenames":["src/fixture.rs"],"regions":[[7,1,8,2,0,0,0,0]]},
            {"name":"partial-executed","count":1,"filenames":["src/fixture.rs"],"regions":[[7,1,8,2,1,0,0,0]]},
            {"name":"complete","count":1,"filenames":["src/fixture.rs"],"regions":[[11,1,12,2,1,0,0,0]]},
            {"name":"external","count":1,"filenames":["/rustc/toolchain/core.rs"],"regions":[[99,1,100,2,1,0,0,0]]},
            {"name":"no-file","count":0,"filenames":[],"regions":[]}
        ]);
        let export = parse_json(&fixture.to_string()).unwrap();
        let data = &export.data[0];
        let groups = definition_groups(data).unwrap();
        let diagnostics = instantiation_diagnostics(data).unwrap().0;
        assert_eq!(
            (diagnostics.entry_count, diagnostics.executed_entries),
            (6, 3)
        );
        assert_eq!(
            (
                diagnostics.workspace_entry_count,
                diagnostics.executed_workspace_entries
            ),
            (4, 2)
        );
        assert_eq!(
            (
                diagnostics.external_entry_count,
                diagnostics.executed_external_entries
            ),
            (2, 1)
        );
        assert_eq!(diagnostics.asymmetric_definition_groups, 1);
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("ownership.json");
        write_diagnostics_report(
            &path,
            target,
            Metric {
                count: 0,
                covered: 0,
            },
            [("integration", data, &groups, &diagnostics, None)],
        )
        .unwrap();
        let report: serde_json::Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        let ownership = report["profiles"][0]["definitions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["ownership"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(ownership, ["unowned", "compiler-asymmetric", "covered"]);
    }

    #[test]
    fn an_unexecuted_unit_group_does_not_own_an_integration_residual() {
        let target = "x86_64-unknown-linux-gnu";
        let export = parse_json(&fixture_export(target).to_string()).unwrap();
        let mut integration = definition_groups(&export.data[0]).unwrap();
        let key = DefinitionKey {
            filename: "src/residual.rs".to_owned(),
            line: 999,
            column: 1,
            regions: Vec::new(),
        };
        integration.insert(
            key.clone(),
            DefinitionGroupState {
                entries: 1,
                covered_entries: 0,
                symbols: BTreeSet::new(),
            },
        );
        let mut units = definition_groups(&export.data[0]).unwrap();
        units.insert(
            key.clone(),
            DefinitionGroupState {
                entries: 1,
                covered_entries: 0,
                symbols: BTreeSet::new(),
            },
        );
        assert!(validate_integration_instantiations(target, &units, &integration).is_err());
        units.get_mut(&key).unwrap().covered_entries = 1;
        validate_integration_instantiations(target, &units, &integration).unwrap();
    }

    // Synthetic contract fixtures are not coverage measurements.
    fn fixture_export(target: &str) -> serde_json::Value {
        let policy = policy_for_target(target).unwrap();
        let complete = |count| serde_json::json!({ "count": count, "covered": count });
        let functions = INTENDED_INTEGRATION_DEFINITIONS
            .iter()
            .chain(
                WINDOWS_INTEGRATION_DEFINITIONS
                    .iter()
                    .filter(|_| target == "x86_64-pc-windows-msvc"),
            )
            .map(|definition| {
                let (source, column) = definition.source.rsplit_once(':').unwrap();
                let (filename, line) = source.rsplit_once(':').unwrap();
                let line: u64 = line.parse().unwrap();
                let column: u64 = column.parse().unwrap();
                serde_json::json!({
                    "name": definition.api,
                    "count": 1,
                    "filenames": [filename],
                    "regions": [[line, column, line, column + 1, 1, 0, 0, 0]]
                })
            })
            .collect::<Vec<_>>();
        let instantiations = u64::try_from(functions.len()).unwrap();
        serde_json::json!({
            "type": LLVM_COVERAGE_EXPORT,
            "data": [{
                "files": [],
                "functions": functions,
                "totals": {
                    "functions": complete(policy.minimum_functions),
                    "instantiations": complete(instantiations),
                    "lines": complete(policy.minimum_aggregate_lines),
                    "regions": complete(policy.minimum_regions)
                }
            }]
        })
    }

    #[test]
    fn json_requires_the_export_schema_and_one_data_set() {
        let valid = fixture_export("x86_64-unknown-linux-gnu");
        let parsed = parse_json(&valid.to_string()).unwrap();
        single_data(&parsed, "combined").unwrap();
        let mut wrong_kind = valid.clone();
        wrong_kind["type"] = serde_json::json!("unreviewed.export");
        assert!(parse_json(&wrong_kind.to_string()).is_err());
        for invalid in ["{", "null", r#"{"type":"llvm.coverage.json.export"}"#] {
            assert!(parse_json(invalid).is_err(), "{invalid}");
        }
        for count in [0, 2] {
            let mut value = valid.clone();
            value["data"] = serde_json::Value::Array(vec![valid["data"][0].clone(); count]);
            let export = parse_json(&value.to_string()).unwrap();
            let error = single_data(&export, "unit").unwrap_err().to_string();
            assert!(error.contains("unit coverage JSON must contain exactly one data set"));
        }
    }

    #[test]
    fn malformed_lcov_is_rejected_with_record_context() {
        for (contents, expected) in [
            ("", "no physical source lines"),
            ("SF:\n", "source path is empty on line 1"),
            ("DA:1,1\n", "precedes a source record on line 1"),
            (
                "SF:src/lib.rs\nend_of_record\nDA:1,1\n",
                "precedes a source record on line 3",
            ),
            (
                "SF:src/lib.rs\nDA:1\n",
                "execution count is missing on line 2",
            ),
            ("SF:src/lib.rs\nDA:,1\n", "line number is invalid on line 2"),
            (
                "SF:src/lib.rs\nDA:1,no\n",
                "execution count is invalid on line 2",
            ),
            (
                "SF:src/lib.rs\nDA:18446744073709551616,1\n",
                "line number is invalid",
            ),
        ] {
            let error = parse_lcov(contents).unwrap_err().to_string();
            assert!(error.contains(expected), "{error}");
        }
        assert!(parse_lcov_number(None, 0, "line number").is_err());
        assert_eq!(
            parse_lcov("SF:src/lib.rs\r\nDA:1,0\r\nDA:1,5\r\nDA:1,2\r\n").unwrap(),
            PhysicalLines {
                count: 1,
                covered: 1
            }
        );
    }

    #[test]
    fn every_metric_floor_and_completeness_check_is_enforced() {
        let target = "x86_64-unknown-linux-gnu";
        let policy = policy_for_target(target).unwrap();
        let fixture = fixture_export(target);
        let total_value = &fixture["data"][0]["totals"];
        let physical = PhysicalLines {
            count: policy.minimum_unique_lines,
            covered: policy.minimum_unique_lines,
        };
        let totals: Totals = serde_json::from_value(total_value.clone()).unwrap();
        validate(target, policy, &totals, physical).unwrap();
        for invalid in [
            PhysicalLines {
                count: 0,
                covered: 0,
            },
            PhysicalLines {
                covered: physical.covered - 1,
                ..physical
            },
            PhysicalLines {
                covered: physical.covered + 1,
                ..physical
            },
        ] {
            assert!(validate(target, policy, &totals, invalid).is_err());
        }
        for (pointer, replacement) in [
            ("/lines/count", 0),
            ("/lines/covered", 0),
            ("/lines/covered", u64::MAX),
            ("/regions/count", 0),
            ("/regions/covered", 0),
            ("/regions/covered", u64::MAX),
            ("/functions/count", 0),
            ("/functions/covered", 0),
            ("/functions/covered", u64::MAX),
            ("/instantiations/covered", u64::MAX),
        ] {
            let mut altered = total_value.clone();
            *altered.pointer_mut(pointer).unwrap() = serde_json::json!(replacement);
            let totals = serde_json::from_value(altered).unwrap();
            assert!(
                validate(target, policy, &totals, physical).is_err(),
                "{pointer}"
            );
        }
        assert!(policy_for_target("unknown-target").is_err());
        assert!(integration_policy_for_target("unknown-target").is_err());
    }

    #[test]
    fn definition_keys_reject_malformed_regions_without_claiming_external_sources() {
        let fixture = fixture_export("x86_64-unknown-linux-gnu");
        let original = fixture["data"][0]["functions"][0].clone();
        for (field, value) in [
            ("filenames", serde_json::json!([])),
            ("filenames", serde_json::json!(["/rustc/library/core.rs"])),
            ("regions", serde_json::json!([])),
            ("regions", serde_json::json!([[1, 1, 2, 1, 0, 0, 0, 1]])),
        ] {
            let mut function = original.clone();
            function[field] = value;
            let function = serde_json::from_value(function).unwrap();
            assert!(definition_key(&function).unwrap().is_none());
        }
        let mut function: FunctionCoverage = serde_json::from_value(original).unwrap();
        function.regions.push(vec![1]);
        assert!(definition_key(&function).is_err());
        assert_eq!(
            normalize_source_path("./tests/api.rs"),
            Some("tests/api.rs".to_owned())
        );
    }

    #[test]
    fn intended_definitions_must_have_unique_names_and_locations() {
        let first = IntendedIntegrationDefinition {
            api: "first",
            source: "src/lib.rs:1:1",
        };
        for duplicate in [
            IntendedIntegrationDefinition {
                api: "first",
                source: "src/lib.rs:2:1",
            },
            IntendedIntegrationDefinition {
                api: "second",
                source: "src/lib.rs:1:1",
            },
        ] {
            let error = validate_intended_integration_definitions(
                "test-target",
                &[first, duplicate],
                &BTreeMap::new(),
            )
            .unwrap_err()
            .to_string();
            assert!(error.contains("duplicate intended integration definition"));
        }
    }

    #[test]
    fn integration_residuals_require_executed_unit_ownership() {
        let target = "x86_64-unknown-linux-gnu";
        let value = fixture_export(target);
        let export = parse_json(&value.to_string()).unwrap();
        let mut units = definition_groups(single_data(&export, "unit").unwrap()).unwrap();
        let mut integrations =
            definition_groups(single_data(&export, "integration").unwrap()).unwrap();
        let key = DefinitionKey {
            filename: "src/private_fixture.rs".to_owned(),
            line: 1,
            column: 1,
            regions: vec![(1, 1, 1, 2, 0)],
        };
        integrations.insert(
            key.clone(),
            DefinitionGroupState {
                entries: 1,
                covered_entries: 0,
                symbols: BTreeSet::from(["private_fixture".to_owned()]),
            },
        );
        assert!(validate_integration_instantiations(target, &units, &integrations).is_err());
        units.insert(
            key.clone(),
            DefinitionGroupState {
                entries: 1,
                covered_entries: 0,
                symbols: BTreeSet::from(["private_fixture".to_owned()]),
            },
        );
        assert!(validate_integration_instantiations(target, &units, &integrations).is_err());
        units.get_mut(&key).unwrap().covered_entries = 1;
        validate_integration_instantiations(target, &units, &integrations).unwrap();
    }

    #[test]
    fn diagnostics_sort_file_gaps_and_reject_impossible_totals() {
        let mut fixture = fixture_export("x86_64-unknown-linux-gnu");
        fixture["data"][0]["files"] = serde_json::json!([
            {"filename":"src/z.rs","summary":{"instantiations":{"count":3,"covered":2}}},
            {"filename":"src/b.rs","summary":{"instantiations":{"count":3,"covered":1}}},
            {"filename":"src/a.rs","summary":{"instantiations":{"count":3,"covered":1}}}
        ]);
        fixture["data"][0]["functions"][0]["count"] = serde_json::json!(0);
        let mut export = parse_json(&fixture.to_string()).unwrap();
        let data = &mut export.data[0];
        let diagnostics = instantiation_diagnostics(data).unwrap().0;
        assert_eq!(
            diagnostics
                .file_gaps
                .iter()
                .map(|gap| gap.filename.as_str())
                .collect::<Vec<_>>(),
            ["src/a.rs", "src/b.rs", "src/z.rs"]
        );
        assert_eq!(diagnostics.uncovered_definition_groups.len(), 1);
        print_instantiation_diagnostics("fixture", "unit", data, &diagnostics);
        assert!(validate_source_definition_completeness("fixture", "unit", &diagnostics).is_err());
        data.files[0].summary.instantiations.covered = 4;
        assert!(instantiation_diagnostics(data).is_err());
    }

    #[test]
    fn native_coverage_command_writes_all_profiles_and_propagates_output_errors() {
        for target in [
            "x86_64-unknown-linux-gnu",
            "x86_64-pc-windows-msvc",
            "aarch64-apple-darwin",
        ] {
            let directory = tempfile::tempdir().unwrap();
            let json = directory.path().join("combined.json");
            let unit = directory.path().join("unit.json");
            let integration = directory.path().join("integration.json");
            let lcov = directory.path().join("coverage.lcov");
            let diagnostics = directory.path().join("diagnostics.json");
            let encoded = fixture_export(target).to_string();
            for path in [&json, &unit, &integration] {
                fs::write(path, &encoded).unwrap();
            }
            let policy = policy_for_target(target).unwrap();
            let mut lines = String::from("SF:src/synthetic_fixture.rs\n");
            for line in 1..=policy.minimum_unique_lines {
                use std::fmt::Write as _;
                writeln!(lines, "DA:{line},1").unwrap();
            }
            lines.push_str("end_of_record\n");
            fs::write(&lcov, lines).unwrap();
            run(target, &json, &lcov, &unit, &integration, &diagnostics).unwrap();
            let report: serde_json::Value =
                serde_json::from_slice(&fs::read(&diagnostics).unwrap()).unwrap();
            assert_eq!(report["schema_version"], 4);
            assert_eq!(report["target"], target);
            assert_eq!(report["profiles"].as_array().unwrap().len(), 3);
            for (profile, expected) in report["profiles"].as_array().unwrap().iter().zip([
                "combined",
                "unit",
                "integration",
            ]) {
                assert_eq!(profile["profile"], expected);
                assert_eq!(
                    profile["source_definitions"]["count"],
                    profile["source_definitions"]["covered"]
                );
            }
            assert!(run(target, &json, &lcov, &unit, &integration, directory.path()).is_err());
            fs::write(&unit, r#"{"type":"llvm.coverage.json.export","data":[]}"#).unwrap();
            let error = run(target, &json, &lcov, &unit, &integration, &diagnostics)
                .unwrap_err()
                .to_string();
            assert!(error.contains("unit coverage JSON must contain exactly one data set"));
        }
    }

    #[test]
    fn rejected_coverage_inputs_preserve_the_previous_diagnostic_report() {
        let target = "x86_64-unknown-linux-gnu";
        let directory = tempfile::tempdir().unwrap();
        let combined = directory.path().join("combined.json");
        let unit = directory.path().join("unit.json");
        let integration = directory.path().join("integration.json");
        let lcov = directory.path().join("coverage.lcov");
        let diagnostics = directory.path().join("diagnostics.json");
        let fixture = fixture_export(target);
        let encoded = fixture.to_string();
        let policy = policy_for_target(target).unwrap();
        let mut lines = String::from("SF:src/synthetic_fixture.rs\n");
        for line in 1..=policy.minimum_unique_lines {
            use std::fmt::Write as _;
            writeln!(lines, "DA:{line},1").unwrap();
        }
        lines.push_str("end_of_record\n");
        for path in [&combined, &unit, &integration] {
            fs::write(path, &encoded).unwrap();
        }
        fs::write(&lcov, &lines).unwrap();
        run(target, &combined, &lcov, &unit, &integration, &diagnostics).unwrap();
        let sentinel = b"previous diagnostic report";
        fs::write(&diagnostics, sentinel).unwrap();
        let reject = || {
            let error =
                run(target, &combined, &lcov, &unit, &integration, &diagnostics).unwrap_err();
            assert_eq!(fs::read(&diagnostics).unwrap(), sentinel);
            error
        };

        for path in [&combined, &unit, &integration, &lcov] {
            fs::remove_file(path).unwrap();
            assert_eq!(
                reject().downcast_ref::<std::io::Error>().unwrap().kind(),
                std::io::ErrorKind::NotFound
            );
            if path == &lcov {
                fs::write(
                    path,
                    "SF:src/synthetic_fixture.rs\nDA:1,bad\nend_of_record\n",
                )
                .unwrap();
                assert!(reject().to_string().contains("execution count is invalid"));
                fs::write(path, &lines).unwrap();
            } else {
                fs::write(path, "not JSON").unwrap();
                reject();
                fs::write(path, &encoded).unwrap();
            }
        }

        for (profile, path) in [
            ("combined", &combined),
            ("unit", &unit),
            ("integration", &integration),
        ] {
            for data in [
                serde_json::json!([]),
                serde_json::json!([fixture["data"][0].clone(), fixture["data"][0].clone()]),
            ] {
                let mut altered = fixture.clone();
                altered["data"] = data;
                fs::write(path, altered.to_string()).unwrap();
                assert!(reject().to_string().contains(&format!(
                    "{profile} coverage JSON must contain exactly one data set"
                )));
            }

            let mut altered = fixture.clone();
            altered["data"][0]["functions"][0]["regions"]
                .as_array_mut()
                .unwrap()
                .push(serde_json::json!([0]));
            fs::write(path, altered.to_string()).unwrap();
            assert!(reject().to_string().contains("malformed region"));

            let mut altered = fixture.clone();
            altered["data"][0]["files"] = serde_json::json!([{
                "filename": "src/synthetic_fixture.rs",
                "summary": {"instantiations": {"count": 0, "covered": 1}}
            }]);
            fs::write(path, altered.to_string()).unwrap();
            assert!(reject().to_string().contains("file instantiations"));

            let mut altered = fixture.clone();
            altered["data"][0]["functions"][0]["count"] = serde_json::json!(0);
            altered["data"][0]["functions"][0]["regions"][0][4] = serde_json::json!(0);
            fs::write(path, altered.to_string()).unwrap();
            reject();
            fs::write(path, &encoded).unwrap();
        }

        let mut altered = fixture;
        altered["data"][0]["totals"]["lines"]["covered"] = serde_json::json!(0);
        fs::write(&combined, altered.to_string()).unwrap();
        reject();
    }

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
    fn source_definition_groups_exclude_toolchain_sources() {
        assert_eq!(
            normalize_source_path(r"src\lib.rs"),
            Some("src/lib.rs".to_owned())
        );
        assert_eq!(
            normalize_source_path(
                r"\rustc\6b00bc3880198600130e1cf62b8f8a93494488cc\library\core\src\panic.rs"
            ),
            None
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
    fn distinguishes_raw_workspace_and_external_entries_from_source_definition_groups() {
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
                FunctionCoverage {
                    name: "external-uncovered-instance".to_owned(),
                    count: 0,
                    filenames: vec![r"\rustc\toolchain\library\core\src\panic.rs".to_owned()],
                    regions: vec![vec![99, 1, 100, 2, 0, 0, 0, 0]],
                },
            ],
            totals: Totals {
                functions: complete,
                instantiations: Metric {
                    count: 4,
                    covered: 1,
                },
                lines: complete,
                regions: complete,
            },
        };

        assert_eq!(
            instantiation_diagnostics(&data).unwrap().0,
            InstantiationDiagnostics {
                entry_count: 4,
                executed_entries: 1,
                workspace_entry_count: 3,
                executed_workspace_entries: 1,
                external_entry_count: 1,
                executed_external_entries: 0,
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

    #[test]
    fn windows_integration_contract_requires_each_native_definition() {
        let make_groups = || {
            INTENDED_INTEGRATION_DEFINITIONS
                .iter()
                .chain(WINDOWS_INTEGRATION_DEFINITIONS)
                .map(|definition| {
                    let (source, column) = definition.source.rsplit_once(':').unwrap();
                    let (filename, line) = source.rsplit_once(':').unwrap();
                    (
                        DefinitionKey {
                            filename: filename.to_owned(),
                            line: line.parse().unwrap(),
                            column: column.parse().unwrap(),
                            regions: Vec::new(),
                        },
                        DefinitionGroupState {
                            entries: 2,
                            covered_entries: 1,
                            symbols: BTreeSet::new(),
                        },
                    )
                })
                .collect::<BTreeMap<_, _>>()
        };
        let unit_groups = make_groups();
        let mut integration_groups = make_groups();
        let target = "x86_64-pc-windows-msvc";
        let metric =
            validate_integration_instantiations(target, &unit_groups, &integration_groups).unwrap();
        assert_eq!(metric.count, 36);
        assert_eq!(metric.covered, 36);

        for definition in WINDOWS_INTEGRATION_DEFINITIONS {
            let key = integration_groups
                .keys()
                .find(|key| definition_id(key) == definition.source)
                .unwrap()
                .clone();
            let mut state = integration_groups.remove(&key).unwrap();
            let error =
                validate_integration_instantiations(target, &unit_groups, &integration_groups)
                    .unwrap_err()
                    .to_string();
            assert!(error.contains(&format!(
                "missing [{} ({})]",
                definition.api, definition.source
            )));

            state.covered_entries = 0;
            integration_groups.insert(key.clone(), state);
            let error =
                validate_integration_instantiations(target, &unit_groups, &integration_groups)
                    .unwrap_err()
                    .to_string();
            assert!(error.contains(&format!(
                "uncovered [{} ({})]",
                definition.api, definition.source
            )));
            integration_groups.get_mut(&key).unwrap().covered_entries = 1;
        }

        integration_groups.retain(|key, _| !key.filename.starts_with("src/windows/"));
        let metric = validate_integration_instantiations(
            "x86_64-unknown-linux-gnu",
            &unit_groups,
            &integration_groups,
        )
        .unwrap();
        assert_eq!(metric.count, 30);
        assert_eq!(metric.covered, 30);
        assert!(
            validate_integration_instantiations(target, &unit_groups, &integration_groups).is_err()
        );
    }

    #[test]
    fn intended_integration_definitions_ignore_compiler_instantiation_asymmetry() {
        let key = DefinitionKey {
            filename: "src/lib.rs".to_owned(),
            line: 7,
            column: 1,
            regions: vec![(7, 1, 9, 2, 0)],
        };
        let alternate_region_shape = DefinitionKey {
            filename: "src/lib.rs".to_owned(),
            line: 7,
            column: 1,
            regions: vec![(7, 1, 10, 2, 0)],
        };
        let groups = BTreeMap::from([
            (
                key,
                DefinitionGroupState {
                    entries: 3,
                    covered_entries: 1,
                    symbols: BTreeSet::new(),
                },
            ),
            (
                alternate_region_shape,
                DefinitionGroupState {
                    entries: 1,
                    covered_entries: 0,
                    symbols: BTreeSet::new(),
                },
            ),
        ]);
        let intended = [IntendedIntegrationDefinition {
            api: "public_api",
            source: "src/lib.rs:7:1",
        }];

        assert_eq!(
            validate_intended_integration_definitions("test-target", &intended, &groups)
                .unwrap()
                .covered,
            1
        );
    }

    #[test]
    fn intended_integration_definitions_reject_missing_or_uncovered_entries() {
        let key = DefinitionKey {
            filename: "src/lib.rs".to_owned(),
            line: 7,
            column: 1,
            regions: vec![(7, 1, 9, 2, 0)],
        };
        let groups = BTreeMap::from([(
            key,
            DefinitionGroupState {
                entries: 1,
                covered_entries: 0,
                symbols: BTreeSet::new(),
            },
        )]);
        let uncovered = [IntendedIntegrationDefinition {
            api: "public_api",
            source: "src/lib.rs:7:1",
        }];
        let missing = [IntendedIntegrationDefinition {
            api: "other_public_api",
            source: "src/lib.rs:11:1",
        }];

        assert!(
            validate_intended_integration_definitions("test-target", &uncovered, &groups).is_err()
        );
        assert!(
            validate_intended_integration_definitions("test-target", &missing, &groups).is_err()
        );
    }
}
