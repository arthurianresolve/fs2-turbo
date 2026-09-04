use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;

use clap::ArgMatches;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{arguments, markdown, paired};
use crate::{Result, invalid_data};

const BASELINE: &str = "9a340454a8292df025de368fc4b310bb736f382f";
const MAX_STATE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_REPORT_BYTES: u64 = 8 * 1024 * 1024;
const PROFILES: [(&str, &str, &str, usize); 3] = [
    (
        "duplicate-single-refs",
        "duplicate-single-call",
        "duplicate",
        16,
    ),
    (
        "file-create-delete-refs",
        "file-create-delete-single-workload",
        "file_create_delete",
        8,
    ),
    ("lock-refs", "lock-exact-refs", "lock_unlock", 8),
];

#[derive(Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct State {
    schema_version: u64,
    branches: BTreeMap<String, Accepted>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Accepted {
    branch: String,
    candidate: String,
    harness: String,
    run_id: u64,
    attempt: u64,
    completed_utc: String,
    image_version: String,
    reports: Vec<Value>,
}

pub(crate) fn run(arguments: &ArgMatches) -> Result<()> {
    let input = arguments::required_path(arguments, "input")?;
    let previous = arguments::required_path(arguments, "previous")?;
    let output = arguments::required_path(arguments, "output")?;
    let branch = arguments::required_string(arguments, "branch")?;
    let candidate = arguments::required_string(arguments, "candidate")?;
    let run_id: u64 = arguments::required_string(arguments, "run-id")?.parse()?;
    let attempt: u64 = arguments::required_string(arguments, "attempt")?.parse()?;
    let mut state: State = serde_json::from_value(read_json(&previous, MAX_STATE_BYTES)?)?;
    validate_state(&state)?;
    let accepted = from_artifact(&input, branch, candidate, run_id, attempt)?;
    update(&mut state, accepted)?;
    publish(&output, &state)
}

fn read_json(path: &Path, limit: u64) -> Result<Value> {
    let file = paired::open_regular_input(path)?;
    ensure(file.metadata()?.len() <= limit, "JSON input is too large")?;
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    ensure(
        bytes.len() as u64 <= limit,
        "JSON input grew beyond its limit",
    )?;
    Ok(serde_json::from_slice(&bytes)?)
}

fn from_artifact(
    input: &Path,
    branch: &str,
    candidate: &str,
    run_id: u64,
    attempt: u64,
) -> Result<Accepted> {
    let plan = read_json(&input.join("audit/plan.json"), 128 * 1024)?;
    let launcher = read_json(&input.join("audit/launcher-result.json"), 128 * 1024)?;
    let worker = read_json(&input.join("audit/worker/pilot-result.json"), 128 * 1024)?;
    let expected_run_id = run_id.to_string();
    let expected_attempt = attempt.to_string();
    ensure(
        plan["candidate"] == candidate
            && plan["branch"] == branch
            && plan["baseline"] == BASELINE
            && plan["run_id"].as_str() == Some(expected_run_id.as_str())
            && plan["run_attempt"].as_str() == Some(expected_attempt.as_str())
            && plan["rust"] == "1.98.1"
            && plan["msrv"] == "1.88.0"
            && launcher["exit"] == 0
            && launcher.get("failure") == Some(&Value::Null)
            && worker["all_profile_gates_passed"] == true
            && worker["attempted"] == 3
            && worker["planned"] == 3
            && worker.get("failure") == Some(&Value::Null),
        "artifact identity or completed pilot gate does not match the workflow run",
    )?;
    let reports = PROFILES
        .iter()
        .map(|(command, _, _, _)| {
            read_json(
                &input.join("measurements").join(command).join("report.json"),
                MAX_REPORT_BYTES,
            )
        })
        .collect::<Result<Vec<_>>>()?;
    let accepted = Accepted {
        branch: branch.to_owned(),
        candidate: candidate.to_owned(),
        harness: text(&plan, "workflow_sha")?.to_owned(),
        run_id,
        attempt,
        completed_utc: text(&worker, "finished_utc")?.to_owned(),
        image_version: text(&plan, "image_version")?.to_owned(),
        reports,
    };
    validate_accepted(&accepted)?;
    Ok(accepted)
}

