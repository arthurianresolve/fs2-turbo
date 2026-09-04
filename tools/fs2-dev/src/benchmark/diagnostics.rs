use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use clap::ArgMatches;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use super::{noise, paired, statistics};
use crate::{Result, invalid_data, lower_hex};

const MAX_INPUT_BYTES: u64 = 16 * 1024 * 1024;
const MAX_EVENTS: usize = 100_000;
const MAX_SAMPLE_INPUTS: usize = 64;
const MAX_TOTAL_SAMPLE_BYTES: usize = 64 * 1024 * 1024;
const MAX_TOTAL_WINDOWS: usize = 100_000;

#[derive(Debug, Serialize)]
struct Window {
    process_id: u32,
    thread_id: u32,
    sample: usize,
    clock: String,
    frequency: u64,
    start_ticks: u64,
    end_ticks: u64,
    start_unix_ns: u64,
    end_unix_ns: u64,
    start_cpu: u32,
    end_cpu: u32,
    ratio: f64,
    thread_cpu_100ns: Option<u64>,
    thread_cycles: Option<u64>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
enum EventKind {
    ContextSwitch,
    Dpc,
    Isr,
    Power,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Event {
    start_unix_ns: u64,
    end_unix_ns: u64,
    cpu: u32,
    kind: EventKind,
}

#[derive(Debug, Default, PartialEq, Eq, Serialize)]
struct Counts {
    context_switch: u64,
    dpc: u64,
    isr: u64,
    power: u64,
}

#[derive(Default)]
struct EventTimes {
    points: Vec<u64>,
    starts: Vec<u64>,
    ends: Vec<u64>,
}

impl EventTimes {
    fn insert(&mut self, event: &Event) {
        if event.start_unix_ns == event.end_unix_ns {
            self.points.push(event.start_unix_ns);
        } else {
            self.starts.push(event.start_unix_ns);
            self.ends.push(event.end_unix_ns);
        }
    }

    fn sort(&mut self) {
        self.points.sort_unstable();
        self.starts.sort_unstable();
        self.ends.sort_unstable();
    }

    fn overlapping(&self, start_unix_ns: u64, end_unix_ns: u64) -> u64 {
        let points = self
            .points
            .partition_point(|timestamp| *timestamp < end_unix_ns)
            - self
                .points
                .partition_point(|timestamp| *timestamp < start_unix_ns);
        let started_before_end = self
            .starts
            .partition_point(|timestamp| *timestamp < end_unix_ns);
        let ended_by_start = self
            .ends
            .partition_point(|timestamp| *timestamp <= start_unix_ns);
        debug_assert!(ended_by_start <= started_before_end);
        (points + started_before_end - ended_by_start) as u64
    }
}

#[derive(Default)]
struct CpuEvents {
    context_switch: EventTimes,
    dpc: EventTimes,
    isr: EventTimes,
    power: EventTimes,
}

impl CpuEvents {
    fn times_mut(&mut self, kind: EventKind) -> &mut EventTimes {
        match kind {
            EventKind::ContextSwitch => &mut self.context_switch,
            EventKind::Dpc => &mut self.dpc,
            EventKind::Isr => &mut self.isr,
            EventKind::Power => &mut self.power,
        }
    }

    fn sort(&mut self) {
        self.context_switch.sort();
        self.dpc.sort();
        self.isr.sort();
        self.power.sort();
    }

    fn counts(&self, start_unix_ns: u64, end_unix_ns: u64) -> Counts {
        Counts {
            context_switch: self.context_switch.overlapping(start_unix_ns, end_unix_ns),
            dpc: self.dpc.overlapping(start_unix_ns, end_unix_ns),
            isr: self.isr.overlapping(start_unix_ns, end_unix_ns),
            power: self.power.overlapping(start_unix_ns, end_unix_ns),
        }
    }
}

#[derive(Default)]
struct EventIndex {
    by_cpu: BTreeMap<u32, CpuEvents>,
}

impl EventIndex {
    fn new(events: &[Event]) -> Self {
        let mut index = Self::default();
        for event in events {
            index
                .by_cpu
                .entry(event.cpu)
                .or_default()
                .times_mut(event.kind)
                .insert(event);
        }
        for events in index.by_cpu.values_mut() {
            events.sort();
        }
        index
    }

