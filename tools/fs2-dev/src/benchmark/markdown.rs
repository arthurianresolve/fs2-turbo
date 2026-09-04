use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::Path;

use clap::ArgMatches;
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::paired;
use crate::{Result, invalid_data, lower_hex};

const MAX_REPORT_BYTES: u64 = 128 * 1024 * 1024;

#[cfg(test)]
std::thread_local! {
    static REF_COUNT_VISITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[derive(Default)]
struct Row {
    metric: String,
    baseline: Vec<f64>,
    candidate: Vec<f64>,
    ratios: Vec<f64>,
    exact_lower: Option<f64>,
    exact_upper: Option<f64>,
    outliers: u64,
    samples: u64,
    disposition: String,
}

pub(crate) fn run(arguments: &ArgMatches) -> Result<()> {
    let reports = arguments
        .get_many::<std::path::PathBuf>("report")
        .ok_or_else(|| invalid_data("missing --report"))?;
    let rendered = reports
        .map(|report| render_report(&read_report(report)?))
        .collect::<Result<Vec<_>>>()?;
    write_reports(&mut std::io::stdout().lock(), &rendered)
}

fn write_reports(output: &mut impl Write, reports: &[String]) -> Result<()> {
    // Render every input first, then emit without duplicating the whole batch.
    for (index, report) in reports.iter().enumerate() {
        if index != 0 {
            output.write_all(b"\n\n")?;
        }
        output.write_all(report.as_bytes())?;
    }
    output.write_all(b"\n")?;
    Ok(())
}

fn read_report(path: &Path) -> Result<Value> {
    let file = paired::open_regular_input(path)?;
    let metadata = file.metadata()?;
    if metadata.len() > MAX_REPORT_BYTES {
        return Err(invalid_data(
            "benchmark report must be a bounded regular file",
        ));
    }
    let mut bytes = Vec::with_capacity(usize::try_from(metadata.len())?);
    file.take(MAX_REPORT_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len())? > MAX_REPORT_BYTES {
        return Err(invalid_data("benchmark report exceeds its size limit"));
    }
    Ok(serde_json::from_slice(&bytes)?)
}

pub(super) fn render_report(report: &Value) -> Result<String> {
    if report.get("valid").and_then(Value::as_bool) != Some(true) {
        return Err(invalid_data("only valid benchmark reports can be rendered"));
    }
    match text(report, "report_kind")? {
        "ref-to-ref" => render_ref_report(report),
        "stats" | "lock" | "common" => render_paired_report(report),
        _ => Err(invalid_data("unsupported benchmark report kind")),
    }
}

fn render_paired_report(report: &Value) -> Result<String> {
    let profile = report
        .pointer("/method/profile")
        .and_then(Value::as_str)
        .unwrap_or("paired-measurement");
    let operations = match report.pointer("/method/operations_per_timed_interval") {
        None => 1,
        Some(value) => value
            .as_u64()
            .filter(|operations| *operations > 0)
            .ok_or_else(|| invalid_data("paired report batch size must be a positive integer"))?,
    };
    let mut rows = BTreeMap::<String, Row>::new();
    for record in array(report, "records")? {
        if record.get("mode").and_then(Value::as_str) != Some("ab") {
            continue;
        }
        let metric = text(record, "metric")?.to_owned();
        let row = rows.entry(metric.clone()).or_default();
        row.metric = metric;
        row.baseline
            .push(number(record, "baseline_ns")? / operations as f64);
        row.candidate
            .push(number(record, "candidate_ns")? / operations as f64);
        add_counts(row, record, "outliers", "samples")?;
    }
    for summary in report
        .pointer("/ab/summary")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid_data("paired report has no A/B summary"))?
    {
        let metric = text(summary, "metric")?.to_owned();
        let row = rows.entry(metric.clone()).or_default();
        row.metric = metric;
        row.ratios = numbers(summary, "ratios")?;
        row.exact_lower = Some(number(summary, "exact_lower_ratio")?);
        row.exact_upper = Some(number(summary, "exact_upper_ratio")?);
        row.disposition = text(summary, "disposition")?.to_owned();
    }
    let mut output = provenance(report, profile);
    if operations == 1 {
        output.push_str("\n\n| Case | Baseline p50 | Candidate p50 | Median delta | Exact ratio interval | Outliers | Disposition |\n");
    } else {
        output.push_str(&format!(
            "\n\nTimings are median batch costs divided by {operations} operations, not individual-call p50 latencies. Raw report and prime timings remain nanoseconds per batch."
        ));
        output.push_str("\n\n| Case | Baseline amortized cost | Candidate amortized cost | Median delta | Exact ratio interval | Outliers | Disposition |\n");
    }
    output.push_str("| --- | ---: | ---: | ---: | ---: | ---: | --- |\n");
    for (metric, mut row) in rows {
        require_complete(&row)?;
        let baseline = median(&mut row.baseline)?;
        let candidate = median(&mut row.candidate)?;
        let ratio = median(&mut row.ratios)?;
        output.push_str(&format!(
            "| {} | {} | {} | {:+.2}% | {:.6} to {:.6} | {} / {} ({:.2}%) | {} |\n",
            escape(&metric),
            duration(baseline),
            duration(candidate),
            (ratio - 1.0) * 100.0,
            row.exact_lower.unwrap(),
            row.exact_upper.unwrap(),
            row.outliers,
            row.samples,
            percentage(row.outliers, row.samples),
            escape(&row.disposition),
        ));
    }
    Ok(output)
}