fn validate_state(state: &State) -> Result<()> {
    ensure(
        state.schema_version == 1 && state.branches.len() <= 2,
        "unsupported accepted-results state",
    )?;
    for (slot, accepted) in &state.branches {
        ensure(
            slot == if accepted.branch == "dev" {
                "dev"
            } else {
                "default"
            },
            "accepted result is in the wrong branch slot",
        )?;
        validate_accepted(accepted)?;
    }
    Ok(())
}

fn validate_accepted(accepted: &Accepted) -> Result<()> {
    ensure(
        !accepted.branch.is_empty()
            && accepted.branch.len() <= 100
            && accepted
                .branch
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._/-".contains(&b))
            && sha(&accepted.candidate)
            && sha(&accepted.harness)
            && accepted.run_id > 0
            && (1..=100).contains(&accepted.attempt)
            && accepted.completed_utc.len() <= 40
            && accepted.completed_utc.contains('T')
            && accepted.completed_utc.ends_with('Z')
            && accepted
                .completed_utc
                .bytes()
                .all(|b| b.is_ascii_digit() || b"-:.TZ".contains(&b))
            && !accepted.image_version.is_empty()
            && accepted.image_version.len() <= 100
            && accepted
                .image_version
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
            && accepted.reports.len() == PROFILES.len(),
        "invalid accepted-result provenance",
    )?;
    for (report, (_, profile, metric, replicates)) in accepted.reports.iter().zip(PROFILES) {
        validate_report(report, &accepted.candidate, profile, metric, replicates)?;
    }
    Ok(())
}

fn validate_report(
    report: &Value,
    candidate: &str,
    profile: &str,
    metric: &str,
    replicates: usize,
) -> Result<()> {
    ensure(
        report["schema_version"] == crate::report::SCHEMA_VERSION
            && report["status"] == "completed"
            && report["valid"] == true
            && report["decision"] == "strict-non-regression-pass"
            && report["strict_configuration"] == true
            && report["evidence_mode"]["strict"] == true
            && report["evidence_mode"]["reasons"]
                .as_array()
                .is_some_and(Vec::is_empty)
            && report["baseline_source"] == BASELINE
            && report["candidate_source"] == candidate
            && report["method"]["profile"] == profile
            && report["method"]["operations_per_timed_interval"] == 1
            && report["method"]["diagnostic_samples"] == false
            && report["method"]["prime_timings_used"] == false
            && report["method"]["aa_control"] == true
            && report["method"]["process_replicates"] == replicates
            && report["method"]["sample_size"] == 50
            && report["ab"]["passed"] == true
            && report["aa_control"]["enabled"] == true
            && report["aa_control"]["passed"] == true
            && report["anomalies"].as_array().is_some_and(Vec::is_empty),
        "only complete, strict, accepted pilot reports may be published",
    )?;
    let margin = positive(&report["method"], "non_regression_margin")?;
    let aa_margin = positive(&report["method"], "aa_equivalence_margin")?;
    ensure(
        margin <= 0.02 && aa_margin <= 0.02 && positive(&report["method"], "confidence")? >= 0.95,
        "the publication gate cannot relax the benchmark policy",
    )?;
    for (field, aa) in [("ab", false), ("aa_control", true)] {
        let summaries = array(&report[field], "summary")?;
        ensure(
            summaries.len() == 1,
            "expected one workload per pilot profile",
        )?;
        let summary = &summaries[0];
        ensure(
            text(summary, "disposition")?.len() <= 64,
            "unbounded workload disposition",
        )?;
        let lower = positive(summary, "exact_lower_ratio")?;
        let upper = positive(summary, "exact_upper_ratio")?;
        ensure(
            summary["metric"] == metric
                && array(summary, "ratios")?.len() == replicates
                && lower <= upper
                && if aa {
                    lower >= 1.0 - aa_margin && upper <= 1.0 + aa_margin
                } else {
                    upper <= 1.0 + margin
                },
            "the exact interval or workload does not satisfy the accepted gate",
        )?;
        for ratio in array(summary, "ratios")? {
            ensure(
                ratio.as_f64().is_some_and(|v| v.is_finite() && v > 0.0),
                "invalid paired ratio",
            )?;
        }
    }
    let records = array(report, "records")?;
    ensure(
        records.len() == replicates * 2,
        "incomplete A/B and A/A records",
    )?;
    let mut modes = [0usize; 2];
    for record in records {
        let mode = match text(record, "mode")? {
            "ab" => 0,
            "aa" => 1,
            _ => return Err(invalid_data("unknown comparison mode")),
        };
        modes[mode] += 1;
        ensure(
            record["metric"] == metric
                && record["samples"] == 50
                && record["failures"] == 0
                && record["warm_up_failures"] == 0
                && record["prime_failures"] == 0
                && record["outliers"].as_u64().is_some_and(|v| v <= 50),
            "incomplete or failed measurement record",
        )?;
        positive(record, "baseline_ns")?;
        positive(record, "candidate_ns")?;
    }
    ensure(modes == [replicates, replicates], "missing paired controls")?;
    let runs = array(&report["processes"], "runs")?;
    ensure(
        runs.len() == replicates * 2
            && runs.iter().all(|run| {
                run["process"]["outcome"]["kind"] == "exited"
                    && run["process"]["outcome"]["code"] == 0
            }),
        "a measurement process did not complete successfully",
    )?;
    markdown::render_report(report)?;
    Ok(())
}

