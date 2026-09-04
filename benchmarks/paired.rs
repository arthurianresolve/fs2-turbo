use std::io::{self, Error, ErrorKind, Write as _};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[path = "paired_protocol.rs"]
mod paired_protocol;

pub(crate) use paired_protocol::{HEADER, PROTOCOL};

const CALIBRATION_BLOCKS: usize = 16;
const MIN_SAMPLE_SIZE: usize = 10;
const MAX_SAMPLE_SIZE: usize = 10_000;
const MAX_DURATION_MILLIS: u64 = 3_600_000;
const MIN_ITERATIONS_PER_SAMPLE: u128 = 32;
const MAX_ITERATIONS_PER_SAMPLE: u128 = 100_000_000;

const DIAGNOSTIC_ENV: &str = "FS2_PAIRED_DIAGNOSTIC_SAMPLES";

struct DiagnosticStamp {
    ticks: u64,
    unix_ns: u128,
    cpu: u32,
    thread: u32,
    cpu_100ns: Option<u64>,
    cycles: Option<u64>,
}

struct DiagnosticCapture {
    frequency: u64,
    windows: Vec<(DiagnosticStamp, DiagnosticStamp)>,
}

fn diagnostic_enabled(value: Option<&std::ffi::OsStr>) -> io::Result<bool> {
    match value.and_then(std::ffi::OsStr::to_str) {
        None if value.is_none() => Ok(false),
        Some("0") => Ok(false),
        Some("1") => Ok(true),
        _ => Err(Error::new(
            ErrorKind::InvalidInput,
            "invalid diagnostic sample setting",
        )),
    }
}

#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn QueryPerformanceCounter(value: *mut i64) -> i32;
    fn QueryPerformanceFrequency(value: *mut i64) -> i32;
    fn GetCurrentProcessorNumber() -> u32;
    fn GetCurrentThreadId() -> u32;
    fn GetCurrentThread() -> *mut std::ffi::c_void;
    fn GetThreadTimes(
        thread: *mut std::ffi::c_void,
        creation: *mut FileTime,
        exit: *mut FileTime,
        kernel: *mut FileTime,
        user: *mut FileTime,
    ) -> i32;
    fn QueryThreadCycleTime(thread: *mut std::ffi::c_void, cycles: *mut u64) -> i32;
}

#[cfg(windows)]
#[derive(Default)]
#[repr(C)]
struct FileTime {
    low: u32,
    high: u32,
}

#[cfg(windows)]
fn thread_counters() -> io::Result<(Option<u64>, Option<u64>)> {
    let mut creation = FileTime::default();
    let mut exit = FileTime::default();
    let mut kernel = FileTime::default();
    let mut user = FileTime::default();
    let mut cycles = 0;
    // This borrowed pseudo-handle refers to this thread and must not be closed.
    let thread = unsafe { GetCurrentThread() };
    // Both APIs write into live, correctly aligned output storage.
    if unsafe { GetThreadTimes(thread, &mut creation, &mut exit, &mut kernel, &mut user) } == 0
        || unsafe { QueryThreadCycleTime(thread, &mut cycles) } == 0
    {
        return Err(Error::last_os_error());
    }
    let ticks = |time: FileTime| u64::from(time.low) | (u64::from(time.high) << 32);
    let cpu = ticks(kernel)
        .checked_add(ticks(user))
        .ok_or_else(|| Error::other("thread CPU accounting overflow"))?;
    Ok((Some(cpu), Some(cycles)))
}

fn diagnostic_delta(start: Option<u64>, end: Option<u64>) -> io::Result<String> {
    match (start, end) {
        (Some(start), Some(end)) => end
            .checked_sub(start)
            .map(|value| value.to_string())
            .ok_or_else(|| Error::other("diagnostic thread counter moved backwards")),
        (None, None) => Ok("unknown".to_owned()),
        _ => Err(Error::other(
            "diagnostic thread counter availability changed",
        )),
    }
}

fn diagnostic_frequency() -> io::Result<u64> {
    #[cfg(windows)]
    {
        let mut frequency = 0i64;
        // The API writes one initialized, correctly aligned LARGE_INTEGER.
        if unsafe { QueryPerformanceFrequency(&mut frequency) } == 0 || frequency <= 0 {
            return Err(Error::other(
                "diagnostic performance counter is unavailable",
            ));
        }
        Ok(frequency as u64)
    }
    #[cfg(not(windows))]
    {
        Ok(1_000_000_000)
    }
}