fn render_ref_report(report: &Value) -> Result<String> {
    let mut rows = BTreeMap::<String, Row>::new();
    for pair in array(report, "pairs")? {
        let metric = text(pair, "metric")?.to_owned();
        let key = format!("{}::{metric}", text(pair, "benchmark")?);
        let row = rows.entry(key).or_default();
        row.metric = metric;
        row.baseline.push(number(pair, "baseline_median_ns")?);
        row.candidate.push(number(pair, "candidate_median_ns")?);
        row.ratios.push(number(pair, "ratio")?);
    }
    for decision in array(report, "decisions")? {
        let key = text(decision, "benchmark")?.to_owned();
        let row = rows.entry(key).or_default();
        row.exact_lower = Some(number(decision, "lower_bound")?);
        row.exact_upper = Some(number(decision, "upper_bound")?);
        row.disposition = text(decision, "disposition")?.to_owned();
    }
    populate_ref_counts(report, &mut rows)?;
    let mut output = provenance(report, "ref-to-ref");
    output.push_str("\n\n| Case | Baseline p50 | Candidate p50 | Paired median delta | Pair range | Exact upper ratio | Outliers | Disposition |\n");
    output.push_str("| --- | ---: | ---: | ---: | ---: | ---: | ---: | --- |\n");
    for (key, mut row) in rows {
        require_complete(&row)?;
        let baseline = median(&mut row.baseline)?;
        let candidate = median(&mut row.candidate)?;
        let ratio = median(&mut row.ratios)?;
        let lower = row.ratios.iter().copied().reduce(f64::min).unwrap();
        let upper = row.ratios.iter().copied().reduce(f64::max).unwrap();
        output.push_str(&format!(
            "| {} | {} | {} | {:+.2}% | {:.2}% to {:.2}% | {:.6} | {} / {} ({:.2}%) | {} |\n",
            escape(&key),
            duration(baseline),
            duration(candidate),
            (ratio - 1.0) * 100.0,
            lower * 100.0,
            upper * 100.0,
            row.exact_upper.unwrap(),
            row.outliers,
            row.samples,
            percentage(row.outliers, row.samples),
            escape(&row.disposition),
        ));
    }
    Ok(output)
}

fn provenance(report: &Value, profile: &str) -> String {
    let baseline = report
        .pointer("/metadata/baseline_commit")
        .or_else(|| report.get("baseline_source"))
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let candidate = report
        .pointer("/metadata/candidate_commit")
        .or_else(|| report.get("candidate_source"))
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    format!(
        "## {}\n\n- Baseline: {}\n- Candidate: {}\n\n{}",
        escape(profile),
        escape(baseline),
        escape(candidate),
        evidence_qualifications(report),
    )
}

