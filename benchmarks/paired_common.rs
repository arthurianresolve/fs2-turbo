mod paired;
#[path = "paired_stats_protocol.rs"]
mod protocol;

use std::cell::Cell;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::hint::black_box;
use std::io::{self, Error, ErrorKind};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const FILE_SIZE: u64 = 32 * 1024 * 1024;
const PATH_COUNT: usize = 4096;
type Value = [u64; 4];
type Operation = fn(&Context, &str) -> io::Result<Value>;

struct Context {
    fixture: PathBuf,
    volume_root: PathBuf,
    allocated: File,
    lock: File,
    file_path: PathBuf,
    reusable_path: PathBuf,
    create_paths: Vec<PathBuf>,
    create_index: Cell<usize>,
    _temporary: tempfile::TempDir,
}

fn open_file(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
}

#[cfg(windows)]
fn volume_root(path: &Path) -> io::Result<PathBuf> {
    use std::path::{Component, Prefix};

    // Canonical fixture paths use a verbatim prefix; the PR's root workload
    // specifically measures the ordinary drive-root spelling and its fast path.
    match path.components().next() {
        Some(Component::Prefix(prefix)) => match prefix.kind() {
            Prefix::Disk(drive) | Prefix::VerbatimDisk(drive) => {
                Ok(PathBuf::from(format!("{}:\\", char::from(drive))))
            }
            _ => Err(Error::new(
                ErrorKind::InvalidInput,
                "expected a local drive",
            )),
        },
        _ => Err(Error::new(
            ErrorKind::InvalidInput,
            "expected a drive prefix",
        )),
    }
}

#[cfg(not(windows))]
fn volume_root(path: &Path) -> io::Result<PathBuf> {
    path.ancestors()
        .last()
        .map(Path::to_owned)
        .ok_or_else(|| Error::other("fixture has no volume root"))
}

impl Context {
    fn new(fixture: &Path) -> io::Result<Self> {
        let temporary = tempfile::tempdir_in(fixture)?;
        let file_path = temporary.path().join("allocated");
        let allocated = open_file(&file_path)?;
        fs2_baseline::FileExt::allocate(&allocated, FILE_SIZE)?;
        fs2_candidate::FileExt::allocate(&allocated, FILE_SIZE)?;
        let lock = open_file(&temporary.path().join("lock"))?;
        let create_paths = (0..PATH_COUNT)
            .map(|index| temporary.path().join(format!("file-{index}")))
            .collect::<Vec<_>>();
        // Match the Criterion fixture: prime all 4096 names outside measurement.
        for path in &create_paths {
            drop(black_box(open_file(path)?));
            fs::remove_file(path)?;
        }
        let fixture = fixture.canonicalize()?;
        let volume_root = volume_root(&fixture)?;
        Ok(Self {
            fixture,
            volume_root,
            allocated,
            lock,
            file_path,
            reusable_path: temporary.path().join("sized-file"),
            create_paths,
            create_index: Cell::new(0),
            _temporary: temporary,
        })
    }
}

