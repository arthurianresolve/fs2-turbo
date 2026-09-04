# Recreating the pull-request benchmark tables

The retained JSON reports are canonical. Run each comparison against immutable
Git refs, then render the reports into review-ready Markdown.

The full common API surface uses one paired binary with both immutable subjects
under one Cargo lockfile. It covers the 19 Windows PR workloads and the general
allocation-granularity query (13 common workloads on Unix):

```text
cargo xtask bench common-refs --baseline 0.4.3 --candidate <candidate-ref> --trust-selected-code --output-root C:\bench-results --output C:\bench-results\fs2-v04-common
```

Windows filesystem statistics use the same-process v0.4-compatible profile:

```text
cargo xtask bench stats --baseline 0.4.3 --candidate <candidate-ref> --common-v0-4 --trust-selected-code --output-root C:\bench-results --output C:\bench-results\fs2-v04-stats
```

Locking uses immutable baseline and candidate sources in one paired binary:

```text
cargo xtask bench lock-refs --baseline 0.4.3 --candidate <candidate-ref> --trust-selected-code --output-root C:\bench-results --output C:\bench-results\fs2-lock-refs
```

Render completed valid reports:

```text
cargo xtask bench markdown --report <report.json> --report <report.json>
```

Each report binds the selected commits, retained source trees, generated
manifest and lockfile, harness and protocol sources, executable, measurement
policy, environment snapshots, process outcomes, raw measurements, outlier
counts, exact bounds, A/A controls, and final disposition.

Common-API rows use the same 32 MiB allocation/truncation sizes, 4096-name
create/delete pool, explicit FileExt locking calls, and snapshot/query sequences
as the Criterion suite. The timing protocol is now the paired protocol, so do
not combine its samples or outlier counts with older Criterion measurements.
File creation/deletion and truncation are standard-library controls; they do not
call either version of fs2. Confidence bounds apply per workload. A full pass
requires every workload and its A/A control to pass the unchanged policy.

## Sampling stability

The shared paired sampler calibrates from the median elapsed time of 16 balanced
two-observation pilot blocks. A single slow pilot block no longer determines the
iteration count for the entire measurement. Every sample contains at least 32
paired observations, rounded up to an even count so both call orders occur
equally often within that sample.

Each two-observation block uses complementary ABBA/BAAB orders. Their starting
order is shuffled with a fixed, source-recorded seed rather than repeating the
same phase indefinitely. Calibration and measurement start from that seed
independently, so warm-up duration does not select the measured order sequence.
Both versions still perform equal work against the unchanged fixtures.

The requested measurement duration remains a calibration target, not a hard
wall-clock cap. The minimum averaging count can lengthen slow workloads; the
existing process timeout and iteration safety limit still apply. No measured
sample is trimmed, retried, or replaced, and all compatibility failures and
outliers remain subject to the existing gates.

The retained sampler source hash distinguishes this method from earlier runs.
Do not combine their samples. Fresh A/A and A/B evidence is required before
claiming that the previously observed instability has been resolved.

## Focused duplicate profile

Use the dedicated profile to investigate duplicate timing without rerunning
unrelated filesystem workloads:

```text
cargo +1.98.1 xtask bench duplicate-refs --baseline 0.4.3 --candidate <candidate-ref> --trust-selected-code --idle-max-core-busy-percent 5 --idle-max-sample-busy-percent 20 --output-root C:\bench-results --output C:\bench-results\duplicate-batch64
```

The retained `duplicate-measurement-policy.json` sets a 20-second measurement
target, 16 independent A/B process replicates and 16 A/A controls. The remaining
sampling settings match the full-suite policy: 50 samples, two-second warm-up,
ten-second cooldown, 95% confidence, 30% maximum outlier fraction per block, and
8 GiB minimum available space. The A/B upper-bound non-regression margin remains
2%. The separate `paired_process.aa_equivalence_margin` is 1%: the simultaneous
two-sided A/A interval must lie within `[1 / 1.01, 1.01]`. Reports record both
effective margins. Existing policies without the optional A/A field retain
their shared-margin behavior; an A/A margin above 2% is exploratory-only.

Freeze these settings before observing acceptance results. Explicit replicate
counts or durations differing from this policy require `--exploratory`; they
cannot silently become strict results. All samples remain retained. The 3-MAD
outlier fraction is not a percentage timing error or a precision target, and
tightening its limit alone does not improve measurement precision.

The metric is `duplicate/batch64`, not the existing `duplicate` metric. Each
timed interval performs 64 explicit FileExt duplicate calls, closing each
duplicate immediately. No handles are accumulated and inheritance behavior is
unchanged. The shared sampler balances and shuffles ABBA/BAAB batch order.
Actual API errors propagate without retries or sample replacement.