fn diagnostic_stamp() -> io::Result<DiagnosticStamp> {
    #[cfg(windows)]
    let (cpu_100ns, cycles) = thread_counters()?;
    #[cfg(not(windows))]
    let (cpu_100ns, cycles) = (None, None);
    #[cfg(windows)]
    let (ticks, cpu, thread) = {
        let mut ticks = 0i64;
        // These queries only observe the calling thread and its counter clock.
        if unsafe { QueryPerformanceCounter(&mut ticks) } == 0 || ticks < 0 {
            return Err(Error::other("diagnostic performance counter query failed"));
        }
        (
            ticks as u64,
            unsafe { GetCurrentProcessorNumber() },
            unsafe { GetCurrentThreadId() },
        )
    };
    let unix_ns = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(Error::other)?
        .as_nanos();
    #[cfg(not(windows))]
    let (ticks, cpu, thread) = (u64::try_from(unix_ns).map_err(Error::other)?, u32::MAX, 0);
    Ok(DiagnosticStamp {
        ticks,
        unix_ns,
        cpu,
        thread,
        cpu_100ns,
        cycles,
    })
}

impl DiagnosticCapture {
    fn new(samples: usize) -> io::Result<Self> {
        Ok(Self {
            frequency: diagnostic_frequency()?,
            windows: Vec::with_capacity(samples),
        })
    }

    fn emit(self, ratios: &[f64]) -> io::Result<()> {
        let clock = if cfg!(windows) { "qpc" } else { "unix-ns" };
        let mut stderr = io::stderr().lock();
        for (sample, ((start, end), ratio)) in self.windows.into_iter().zip(ratios).enumerate() {
            if end.ticks < start.ticks || end.unix_ns < start.unix_ns || start.thread != end.thread
            {
                return Err(Error::other("diagnostic sample clock or thread changed"));
            }
            writeln!(
                stderr,
                "fs2-sample-v2\t{}\t{}\t{sample}\t{clock}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{ratio}\t{}\t{}",
                std::process::id(),
                start.thread,
                self.frequency,
                start.ticks,
                end.ticks,
                start.unix_ns,
                end.unix_ns,
                start.cpu,
                end.cpu,
                diagnostic_delta(start.cpu_100ns, end.cpu_100ns)?,
                diagnostic_delta(start.cycles, end.cycles)?,
            )?;
        }
        Ok(())
    }
}

pub(crate) struct PairObservation {
    pub(crate) baseline_ns: u128,
    pub(crate) candidate_ns: u128,
    pub(crate) failures: u64,
}

pub(crate) struct Measurement {
    pub(crate) baseline_ns: f64,
    pub(crate) candidate_ns: f64,
    pub(crate) ratio: f64,
    pub(crate) aggregate_ratio: f64,
    pub(crate) ratio_mad: f64,
    pub(crate) samples: usize,
    pub(crate) iterations: usize,
    pub(crate) outliers: usize,
    pub(crate) warm_up_failures: u64,
    pub(crate) failures: u64,
    pub(crate) prime_baseline_ns: u128,
    pub(crate) prime_candidate_ns: u128,
    pub(crate) prime_failures: u64,
    pub(crate) ratio_samples: Vec<f64>,
}

struct BalancedOrder {
    state: u64,
    pending: Option<bool>,
}

impl BalancedOrder {
    fn new() -> Self {
        Self {
            state: 0x6a09_e667_f3bc_c909,
            pending: None,
        }
    }

    fn next(&mut self) -> bool {
        if let Some(first) = self.pending.take() {
            return !first;
        }
        // Reproducible scheduling, not security randomness. Each block still
        // contains both orders, without a fixed repeating ABBA/BAAB phase.
        self.state ^= self.state << 13;
        self.state ^= self.state >> 7;
        self.state ^= self.state << 17;
        let first = self.state & 1 == 0;
        self.pending = Some(first);
        first
    }
}

fn calibrated_iterations(
    block_timings: &mut [u128; CALIBRATION_BLOCKS],
    target_sample_ns: u128,
) -> io::Result<usize> {
    block_timings.sort_unstable();
    let middle = CALIBRATION_BLOCKS / 2;
    let lower = block_timings[middle - 1];
    let block_ns = lower + (block_timings[middle] - lower) / 2;
    let pair_ns = block_ns.div_ceil(2).max(1);
    let iterations = target_sample_ns
        .div_ceil(pair_ns)
        .max(MIN_ITERATIONS_PER_SAMPLE);
    if iterations > MAX_ITERATIONS_PER_SAMPLE {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "calibrated iteration count exceeds the safety limit",
        ));
    }
    // Complete both call orders within every sample, including slow workloads.
    let iterations = (iterations + 1) & !1;
    usize::try_from(iterations).map_err(|_| {
        Error::new(
            ErrorKind::InvalidInput,
            "calibrated iteration count is not representable",
        )
    })
}