macro_rules! subject {
    ($name:ident, $subject:ident) => {
        #[allow(deprecated)]
        fn $name(context: &Context, metric: &str) -> io::Result<Value> {
            let path = if metric.starts_with("windows_root_stats/") {
                &context.volume_root
            } else if metric.ends_with("_file_fallback") {
                &context.file_path
            } else {
                &context.fixture
            };
            let value = match metric {
                "allocated_size" => $subject::FileExt::allocated_size(&context.allocated)?,
                "duplicate" => {
                    drop(black_box($subject::FileExt::duplicate(&context.lock)?));
                    1
                }
                "file_allocate_already_satisfied" => {
                    $subject::FileExt::allocate(&context.allocated, FILE_SIZE)?;
                    1
                }
                "file_create_delete" => {
                    let index = context.create_index.get();
                    let path = &context.create_paths[index & (PATH_COUNT - 1)];
                    context.create_index.set(index.wrapping_add(1));
                    drop(black_box(open_file(path)?));
                    fs::remove_file(path)?;
                    1
                }
                "file_open_allocate_delete" => {
                    let file = open_file(&context.reusable_path)?;
                    $subject::FileExt::allocate(&file, FILE_SIZE)?;
                    fs::remove_file(&context.reusable_path)?;
                    1
                }
                "file_open_truncate_delete" => {
                    let file = open_file(&context.reusable_path)?;
                    file.set_len(FILE_SIZE)?;
                    fs::remove_file(&context.reusable_path)?;
                    1
                }
                "lock_unlock" => {
                    $subject::FileExt::lock_exclusive(&context.lock)?;
                    $subject::FileExt::unlock(&context.lock)?;
                    1
                }
                "free_space" | "free_space_file_fallback" | "windows_root_stats/free_space" => {
                    $subject::free_space(path)?
                }
                "available_space"
                | "available_space_file_fallback"
                | "windows_root_stats/available_space" => $subject::available_space(path)?,
                "total_space" | "windows_root_stats/total_space" => $subject::total_space(path)?,
                "allocation_granularity" | "windows_root_stats/allocation_granularity" => {
                    $subject::allocation_granularity(path)?
                }
                "stats_snapshot/one_snapshot" | "windows_root_stats/one_top_level_snapshot" => {
                    let stats = $subject::statvfs(path)?;
                    return Ok([
                        stats.free_space(),
                        stats.available_space(),
                        stats.total_space(),
                        stats.allocation_granularity(),
                    ]);
                }
                "stats_snapshot/four_convenience_queries" => {
                    return Ok([
                        $subject::free_space(path)?,
                        $subject::available_space(path)?,
                        $subject::total_space(path)?,
                        $subject::allocation_granularity(path)?,
                    ]);
                }
                _ => {
                    return Err(Error::new(
                        ErrorKind::InvalidInput,
                        "unknown common workload",
                    ))
                }
            };
            Ok([value, 0, 0, 0])
        }
    };
}

subject!(baseline, fs2_baseline);
subject!(candidate, fs2_candidate);

fn nearby(left: u64, right: u64) -> bool {
    left.abs_diff(right) <= (256 * 1024 * 1024).max(left.max(right) / 100)
}

fn compatible(metric: &str, left: Value, right: Value) -> bool {
    if metric.contains("snapshot") || metric.ends_with("four_convenience_queries") {
        let separate_queries = metric == "stats_snapshot/four_convenience_queries";
        let valid = |value: Value| {
            // Separate OS calls need not preserve snapshot ordering: space can
            // become available between the free-space and available-space calls.
            value[2] > 0
                && value[3] > 0
                && value[0] <= value[2]
                && value[1] <= value[2]
                && (separate_queries || value[1] <= value[0])
        };
        valid(left)
            && valid(right)
            && left[2..] == right[2..]
            && nearby(left[0], right[0])
            && nearby(left[1], right[1])
    } else if metric.contains("free_space") || metric.contains("available_space") {
        nearby(left[0], right[0])
    } else {
        left == right && left[0] > 0
    }
}

fn observe(context: &Context, metric: &str, operation: Operation) -> io::Result<(u128, Value)> {
    let started = Instant::now();
    let result = black_box(operation(black_box(context), black_box(metric)));
    let elapsed = started.elapsed().as_nanos();
    Ok((elapsed, result?))
}

fn pair(
    context: &Context,
    metric: &str,
    candidate: Operation,
    baseline_first: bool,
) -> io::Result<paired::PairObservation> {
    let (a1, a2, b1, b2) = if baseline_first {
        let a1 = observe(context, metric, baseline)?;
        let b1 = observe(context, metric, candidate)?;
        let b2 = observe(context, metric, candidate)?;
        let a2 = observe(context, metric, baseline)?;
        (a1, a2, b1, b2)
    } else {
        let b1 = observe(context, metric, candidate)?;
        let a1 = observe(context, metric, baseline)?;
        let a2 = observe(context, metric, baseline)?;
        let b2 = observe(context, metric, candidate)?;
        (a1, a2, b1, b2)
    };
    Ok(paired::PairObservation {
        baseline_ns: a1.0.saturating_add(a2.0) / 2,
        candidate_ns: b1.0.saturating_add(b2.0) / 2,
        failures: u64::from(!compatible(metric, a1.1, b1.1))
            + u64::from(!compatible(metric, a2.1, b2.1)),
    })
}

