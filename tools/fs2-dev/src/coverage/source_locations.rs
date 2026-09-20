use std::collections::BTreeMap;

use serde::Serialize;

use super::{DefinitionGroupState, DefinitionKey, Metric};

const REVIEW_BASELINE: &str = "e59918183b2789ba6986fbfe9df5c607150cb9a8";
const WINDOWS: &str = "x86_64-pc-windows-msvc";
const LINUX: &str = "x86_64-unknown-linux-gnu";
const MACOS: &str = "aarch64-apple-darwin";

#[derive(Serialize)]
pub(super) struct SourceLocationExecutionUnion {
    informational_only: bool,
    locations: Metric,
    multi_topology_locations: usize,
    uncovered_groups_with_executed_location: usize,
    gap_review_baseline: &'static str,
    records: Vec<SourceLocationRecord>,
}

#[derive(Serialize)]
struct SourceLocationRecord {
    source: String,
    line: u64,
    column: u64,
    executed: bool,
    // Zero-based indices into this profile's unchanged `definitions` array.
    definition_indexes: Vec<usize>,
    uncovered_definition_indexes: Vec<usize>,
    all_uncovered_topologies_unit_owned: Option<bool>,
    reviewed_integration_gap: Option<GapReview>,
}

#[derive(Serialize)]
struct GapReview {
    category: &'static str,
    symbol_fragment: &'static str,
}

type LocationMember<'a> = (usize, &'a DefinitionKey, &'a DefinitionGroupState);

pub(super) fn summarize(
    target: &str,
    profile: &str,
    groups: &BTreeMap<DefinitionKey, DefinitionGroupState>,
    unit_groups: Option<&BTreeMap<DefinitionKey, DefinitionGroupState>>,
) -> SourceLocationExecutionUnion {
    const { assert!(usize::BITS <= u64::BITS) };
    let mut locations = BTreeMap::<(&str, u64, u64), Vec<LocationMember<'_>>>::new();
    for (index, (key, state)) in groups.iter().enumerate() {
        locations
            .entry((&key.filename, key.line, key.column))
            .or_default()
            .push((index, key, state));
    }

    let records = locations
        .into_iter()
        .map(|((source, line, column), members)| {
            let executed = members
                .iter()
                .any(|(_, _, state)| state.covered_entries != 0);
            let uncovered_definition_indexes = members
                .iter()
                .filter(|(_, _, state)| state.covered_entries == 0)
                .map(|(index, _, _)| *index)
                .collect::<Vec<_>>();
            let all_uncovered_topologies_unit_owned = if uncovered_definition_indexes.is_empty() {
                None
            } else {
                unit_groups.map(|units| {
                    members
                        .iter()
                        .filter(|(_, _, state)| state.covered_entries == 0)
                        .all(|(_, key, _)| {
                            units
                                .get(*key)
                                .is_some_and(|state| state.covered_entries != 0)
                        })
                })
            };
            let reviewed_integration_gap = if profile == "integration"
                && !executed
                && all_uncovered_topologies_unit_owned == Some(true)
            {
                reviewed_gap(target, source, line, column).filter(|review| {
                    members.iter().all(|(_, _, state)| {
                        !state.symbols.is_empty()
                            && state
                                .symbols
                                .iter()
                                .all(|symbol| symbol.contains(review.symbol_fragment))
                    })
                })
            } else {
                None
            };
            SourceLocationRecord {
                source: source.to_owned(),
                line,
                column,
                executed,
                definition_indexes: members.iter().map(|(index, _, _)| *index).collect(),
                uncovered_definition_indexes,
                all_uncovered_topologies_unit_owned,
                reviewed_integration_gap,
            }
        })
        .collect::<Vec<_>>();

    SourceLocationExecutionUnion {
        informational_only: true,
        locations: Metric {
            count: records.len() as u64,
            covered: records.iter().filter(|record| record.executed).count() as u64,
        },
        multi_topology_locations: records
            .iter()
            .filter(|record| record.definition_indexes.len() > 1)
            .count(),
        uncovered_groups_with_executed_location: records
            .iter()
            .filter(|record| record.executed)
            .map(|record| record.uncovered_definition_indexes.len())
            .sum(),
        gap_review_baseline: REVIEW_BASELINE,
        records,
    }
}

