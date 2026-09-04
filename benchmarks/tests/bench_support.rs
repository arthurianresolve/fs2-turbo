#[path = "../benches/common/bench_prime.rs"]
mod prime;
#[allow(dead_code)]
#[path = "../benches/common/bench_reporting.rs"]
mod reporting;

use std::ffi::OsStr;
use std::sync::atomic::{AtomicUsize, Ordering};

mod bench_support {
    pub(crate) use crate::prime::iter_primed;

    pub(crate) fn record_prime_once<F, O>(label: &'static str, operation: &mut F)
    where
        F: FnMut() -> O,
    {
        static PRIME: std::sync::Once = std::sync::Once::new();
        assert_eq!(label, "test/workload");
        PRIME.call_once(|| {
            let _ = operation();
        });
    }
}

struct TestBencher {
    iterations: usize,
}

impl TestBencher {
    fn iter<F, O>(&mut self, mut operation: F)
    where
        F: FnMut() -> O,
    {
        self.iterations += 1;
        let _ = operation();
    }
}

fn exercise_primed_iteration(bencher: &mut TestBencher, calls: &AtomicUsize) {
    bench_support::iter_primed!(bencher, "test/workload", || {
        calls.fetch_add(1, Ordering::Relaxed);
    });
}

#[test]
fn report_errors_requires_an_enabled_value() {
    assert!(!reporting::report_errors_value(None));
    assert!(!reporting::report_errors_value(Some(OsStr::new("0"))));
    assert!(!reporting::report_errors_value(Some(OsStr::new("FALSE"))));
    assert!(!reporting::report_errors_value(Some(OsStr::new("off"))));
    assert!(!reporting::report_errors_value(Some(OsStr::new("no"))));
    assert!(!reporting::report_errors_value(Some(OsStr::new(""))));
    assert!(reporting::report_errors_value(Some(OsStr::new("1"))));
}

#[test]
fn failure_records_are_machine_parseable_and_escape_labels() {
    assert_eq!(
        reporting::failure_record("free_space", 3),
        "[fs2-bench] FS2_BENCH_FAILURE\tfree_space\t3"
    );
    assert_eq!(
        reporting::failure_record("a\tb", 1),
        "[fs2-bench] FS2_BENCH_FAILURE\ta\\tb\t1"
    );
    assert_eq!(
        reporting::failure_record("a\\b\r\n", 1),
        "[fs2-bench] FS2_BENCH_FAILURE\ta\\\\b\\r\\n\t1"
    );
}

#[test]
fn failure_probe_counts_differently_labeled_operation_failures() {
    let probe = reporting::FailureProbe::new("benchmark.boundary");
    let fallback = reporting::observe(
        Err(std::io::Error::other("expected test failure")),
        7_u64,
        "operation.detail",
    );

    assert_eq!(fallback, 7);
    assert_eq!(probe.failure_delta(), 1);
}

#[test]
fn prime_records_are_machine_parseable_and_escape_labels() {
    assert_eq!(
        reporting::prime_record("a\tb", 17),
        "[fs2-bench] FS2_BENCH_PRIME\ta\\tb\t17"
    );
}

#[test]
fn priming_runs_once_outside_measured_iterations() {
    let calls = AtomicUsize::new(0);
    let mut bencher = TestBencher { iterations: 0 };
    exercise_primed_iteration(&mut bencher, &calls);
    exercise_primed_iteration(&mut bencher, &calls);
    assert_eq!(bencher.iterations, 2);
    assert_eq!(calls.load(Ordering::Relaxed), 3);
}