fn evidence_qualifications(report: &Value) -> String {
    let mut output = String::from(
        "### Evidence qualifications\n\nWorkload dispositions below do not override the report-level decision and controls. Unknown fields do not establish strict performance evidence.\n",
    );
    if report
        .pointer("/method/diagnostic_samples")
        .and_then(Value::as_bool)
        == Some(true)
    {
        output.push_str("\n**Diagnostic only: not performance evidence.**\n");
    }
    if report
        .pointer("/aa_control/enabled")
        .and_then(Value::as_bool)
        == Some(false)
        || report
            .pointer("/method/aa_control")
            .and_then(Value::as_bool)
            == Some(false)
    {
        output.push_str("\n**A/A control was skipped, not passed.**\n");
    }
    for (label, pointers) in [
        ("Execution status", &["/status"][..]),
        ("Overall decision", &["/decision", "/decision_passed"][..]),
        ("A/B gate passed", &["/ab/passed"][..]),
        ("Strict configuration", &["/strict_configuration"][..]),
        (
            "Evidence mode and reasons",
            &["/evidence_mode", "/metadata/evidence_mode"][..],
        ),
        ("Ref exploratory flag", &["/metadata/exploratory"][..]),
        ("Diagnostic samples", &["/method/diagnostic_samples"][..]),
        ("Method reason", &["/method/reason"][..]),
        ("A/A method control", &["/method/aa_control"][..]),
        ("A/A control enabled", &["/aa_control/enabled"][..]),
        ("A/A control passed field", &["/aa_control/passed"][..]),
    ] {
        let value = pointers.iter().find_map(|pointer| report.pointer(pointer));
        output.push_str(&format!("\n- {label}: {}", qualification_value(value)));
    }
    output
}

fn qualification_value(value: Option<&Value>) -> String {
    let text = value
        .filter(|value| !value.is_null())
        .map(Value::to_string)
        .unwrap_or_else(|| "unknown".to_owned());
    escape(&text)
}

fn populate_ref_counts(report: &Value, rows: &mut BTreeMap<String, Row>) -> Result<()> {
    if rows.is_empty() {
        return Ok(());
    }
    let mut totals: BTreeMap<_, _> = rows
        .values()
        .map(|row| (workload_id(&row.metric), (Row::default(), None)))
        .collect();
    // Visit each estimate once, even when many benchmark rows share a metric.
    // Defer count errors to preserve the original row-order error precedence.
    let mut structural_error = (|| -> Result<()> {
        for run in array(report, "runs")? {
            #[cfg(test)]
            REF_COUNT_VISITS.with(|visits| visits.set(visits.get() + 1));
            for estimate in array(run, "estimates")? {
                #[cfg(test)]
                REF_COUNT_VISITS.with(|visits| visits.set(visits.get() + 1));
                let metric = text(estimate, "metric")?;
                if let Some((counts, error)) = totals.get_mut(metric)
                    && error.is_none()
                {
                    *error = add_counts(counts, estimate, "outliers", "sample_count").err();
                }
            }
        }
        Ok(())
    })()
    .err();
    for row in rows.values_mut() {
        let (counts, error) = totals
            .get_mut(&workload_id(&row.metric))
            .expect("every report row has an indexed workload");
        if let Some(error) = error.take() {
            return Err(error);
        }
        // A structural failure ends the first row's traversal, after any
        // earlier matching count failure but before any later row's failure.
        if let Some(error) = structural_error.take() {
            return Err(error);
        }
        row.outliers = counts.outliers;
        row.samples = counts.samples;
    }
    Ok(())
}

fn add_counts(row: &mut Row, value: &Value, outliers: &str, samples: &str) -> Result<()> {
    row.outliers = row
        .outliers
        .checked_add(integer(value, outliers)?)
        .ok_or_else(|| invalid_data("outlier total overflow"))?;
    row.samples = row
        .samples
        .checked_add(integer(value, samples)?)
        .ok_or_else(|| invalid_data("sample total overflow"))?;
    Ok(())
}

fn require_complete(row: &Row) -> Result<()> {
    if row.metric.is_empty()
        || row.baseline.is_empty()
        || row.candidate.is_empty()
        || row.ratios.is_empty()
        || row.exact_lower.is_none()
        || row.exact_upper.is_none()
        || row.disposition.is_empty()
    {
        Err(invalid_data("benchmark report row is incomplete"))
    } else {
        Ok(())
    }
}

fn array<'a>(value: &'a Value, field: &str) -> Result<&'a Vec<Value>> {
    value
        .get(field)
        .and_then(Value::as_array)
        .ok_or_else(|| invalid_data(format!("benchmark report has no {field} array")))
}

fn text<'a>(value: &'a Value, field: &str) -> Result<&'a str> {
    value
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| invalid_data(format!("benchmark report has no {field} text")))
}