fn update(state: &mut State, accepted: Accepted) -> Result<()> {
    validate_accepted(&accepted)?;
    let slot = if accepted.branch == "dev" {
        "dev"
    } else {
        "default"
    };
    if let Some(previous) = state.branches.get(slot) {
        ensure(
            (accepted.run_id, accepted.attempt) > (previous.run_id, previous.attempt),
            "refusing an older or duplicate publication",
        )?;
    }
    state.branches.insert(slot.to_owned(), accepted);
    Ok(())
}

fn publish(output: &Path, state: &State) -> Result<()> {
    validate_state(state)?;
    // The CI publisher supplies a fresh staging directory. Never replace an
    // existing directory or let report-controlled paths select output names.
    fs::create_dir(output)?;
    write_new(&output.join("accepted.json"), &serde_json::to_vec(state)?)?;
    write_new(&output.join(".nojekyll"), b"")?;
    for slot in ["dev", "default"] {
        let accepted = state.branches.get(slot);
        write_new(
            &output.join(format!("{slot}.svg")),
            svg(slot, accepted)?.as_bytes(),
        )?;
        write_new(
            &output.join(format!("{slot}.html")),
            html(slot, accepted)?.as_bytes(),
        )?;
        if let Some(accepted) = accepted {
            for (index, report) in accepted.reports.iter().enumerate() {
                write_new(
                    &output.join(format!("{slot}-report-{index}.json")),
                    &serde_json::to_vec(report)?,
                )?;
            }
        }
    }
    write_new(
        &output.join("index.html"),
        b"<!doctype html><meta charset=\"utf-8\"><title>Accepted runner benchmarks</title><h1>Accepted runner benchmarks</h1><p><a href=\"dev.html\">dev</a> | <a href=\"default.html\">Default branch</a></p>",
    )
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new().create_new(true).write(true).open(path)?;
    file.write_all(bytes)?;
    Ok(())
}

fn svg(slot: &str, accepted: Option<&Accepted>) -> Result<String> {
    let mut lines = vec![format!("Latest accepted runner benchmark | {slot}")];
    let Some(accepted) = accepted else {
        lines.push("No accepted measurements published yet.".to_owned());
        return Ok(svg_lines(&lines));
    };
    lines.push(format!(
        "Branch {} | Candidate {} | {}",
        accepted.branch, accepted.candidate, accepted.completed_utc
    ));
    lines.push(format!(
        "Windows 2022 image {} | Rust 1.98.1 | Hosted VM pilot, three workloads",
        accepted.image_version
    ));
    lines.push(
        "Baseline: fs2 v0.4.3 | Latest accepted measurement, not a current-head status badge"
            .to_owned(),
    );
    lines.push(String::new());
    let mut header = false;
    for report in &accepted.reports {
        let markdown = markdown::render_report(report)?;
        for line in markdown.lines().filter(|line| line.starts_with("| ")) {
            if line.starts_with("| Case |") {
                if header {
                    continue;
                }
                header = true;
            } else if line.starts_with("| ---") {
                continue;
            }
            // Canonical Rust formatting owns every value; the display never
            // recalculates timings or extracts numbers from runner log text.
            lines.push(line.replace('\u{60}', ""));
        }
    }
    lines.push(String::new());
    lines.push(
        "All three A/B gates and A/A controls passed. Click for provenance and retained JSON."
            .to_owned(),
    );
    Ok(svg_lines(&lines))
}