pub(crate) fn encode_ratio_samples(samples: &[f64]) -> String {
    samples
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

pub(crate) fn measure<F>(
    sample_size: usize,
    warm_up: Duration,
    measurement: Duration,
    prime: PairObservation,
    mut observe_pair: F,
) -> io::Result<Measurement>
where
    F: FnMut(bool) -> io::Result<PairObservation>,
{
    validate_inputs(sample_size, warm_up, measurement)?;
    let mut order = BalancedOrder::new();
    let mut failures = 0u64;
    let mut warm_up_failures = 0u64;
    let warm_up_start = Instant::now();
    while warm_up_start.elapsed() < warm_up {
        let observation = observe_pair(order.next())?;
        warm_up_failures = warm_up_failures.saturating_add(observation.failures);
    }

    order = BalancedOrder::new();
    let mut block_timings = [0u128; CALIBRATION_BLOCKS];
    for block_ns in &mut block_timings {
        let started = Instant::now();
        for _ in 0..2 {
            let observation = observe_pair(order.next())?;
            failures = failures.saturating_add(observation.failures);
        }
        *block_ns = started.elapsed().as_nanos();
    }
    let iterations = calibrated_iterations(
        &mut block_timings,
        measurement.as_nanos().div_ceil(sample_size as u128),
    )?;

    order = BalancedOrder::new();
    let mut baseline_samples = Vec::with_capacity(sample_size);
    let mut candidate_samples = Vec::with_capacity(sample_size);
    let mut ratios = Vec::with_capacity(sample_size);
    // The parent explicitly sets this to 0 for every acceptance child. Capture
    // boundaries are outside timed API calls; output is deferred until sampling ends.
    let mut diagnostic = if diagnostic_enabled(std::env::var_os(DIAGNOSTIC_ENV).as_deref())? {
        Some(DiagnosticCapture::new(sample_size)?)
    } else {
        None
    };
    for _ in 0..sample_size {
        let sample_start = diagnostic
            .as_ref()
            .map(|_| diagnostic_stamp())
            .transpose()?;
        let mut baseline_ns = 0u128;
        let mut candidate_ns = 0u128;
        for _ in 0..iterations {
            let observation = observe_pair(order.next())?;
            baseline_ns += observation.baseline_ns;
            candidate_ns += observation.candidate_ns;
            failures = failures.saturating_add(observation.failures);
        }
        if let (Some(capture), Some(start)) = (&mut diagnostic, sample_start) {
            capture.windows.push((start, diagnostic_stamp()?));
        }
        let baseline_average = baseline_ns as f64 / iterations as f64;
        let candidate_average = candidate_ns as f64 / iterations as f64;
        if baseline_average <= 0.0 || candidate_average <= 0.0 {
            return Err(Error::other("timer resolution produced an empty sample"));
        }
        baseline_samples.push(baseline_average);
        candidate_samples.push(candidate_average);
        ratios.push(candidate_average / baseline_average);
    }
    if let Some(capture) = diagnostic {
        capture.emit(&ratios)?;
    }

    let baseline_ns = median(&mut baseline_samples);
    let candidate_ns = median(&mut candidate_samples);
    let ratio = median(&mut ratios.clone());
    let aggregate_ratio = candidate_ns / baseline_ns;
    let mut deviations = ratios
        .iter()
        .map(|sample_ratio| (sample_ratio - ratio).abs())
        .collect::<Vec<_>>();
    let ratio_mad = median(&mut deviations);
    let outliers = if ratio_mad == 0.0 {
        let tolerance = f64::EPSILON * ratio.abs().max(1.0);
        ratios
            .iter()
            .filter(|sample_ratio| (*sample_ratio - ratio).abs() > tolerance)
            .count()
    } else {
        ratios
            .iter()
            .filter(|sample_ratio| (*sample_ratio - ratio).abs() > 3.0 * ratio_mad)
            .count()
    };

    Ok(Measurement {
        baseline_ns,
        candidate_ns,
        ratio,
        aggregate_ratio,
        ratio_mad,
        samples: sample_size,
        iterations,
        outliers,
        warm_up_failures,
        failures,
        prime_baseline_ns: prime.baseline_ns,
        prime_candidate_ns: prime.candidate_ns,
        prime_failures: prime.failures,
        ratio_samples: ratios,
    })
}

pub(crate) fn parse_sample_size(value: Option<String>) -> io::Result<usize> {
    parse_bounded(value, "sample size", MIN_SAMPLE_SIZE, MAX_SAMPLE_SIZE)
}

pub(crate) fn parse_duration_millis(value: Option<String>, name: &str) -> io::Result<u64> {
    parse_bounded(value, name, 1, MAX_DURATION_MILLIS)
}

fn parse_bounded<T>(value: Option<String>, name: &str, minimum: T, maximum: T) -> io::Result<T>
where
    T: std::str::FromStr + PartialOrd + Copy,
{
    let value = value
        .ok_or_else(|| Error::new(ErrorKind::InvalidInput, format!("missing {name} argument")))?;
    let parsed = value
        .parse::<T>()
        .map_err(|_| Error::new(ErrorKind::InvalidInput, format!("invalid {name} argument")))?;
    if parsed < minimum || parsed > maximum {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            format!("{name} is outside the supported range"),
        ));
    }
    Ok(parsed)
}

