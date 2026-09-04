mod paired;
#[path = "paired_stats_protocol.rs"]
mod protocol;

use std::ffi::OsString;
use std::fs::File;
use std::hint::black_box;
use std::io::{self, Error, ErrorKind};
use std::path::Path;
use std::time::{Duration, Instant};

type Operation = fn(&File) -> io::Result<()>;

macro_rules! subject {
    ($name:ident, $subject:ident) => {
        #[allow(deprecated)]
        fn $name(file: &File) -> io::Result<()> {
            for _ in 0..protocol::OPERATIONS_PER_TIMED_INTERVAL {
                // Close each duplicate immediately, as in the single-call case.
                // Do not accumulate handles or change inheritance behavior.
                drop(black_box($subject::FileExt::duplicate(black_box(file))?));
            }
            Ok(())
        }
    };
}

subject!(baseline, fs2_baseline);
subject!(candidate, fs2_candidate);

fn observe(file: &File, operation: Operation) -> io::Result<u128> {
    let started = Instant::now();
    let result = black_box(operation)(black_box(file));
    let elapsed = started.elapsed().as_nanos();
    result?;
    Ok(elapsed)
}

fn observe_pair(
    file: &File,
    candidate: Operation,
    baseline_first: bool,
) -> io::Result<paired::PairObservation> {
    let (a1, a2, b1, b2) = if baseline_first {
        let a1 = observe(file, baseline)?;
        let b1 = observe(file, candidate)?;
        let b2 = observe(file, candidate)?;
        let a2 = observe(file, baseline)?;
        (a1, a2, b1, b2)
    } else {
        let b1 = observe(file, candidate)?;
        let a1 = observe(file, baseline)?;
        let a2 = observe(file, baseline)?;
        let b2 = observe(file, candidate)?;
        (a1, a2, b1, b2)
    };
    Ok(paired::PairObservation {
        baseline_ns: a1.saturating_add(a2) / 2,
        candidate_ns: b1.saturating_add(b2) / 2,
        failures: 0,
    })
}

fn run(mut args: impl Iterator<Item = OsString>) -> io::Result<()> {
    let fixture = args
        .next()
        .ok_or_else(|| Error::new(ErrorKind::InvalidInput, "missing fixture directory"))?;
    let mut args = args.map(|argument| {
        argument.into_string().map_err(|_| {
            Error::new(
                ErrorKind::InvalidInput,
                "measurement arguments must be UTF-8",
            )
        })
    });
    let candidate: Operation = match args.next().transpose()?.as_deref() {
        Some("ab") => candidate,
        Some("aa") => baseline,
        _ => return Err(Error::new(ErrorKind::InvalidInput, "expected ab or aa")),
    };
    let sample_size = paired::parse_sample_size(args.next().transpose()?)?;
    let warm_up = paired::parse_duration_millis(args.next().transpose()?, "warm-up milliseconds")?;
    let measurement =
        paired::parse_duration_millis(args.next().transpose()?, "measurement milliseconds")?;
    if args.next().is_some() {
        return Err(Error::new(ErrorKind::InvalidInput, "too many arguments"));
    }

    let file = tempfile::tempfile_in(Path::new(&fixture))?;
    let prime = paired::PairObservation {
        baseline_ns: observe(&file, baseline)?,
        candidate_ns: observe(&file, candidate)?,
        failures: 0,
    };
    let result = paired::measure(
        sample_size,
        Duration::from_millis(warm_up),
        Duration::from_millis(measurement),
        prime,
        |baseline_first| observe_pair(&file, candidate, baseline_first),
    )?;

    // Preserve raw batch nanoseconds, including prime timings. The report binds
    // the batch size; only presentation divides by it for amortized cost.
    let metric = protocol::METRICS[0];
    println!("{}", paired::PROTOCOL);
    println!("{}", paired::HEADER);
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
    Ok(())
}

fn main() -> io::Result<()> {
    run(std::env::args_os().skip(1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn arguments(fixture: &Path, mode: &str) -> Vec<OsString> {
        vec![
            fixture.as_os_str().to_owned(),
            mode.into(),
            "10".into(),
            "1".into(),
            "1".into(),
        ]
    }

    #[test]
    fn rejects_ambient_fixtures_invalid_bounds_and_extra_arguments() {
        assert!(run(std::iter::empty()).is_err());
        assert!(run(["ab", "10", "1", "1"].into_iter().map(OsString::from)).is_err());
        let fixture = Path::new("unused fixture");
        for (index, value) in [(1, "invalid"), (2, "9"), (3, "0"), (4, "0")] {
            let mut args = arguments(fixture, "ab");
            args[index] = value.into();
            assert!(run(args.into_iter()).is_err());
        }
        let mut args = arguments(fixture, "aa");
        args.push("unexpected".into());
        assert!(run(args.into_iter()).is_err());
    }

    #[test]
    fn missing_fixture_never_falls_back_to_ambient_temp() {
        let root = tempfile::tempdir().unwrap();
        let missing = root.path().join("missing fixture");
        assert_eq!(
            run(arguments(&missing, "aa").into_iter())
                .unwrap_err()
                .kind(),
            ErrorKind::NotFound,
        );
    }

    #[test]
    fn both_comparisons_accept_an_explicit_fixture_with_spaces() {
        let root = tempfile::tempdir().unwrap();
        let fixture = root.path().join("fixture with spaces");
        std::fs::create_dir(&fixture).unwrap();
        assert_eq!(protocol::METRICS, &["duplicate/batch64"]);
        assert_eq!(protocol::OPERATIONS_PER_TIMED_INTERVAL, 64);
        for mode in ["ab", "aa"] {
            run(arguments(&fixture, mode).into_iter()).unwrap();
        }
    }

    #[test]
    fn operation_errors_are_propagated_without_retry() {
        static CALLS: AtomicUsize = AtomicUsize::new(0);
        fn fail(_: &File) -> io::Result<()> {
            CALLS.fetch_add(1, Ordering::Relaxed);
            Err(Error::new(ErrorKind::PermissionDenied, "test failure"))
        }
        let file = tempfile::tempfile().unwrap();
        assert_eq!(
            observe(&file, fail).unwrap_err().kind(),
            ErrorKind::PermissionDenied
        );
        assert_eq!(CALLS.load(Ordering::Relaxed), 1);
    }
}