fn svg_lines(lines: &[String]) -> String {
    let height = 44 + lines.len() * 28;
    let width = 1540.max(
        48 + lines
            .iter()
            .map(|line| line.chars().count())
            .max()
            .unwrap_or(0)
            * 9,
    );
    let mut output = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{width}\" height=\"{height}\" viewBox=\"0 0 {width} {height}\" role=\"img\"><title>Latest accepted runner benchmark</title><rect width=\"{width}\" height=\"{height}\" rx=\"12\" fill=\"#f4f1e8\"/><path d=\"M20 20H1520\" stroke=\"#18635b\" stroke-width=\"4\"/><g fill=\"#182c2a\" font-family=\"Cascadia Code,DejaVu Sans Mono,monospace\" font-size=\"14\">"
    );
    for (index, line) in lines.iter().enumerate() {
        output.push_str(&format!(
            "<text x=\"24\" y=\"{}\" xml:space=\"preserve\">{}</text>",
            48 + index * 28,
            escape(line)
        ));
    }
    output.push_str("</g></svg>");
    output
}

fn html(slot: &str, accepted: Option<&Accepted>) -> Result<String> {
    let mut output = format!(
        "<!doctype html><html lang=\"en\"><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width\"><meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; style-src 'unsafe-inline'; img-src 'self'; base-uri 'none'\"><title>Accepted {slot} runner benchmarks</title><style>body{{max-width:100rem;margin:3rem auto;padding:0 1rem;background:#f4f1e8;color:#182c2a;font-family:Georgia,serif}}pre{{overflow:auto;padding:1rem;background:#fff;font-family:'Cascadia Code',monospace}}a{{color:#18635b}}</style><h1>Latest accepted {slot} runner benchmark</h1>"
    );
    if let Some(accepted) = accepted {
        output.push_str(&format!(
            "<p>Branch: {}. Recorded: {}. Candidate: <code>{}</code>. Harness: <code>{}</code>.</p><p>Windows 2022 image {}; Rust 1.98.1. Three-workload pilot on this VM, not full-suite or physical-host evidence.</p><p><a href=\"https://github.com/arthurianresolve/fs2-turbo/actions/runs/{}/attempts/{}\">Workflow and evidence</a>. Runner artifacts have limited retention; the accepted canonical JSON below is retained with this display.</p>",
            escape(&accepted.branch),
            escape(&accepted.completed_utc),
            accepted.candidate,
            accepted.harness,
            escape(&accepted.image_version),
            accepted.run_id,
            accepted.attempt
        ));
        for (index, report) in accepted.reports.iter().enumerate() {
            output.push_str(&format!(
                "<p><a href=\"{slot}-report-{index}.json\">Canonical report {}</a></p><pre>{}</pre>",
                index + 1,
                escape(&markdown::render_report(report)?)
            ));
        }
    } else {
        output.push_str("<p>No accepted measurements published yet.</p>");
    }
    output.push_str("</html>");
    Ok(output)
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn sha(value: &str) -> bool {
    value.len() == 40
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn ensure(condition: bool, message: &str) -> Result<()> {
    if condition {
        Ok(())
    } else {
        Err(invalid_data(message))
    }
}

fn text<'a>(value: &'a Value, field: &str) -> Result<&'a str> {
    value[field]
        .as_str()
        .ok_or_else(|| invalid_data(format!("missing {field} text")))
}

fn array<'a>(value: &'a Value, field: &str) -> Result<&'a Vec<Value>> {
    value[field]
        .as_array()
        .ok_or_else(|| invalid_data(format!("missing {field} array")))
}