fn validate_inputs(sample_size: usize, warm_up: Duration, measurement: Duration) -> io::Result<()> {
    if !(MIN_SAMPLE_SIZE..=MAX_SAMPLE_SIZE).contains(&sample_size)
        || warm_up.is_zero()
        || measurement.is_zero()
        || warm_up > Duration::from_millis(MAX_DURATION_MILLIS)
        || measurement > Duration::from_millis(MAX_DURATION_MILLIS)
    {
        Err(Error::new(
            ErrorKind::InvalidInput,
            "paired measurement settings are outside the supported range",
        ))
    } else {
        Ok(())
    }
}

fn median(values: &mut [f64]) -> f64 {
    values.sort_unstable_by(f64::total_cmp);
    let middle = values.len() / 2;
    if values.len().is_multiple_of(2) {
        (values[middle - 1] + values[middle]) / 2.0
    } else {
        values[middle]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostic_counters_preserve_unknown_zero_and_monotonicity() {
        assert_eq!(diagnostic_delta(None, None).unwrap(), "unknown");
        assert_eq!(diagnostic_delta(Some(3), Some(3)).unwrap(), "0");
        assert_eq!(diagnostic_delta(Some(3), Some(8)).unwrap(), "5");
        assert!(diagnostic_delta(Some(4), Some(3)).is_err());
        assert!(diagnostic_delta(None, Some(3)).is_err());
    }

    #[test]
    fn diagnostic_mode_is_explicit_and_preserves_unknown_cpu_metadata() {
        use std::ffi::OsStr;
        assert!(!diagnostic_enabled(None).unwrap());
        assert!(!diagnostic_enabled(Some(OsStr::new("0"))).unwrap());
        assert!(diagnostic_enabled(Some(OsStr::new("1"))).unwrap());
        assert!(diagnostic_enabled(Some(OsStr::new("true"))).is_err());
        let capture = DiagnosticCapture::new(50).unwrap();
        assert!(capture.frequency > 0);
        assert!(capture.windows.is_empty());
        let start = diagnostic_stamp().unwrap();
        let end = diagnostic_stamp().unwrap();
        assert!(end.ticks >= start.ticks);
        assert!(end.unix_ns >= start.unix_ns);
        #[cfg(windows)]
        assert_ne!(start.thread, 0);
        #[cfg(not(windows))]
        assert_eq!((start.cpu, start.thread), (u32::MAX, 0));
    }

    #[test]
    fn median_handles_even_and_odd_samples() {
        assert_eq!(median(&mut [3.0, 1.0, 2.0]), 2.0);
        assert_eq!(median(&mut [4.0, 1.0, 3.0, 2.0]), 2.5);
    }

    #[test]
    fn direct_arguments_are_bounded() {
        assert!(parse_sample_size(Some("9".into())).is_err());
        assert!(parse_sample_size(Some((MAX_SAMPLE_SIZE + 1).to_string())).is_err());
        assert!(
            parse_duration_millis(Some((MAX_DURATION_MILLIS + 1).to_string()), "duration").is_err()
        );
    }

    #[test]
    fn calibration_resists_an_isolated_pilot_stall() {
        let mut ordinary = [2_000; CALIBRATION_BLOCKS];
        let expected = calibrated_iterations(&mut ordinary, 100_000).unwrap();
        assert_eq!(expected, 100);
        for stalled in 0..CALIBRATION_BLOCKS {
            let mut timings = [2_000; CALIBRATION_BLOCKS];
            timings[stalled] = u128::MAX;
            assert_eq!(
                calibrated_iterations(&mut timings, 100_000).unwrap(),
                expected
            );
        }
    }

    #[test]
    fn calibration_keeps_minimum_averaging_and_complete_order_blocks() {
        let mut slow = [200_000_000; CALIBRATION_BLOCKS];
        assert_eq!(calibrated_iterations(&mut slow, 100_000_000).unwrap(), 32);
        let mut ordinary = [2_000; CALIBRATION_BLOCKS];
        assert_eq!(calibrated_iterations(&mut ordinary, 33_000).unwrap(), 34);
        let mut zero_resolution = [0; CALIBRATION_BLOCKS];
        assert_eq!(calibrated_iterations(&mut zero_resolution, 1).unwrap(), 32);
    }

    #[test]
    fn calibration_retains_iteration_limits_and_avoids_median_overflow() {
        let mut ordinary = [2_000; CALIBRATION_BLOCKS];
        assert_eq!(
            calibrated_iterations(&mut ordinary, (MAX_ITERATIONS_PER_SAMPLE - 1) * 1_000).unwrap()
                as u128,
            MAX_ITERATIONS_PER_SAMPLE
        );
        assert!(
            calibrated_iterations(&mut ordinary, (MAX_ITERATIONS_PER_SAMPLE + 1) * 1_000).is_err()
        );
        let mut extreme = [u128::MAX; CALIBRATION_BLOCKS];
        assert_eq!(calibrated_iterations(&mut extreme, u128::MAX).unwrap(), 32);
    }

    #[test]
    fn shuffled_order_is_reproducible_and_balanced_without_a_fixed_phase() {
        let mut first = BalancedOrder::new();
        let mut second = BalancedOrder::new();
        let mut starts = Vec::new();
        for _ in 0..128 {
            let start = first.next();
            assert_eq!(start, second.next());
            assert_eq!(first.next(), !start);
            assert_eq!(second.next(), !start);
            starts.push(start);
        }
        assert!(starts.contains(&true));
        assert!(starts.contains(&false));
    }

    #[test]
    fn measurement_balances_order_bias_and_retains_every_failure() {
        let mut observed_orders = Vec::new();
        let result = measure(
            MIN_SAMPLE_SIZE,
            Duration::from_nanos(1),
            Duration::from_nanos(1),
            PairObservation {
                baseline_ns: 100,
                candidate_ns: 300,
                failures: 7,
            },
            |baseline_first| {
                observed_orders.push(baseline_first);
                Ok(PairObservation {
                    baseline_ns: if baseline_first { 300 } else { 100 },
                    candidate_ns: if baseline_first { 100 } else { 300 },
                    failures: 1,
                })
            },
        )
        .unwrap();
        assert_eq!(result.iterations, 32);
        assert_eq!(result.samples, MIN_SAMPLE_SIZE);
        assert_eq!(result.baseline_ns, 200.0);
        assert_eq!(result.candidate_ns, 200.0);
        assert_eq!(result.ratio_samples, vec![1.0; MIN_SAMPLE_SIZE]);
        assert_eq!(result.outliers, 0);
        assert_eq!(result.prime_failures, 7);
        let measurement_pairs = result.samples * result.iterations;
        let calibration_pairs = CALIBRATION_BLOCKS * 2;
        assert_eq!(
            result.failures,
            (measurement_pairs + calibration_pairs) as u64
        );
        assert_eq!(
            result.warm_up_failures,
            (observed_orders.len() - measurement_pairs - calibration_pairs) as u64
        );
        let measured = &observed_orders[observed_orders.len() - measurement_pairs..];
        let mut expected_order = BalancedOrder::new();
        for &order in measured {
            assert_eq!(order, expected_order.next());
        }
        for sample in measured.chunks_exact(result.iterations) {
            assert_eq!(
                sample.iter().filter(|&&order| order).count(),
                result.iterations / 2
            );
        }
    }

    #[test]
    fn observation_errors_are_not_retried_or_suppressed() {
        let mut calls = 0;
        let result = measure(
            MIN_SAMPLE_SIZE,
            Duration::from_nanos(1),
            Duration::from_nanos(1),
            PairObservation {
                baseline_ns: 1,
                candidate_ns: 1,
                failures: 0,
            },
            |_| {
                calls += 1;
                Err(Error::new(
                    ErrorKind::PermissionDenied,
                    "observation denied",
                ))
            },
        );
        match result {
            Err(error) => assert_eq!(error.kind(), ErrorKind::PermissionDenied),
            Ok(_) => panic!("observation failure was suppressed"),
        }
        assert_eq!(calls, 1);
    }
}