fn number(value: &Value, field: &str) -> Result<f64> {
    let number = value
        .get(field)
        .and_then(Value::as_f64)
        .ok_or_else(|| invalid_data(format!("benchmark report has no {field} number")))?;
    if number.is_finite() && number > 0.0 {
        Ok(number)
    } else {
        Err(invalid_data(format!("benchmark report {field} is invalid")))
    }
}

fn integer(value: &Value, field: &str) -> Result<u64> {
    value
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| invalid_data(format!("benchmark report has no {field} integer")))
}

fn numbers(value: &Value, field: &str) -> Result<Vec<f64>> {
    value
        .get(field)
        .and_then(Value::as_array)
        .ok_or_else(|| invalid_data(format!("benchmark report has no {field} array")))?
        .iter()
        .map(|value| {
            value
                .as_f64()
                .filter(|value| value.is_finite() && *value > 0.0)
                .ok_or_else(|| invalid_data(format!("benchmark report {field} is invalid")))
        })
        .collect()
}

fn median(values: &mut [f64]) -> Result<f64> {
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

fn duration(ns: f64) -> String {
    if ns >= 1_000_000.0 {
        format!("{:.2} ms", ns / 1_000_000.0)
    } else if ns >= 1_000.0 {
        format!("{:.2} us", ns / 1_000.0)
    } else {
        format!("{ns:.2} ns")
    }
}

fn percentage(part: u64, total: u64) -> f64 {
    if total == 0 {
        0.0
    } else {
        part as f64 / total as f64 * 100.0
    }
}

fn escape(value: &str) -> String {
    let mut output = String::new();
    let mut segment = String::new();
    for character in value.chars() {
        match character {
            // These separators stay outside code spans, so tables and fences
            // cannot reinterpret them. All other text stays in a literal span.
            '|' | '\u{60}' => {
                literal_segment(&mut output, &segment);
                segment.clear();
                output.push('\\');
                output.push(character);
            }
            '\r' | '\n' | '\t' => segment.push(' '),
            character
                if character.is_control()
                    || matches!(
                        character,
                        '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{2028}'..='\u{202e}'
                            | '\u{2066}'..='\u{2069}'
                    ) =>
            {
                segment.push_str(&format!("\\u{{{:x}}}", u32::from(character)));
            }
            _ => segment.push(character),
        }
    }
    literal_segment(&mut output, &segment);
    output
}

fn literal_segment(output: &mut String, segment: &str) {
    if segment.is_empty() {
        return;
    }
    // CommonMark removes one enclosing space unless the span is all spaces.
    let padded = !segment.bytes().all(|byte| byte == b' ');
    output.push('\u{60}');
    if padded {
        output.push(' ');
    }
    output.push_str(segment);
    if padded {
        output.push(' ');
    }
    output.push('\u{60}');
}

fn workload_id(metric: &str) -> String {
    format!("workload-{}", lower_hex(Sha256::digest(metric.as_bytes())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_paired_reports_with_evidence_columns() {
        let report = serde_json::json!({
            "valid": true,
            "report_kind": "stats",
            "baseline_source": "base",
            "candidate_source": "head",
            "method": {"profile": "filesystem-stats-v0.4-common"},
            "records": [{
                "mode": "ab", "metric": "free_space", "baseline_ns": 2000.0,
                "candidate_ns": 1000.0, "outliers": 1, "samples": 50
            }],
            "ab": {"summary": [{
                "metric": "free_space", "ratios": [0.5, 0.6],
                "exact_lower_ratio": 0.5, "exact_upper_ratio": 0.6,
                "disposition": "non-inferior"
            }]}
        });
        let markdown = render_report(&report).unwrap();
        assert!(markdown.contains("filesystem-stats-v0.4-common"));
        assert!(markdown.contains("| ` free_space ` | 2.00 us | 1.00 us | -45.00% |"));
    }

    #[test]
    fn renders_ref_reports_and_joins_hashed_run_metrics() {
        let report = serde_json::json!({
            "valid": true,
            "report_kind": "ref-to-ref",
            "metadata": {
                "baseline_commit": "base",
                "candidate_commit": "head"
            },
            "pairs": [{
                "benchmark": "fs_compat", "metric": "lock_unlock",
                "baseline_median_ns": 4000.0, "candidate_median_ns": 3000.0,
                "ratio": 0.75
            }],
            "decisions": [{
                "benchmark": "fs_compat::lock_unlock",
                "lower_bound": 0.70, "upper_bound": 0.80,
                "disposition": "non-inferior"
            }],
            "runs": [{
                "estimates": [{
                    "metric": workload_id("lock_unlock"),
                    "outliers": 2, "sample_count": 50
                }]
            }]
        });
        let markdown = render_report(&report).unwrap();
        assert!(markdown.contains("| ` fs_compat::lock_unlock ` | 4.00 us | 3.00 us | -25.00% |"));
        assert!(markdown.contains("| 2 / 50 (4.00%) | ` non-inferior ` |"));
    }

    fn legacy_ref_counts(report: &Value, rows: &mut BTreeMap<String, Row>) -> Result<()> {
        for row in rows.values_mut() {
            let metric = workload_id(&row.metric);
            for run in array(report, "runs")? {
                for estimate in array(run, "estimates")? {
                    if text(estimate, "metric")? == metric {
                        add_counts(row, estimate, "outliers", "sample_count")?;
                    }
                }
            }
        }
        Ok(())
    }

    fn assert_ref_counts_match_legacy(report: &Value) {
        let make_rows = || {
            [("a", "first"), ("b", "second"), ("c", "first")]
                .into_iter()
                .map(|(key, metric)| {
                    (
                        key.to_owned(),
                        Row {
                            metric: metric.to_owned(),
                            ..Row::default()
                        },
                    )
                })
                .collect::<BTreeMap<_, _>>()
        };
        let mut expected = make_rows();
        let mut actual = make_rows();
        let expected_result = legacy_ref_counts(report, &mut expected).map_err(|e| e.to_string());
        let actual_result = populate_ref_counts(report, &mut actual).map_err(|e| e.to_string());
        assert_eq!(actual_result, expected_result, "{report}");
        if expected_result.is_ok() {
            for (key, row) in actual {
                let expected = &expected[&key];
                assert_eq!(
                    (row.outliers, row.samples),
                    (expected.outliers, expected.samples)
                );
            }
        }
    }

    #[test]
    fn indexed_ref_counts_preserve_validation_and_error_order() {
        let estimates = [
            serde_json::json!({}),
            serde_json::json!({"metric": 7}),
            serde_json::json!({"metric": "unmatched", "outliers": "ignored"}),
            serde_json::json!({"metric": workload_id("first"), "outliers": 0, "sample_count": 0}),
            serde_json::json!({"metric": workload_id("first"), "outliers": 1, "sample_count": 10}),
            serde_json::json!({"metric": workload_id("first"), "outliers": u64::MAX, "sample_count": 0}),
            serde_json::json!({"metric": workload_id("first"), "outliers": 0, "sample_count": u64::MAX}),
            serde_json::json!({"metric": workload_id("first"), "outliers": -1}),
            serde_json::json!({"metric": workload_id("first"), "outliers": 1, "sample_count": 0.5}),
            serde_json::json!({"metric": workload_id("second"), "outliers": 2, "sample_count": 20}),
            serde_json::json!({"metric": workload_id("second"), "outliers": 1}),
        ];
        for first in &estimates {
            for second in &estimates {
                assert_ref_counts_match_legacy(&serde_json::json!({
                    "runs": [{"estimates": [first, second]}]
                }));
                assert_ref_counts_match_legacy(&serde_json::json!({
                    "runs": [{"estimates": [first, second]}, {}]
                }));
                assert_ref_counts_match_legacy(&serde_json::json!({
                    "runs": [{"estimates": [first]}, {"estimates": [second]}]
                }));
            }
        }
        for report in [
            serde_json::json!({}),
            serde_json::json!({"runs": null}),
            serde_json::json!({"runs": []}),
            serde_json::json!({"runs": [{"estimates": null}]}),
        ] {
            assert_ref_counts_match_legacy(&report);
            populate_ref_counts(&report, &mut BTreeMap::new()).unwrap();
        }
    }

    #[test]
    fn indexed_ref_counts_handle_independent_and_shared_dimensions() {
        const ROWS: usize = 4096;
        const ESTIMATES: usize = 16384;
        for shared in [false, true] {
            let mut rows = (0..ROWS)
                .map(|index| {
                    (
                        format!("case-{index}"),
                        Row {
                            metric: if shared {
                                "shared".to_owned()
                            } else {
                                index.to_string()
                            },
                            ..Row::default()
                        },
                    )
                })
                .collect::<BTreeMap<_, _>>();
            let estimate = if shared {
                serde_json::json!({"metric": workload_id("shared"), "outliers": 1, "sample_count": 10})
            } else {
                serde_json::json!({"metric": "unmatched", "outliers": "ignored"})
            };
            let mut runs = vec![serde_json::json!({"estimates": []}); ROWS];
            runs.push(serde_json::json!({"estimates": vec![estimate; ESTIMATES]}));
            REF_COUNT_VISITS.with(|visits| visits.set(0));
            populate_ref_counts(&serde_json::json!({"runs": runs}), &mut rows).unwrap();
            assert_eq!(
                REF_COUNT_VISITS.with(|visits| visits.get()),
                ROWS + 1 + ESTIMATES
            );
            let expected = if shared { ESTIMATES as u64 } else { 0 };
            assert!(
                rows.values()
                    .all(|row| row.outliers == expected && row.samples == expected * 10)
            );
        }
    }

    #[test]
    fn indexed_ref_rendering_preserves_duplicate_and_composite_keys() {
        let pairs = [
            ("a::b", "c", 4000.0),
            ("a", "b::c", 8000.0),
            ("z", "b::c", 4000.0),
        ]
        .into_iter()
        .map(|(benchmark, metric, baseline)| {
            serde_json::json!({
                "benchmark": benchmark, "metric": metric,
                "baseline_median_ns": baseline, "candidate_median_ns": baseline * 0.75,
                "ratio": 0.75
            })
        })
        .collect::<Vec<_>>();
        let decisions = [
            ("a::b::c", "earlier"),
            ("a::b::c", "non-inferior"),
            ("z::b::c", "non-inferior"),
        ]
        .into_iter()
        .map(|(benchmark, disposition)| {
            serde_json::json!({
                "benchmark": benchmark, "lower_bound": 0.70, "upper_bound": 0.80,
                "disposition": disposition
            })
        })
        .collect::<Vec<_>>();
        let estimate =
            serde_json::json!({"metric": workload_id("b::c"), "outliers": 2, "sample_count": 50});
        let report = serde_json::json!({
            "valid": true, "report_kind": "ref-to-ref", "pairs": pairs, "decisions": decisions,
            "runs": [{"estimates": [estimate.clone(), estimate]}, {"estimates": [
                {"metric": workload_id("c"), "outliers": "ignored"}
            ]}]
        });
        let markdown = render_report(&report).unwrap();
        assert!(markdown.contains("| ` a::b::c ` | 6.00 us | 4.50 us | -25.00% |"));
        assert!(markdown.contains("| ` z::b::c ` | 4.00 us | 3.00 us | -25.00% |"));
        assert_eq!(
            markdown
                .matches("| 4 / 100 (4.00%) | ` non-inferior ` |")
                .count(),
            2
        );
        assert!(!markdown.contains("earlier"));
    }

    #[test]
    fn rejects_invalid_reports() {
        assert!(
            render_report(&serde_json::json!({
                "valid": false,
                "report_kind": "stats"
            }))
            .is_err()
        );
    }

    #[test]
    fn batched_costs_are_amortized_without_rescaling_ratios_or_counts() {
        let mut report = serde_json::json!({
            "valid": true,
            "report_kind": "common-api",
            "method": {
                "profile": "duplicate-batch64",
                "operations_per_timed_interval": 64
            },
            "records": [{
                "mode": "ab", "metric": "duplicate/batch64", "baseline_ns": 128000.0,
                "candidate_ns": 64000.0, "outliers": 2, "samples": 50
            }],
            "ab": {"summary": [{
                "metric": "duplicate/batch64", "ratios": [0.5, 0.5],
                "exact_lower_ratio": 0.5, "exact_upper_ratio": 0.5,
                "disposition": "non-inferior"
            }]}
        });
        let markdown = render_paired_report(&report).unwrap();
        assert!(markdown.contains("Baseline amortized cost"));
        assert!(!markdown.contains("| Baseline p50 |"));
        assert!(markdown.contains("| ` duplicate/batch64 ` | 2.00 us | 1.00 us | -50.00% |"));
        assert!(markdown.contains("0.500000 to 0.500000 | 2 / 50 (4.00%)"));
        for invalid in [
            serde_json::json!(0),
            serde_json::json!(-1),
            serde_json::json!(1.5),
        ] {
            report["method"]["operations_per_timed_interval"] = invalid;
            assert!(render_paired_report(&report).is_err());
        }
    }

    #[test]
    fn literal_fields_neutralize_markup_links_and_nonprinting_controls() {
        assert_eq!(
            escape("[link](https://example.invalid) <img> & www.example.invalid a@b.test"),
            "` [link](https://example.invalid) <img> & www.example.invalid a@b.test `"
        );
        assert_eq!(
            escape("\u{1b}\u{7}\u{9b}\u{202e}\r\n\t"),
            r"` \u{1b}\u{7}\u{9b}\u{202e}    `"
        );
        assert_eq!(escape("GH-1 a@b.test"), "` GH-1 a@b.test `");
        assert_eq!(escape("a\\|b"), r"` a\ `\|` b `");
        assert_eq!(escape("a\u{60}b"), r"` a `\`` b `");
        assert_eq!(escape("   "), "`   `");
        assert_eq!(escape(""), "");
    }

    #[test]
    fn literal_punctuation_does_not_expand_into_entities() {
        let input = ".:@".repeat(64 * 1024);
        assert_eq!(escape(&input).len(), input.len() + 4);
        assert_eq!(escape(&"|".repeat(128)).len(), 256);
        assert_eq!(escape(&"\u{60}".repeat(128)).len(), 256);
    }

    #[test]
    fn report_emission_preserves_the_complete_batch_bytes() {
        for reports in [
            vec![],
            vec!["first".to_owned()],
            vec!["first".to_owned(), String::new(), "last".to_owned()],
        ] {
            let mut output = Vec::new();
            write_reports(&mut output, &reports).unwrap();
            assert_eq!(
                String::from_utf8(output).unwrap(),
                format!("{}\n", reports.join("\n\n"))
            );
        }
    }

    #[test]
    fn every_renderer_literalizes_report_controlled_fields() {
        let attack = "\u{1b}[31m\u{9b}1m\u{202e}[link](https://example.invalid) <img> ![image](x)";
        for kind in ["stats", "lock", "common", "ref-to-ref"] {
            let mut report = qualification_report(kind);
            report["baseline_source"] = serde_json::json!(attack);
            report["candidate_source"] = serde_json::json!(attack);
            report["method"] = serde_json::json!({"profile": attack, "reason": attack});
            report["records"] = serde_json::json!([{
                "mode": "ab", "metric": attack, "baseline_ns": 2000.0,
                "candidate_ns": 1000.0, "outliers": 1, "samples": 50
            }]);
            report["ab"]["summary"] = serde_json::json!([{
                "metric": attack, "ratios": [0.5], "exact_lower_ratio": 0.5,
                "exact_upper_ratio": 0.5, "disposition": attack
            }]);
            report["metadata"] = serde_json::json!({
                "baseline_commit": attack, "candidate_commit": attack
            });
            report["pairs"] = serde_json::json!([{
                "benchmark": attack, "metric": attack, "baseline_median_ns": 2000.0,
                "candidate_median_ns": 1000.0, "ratio": 0.5
            }]);
            report["decisions"] = serde_json::json!([{
                "benchmark": format!("{attack}::{attack}"), "lower_bound": 0.5,
                "upper_bound": 0.5, "disposition": attack
            }]);
            let rendered = render_report(&report).unwrap();
            assert!(!rendered.chars().any(|c| c.is_control() && c != '\n'));
            assert!(!rendered.contains('\u{202e}'));
            let expected_fields = if kind == "ref-to-ref" { 3 } else { 5 };
            assert_eq!(rendered.matches(&escape(attack)).count(), expected_fields);
            assert!(rendered.contains("2.00 us | 1.00 us | -50.00%"));
            assert!(rendered.contains("Evidence qualifications"));
        }
    }

    #[test]
    fn report_reader_accepts_json_and_rejects_invalid_or_oversized_inputs() {
        use std::fs;

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("report.json");
        fs::write(&path, br#"{"valid":true}"#).unwrap();
        assert_eq!(read_report(&path).unwrap()["valid"], true);
        assert!(read_report(directory.path()).is_err());
        fs::write(&path, [0xff]).unwrap();
        assert!(read_report(&path).is_err());
        fs::File::create(&path)
            .unwrap()
            .set_len(MAX_REPORT_BYTES + 1)
            .unwrap();
        assert!(read_report(&path).is_err());
    }

    fn qualification_report(kind: &str) -> Value {
        serde_json::json!({
            "valid": true, "status": "completed", "report_kind": kind,
            "records": [], "ab": {"summary": [], "passed": true},
            "pairs": [], "decisions": [], "runs": [],
        })
    }

    #[test]
    fn paired_exports_preserve_strict_exploratory_and_skipped_control_states() {
        for kind in ["stats", "lock", "common"] {
            let mut report = qualification_report(kind);
            report["decision"] = serde_json::json!("strict-non-regression-pass");
            report["strict_configuration"] = serde_json::json!(true);
            report["evidence_mode"] = serde_json::json!({"strict": true, "reasons": []});
            report["method"] = serde_json::json!({"diagnostic_samples": false, "aa_control": true});
            report["aa_control"] = serde_json::json!({"enabled": true, "passed": true});
            let strict = render_report(&report).unwrap();
            assert!(strict.contains("strict-non-regression-pass"));
            assert!(strict.contains("- Strict configuration: ` true `"));
            assert!(strict.contains("- A/A control enabled: ` true `"));
            assert!(!strict.contains("Diagnostic only"));
            assert!(!strict.contains("was skipped"));

            report["decision"] = serde_json::json!("exploratory-non-inferior");
            report["strict_configuration"] = serde_json::json!(false);
            report["evidence_mode"] = serde_json::json!({
                "strict": false, "reasons": ["A/A disabled <script> | \u{60}"]
            });
            report["method"]["aa_control"] = serde_json::json!(false);
            report["aa_control"]["enabled"] = serde_json::json!(false);
            let exploratory = render_report(&report).unwrap();
            assert!(exploratory.contains("exploratory-non-inferior"));
            assert!(exploratory.contains("- Strict configuration: ` false `"));
            assert!(exploratory.contains("A/A control was skipped, not passed"));
            assert!(exploratory.contains(&qualification_value(report.pointer("/evidence_mode"))));

            report["method"]["diagnostic_samples"] = serde_json::json!(true);
            assert!(
                render_report(&report)
                    .unwrap()
                    .contains("Diagnostic only: not performance evidence")
            );
            // A conflicting old strict flag must not hide diagnostic restrictions.
            report["strict_configuration"] = serde_json::json!(true);
            assert!(
                render_report(&report)
                    .unwrap()
                    .contains("Diagnostic only: not performance evidence")
            );
        }
    }

    #[test]
    fn ref_exports_preserve_nested_mode_and_overall_decision() {
        let mut report = qualification_report("ref-to-ref");
        report["decision_passed"] = serde_json::json!(false);
        report["metadata"] = serde_json::json!({
            "exploratory": true,
            "evidence_mode": {"strict": false, "reasons": ["different resolved lockfiles"]}
        });
        let markdown = render_report(&report).unwrap();
        assert!(markdown.contains("- Overall decision: ` false `"));
        assert!(markdown.contains("- Ref exploratory flag: ` true `"));
        assert!(markdown.contains("different resolved lockfiles"));
        assert!(markdown.contains("- A/A control enabled: ` unknown `"));
        assert!(!markdown.contains("was skipped"));
        report["metadata"]["exploratory"] = serde_json::json!(false);
        report["metadata"]["evidence_mode"] = serde_json::json!({"strict": true, "reasons": []});
        report["decision_passed"] = serde_json::json!(true);
        assert!(
            render_report(&report)
                .unwrap()
                .contains("- Overall decision: ` true `")
        );
    }

    #[test]
    fn historical_qualifications_remain_unknown_in_every_renderer() {
        for kind in ["stats", "lock", "common", "ref-to-ref"] {
            let markdown = render_report(&qualification_report(kind)).unwrap();
            assert!(markdown.contains("- Strict configuration: ` unknown `"));
            assert!(markdown.contains("- Evidence mode and reasons: ` unknown `"));
            assert!(markdown.contains("- Diagnostic samples: ` unknown `"));
            assert!(markdown.contains("- A/A control enabled: ` unknown `"));
            assert!(
                markdown.contains("Unknown fields do not establish strict performance evidence")
            );
        }
    }
}