    fn counts(&self, cpu: u32, start_unix_ns: u64, end_unix_ns: u64) -> Counts {
        self.by_cpu
            .get(&cpu)
            .map(|events| events.counts(start_unix_ns, end_unix_ns))
            .unwrap_or_default()
    }
}

fn windows(text: &str) -> Result<Vec<Window>> {
    let mut windows = Vec::new();
    let mut seen = BTreeSet::new();
    for line in text.lines() {
        if !line.starts_with("fs2-sample-") {
            continue;
        }
        let fields = line.split('\t').collect::<Vec<_>>();
        let v2 = fields[0] == "fs2-sample-v2";
        if !((v2 && fields.len() == 15) || (fields[0] == "fs2-sample-v1" && fields.len() == 13)) {
            return Err(invalid_data(
                "unsupported or malformed diagnostic sample row",
            ));
        }
        let window = Window {
            process_id: fields[1].parse()?,
            thread_id: fields[2].parse()?,
            sample: fields[3].parse()?,
            clock: fields[4].to_owned(),
            frequency: fields[5].parse()?,
            start_ticks: fields[6].parse()?,
            end_ticks: fields[7].parse()?,
            start_unix_ns: fields[8].parse()?,
            end_unix_ns: fields[9].parse()?,
            start_cpu: fields[10].parse()?,
            end_cpu: fields[11].parse()?,
            ratio: fields[12].parse()?,
            thread_cpu_100ns: if v2 { counter(fields[13])? } else { None },
            thread_cycles: if v2 { counter(fields[14])? } else { None },
        };
        if !matches!(window.clock.as_str(), "qpc" | "unix-ns")
            || window.frequency == 0
            || window.process_id == 0
            || window.end_ticks < window.start_ticks
            || window.end_unix_ns < window.start_unix_ns
            || !window.ratio.is_finite()
            || window.ratio <= 0.0
            || window.sample >= crate::policy::MAX_SAMPLE_SIZE as usize
            || windows.len() >= crate::policy::MAX_SAMPLE_SIZE as usize
            || window.thread_cpu_100ns.is_some() != window.thread_cycles.is_some()
            || (window.thread_cpu_100ns.is_some()
                && (window.clock != "qpc" || window.thread_id == 0))
            || !seen.insert((window.process_id, window.sample))
        {
            return Err(invalid_data(
                "invalid or duplicate diagnostic sample window",
            ));
        }
        windows.push(window);
    }
    if windows.is_empty() {
        return Err(invalid_data("input has no diagnostic sample windows"));
    }
    Ok(windows)
}

fn counter(text: &str) -> Result<Option<u64>> {
    if text == "unknown" {
        Ok(None)
    } else {
        Ok(Some(text.parse()?))
    }
}

fn events(text: &str) -> Result<Vec<Event>> {
    let mut result = Vec::new();
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        if result.len() == MAX_EVENTS {
            return Err(invalid_data(
                "normalized trace event count exceeds its limit",
            ));
        }
        let event: Event = serde_json::from_str(line)?;
        if event.end_unix_ns < event.start_unix_ns || event.cpu == u32::MAX {
            return Err(invalid_data("invalid normalized trace interval or CPU"));
        }
        result.push(event);
    }
    Ok(result)
}

fn validate_sample_input_count(count: usize) -> Result<()> {
    if count == 0 || count > MAX_SAMPLE_INPUTS {
        return Err(invalid_data(
            "diagnostic sample file count exceeds its limit",
        ));
    }
    Ok(())
}

fn add_sample_totals(
    total_bytes: &mut usize,
    total_windows: &mut usize,
    bytes: usize,
    windows: usize,
) -> Result<()> {
    *total_bytes = total_bytes
        .checked_add(bytes)
        .ok_or_else(|| invalid_data("aggregate diagnostic sample input exceeds its limit"))?;
    *total_windows = total_windows
        .checked_add(windows)
        .ok_or_else(|| invalid_data("aggregate diagnostic sample input exceeds its limit"))?;
    if *total_bytes > MAX_TOTAL_SAMPLE_BYTES || *total_windows > MAX_TOTAL_WINDOWS {
        return Err(invalid_data(
            "aggregate diagnostic sample input exceeds its limit",
        ));
    }
    Ok(())
}

fn correlate(window: &Window, events: &EventIndex) -> Option<Counts> {
    if window.start_cpu == u32::MAX || window.start_cpu != window.end_cpu {
        return None;
    }
    let counter_ns =
        (window.end_ticks - window.start_ticks) as f64 * 1_000_000_000.0 / window.frequency as f64;
    let wall_ns = (window.end_unix_ns - window.start_unix_ns) as f64;
    if (counter_ns - wall_ns).abs() > 1_000_000.0 {
        return None;
    }
    Some(events.counts(window.start_cpu, window.start_unix_ns, window.end_unix_ns))
}

pub(crate) fn run(arguments: &ArgMatches) -> Result<()> {
    let events_path = arguments.get_one::<PathBuf>("events");
    let events_text = events_path
        .map(|path| paired::read_bounded_utf8(path, MAX_INPUT_BYTES))
        .transpose()?;
    let trace_events = events_text
        .as_deref()
        .map(events)
        .transpose()?
        .unwrap_or_default();
    let event_index = EventIndex::new(&trace_events);
    let sample_paths = arguments
        .get_many::<PathBuf>("samples")
        .expect("required by clap");
    validate_sample_input_count(sample_paths.len())?;
    let mut total_sample_bytes = 0;
    let mut total_windows = 0;
    let mut inputs = Vec::new();
    for path in sample_paths {
        let text = paired::read_bounded_utf8(path, MAX_INPUT_BYTES)?;
        let windows = windows(&text)?;
        add_sample_totals(
            &mut total_sample_bytes,
            &mut total_windows,
            text.len(),
            windows.len(),
        )?;
        if windows
            .iter()
            .any(|window| window.process_id != windows[0].process_id)
        {
            return Err(invalid_data(
                "supply one process log per --samples argument",
            ));
        }
        let ratios = windows
            .iter()
            .map(|window| window.ratio)
            .collect::<Vec<_>>();
        let median = statistics::median(&mut ratios.clone())?;
        let mad = statistics::median_absolute_deviation(&ratios)?;
        let threshold = if mad == 0.0 {
            f64::EPSILON * median.abs().max(1.0)
        } else {
            3.0 * mad
        };
        let samples = windows
            .iter()
            .map(|window| {
                serde_json::json!({
                    "window": window,
                    "relative_3mad_outlier": (window.ratio - median).abs() > threshold,
                    "cpu_event_counts": events_path.and_then(|_| correlate(window, &event_index)),
                })
            })
            .collect::<Vec<_>>();
        inputs.push(serde_json::json!({
            "source_sha256": lower_hex(Sha256::digest(text.as_bytes())),
            "noise": noise::summarize(&ratios)?,
            "samples": samples,
        }));
    }
    let output = serde_json::json!({
        "schema_version": 2,
        "kind": "diagnostic-correlation",
        "performance_evidence": false,
        "trace_supplied": events_path.is_some(),
        "events_sha256": events_text.as_ref().map(|text| lower_hex(Sha256::digest(text.as_bytes()))),
        "event_count": trace_events.len(),
        "limitations": "CPU-wide event overlap is correlation, not thread-wait attribution or causation. Trace coverage/loss must be assessed separately. Null counts mean no events input, CPU migration/unknown CPU, or clock disagreement over 1 ms. Power events do not establish effective frequency.",
        "thread_counter_limitations": "CPU time uses coarse accounting in 100 ns units, not 100 ns resolution; its delta may exceed the wall interval and is not clamped. Raw cycle counts must not be converted to time. Counters cover the entire sample, both A/B sides and harness work; they cannot attribute a delay to a side or identify DPC/ISR causes. Null means unavailable or an older v1 row, not zero.",
        "inputs": inputs,
    });
    serde_json::to_writer_pretty(std::io::stdout().lock(), &output)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v2_counters_are_optional_accounting_not_wall_time() {
        let text = "fs2-sample-v2\t1\t2\t0\tqpc\t1000000000\t100\t200\t100\t200\t3\t3\t1.01\t156250\t900\n";
        let rows = windows(text).unwrap();
        assert_eq!(rows[0].thread_cpu_100ns, Some(156_250));
        assert_eq!(rows[0].thread_cycles, Some(900));
        let unknown = text.replace("156250\t900", "unknown\tunknown");
        assert!(windows(&unknown).unwrap()[0].thread_cpu_100ns.is_none());
        assert!(windows(&text.replace("156250\t900", "unknown\t900")).is_err());
        assert!(windows(&text.replace("156250", "-1")).is_err());
        assert!(windows(&text.replace("qpc", "unix-ns")).is_err());
    }

    fn window() -> Window {
        windows("fs2-sample-v1\t1\t2\t0\tqpc\t1000000000\t100\t200\t100\t200\t3\t3\t1.01\n")
            .unwrap()
            .remove(0)
    }

    #[test]
    fn sample_parser_rejects_bad_clocks_nonfinite_ratios_and_duplicates() {
        let text = "fs2-sample-v1\t1\t2\t0\tqpc\t1000000000\t100\t200\t100\t200\t3\t3\t1.01\n";
        assert!(windows(&text.replace("qpc", "unknown")).is_err());
        assert!(windows(&text.replace("1.01", "NaN")).is_err());
        assert!(windows(&text.repeat(2)).is_err());
        assert!(windows("ordinary stderr only").is_err());
    }

    #[test]
    fn aggregate_sample_limits_are_enforced() {
        assert!(validate_sample_input_count(1).is_ok());
        assert!(validate_sample_input_count(MAX_SAMPLE_INPUTS).is_ok());
        assert!(validate_sample_input_count(MAX_SAMPLE_INPUTS + 1).is_err());

        let mut bytes = 0;
        let mut windows = 0;
        assert!(
            add_sample_totals(
                &mut bytes,
                &mut windows,
                MAX_TOTAL_SAMPLE_BYTES,
                MAX_TOTAL_WINDOWS,
            )
            .is_ok()
        );
        assert!(add_sample_totals(&mut bytes, &mut windows, 1, 0).is_err());

        let mut bytes = 0;
        let mut windows = 0;
        assert!(add_sample_totals(&mut bytes, &mut windows, 0, MAX_TOTAL_WINDOWS + 1).is_err());
    }

    #[test]
    fn correlation_obeys_cpu_and_half_open_sample_boundaries() {
        let events = events(concat!(
            "{\"start_unix_ns\":100,\"end_unix_ns\":100,\"cpu\":3,\"kind\":\"context-switch\"}\n",
            "{\"start_unix_ns\":190,\"end_unix_ns\":210,\"cpu\":3,\"kind\":\"dpc\"}\n",
            "{\"start_unix_ns\":200,\"end_unix_ns\":200,\"cpu\":3,\"kind\":\"isr\"}\n",
            "{\"start_unix_ns\":150,\"end_unix_ns\":150,\"cpu\":4,\"kind\":\"isr\"}\n"
        ))
        .unwrap();
        let index = EventIndex::new(&events);
        let counts = correlate(&window(), &index).unwrap();
        assert_eq!((counts.context_switch, counts.dpc, counts.isr), (1, 1, 0));
        let mut migrated = window();
        migrated.end_cpu = 4;
        assert!(correlate(&migrated, &index).is_none());
        migrated.end_cpu = 3;
        migrated.end_unix_ns += 2_000_000;
        assert!(correlate(&migrated, &index).is_none());
    }

    fn reference_counts(
        cpu: u32,
        start_unix_ns: u64,
        end_unix_ns: u64,
        events: &[Event],
    ) -> Counts {
        let mut counts = Counts::default();
        for event in events {
            let overlaps = if event.start_unix_ns == event.end_unix_ns {
                event.start_unix_ns >= start_unix_ns && event.start_unix_ns < end_unix_ns
            } else {
                event.start_unix_ns < end_unix_ns && event.end_unix_ns > start_unix_ns
            };
            if event.cpu == cpu && overlaps {
                match event.kind {
                    EventKind::ContextSwitch => counts.context_switch += 1,
                    EventKind::Dpc => counts.dpc += 1,
                    EventKind::Isr => counts.isr += 1,
                    EventKind::Power => counts.power += 1,
                }
            }
        }
        counts
    }

    #[test]
    fn indexed_correlation_matches_reference_for_unsorted_events() {
        let events = events(concat!(
            "{\"start_unix_ns\":200,\"end_unix_ns\":200,\"cpu\":3,\"kind\":\"isr\"}\n",
            "{\"start_unix_ns\":50,\"end_unix_ns\":101,\"cpu\":3,\"kind\":\"power\"}\n",
            "{\"start_unix_ns\":150,\"end_unix_ns\":150,\"cpu\":4,\"kind\":\"dpc\"}\n",
            "{\"start_unix_ns\":100,\"end_unix_ns\":100,\"cpu\":3,\"kind\":\"context-switch\"}\n",
            "{\"start_unix_ns\":190,\"end_unix_ns\":210,\"cpu\":3,\"kind\":\"dpc\"}\n",
            "{\"start_unix_ns\":50,\"end_unix_ns\":100,\"cpu\":3,\"kind\":\"context-switch\"}\n"
        ))
        .unwrap();
        let index = EventIndex::new(&events);
        for (start, end, cpu) in [(100, 200, 3), (200, 200, 3), (0, 1_000, 4)] {
            assert_eq!(
                index.counts(cpu, start, end),
                reference_counts(cpu, start, end, &events)
            );
        }
    }
}