fn print_measurement(metric: &str, result: &paired::Measurement) {
    println!(
        "{metric}\t{:.6}\t{:.6}\t{:.9}\t{:.9}\t{:.9}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
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

fn run(mut args: impl Iterator<Item = OsString>) -> io::Result<()> {
    let fixture = args
        .next()
        .ok_or_else(|| Error::new(ErrorKind::InvalidInput, "missing fixture"))?;
    let mut args = args.map(|argument| {
        argument
            .into_string()
            .map_err(|_| Error::new(ErrorKind::InvalidInput, "non-UTF-8 measurement argument"))
    });
    let candidate: Operation = match args.next().transpose()?.as_deref() {
        Some("ab") => candidate,
        Some("aa") => baseline,
        _ => return Err(Error::new(ErrorKind::InvalidInput, "expected ab or aa")),
    };
    let samples = paired::parse_sample_size(args.next().transpose()?)?;
    let warm_up = paired::parse_duration_millis(args.next().transpose()?, "warm-up milliseconds")?;
    let measurement =
        paired::parse_duration_millis(args.next().transpose()?, "measurement milliseconds")?;
    let rotation = args
        .next()
        .transpose()?
        .ok_or_else(|| Error::new(ErrorKind::InvalidInput, "missing workload rotation"))?
        .parse::<usize>()
        .map_err(|_| Error::new(ErrorKind::InvalidInput, "invalid workload rotation"))?;
    if args.next().is_some() {
        return Err(Error::new(ErrorKind::InvalidInput, "too many arguments"));
    }
    let context = Context::new(Path::new(&fixture))?;
    println!("{}", paired::PROTOCOL);
    println!("{}", paired::HEADER);
    for offset in 0..protocol::METRICS.len() {
        let metric = protocol::METRICS
            [(offset + rotation % protocol::METRICS.len()) % protocol::METRICS.len()];
        eprintln!("measuring {metric}");
        let a = observe(&context, metric, baseline)?;
        let b = observe(&context, metric, candidate)?;
        let prime = paired::PairObservation {
            baseline_ns: a.0,
            candidate_ns: b.0,
            failures: u64::from(!compatible(metric, a.1, b.1)),
        };
        let result = paired::measure(
            samples,
            Duration::from_millis(warm_up),
            Duration::from_millis(measurement),
            prime,
            |baseline_first| pair(&context, metric, candidate, baseline_first),
        )?;
        print_measurement(metric, &result);
        if result.prime_failures + result.warm_up_failures + result.failures != 0 {
            return Err(Error::other(format!(
                "{metric} returned incompatible results"
            )));
        }
    }
    Ok(())
}

fn main() -> io::Result<()> {
    run(std::env::args_os().skip(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn root_workloads_keep_the_ordinary_drive_spelling() {
        for path in [r"C:\fixture", r"\\?\C:\fixture"] {
            assert_eq!(volume_root(Path::new(path)).unwrap(), Path::new(r"C:\"));
        }
        assert!(volume_root(Path::new(r"\\server\share")).is_err());
    }

    #[test]
    fn snapshot_validation_accepts_activity_but_rejects_structural_mismatches() {
        let baseline = [100, 80, 200, 4096];
        assert!(compatible(
            "stats_snapshot/one_snapshot",
            baseline,
            [99, 79, 200, 4096]
        ));
        assert!(!compatible(
            "stats_snapshot/one_snapshot",
            baseline,
            [100, 80, 201, 4096]
        ));
        assert!(!compatible(
            "stats_snapshot/one_snapshot",
            baseline,
            [100, 101, 200, 4096]
        ));
        assert!(!compatible("allocated_size", [32, 0, 0, 0], [31, 0, 0, 0]));
    }

    #[test]
    fn separate_queries_allow_space_changes_without_relaxing_counter_bounds() {
        let metric = "stats_snapshot/four_convenience_queries";
        let baseline = [100, 80, 200, 4096];
        let changed = [100, 101, 200, 4096];
        assert!(compatible(metric, baseline, changed));
        assert!(compatible(metric, changed, baseline));
        assert!(compatible(metric, changed, changed));
        for invalid in [
            [201, 80, 200, 4096],
            [100, 201, 200, 4096],
            [100, 80, 201, 4096],
            [100, 80, 200, 8192],
            [100, 80, 0, 4096],
            [100, 80, 200, 0],
        ] {
            assert!(!compatible(metric, baseline, invalid));
            assert!(!compatible(metric, invalid, baseline));
        }
    }
}