impl SourceLocationExecutionUnion {
    pub(super) fn print(&self, target: &str, profile: &str) {
        println!(
            "{profile} source-location execution union for {target}: {}/{}, multi-topology locations {}, unexecuted topology groups at executed locations {} (informational only; original metrics and gates unchanged)",
            self.locations.covered,
            self.locations.count,
            self.multi_topology_locations,
            self.uncovered_groups_with_executed_location,
        );
        if profile != "integration" {
            return;
        }
        println!(
            "integration gap annotations were reviewed at {}",
            self.gap_review_baseline
        );
        for record in self.records.iter().filter(|record| !record.executed) {
            let category = record
                .reviewed_integration_gap
                .as_ref()
                .map_or("unclassified", |review| review.category);
            println!(
                "integration source-location gap for {target}: {}:{}:{}; review={category}; all uncovered topology groups unit-owned={:?}",
                record.source,
                record.line,
                record.column,
                record.all_uncovered_topologies_unit_owned,
            );
        }
    }
}

// Historical annotations, not exclusions or proofs about a later revision.
// Coordinates plus symbol fragments avoid inheriting an annotation on ordinary
// line/name drift. Body or platform-policy changes still require human review.
fn reviewed_gap(target: &str, source: &str, line: u64, column: u64) -> Option<GapReview> {
    let (category, symbol_fragment) = match (target, source, line, column) {
        (WINDOWS | LINUX | MACOS, "src/stats.rs", 20, 1) => {
            ("defensive-boundary", "13invalid_stats")
        }
        (LINUX | MACOS, "src/stats/validation.rs", 65, 24) => {
            ("defensive-boundary", "22validate_unix_counters")
        }
        (LINUX | MACOS, "src/unix/allocation.rs", 90, 1) => {
            ("defensive-boundary", "23allocated_size_overflow")
        }
        (LINUX, "src/unix/allocation.rs", 85, 22) => ("defensive-boundary", "10i64_to_u64"),
        (LINUX, "src/unix/stats.rs", 172, 34) => {
            ("defensive-boundary", "23signed_filesystem_value")
        }
        (LINUX, "src/allocation.rs", 73, 1) | (LINUX, "src/allocation.rs", 77, 40) => (
            "backend-inapplicable",
            "33extend_file_length_after_snapshot",
        ),
        (LINUX, "src/allocation.rs", 83, 1) => (
            "backend-inapplicable",
            "38extend_file_length_after_snapshot_with",
        ),
        (WINDOWS, "src/windows/allocation.rs", 58, 67)
        | (WINDOWS, "src/windows/allocation.rs", 64, 57) => {
            ("defensive-boundary", "28allocation_state_from_values")
        }
        (WINDOWS, "src/windows/allocation.rs", 378, 22) => {
            ("defensive-boundary", "28requested_range_is_allocated")
        }
        (WINDOWS, "src/windows/stats/legacy.rs", 128, 1) => {
            ("defensive-boundary", "23byte_space_domain_error")
        }
        (WINDOWS, "src/windows/stats/modern.rs", 143, 1) => {
            ("defensive-boundary", "20stats_overflow_error")
        }
        (WINDOWS, "src/windows/allocation.rs", 332, 55) => {
            ("pending-io-candidate", "28overlapped_device_io_control")
        }
        (WINDOWS, "src/windows/allocation.rs", 360, 1) => {
            ("pending-io-candidate", "23wait_for_device_control")
        }
        (WINDOWS, "src/windows/overlapped.rs", 43, 5) => {
            ("pending-io-candidate", "17PrivateOverlapped5state")
        }
        (WINDOWS, "src/stats/counters.rs", 70, 5) => {
            ("legacy-provider-candidate", "20windows_legacy_bytes")
        }
        (WINDOWS, "src/windows/stats/legacy.rs", 8, 1) => {
            ("legacy-provider-candidate", "14legacy_statvfs")
        }
        (WINDOWS, "src/windows/stats/legacy.rs", 12, 1) => (
            "legacy-provider-candidate",
            "29legacy_statvfs_after_geometry",
        ),
        (WINDOWS, "src/windows/stats/legacy.rs", 33, 1)
        | (WINDOWS, "src/windows/stats/legacy.rs", 36, 12)
        | (WINDOWS, "src/windows/stats/legacy.rs", 37, 12) => {
            ("legacy-provider-candidate", "12legacy_space")
        }
        (WINDOWS, "src/windows/stats/legacy.rs", 41, 1) => {
            ("legacy-provider-candidate", "17legacy_space_with")
        }
        (WINDOWS, "src/windows/stats/legacy.rs", 54, 1) => {
            ("legacy-provider-candidate", "16cluster_geometry")
        }
        (WINDOWS, "src/windows/stats/legacy.rs", 73, 1) => {
            ("legacy-provider-candidate", "23cluster_geometry_result")
        }
        (WINDOWS, "src/windows/stats/legacy.rs", 87, 1) => {
            ("legacy-provider-candidate", "10byte_space")
        }
        (WINDOWS, "src/windows/stats/legacy.rs", 109, 1) => {
            ("legacy-provider-candidate", "17byte_space_result")
        }
        _ => return None,
    };
    Some(GapReview {
        category,
        symbol_fragment,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::super::{
        CoverageData, FunctionCoverage, Totals, definition_groups, instantiation_diagnostics,
        validate_source_definition_completeness, write_diagnostics_report,
    };
    use super::*;

    fn key(source: &str, line: u64, column: u64, end_line: u64) -> DefinitionKey {
        DefinitionKey {
            filename: source.to_owned(),
            line,
            column,
            regions: vec![(line, column, end_line, column + 1, 0)],
        }
    }

    fn state(symbol: &str, executed: bool) -> DefinitionGroupState {
        DefinitionGroupState {
            entries: 1,
            covered_entries: u64::from(executed),
            symbols: BTreeSet::from([symbol.to_owned()]),
        }
    }

    #[test]
    fn every_reviewed_location_retains_its_target_symbol_and_category() {
        let cases = [
            (
                WINDOWS,
                "src/stats.rs",
                20,
                1,
                "13invalid_stats",
                "defensive-boundary",
            ),
            (
                LINUX,
                "src/stats/validation.rs",
                65,
                24,
                "22validate_unix_counters",
                "defensive-boundary",
            ),
            (
                MACOS,
                "src/unix/allocation.rs",
                90,
                1,
                "23allocated_size_overflow",
                "defensive-boundary",
            ),
            (
                LINUX,
                "src/unix/allocation.rs",
                85,
                22,
                "10i64_to_u64",
                "defensive-boundary",
            ),
            (
                LINUX,
                "src/unix/stats.rs",
                172,
                34,
                "23signed_filesystem_value",
                "defensive-boundary",
            ),
            (
                LINUX,
                "src/allocation.rs",
                73,
                1,
                "33extend_file_length_after_snapshot",
                "backend-inapplicable",
            ),
            (
                LINUX,
                "src/allocation.rs",
                77,
                40,
                "33extend_file_length_after_snapshot",
                "backend-inapplicable",
            ),
            (
                LINUX,
                "src/allocation.rs",
                83,
                1,
                "38extend_file_length_after_snapshot_with",
                "backend-inapplicable",
            ),
            (
                WINDOWS,
                "src/windows/allocation.rs",
                58,
                67,
                "28allocation_state_from_values",
                "defensive-boundary",
            ),
            (
                WINDOWS,
                "src/windows/allocation.rs",
                64,
                57,
                "28allocation_state_from_values",
                "defensive-boundary",
            ),
            (
                WINDOWS,
                "src/windows/allocation.rs",
                378,
                22,
                "28requested_range_is_allocated",
                "defensive-boundary",
            ),
            (
                WINDOWS,
                "src/windows/stats/legacy.rs",
                128,
                1,
                "23byte_space_domain_error",
                "defensive-boundary",
            ),
            (
                WINDOWS,
                "src/windows/stats/modern.rs",
                143,
                1,
                "20stats_overflow_error",
                "defensive-boundary",
            ),
            (
                WINDOWS,
                "src/windows/allocation.rs",
                332,
                55,
                "28overlapped_device_io_control",
                "pending-io-candidate",
            ),
            (
                WINDOWS,
                "src/windows/allocation.rs",
                360,
                1,
                "23wait_for_device_control",
                "pending-io-candidate",
            ),
            (
                WINDOWS,
                "src/windows/overlapped.rs",
                43,
                5,
                "17PrivateOverlapped5state",
                "pending-io-candidate",
            ),
            (
                WINDOWS,
                "src/stats/counters.rs",
                70,
                5,
                "20windows_legacy_bytes",
                "legacy-provider-candidate",
            ),
            (
                WINDOWS,
                "src/windows/stats/legacy.rs",
                8,
                1,
                "14legacy_statvfs",
                "legacy-provider-candidate",
            ),
            (
                WINDOWS,
                "src/windows/stats/legacy.rs",
                12,
                1,
                "29legacy_statvfs_after_geometry",
                "legacy-provider-candidate",
            ),
            (
                WINDOWS,
                "src/windows/stats/legacy.rs",
                33,
                1,
                "12legacy_space",
                "legacy-provider-candidate",
            ),
            (
                WINDOWS,
                "src/windows/stats/legacy.rs",
                36,
                12,
                "12legacy_space",
                "legacy-provider-candidate",
            ),
            (
                WINDOWS,
                "src/windows/stats/legacy.rs",
                37,
                12,
                "12legacy_space",
                "legacy-provider-candidate",
            ),
            (
                WINDOWS,
                "src/windows/stats/legacy.rs",
                41,
                1,
                "17legacy_space_with",
                "legacy-provider-candidate",
            ),
            (
                WINDOWS,
                "src/windows/stats/legacy.rs",
                54,
                1,
                "16cluster_geometry",
                "legacy-provider-candidate",
            ),
            (
                WINDOWS,
                "src/windows/stats/legacy.rs",
                73,
                1,
                "23cluster_geometry_result",
                "legacy-provider-candidate",
            ),
            (
                WINDOWS,
                "src/windows/stats/legacy.rs",
                87,
                1,
                "10byte_space",
                "legacy-provider-candidate",
            ),
            (
                WINDOWS,
                "src/windows/stats/legacy.rs",
                109,
                1,
                "17byte_space_result",
                "legacy-provider-candidate",
            ),
        ];
        for (target, source, line, column, symbol, category) in cases {
            let definition = key(source, line, column, line + 1);
            let groups = BTreeMap::from([(definition.clone(), state(symbol, false))]);
            let units = BTreeMap::from([(definition, state(symbol, true))]);
            let report = summarize(target, "integration", &groups, Some(&units));
            let review = report.records[0].reviewed_integration_gap.as_ref().unwrap();
            assert_eq!(review.category, category);
            assert_eq!(review.symbol_fragment, symbol);
            assert_eq!(report.locations.covered, 0);
            report.print(target, "integration");
            assert!(reviewed_gap(target, source, line + 10_000, column).is_none());
        }
    }

    #[test]
    fn unix_counter_gap_annotations_cover_both_native_targets() {
        for target in [LINUX, MACOS] {
            let review = reviewed_gap(target, "src/stats/validation.rs", 65, 24).unwrap();
            assert_eq!(review.category, "defensive-boundary");
            assert_eq!(review.symbol_fragment, "22validate_unix_counters");
        }
        assert!(reviewed_gap(WINDOWS, "src/stats/validation.rs", 65, 24).is_none());
    }

    #[test]
    fn empty_location_union_is_informational() {
        let report = summarize(LINUX, "combined", &BTreeMap::new(), None);
        assert!(report.informational_only);
        assert_eq!(report.locations.count, 0);
        assert_eq!(report.locations.covered, 0);
        assert_eq!(report.multi_topology_locations, 0);
        assert_eq!(report.uncovered_groups_with_executed_location, 0);
        assert_eq!(report.gap_review_baseline, REVIEW_BASELINE);
        assert!(report.records.is_empty());
    }

    #[test]
    fn location_union_preserves_topology_membership_and_location_boundaries() {
        let groups = BTreeMap::from([
            (key("src/a.rs", 7, 1, 8), state("uncovered", false)),
            (key("src/a.rs", 7, 1, 9), state("executed", true)),
            (key("src/a.rs", 7, 2, 8), state("other-column", false)),
            (key("src/a.rs", 8, 1, 9), state("other-line", false)),
            (key("src/b.rs", 7, 1, 8), state("other-file", false)),
        ]);
        let report = summarize(LINUX, "integration", &groups, None);
        assert_eq!(report.locations.count, 4);
        assert_eq!(report.locations.covered, 1);
        assert_eq!(report.multi_topology_locations, 1);
        assert_eq!(report.uncovered_groups_with_executed_location, 1);
        assert_eq!(report.records[0].definition_indexes, [0, 1]);
        assert_eq!(report.records[0].uncovered_definition_indexes, [0]);

        let original = groups.iter().collect::<Vec<_>>();
        let mut referenced = BTreeSet::new();
        for record in &report.records {
            assert_eq!(record.all_uncovered_topologies_unit_owned, None);
            assert!(record.reviewed_integration_gap.is_none());
            for index in &record.definition_indexes {
                assert!(referenced.insert(*index));
                let (definition, entry) = original[*index];
                assert_eq!(record.source, definition.filename);
                assert_eq!(record.line, definition.line);
                assert_eq!(record.column, definition.column);
                assert_eq!(
                    record.uncovered_definition_indexes.contains(index),
                    entry.covered_entries == 0,
                );
            }
        }
        assert_eq!(referenced.len(), groups.len());
    }

    #[test]
    fn unit_ownership_requires_every_exact_uncovered_topology() {
        let first = key("src/stats.rs", 20, 1, 21);
        let second = key("src/stats.rs", 20, 1, 22);
        let different_topology = key("src/stats.rs", 20, 1, 23);
        let symbol = "_RN13invalid_stats";
        let groups = BTreeMap::from([
            (first.clone(), state(symbol, false)),
            (second.clone(), state(symbol, false)),
        ]);
        let mut units = BTreeMap::from([
            (first, state(symbol, true)),
            (different_topology, state(symbol, true)),
        ]);
        let report = summarize(LINUX, "integration", &groups, Some(&units));
        assert_eq!(
            report.records[0].all_uncovered_topologies_unit_owned,
            Some(false)
        );
        assert!(report.records[0].reviewed_integration_gap.is_none());

        units.insert(second.clone(), state(symbol, true));
        let report = summarize(LINUX, "integration", &groups, Some(&units));
        assert_eq!(report.locations.covered, 0);
        assert_eq!(
            report.records[0].all_uncovered_topologies_unit_owned,
            Some(true)
        );
        assert_eq!(
            report.records[0]
                .reviewed_integration_gap
                .as_ref()
                .unwrap()
                .category,
            "defensive-boundary",
        );

        units.get_mut(&second).unwrap().covered_entries = 0;
        let report = summarize(LINUX, "integration", &groups, Some(&units));
        assert_eq!(
            report.records[0].all_uncovered_topologies_unit_owned,
            Some(false)
        );
        assert!(report.records[0].reviewed_integration_gap.is_none());
    }

    #[test]
    fn annotations_require_reviewed_target_profile_coordinates_and_owner() {
        let cases = [
            (
                LINUX,
                "integration",
                "src/stats.rs",
                20,
                1,
                "13invalid_stats",
                false,
                true,
            ),
            (
                WINDOWS,
                "integration",
                "src/stats.rs",
                20,
                1,
                "13invalid_stats",
                false,
                true,
            ),
            (
                MACOS,
                "integration",
                "src/stats.rs",
                20,
                1,
                "13invalid_stats",
                false,
                true,
            ),
            (
                "unknown-target",
                "integration",
                "src/stats.rs",
                20,
                1,
                "13invalid_stats",
                false,
                false,
            ),
            (
                LINUX,
                "unit",
                "src/stats.rs",
                20,
                1,
                "13invalid_stats",
                false,
                false,
            ),
            (
                LINUX,
                "combined",
                "src/stats.rs",
                20,
                1,
                "13invalid_stats",
                false,
                false,
            ),
            (
                LINUX,
                "integration",
                "src/other.rs",
                20,
                1,
                "13invalid_stats",
                false,
                false,
            ),
            (
                LINUX,
                "integration",
                "src/stats.rs",
                21,
                1,
                "13invalid_stats",
                false,
                false,
            ),
            (
                LINUX,
                "integration",
                "src/stats.rs",
                20,
                2,
                "13invalid_stats",
                false,
                false,
            ),
            (
                LINUX,
                "integration",
                "src/stats.rs",
                20,
                1,
                "another_owner",
                false,
                false,
            ),
            (
                LINUX,
                "integration",
                "src/stats.rs",
                20,
                1,
                "13invalid_stats",
                true,
                false,
            ),
        ];
        for (target, profile, source, line, column, symbol, executed, expected) in cases {
            let definition = key(source, line, column, line + 1);
            let groups = BTreeMap::from([(definition.clone(), state(symbol, executed))]);
            let units = BTreeMap::from([(definition, state(symbol, true))]);
            let report = summarize(target, profile, &groups, Some(&units));
            assert_eq!(
                report.records[0].reviewed_integration_gap.is_some(),
                expected,
                "{target} {profile} {source}:{line}:{column} {symbol} executed={executed}",
            );
            if executed {
                assert_eq!(report.records[0].all_uncovered_topologies_unit_owned, None);
            }
        }
    }

    #[test]
    fn ambiguous_or_missing_symbols_remain_unclassified() {
        let definition = key("src/stats.rs", 20, 1, 21);
        let units = BTreeMap::from([(definition.clone(), state("13invalid_stats", true))]);
        for symbols in [
            BTreeSet::new(),
            BTreeSet::from(["13invalid_stats".to_owned(), "another_owner".to_owned()]),
        ] {
            let groups = BTreeMap::from([(
                definition.clone(),
                DefinitionGroupState {
                    entries: 2,
                    covered_entries: 0,
                    symbols,
                },
            )]);
            let report = summarize(LINUX, "integration", &groups, Some(&units));
            assert_eq!(
                report.records[0].all_uncovered_topologies_unit_owned,
                Some(true)
            );
            assert!(report.records[0].reviewed_integration_gap.is_none());
        }
    }

    #[test]
    fn native_gap_categories_remain_distinct_and_target_specific() {
        let cases = [
            (
                WINDOWS,
                "src/windows/stats/legacy.rs",
                8,
                1,
                "14legacy_statvfs",
                "legacy-provider-candidate",
            ),
            (
                WINDOWS,
                "src/windows/allocation.rs",
                360,
                1,
                "23wait_for_device_control",
                "pending-io-candidate",
            ),
            (
                LINUX,
                "src/allocation.rs",
                73,
                1,
                "33extend_file_length_after_snapshot",
                "backend-inapplicable",
            ),
            (
                MACOS,
                "src/unix/allocation.rs",
                90,
                1,
                "23allocated_size_overflow",
                "defensive-boundary",
            ),
        ];
        for (target, source, line, column, symbol, expected) in cases {
            let definition = key(source, line, column, line + 1);
            let groups = BTreeMap::from([(definition.clone(), state(symbol, false))]);
            let units = BTreeMap::from([(definition, state(symbol, true))]);
            let report = summarize(target, "integration", &groups, Some(&units));
            assert_eq!(
                report.records[0]
                    .reviewed_integration_gap
                    .as_ref()
                    .unwrap()
                    .category,
                expected,
            );
        }
        assert!(reviewed_gap(MACOS, "src/allocation.rs", 73, 1).is_none());
        assert!(reviewed_gap(LINUX, "src/windows/allocation.rs", 360, 1).is_none());
    }

    #[test]
    fn schema_four_preserves_raw_metrics_and_does_not_bypass_existing_gates() {
        let complete = Metric {
            count: 1,
            covered: 1,
        };
        let data = CoverageData {
            files: Vec::new(),
            // Deliberately reverse the topology sort order to test index provenance.
            functions: vec![
                FunctionCoverage {
                    name: "covered-instance".to_owned(),
                    count: 1,
                    filenames: vec!["src/lib.rs".to_owned()],
                    regions: vec![vec![7, 1, 9, 2, 1, 0, 0, 0]],
                },
                FunctionCoverage {
                    name: "uncovered-instance".to_owned(),
                    count: 0,
                    filenames: vec!["src/lib.rs".to_owned()],
                    regions: vec![vec![7, 1, 8, 2, 0, 0, 0, 0]],
                },
            ],
            totals: Totals {
                functions: complete,
                instantiations: Metric {
                    count: 2,
                    covered: 1,
                },
                lines: complete,
                regions: complete,
            },
        };
        let diagnostics = instantiation_diagnostics(&data).unwrap().0;
        let groups = definition_groups(&data).unwrap();
        let units = groups
            .iter()
            .map(|(key, state)| {
                (
                    key.clone(),
                    DefinitionGroupState {
                        entries: state.entries,
                        covered_entries: state.entries,
                        symbols: state.symbols.clone(),
                    },
                )
            })
            .collect::<BTreeMap<_, _>>();
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("synthetic-diagnostics.json");
        write_diagnostics_report(
            &path,
            LINUX,
            Metric {
                count: 0,
                covered: 0,
            },
            [("integration", &data, &groups, &diagnostics, Some(&units))],
        )
        .unwrap();
        let encoded = std::fs::read(&path).unwrap();
        assert_eq!(encoded.last(), Some(&b'\n'));
        let report: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(report["schema_version"], 4);
        let profile = &report["profiles"][0];
        for name in [
            "source_definitions",
            "json_entries",
            "workspace_json_entries",
            "llvm_instantiations",
        ] {
            assert_eq!(profile[name]["count"], 2);
            assert_eq!(profile[name]["covered"], 1);
        }
        let definitions = profile["definitions"].as_array().unwrap();
        assert_eq!(definitions.len(), 2);
        assert_eq!(definitions[0]["covered_entries"], 0);
        assert_eq!(definitions[0]["ownership"], "private-unit");
        assert_eq!(definitions[0]["symbols"][0], "uncovered-instance");
        assert_eq!(definitions[0]["regions"][0][2], 8);
        assert_eq!(definitions[1]["covered_entries"], 1);
        assert_eq!(definitions[1]["symbols"][0], "covered-instance");
        assert_eq!(definitions[1]["regions"][0][2], 9);
        let union = &profile["source_location_execution_union"];
        assert_eq!(union["informational_only"], true);
        assert_eq!(union["locations"]["count"], 1);
        assert_eq!(union["locations"]["covered"], 1);
        assert_eq!(union["multi_topology_locations"], 1);
        assert_eq!(union["uncovered_groups_with_executed_location"], 1);
        assert_eq!(
            union["records"][0]["definition_indexes"],
            serde_json::json!([0, 1])
        );
        assert_eq!(
            union["records"][0]["uncovered_definition_indexes"],
            serde_json::json!([0])
        );
        assert_eq!(
            union["records"][0]["all_uncovered_topologies_unit_owned"],
            true
        );
        assert!(union["records"][0]["reviewed_integration_gap"].is_null());
        for profile in ["combined", "unit"] {
            assert!(validate_source_definition_completeness(LINUX, profile, &diagnostics).is_err());
        }
    }
}