fn positive(value: &Value, field: &str) -> Result<f64> {
    value[field]
        .as_f64()
        .filter(|v| v.is_finite() && *v > 0.0)
        .ok_or_else(|| invalid_data(format!("invalid {field} number")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn accepted(branch: &str, run_id: u64) -> Accepted {
        let reports = PROFILES.iter().map(|(_, profile, metric, replicates)| {
            let records: Vec<_> = ["ab", "aa"].into_iter().flat_map(|mode| {
                (0..*replicates).map(move |_| serde_json::json!({
                    "mode": mode, "metric": metric, "baseline_ns": 2000.0,
                    "candidate_ns": 2000.0, "samples": 50, "outliers": 0,
                    "failures": 0, "warm_up_failures": 0, "prime_failures": 0
                }))
            }).collect();
            let summary = serde_json::json!([{
                "metric": metric, "ratios": vec![1.0; *replicates],
                "exact_lower_ratio": 1.0, "exact_upper_ratio": 1.0,
                "disposition": "non-inferior"
            }]);
            serde_json::json!({
                "schema_version": 11, "report_kind": "common", "status": "completed",
                "valid": true, "decision": "strict-non-regression-pass",
                "strict_configuration": true, "evidence_mode": {"strict": true, "reasons": []},
                "baseline_source": BASELINE, "candidate_source": "a".repeat(40),
                "method": {"profile": profile, "operations_per_timed_interval": 1,
                    "diagnostic_samples": false, "prime_timings_used": false,
                    "aa_control": true, "process_replicates": replicates, "sample_size": 50,
                    "non_regression_margin": 0.02, "aa_equivalence_margin": 0.01, "confidence": 0.95},
                "ab": {"passed": true, "summary": summary},
                "aa_control": {"enabled": true, "passed": true, "summary": summary},
                "anomalies": [], "records": records,
                "processes": {"runs": vec![serde_json::json!({
                    "process": {"outcome": {"kind": "exited", "code": 0}}
                }); *replicates * 2]}
            })
        }).collect();
        Accepted {
            branch: branch.to_owned(),
            candidate: "a".repeat(40),
            harness: "b".repeat(40),
            run_id,
            attempt: 1,
            completed_utc: "2026-09-11T12:00:00Z".to_owned(),
            image_version: "20260906.1.0".to_owned(),
            reports,
        }
    }

    #[test]
    fn accepted_results_preserve_the_other_branch_and_reject_older_runs() {
        let mut state = State {
            schema_version: 1,
            ..State::default()
        };
        update(&mut state, accepted("dev", 10)).unwrap();
        update(&mut state, accepted("1.0.0", 11)).unwrap();
        update(&mut state, accepted("dev", 12)).unwrap();
        assert_eq!(state.branches["default"].run_id, 11);
        assert_eq!(state.branches["dev"].run_id, 12);
        assert!(update(&mut state, accepted("dev", 10)).is_err());
    }

    #[test]
    fn failed_inconclusive_diagnostic_and_incomplete_reports_are_never_published() {
        for (pointer, value) in [
            ("/valid", Value::Bool(false)),
            ("/ab/passed", Value::Bool(false)),
            ("/aa_control/passed", Value::Bool(false)),
            ("/aa_control/enabled", Value::Bool(false)),
            ("/strict_configuration", Value::Bool(false)),
            ("/method/diagnostic_samples", Value::Bool(true)),
            ("/decision", Value::String("invalid-aa-control".to_owned())),
            ("/method/confidence", serde_json::json!(0.90)),
            ("/records", serde_json::json!([])),
            ("/ab/summary/0/exact_upper_ratio", serde_json::json!(1.03)),
            (
                "/aa_control/summary/0/exact_lower_ratio",
                serde_json::json!(0.98),
            ),
        ] {
            let mut item = accepted("dev", 1);
            *item.reports[0].pointer_mut(pointer).unwrap() = value;
            assert!(validate_accepted(&item).is_err(), "{pointer}");
        }
    }

    #[test]
    fn display_uses_canonical_values_and_does_not_overwrite_existing_output() {
        let mut state = State {
            schema_version: 1,
            ..State::default()
        };
        update(&mut state, accepted("dev", 1)).unwrap();
        let root = tempfile::tempdir().unwrap();
        let output = root.path().join("site");
        publish(&output, &state).unwrap();
        let display = fs::read_to_string(output.join("dev.svg")).unwrap();
        assert!(display.contains("2.00 us"));
        assert!(display.contains("Latest accepted"));
        assert!(!display.contains("invalid-aa-control"));
        assert!(
            fs::read_to_string(output.join("default.svg"))
                .unwrap()
                .contains("No accepted")
        );
        assert!(publish(&output, &state).is_err());
        assert_eq!(escape("<script>&\""), "&lt;script&gt;&amp;&quot;");
    }

    #[test]
    fn branch_and_source_provenance_are_required() {
        for branch in ["", "<script>", "branch with spaces"] {
            assert!(validate_accepted(&accepted(branch, 1)).is_err());
        }
        let mut item = accepted("dev", 1);
        item.reports[1]["candidate_source"] = Value::String("c".repeat(40));
        assert!(validate_accepted(&item).is_err());
    }
}