Canonical records retain nanoseconds per batch, including prime timings.
`method.operations_per_timed_interval` records 64; `iterations` counts paired
batch observations per sample. Markdown divides timing medians by 64 and labels
the result as amortized operation cost, not individual-call p50 latency.
Ratios, outlier counts, raw samples and gates are unchanged by presentation.
Do not splice these values into the historical single-call table.

### Trace separately from acceptance

First use WPR/WPA to inspect CPU Usage (Precise) context-switch activity and
DPC/ISR activity by CPU, following Microsoft's
[CPU analysis guidance](https://learn.microsoft.com/en-us/windows-hardware/test/wpt/cpu-analysis).
For a run with external tracing enabled, add `--diagnostic-trace --exploratory`
and use a separate output directory. The flag records an operator-declared
diagnostic profile; it does not detect tracing or start, stop, or replace any
system trace session. Retain the ETL and CPU-selection rationale alongside the
diagnostic evidence. Stop only a trace session you started.

Choose affinity from the independent trace, rather than selecting a core for
favorable benchmark ratios. Run acceptance afterward without tracing, pinned
to the recorded CPU, with both diagnostic flags omitted and a fresh output
directory. Do not disable security protections, elevate process priority to
real-time, trim outliers, or retry until a pass appears. A diagnostic trace is
not performance evidence, and stable A/A controls are still required before
making any performance claim.

### Original single-call comparison

`bench duplicate-single-refs` uses `paired_common.rs` unchanged, with a retained
protocol selecting only `duplicate`. It preserves that original timed body,
including immediate close, rather than changing the batch64 loop to one iteration.
It uses the same dedicated 16-process, 20-second policy as `duplicate-refs`.

```text
cargo +1.98.1 xtask bench duplicate-single-refs --baseline <exact-baseline-sha> --candidate <exact-candidate-sha> --trust-selected-code --idle-max-core-busy-percent 5 --idle-max-sample-busy-percent 20 --output-root C:\new-private-bench-root --output C:\new-private-bench-root\single
```

Report single-call and batch64 costs separately. The original single-call result
is still a median of sample-averaged costs, not a histogram of individual-call
latencies. Batch division cannot recover individual-call percentiles.

### Diagnostic sample correlation

Both duplicate profiles accept `--diagnostic-samples --exploratory`. This records
sample windows without claiming that a trace was active. `--diagnostic-trace`
also enables sample windows and declares externally managed tracing. Neither
flag starts or elevates WPR. The report records `method.diagnostic_samples`.

Acceptance children explicitly receive `FS2_PAIRED_DIAGNOSTIC_SAMPLES=0`, even
when that variable is set in the caller's environment. Diagnostic children
receive `1`. Clock queries occur at sample boundaries, outside the timed API
calls; buffered diagnostic rows are written to stderr after sampling ends.
Diagnostic runs never become strict performance evidence.

Each `fs2-sample-v2` TSV row contains, in order: marker, process ID, thread ID,
zero-based sample index, clock name, clock frequency, start/end clock ticks,
start/end Unix nanoseconds, start/end logical CPU, sample ratio, thread CPU-time
delta in 100 ns units, and raw thread-cycle delta. Windows
uses QPC and records native thread/CPU IDs. Other platforms use Unix nanoseconds,
thread ID zero and CPU `4294967295` to mark unavailable topology information.
Unavailable counters use `unknown`; the correlator also accepts historical v1
rows with counter fields absent. Missing counters serialize as null, not zero.

Thread counters are diagnostic-only. CPU accounting is coarse, not 100 ns
resolution, and its delta can exceed the sample wall interval; it is not clamped.
Never convert raw cycles into elapsed time. Both counters cover the whole sample,
including both A/B sides and harness work, so they cannot identify the delayed
side or a DPC/ISR source. See Microsoft's [GetThreadTimes documentation](https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-getthreadtimes)
and [QueryThreadCycleTime restrictions](https://learn.microsoft.com/en-us/windows/win32/api/realtimeapiset/nf-realtimeapiset-querythreadcycletime).

`bench correlate-samples --samples <run.stderr.log>` validates and inventories
the windows. Repeat `--samples` for separate process logs. An optional `--events`
accepts bounded UTF-8 JSONL from a normalized trace export:

```json
{"start_unix_ns":100,"end_unix_ns":200,"cpu":1,"kind":"dpc"}
```

Kinds are `context-switch`, `dpc`, `isr` and `power`. Equal start/end values mean
a point event. Normalize event times to the same UTC/Unix epoch; retain the
original ETL, exporter settings and event-loss assessment separately. Raw ETL or
arbitrary WPA CSV is not accepted as if it already followed this schema.

The correlator uses half-open sample windows and the observed CPU, retains all
samples, and recomputes relative 3-MAD flags. CPU-wide overlap is not proof that
an event delayed the benchmark thread. Null counts indicate missing trace input,
CPU migration/unknown CPU, or clock-duration disagreement exceeding 1 ms. `power`
events do not establish effective CPU frequency. Output includes input hashes
and is explicitly diagnostic, never a performance pass/fail report.

### Confirmation sessions and host controls

Freeze exact refs, policy, harness hashes, number of sessions and profile order
before measurement. Use fresh private output roots; do not pre-create roots with
inherited DACLs. Retain failures and setup records, and never retry until green.
Report each session independently rather than pooling sessions or changed
harnesses as if their process replicates were interchangeable.

Use AC power and a recorded, unchanged power plan. Allow a fixed cooling/settling
period, capture CPU-performance and available thermal counters, and record missing
sensors rather than claiming thermal stability. Select a CPU from independent
host observations using physical-core/SMT topology, not only the lowest logical
CPU load. Pin the benchmark normally; do not change other processes' affinity,
disable security tools, or claim that affinity eliminates interrupts.

For WPR, use an elevated recorder with a unique `-instancename` for start/status/stop,
while keeping Cargo and benchmark processes unelevated. Never change broad user
rights to fix a filtered-token problem or stop another trace instance. A canceled
elevation prompt leaves live tracing blocked and must not be bypassed.

Additional sessions on this computer establish repeatability here only. WSL tests
exercise Unix code, not an independent physical Linux performance host. Broader
Windows/Linux/macOS performance claims require separately recorded measurements
on those hosts; unavailable platforms remain explicit coverage gaps.

### Native idle-host admission

Strict Windows duplicate profiles require two explicit, predeclared limits:
`--idle-max-core-busy-percent` and `--idle-max-sample-busy-percent`. They are also
available on `common-refs` and `lock-refs`. Both must be finite with
`0 < mean <= sample <= 100`. There is no inferred or automatically tuned limit.
The 5/20 example above is a conservative operational admission choice, not a
calibrated guarantee of a particular confidence interval or outlier rate.

The native 64-bit Windows collector waits 60 seconds, then takes exactly 30
one-second intervals using language-neutral PDH counters. It sums all SMT-sibling
busy percentages per physical core rather than dividing by the sibling count.
Both the mean and every sample must meet their limits. Among admitted cores it
selects minimum mean, peak, mask, then sibling mean/CPU, within inherited allowed
affinity. No benchmark ratio enters selection. Unsupported processor groups,
missing/invalid counters, delayed observations or unknown/offline AC power fail
closed. It neither changes the power plan nor disables any security control.

Admission occurs after the build, before measurements. Counter collection ends
before timing starts. Only the runner and subsequently spawned benchmark children
are pinned; the runner restores its previous affinity afterward. The observation,
policy and selection are retained in `artifacts/host-admission.json`, and successful
comparison reports bind its SHA-256. A refusal publishes invalid execution
evidence and starts no measured child. There are no automatic replacement windows.
Pre-run admission does not guarantee continued quietness or thermal stability;
the existing A/A, error, outlier and environment-drift gates remain mandatory.

For a standalone preflight without building or pinning a benchmark, use:

```text
cargo +1.98.1 xtask bench admit-host --idle-max-core-busy-percent 5 --idle-max-sample-busy-percent 20
```

### Noise magnitudes without gate changes

Paired-ref runs retain `artifacts/noise.json`, bound by
`method.noise_report_sha256`, even when canonical blocks are rejected. The same
summary is available using `bench noise-report --samples <run.stdout.tsv>`;
repeat `--samples` for multiple process logs. Missing or malformed inputs are
reported rather than silently dropped. The sample correlator also includes it.

Each metric reports absolute `ratio / process_median - 1` excursions, counts
above 1% and 2%, the number of relative 3-MAD outliers within 1%, and p50/p90/p95/
p99/max excursion magnitudes. Quantiles use linear interpolation at `(n-1)*p`.
Values are fractions and describe sample-average ratios, not individual-call
latency percentiles or inferential confidence bounds. All samples participate;
this diagnostic sidecar cannot validate timing summaries, comparison completeness
or acceptance. The 30% per-process outlier gate is unchanged, not replaced by
these diagnostic magnitude thresholds.
