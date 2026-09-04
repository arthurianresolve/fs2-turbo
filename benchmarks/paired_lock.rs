mod paired;

use std::ffi::OsString;
use std::fs::File;
use std::hint::black_box;
use std::io::{self, Error, ErrorKind};
use std::path::Path;
use std::time::{Duration, Instant};

use fs2::FileExt;

const METRIC: &str = "lock_unlock";

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

#[inline]
fn current(file: &File) -> io::Result<()> {
    file.fs2_lock_exclusive()?;
    file.fs2_unlock()
}

#[inline]
fn legacy(file: &File) -> io::Result<()> {
    FileExt::lock_exclusive(file)?;
    FileExt::unlock(file)
}

#[inline]
fn observe<S>(file: &File, subject: S) -> io::Result<u128>
where
    S: FnOnce(&File) -> io::Result<()>,
{
    let start = Instant::now();
    black_box(subject(black_box(file)))?;
    Ok(start.elapsed().as_nanos())
}

#[inline]
fn observe_prime<B, C>(
    file: &File,
    baseline: B,
    candidate: C,
) -> io::Result<paired::PairObservation>
where
    B: Copy + Fn(&File) -> io::Result<()>,
    C: Copy + Fn(&File) -> io::Result<()>,
{
    Ok(paired::PairObservation {
        baseline_ns: observe(file, baseline)?,
        candidate_ns: observe(file, candidate)?,
        failures: 0,
    })
}

fn observe_pair<B, C>(
    file: &File,
    baseline: B,
    candidate: C,
    baseline_first: bool,
) -> io::Result<paired::PairObservation>
where
    B: Copy + Fn(&File) -> io::Result<()>,
    C: Copy + Fn(&File) -> io::Result<()>,
{
    let (baseline_first_ns, baseline_second_ns, candidate_first_ns, candidate_second_ns) =
        if baseline_first {
            let baseline_first_ns = observe(file, baseline)?;
            let candidate_first_ns = observe(file, candidate)?;
            let candidate_second_ns = observe(file, candidate)?;
            let baseline_second_ns = observe(file, baseline)?;
            (
                baseline_first_ns,
                baseline_second_ns,
                candidate_first_ns,
                candidate_second_ns,
            )
        } else {
            let candidate_first_ns = observe(file, candidate)?;
            let baseline_first_ns = observe(file, baseline)?;
            let baseline_second_ns = observe(file, baseline)?;
            let candidate_second_ns = observe(file, candidate)?;
            (
                baseline_first_ns,
                baseline_second_ns,
                candidate_first_ns,
                candidate_second_ns,
            )
        };
    Ok(paired::PairObservation {
        baseline_ns: baseline_first_ns.saturating_add(baseline_second_ns) / 2,
        candidate_ns: candidate_first_ns.saturating_add(candidate_second_ns) / 2,
        failures: 0,
    })
}

fn measure(
    file: &File,
    comparison: Comparison,
    sample_size: usize,
    warm_up: Duration,
    measurement: Duration,
) -> io::Result<paired::Measurement> {
    match comparison {
        Comparison::Ab => {
            measure_subjects(file, current, legacy, sample_size, warm_up, measurement)
        }
        Comparison::Aa => {
            measure_subjects(file, current, current, sample_size, warm_up, measurement)
        }
    }
}

fn measure_subjects<B, C>(
    file: &File,
    baseline: B,
    candidate: C,
    sample_size: usize,
    warm_up: Duration,
    measurement: Duration,
) -> io::Result<paired::Measurement>
where
    B: Copy + Fn(&File) -> io::Result<()>,
    C: Copy + Fn(&File) -> io::Result<()>,
{
    let prime = observe_prime(file, baseline, candidate)?;
    paired::measure(sample_size, warm_up, measurement, prime, |baseline_first| {
        observe_pair(file, baseline, candidate, baseline_first)
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
    let comparison = Comparison::parse(
        &args
            .next()
            .transpose()?
            .ok_or_else(|| Error::new(ErrorKind::InvalidInput, "missing comparison"))?,
    )?;
    let sample_size = paired::parse_sample_size(args.next().transpose()?)?;
    let warm_up_ms =
        paired::parse_duration_millis(args.next().transpose()?, "warm-up milliseconds")?;
    let measurement_ms =
        paired::parse_duration_millis(args.next().transpose()?, "measurement milliseconds")?;
    if args.next().is_some() {
        return Err(Error::new(ErrorKind::InvalidInput, "too many arguments"));
    }

    let file = tempfile::tempfile_in(Path::new(&fixture))?;
    let result = measure(
        &file,
        comparison,
        sample_size,
        Duration::from_millis(warm_up_ms),
        Duration::from_millis(measurement_ms),
    )?;
    println!("{}", paired::PROTOCOL);
    println!("{}", paired::HEADER);
    println!(
        "{METRIC}\t{:.6}\t{:.6}\t{:.9}\t{:.9}\t{:.9}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
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
    fn rejects_missing_fixture_and_the_old_ambient_temp_grammar() {
        assert_eq!(
            run(std::iter::empty()).unwrap_err().kind(),
            ErrorKind::InvalidInput
        );
        assert_eq!(
            run(["ab", "10", "1", "1"].into_iter().map(OsString::from))
                .unwrap_err()
                .kind(),
            ErrorKind::InvalidInput
        );
    }

    #[test]
    fn invalid_fixture_does_not_fall_back_to_default_temp() {
        let root = tempfile::tempdir().unwrap();
        let missing = root.path().join("missing fixture");
        assert_eq!(
            run(arguments(&missing, "ab").into_iter())
                .unwrap_err()
                .kind(),
            ErrorKind::NotFound
        );
    }

    #[test]
    fn preserves_argument_bounds_and_rejects_extra_arguments() {
        let fixture = Path::new("unused fixture");
        for (index, value) in [(1, "invalid"), (2, "9"), (3, "0"), (4, "0")] {
            let mut args = arguments(fixture, "ab");
            args[index] = value.into();
            assert!(run(args.into_iter()).is_err());
        }
        let mut args = arguments(fixture, "aa");
        args.push("unexpected".into());
        assert_eq!(
            run(args.into_iter()).unwrap_err().kind(),
            ErrorKind::InvalidInput
        );
    }

    #[test]
    fn accepts_explicit_fixture_with_spaces_for_both_comparisons() {
        let root = tempfile::tempdir().unwrap();
        let fixture = root.path().join("fixture with spaces");
        std::fs::create_dir(&fixture).unwrap();
        for mode in ["ab", "aa"] {
            run(arguments(&fixture, mode).into_iter()).unwrap();
        }
    }

    #[cfg(unix)]
    #[test]
    fn preserves_non_unicode_fixture_paths() {
        use std::os::unix::ffi::OsStringExt;

        let root = tempfile::tempdir().unwrap();
        let fixture = root.path().join(OsString::from_vec(vec![b'f', 0xff]));
        std::fs::create_dir(&fixture).unwrap();
        run(arguments(&fixture, "ab").into_iter()).unwrap();
    }
}
