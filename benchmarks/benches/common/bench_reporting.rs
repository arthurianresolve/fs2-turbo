use std::cell::Cell;
use std::io;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

#[path = "bench_reporting_policy.rs"]
pub(crate) mod policy;

pub(crate) use policy::{failure_record, prime_record, report_errors_value};

static REPORT_ERRORS: OnceLock<bool> = OnceLock::new();
// Each benchmark binary is a process-local counter; relaxed ordering is enough
// because probes read it only to report a completed measurement boundary.
static TRANSIENT_FAILURE_SEQUENCE: AtomicUsize = AtomicUsize::new(0);
thread_local! {
    static TRANSIENT_FAILURES: Cell<usize> = const { Cell::new(0) };
}

pub(crate) struct FailureProbe {
    label: &'static str,
    start: usize,
}

impl FailureProbe {
    pub(crate) fn new(label: &'static str) -> Self {
        Self {
            label,
            start: transient_failures(),
        }
    }

    pub(crate) fn failure_delta(&self) -> usize {
        transient_failures().saturating_sub(self.start)
    }
}

impl Drop for FailureProbe {
    fn drop(&mut self) {
        let delta = self.failure_delta();
        if delta > 0 {
            eprintln!("{}", failure_record(self.label, delta));
        }
    }
}

#[inline]
pub(crate) fn observe<T: Copy>(result: io::Result<T>, fallback: T, label: &'static str) -> T {
    match result {
        Ok(value) => value,
        Err(error) => {
            let total = record_transient_failure();
            if report_errors() {
                eprintln!("[fs2-bench] transient failure #{total} ({label}): {error}");
            }
            fallback
        }
    }
}

pub(crate) fn report_prime(label: &str, duration_ns: u128) {
    eprintln!("{}", prime_record(label, duration_ns));
}

#[inline(always)]
fn record_transient_failure() -> usize {
    TRANSIENT_FAILURES.with(|failures| failures.set(failures.get().saturating_add(1)));
    TRANSIENT_FAILURE_SEQUENCE.fetch_add(1, Ordering::Relaxed) + 1
}

#[inline(always)]
fn transient_failures() -> usize {
    TRANSIENT_FAILURES.with(Cell::get)
}

/// Set `FS2_BENCH_REPORT_ERRORS=1` to emit per-iteration transient failure details to
/// stderr. `0`, `false`, `off`, and `no` keep per-iteration reporting disabled while
/// the aggregated counter line remains available at each bench probe boundary.
#[inline(always)]
fn report_errors() -> bool {
    *REPORT_ERRORS
        .get_or_init(|| report_errors_value(std::env::var_os("FS2_BENCH_REPORT_ERRORS").as_deref()))
}
