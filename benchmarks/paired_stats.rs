mod paired;
#[path = "paired_stats_protocol.rs"]
mod paired_stats_protocol;

use std::hint::black_box;
use std::io::{self, Error, ErrorKind};
use std::path::PathBuf;
use std::time::{Duration, Instant};

type Value = [u64; 4];

#[cfg(not(feature = "prepared-query"))]
const METRICS: &[&str] = &paired_stats_protocol::COMMON_METRICS;
#[cfg(feature = "prepared-query")]
const METRICS: &[&str] = &paired_stats_protocol::METRICS;

#[derive(Clone, Copy)]
enum Comparison {
    Ab,
    Aa,
}

impl Comparison {
    fn parse(value: &str) -> io::Result<Self> {
        match value {
            "ab" => Ok(Self::Ab),
            "aa" => Ok(Self::Aa),
            _ => Err(Error::new(
                ErrorKind::InvalidInput,
                "comparison must be 'ab' or 'aa'",
            )),
        }
    }
}

#[derive(Clone, Copy)]
enum Workload {
    FreeSpace,
    AvailableSpace,
    TotalSpace,
    AllocationGranularity,
    Snapshot,
    #[cfg(feature = "prepared-query")]
    QueryConstruction,
    #[cfg(feature = "prepared-query")]
    PreparedSnapshot,
}

impl Workload {
    const fn name(self) -> &'static str {
        METRICS[self as usize]
    }
}

struct Context {
    path: PathBuf,
    #[cfg(feature = "prepared-query")]
    baseline_query: Option<fs2_baseline::FsStatsQuery>,
    #[cfg(feature = "prepared-query")]
    candidate_query: Option<fs2_candidate::FsStatsQuery>,
}

impl Context {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            #[cfg(feature = "prepared-query")]
            baseline_query: None,
            #[cfg(feature = "prepared-query")]
            candidate_query: None,
        }
    }

    #[cfg(feature = "prepared-query")]
    fn prepare_queries(&mut self, comparison: Comparison) -> io::Result<()> {
        self.baseline_query = Some(fs2_baseline::FsStatsQuery::new(&self.path)?);
        if matches!(comparison, Comparison::Ab) {
            self.candidate_query = Some(fs2_candidate::FsStatsQuery::new(&self.path)?);
        }
        Ok(())
    }
}

#[inline]
fn baseline(context: &Context, workload: Workload) -> io::Result<Value> {
    match workload {
        Workload::FreeSpace => {
            fs2_baseline::free_space(&context.path).map(|value| [value, 0, 0, 0])
        }
        Workload::AvailableSpace => {
            fs2_baseline::available_space(&context.path).map(|value| [value, 0, 0, 0])
        }
        Workload::TotalSpace => {
            fs2_baseline::total_space(&context.path).map(|value| [value, 0, 0, 0])
        }
        Workload::AllocationGranularity => fs2_baseline::allocation_granularity(&context.path)
            .map(|value| [value, 0, 0, 0]),
        Workload::Snapshot => fs2_baseline::statvfs(&context.path).map(|stats| {
            [
                stats.free_space(),
                stats.available_space(),
                stats.total_space(),
                stats.allocation_granularity(),
            ]
        }),
        #[cfg(feature = "prepared-query")]
        Workload::QueryConstruction => fs2_baseline::FsStatsQuery::new(&context.path).map(|query| {
            black_box(query);
            [1, 0, 0, 0]
        }),
        #[cfg(feature = "prepared-query")]
        Workload::PreparedSnapshot => context
            .baseline_query
            .as_ref()
            .ok_or_else(|| Error::other("baseline query was not prepared"))?
            .snapshot()
            .map(|stats| {
            [
                stats.free_space(),
                stats.available_space(),
                stats.total_space(),
                stats.allocation_granularity(),
            ]
        }),
    }
}

#[inline]
fn candidate(context: &Context, workload: Workload) -> io::Result<Value> {
    match workload {
        Workload::FreeSpace => {
            fs2_candidate::free_space(&context.path).map(|value| [value, 0, 0, 0])
        }
        Workload::AvailableSpace => {
            fs2_candidate::available_space(&context.path).map(|value| [value, 0, 0, 0])
        }
        Workload::TotalSpace => {
            fs2_candidate::total_space(&context.path).map(|value| [value, 0, 0, 0])
        }
        Workload::AllocationGranularity => fs2_candidate::allocation_granularity(&context.path)
            .map(|value| [value, 0, 0, 0]),
        Workload::Snapshot => fs2_candidate::statvfs(&context.path).map(|stats| {
            [
                stats.free_space(),
                stats.available_space(),
                stats.total_space(),
                stats.allocation_granularity(),
            ]
        }),
        #[cfg(feature = "prepared-query")]
        Workload::QueryConstruction => fs2_candidate::FsStatsQuery::new(&context.path).map(|query| {
            black_box(query);
            [1, 0, 0, 0]
        }),
        #[cfg(feature = "prepared-query")]
        Workload::PreparedSnapshot => context
            .candidate_query
            .as_ref()
            .ok_or_else(|| Error::other("candidate query was not prepared"))?
            .snapshot()
            .map(|stats| {
            [
                stats.free_space(),
                stats.available_space(),
                stats.total_space(),
                stats.allocation_granularity(),
            ]
        }),
    }
}

#[inline]
fn observe_subject<S>(
    context: &Context,
    workload: Workload,
    subject: &S,
) -> (u128, io::Result<Value>)
where
    S: Fn(&Context, Workload) -> io::Result<Value>,
{
    let start = Instant::now();
    let result = black_box(subject(context, workload));
    (start.elapsed().as_nanos(), result)
}

fn compare_results(
    workload: Workload,
    baseline_result: io::Result<Value>,
    candidate_result: io::Result<Value>,
    failure_samples: &mut Vec<String>,
) -> u64 {
    match (baseline_result, candidate_result) {
        (Ok(baseline_value), Ok(candidate_value)) => {
            let compatible = match workload {
                Workload::FreeSpace | Workload::AvailableSpace => {
                    nearby_space_values(baseline_value[0], candidate_value[0])
                }
                Workload::TotalSpace => baseline_value[0] == candidate_value[0],
                Workload::AllocationGranularity => {
                    baseline_value[0] > 0 && baseline_value[0] == candidate_value[0]
                }
                #[cfg(feature = "prepared-query")]
                Workload::QueryConstruction => baseline_value == candidate_value,
                Workload::Snapshot => snapshots_compatible(baseline_value, candidate_value),
                #[cfg(feature = "prepared-query")]
                Workload::PreparedSnapshot => {
                    snapshots_compatible(baseline_value, candidate_value)
                }
            };
            if !compatible && failure_samples.len() < 8 {
                failure_samples.push(format!(
                    "{} returned incompatible values: baseline={baseline_value:?}, candidate={candidate_value:?}",
                    workload.name()
                ));
            }
            u64::from(!compatible)
        }
        (baseline_result, candidate_result) => {
            if failure_samples.len() < 8 {
                failure_samples.push(format!(
                    "{} failed: baseline={baseline_result:?}, candidate={candidate_result:?}",
                    workload.name()
                ));
            }
            1
        }
    }
}

fn snapshots_compatible(baseline: Value, candidate: Value) -> bool {
    valid_snapshot(baseline)
        && valid_snapshot(candidate)
        && baseline[2..] == candidate[2..]
        && nearby_space_values(baseline[0], candidate[0])
        && nearby_space_values(baseline[1], candidate[1])
}

fn observe_prime<B, C>(
    context: &Context,
    workload: Workload,
    baseline_subject: B,
    candidate_subject: C,
    failure_samples: &mut Vec<String>,
) -> paired::PairObservation
where
    B: Fn(&Context, Workload) -> io::Result<Value>,
    C: Fn(&Context, Workload) -> io::Result<Value>,
{
    let (baseline_ns, baseline_result) = observe_subject(context, workload, &baseline_subject);
    let (candidate_ns, candidate_result) = observe_subject(context, workload, &candidate_subject);
    let failures = compare_results(
        workload,
        baseline_result,
        candidate_result,
        failure_samples,
    );
    paired::PairObservation {
        baseline_ns,
        candidate_ns,
        failures,
    }
}

fn observe_pair<B, C>(
    context: &Context,
    workload: Workload,
    baseline_subject: B,
    candidate_subject: C,
    baseline_first: bool,
    failure_samples: &mut Vec<String>,
) -> paired::PairObservation
where
    B: Fn(&Context, Workload) -> io::Result<Value>,
    C: Fn(&Context, Workload) -> io::Result<Value>,
{
    let (baseline_first_sample, baseline_second_sample, candidate_first_sample, candidate_second_sample) =
        if baseline_first {
            let baseline_first_sample = observe_subject(context, workload, &baseline_subject);
            let candidate_first_sample = observe_subject(context, workload, &candidate_subject);
            let candidate_second_sample = observe_subject(context, workload, &candidate_subject);
            let baseline_second_sample = observe_subject(context, workload, &baseline_subject);
            (
                baseline_first_sample,
                baseline_second_sample,
                candidate_first_sample,
                candidate_second_sample,
            )
        } else {
            let candidate_first_sample = observe_subject(context, workload, &candidate_subject);
            let baseline_first_sample = observe_subject(context, workload, &baseline_subject);
            let baseline_second_sample = observe_subject(context, workload, &baseline_subject);
            let candidate_second_sample = observe_subject(context, workload, &candidate_subject);
            (
                baseline_first_sample,
                baseline_second_sample,
                candidate_first_sample,
                candidate_second_sample,
            )
        };
    let failures = compare_results(
        workload,
        baseline_first_sample.1,
        candidate_first_sample.1,
        failure_samples,
    )
    .saturating_add(compare_results(
        workload,
        baseline_second_sample.1,
        candidate_second_sample.1,
        failure_samples,
    ));
    paired::PairObservation {
        baseline_ns: baseline_first_sample
            .0
            .saturating_add(baseline_second_sample.0)
            / 2,
        candidate_ns: candidate_first_sample
            .0
            .saturating_add(candidate_second_sample.0)
            / 2,
        failures,
    }
}

fn valid_snapshot(value: Value) -> bool {
    value[2] > 0 && value[3] > 0 && value[1] <= value[0] && value[0] <= value[2]
}

fn nearby_space_values(baseline: u64, candidate: u64) -> bool {
    const ACTIVITY_ALLOWANCE: u64 = 256 * 1024 * 1024;
    let relative_allowance = baseline.max(candidate) / 100;
    baseline.abs_diff(candidate) <= ACTIVITY_ALLOWANCE.max(relative_allowance)
}

fn measure(
    context: &Context,
    workload: Workload,
    comparison: Comparison,
    sample_size: usize,
    warm_up: Duration,
    measurement: Duration,
) -> io::Result<paired::Measurement> {
    match comparison {
        Comparison::Ab => measure_subjects(
            context,
            workload,
            baseline,
            candidate,
            sample_size,
            warm_up,
            measurement,
        ),
        Comparison::Aa => measure_subjects(
            context,
            workload,
            baseline,
            baseline,
            sample_size,
            warm_up,
            measurement,
        ),
    }
}

fn measure_subjects<B, C>(
    context: &Context,
    workload: Workload,
    baseline_subject: B,
    candidate_subject: C,
    sample_size: usize,
    warm_up: Duration,
    measurement: Duration,
) -> io::Result<paired::Measurement>
where
    B: Copy + Fn(&Context, Workload) -> io::Result<Value>,
    C: Copy + Fn(&Context, Workload) -> io::Result<Value>,
{
    let mut failure_samples = Vec::new();
    let prime = observe_prime(
        context,
        workload,
        baseline_subject,
        candidate_subject,
        &mut failure_samples,
    );
    let result = paired::measure(
        sample_size,
        warm_up,
        measurement,
        prime,
        |baseline_first| {
            Ok(observe_pair(
                context,
                workload,
                baseline_subject,
                candidate_subject,
                baseline_first,
                &mut failure_samples,
            ))
        },
    );
    for failure in failure_samples {
        eprintln!("measurement failure: {failure}");
    }
    result
}

fn main() -> io::Result<()> {
    let mut args = std::env::args();
    let _program = args.next();
    let path = args
        .next()
        .map(PathBuf::from)
        .ok_or_else(|| Error::new(ErrorKind::InvalidInput, "missing fixture path"))?;
    let comparison = Comparison::parse(
        &args
            .next()
            .ok_or_else(|| Error::new(ErrorKind::InvalidInput, "missing comparison"))?,
    )?;
    let sample_size = paired::parse_sample_size(args.next())?;
    let warm_up_ms = paired::parse_duration_millis(args.next(), "warm-up milliseconds")?;
    let measurement_ms = paired::parse_duration_millis(args.next(), "measurement milliseconds")?;
    let rotation: usize = args
        .next()
        .ok_or_else(|| Error::new(ErrorKind::InvalidInput, "missing workload rotation"))?
        .parse()
        .map_err(|_| Error::new(ErrorKind::InvalidInput, "invalid workload rotation"))?;
    if args.next().is_some() {
        return Err(Error::new(ErrorKind::InvalidInput, "too many arguments"));
    }
    if !path.exists() {
        return Err(Error::new(ErrorKind::NotFound, "fixture path is missing"));
    }

    #[cfg(feature = "prepared-query")]
    let mut context = Context::new(path);
    #[cfg(not(feature = "prepared-query"))]
    let context = Context::new(path);
    let warm_up = Duration::from_millis(warm_up_ms);
    let measurement = Duration::from_millis(measurement_ms);
    println!("{}", paired::PROTOCOL);
    println!("{}", paired::HEADER);
    let mut failures = 0u64;
    #[cfg(feature = "prepared-query")]
    let query_construction = measure(
        &context,
        Workload::QueryConstruction,
        comparison,
        sample_size,
        warm_up,
        measurement,
    )?;
    #[cfg(feature = "prepared-query")]
    {
        failures += query_construction.warm_up_failures
            + query_construction.failures
            + query_construction.prime_failures;
        print_measurement(Workload::QueryConstruction, &query_construction);
        context.prepare_queries(comparison)?;
    }
    #[cfg(feature = "prepared-query")]
    let remaining = [
        Workload::FreeSpace,
        Workload::AvailableSpace,
        Workload::TotalSpace,
        Workload::AllocationGranularity,
        Workload::Snapshot,
        Workload::PreparedSnapshot,
    ];
    #[cfg(not(feature = "prepared-query"))]
    let remaining = [
        Workload::FreeSpace,
        Workload::AvailableSpace,
        Workload::TotalSpace,
        Workload::AllocationGranularity,
        Workload::Snapshot,
    ];
    for offset in 0..remaining.len() {
        let workload = remaining[(offset + rotation) % remaining.len()];
        let result = measure(
            &context,
            workload,
            comparison,
            sample_size,
            warm_up,
            measurement,
        )?;
        failures += result.warm_up_failures + result.failures + result.prime_failures;
        print_measurement(workload, &result);
    }
    if failures != 0 {
        return Err(Error::other(format!(
            "{failures} filesystem-stat measurements failed"
        )));
    }
    Ok(())
}

fn print_measurement(workload: Workload, result: &paired::Measurement) {
    println!(
        "{}\t{:.6}\t{:.6}\t{:.9}\t{:.9}\t{:.9}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
        workload.name(),
        result.baseline_ns,
        result.candidate_ns,
        result.ratio,
        result.aggregate_ratio,
        result.ratio_mad,
        result.samples,
        result.iterations,
        result.outliers,
        result.warm_up_failures,
        result.failures,
        result.prime_baseline_ns,
        result.prime_candidate_ns,
        result.prime_failures,
        paired::encode_ratio_samples(&result.ratio_samples),
    );
}
