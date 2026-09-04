# Paired filesystem-stat comparisons

Use `cargo xtask bench stats` for strict Windows filesystem-stat A/B
comparisons. Do not use the separate-process ABBA runner as the acceptance gate
for `free_space`, `available_space`, `total_space`, `statvfs`, or prepared
filesystem-stat snapshots on Windows.

## Why ABBA is not sufficient here

ABBA geometric aggregation removes smooth multiplicative drift. Windows stat
runs have also exhibited abrupt, sustained latency changes between benchmark
processes. A state transition between A and B is attributed to the revision,
and reversing the later pair cannot reliably cancel it. Large directional pair
spread detects this condition but leaves the comparison without a verdict.

The paired runner loads exact baseline and candidate revisions into one binary,
alternates call order within every sample, and evaluates process-replicate
ratios with exact one-sided median bounds. This keeps both calls in the same
host state. It also runs an A/A control by default to detect harness bias.

## Strict run

Run from the repository root with an unused output path:

> [!WARNING]
> Both selected revisions are compiled and executed unsandboxed with the
> current user's ambient authority. Inspect and trust them before acknowledging
> that boundary on the command line.

```text
cargo xtask bench stats --baseline <baseline-ref> --candidate <candidate-ref> --trust-selected-code --output C:\bench-results\fs2-paired-stats
```

The controlled defaults are in the `paired_process` section of
`benchmarks/measurement-policy.json`: eight
process replicates, 50 samples, two seconds of warm-up, five seconds of
measurement per workload, ten seconds between processes, a 95% exact one-sided
median bound, and a 2% non-regression limit. Overrides are supported for smoke
testing, but they mark the report exploratory.

The runner creates isolated local Git sources containing only the two exact
commits. This avoids unrelated local refs; each selected package is renamed to
a benchmark-only baseline or candidate identity before the combined build. It
records the refs, toolchain, harness and policy hashes,
lockfile and binary hashes, native exits, raw output, sample MAD and outlier
diagnostics, and the final bounds in `report.json`.

Operation failures and malformed output remain observable in per-process logs.

By default, secure staging and publication remain beneath the repository root.
If that checkout cannot serve as a protected publication boundary, pass a new
or already protected directory with `--output-root` and place `--output` beneath
it. A missing root is created using the private-directory policy; an existing
root is accepted only when it already satisfies that policy. This changes only
the staging and publication anchor; policy, protocol, harness, and selected Git
revisions remain bound to the repository checkout.
The runner continues subsequent replicates, then marks the report invalid. No
failed or outlying measurement is silently removed.

Each process primes both implementations once per workload before warm-up. The
prime timings and failures are recorded separately and never contribute to the
reported runtime ratio.

The existing ABBA workflow remains appropriate for lock benchmarks and other
workloads whose process-level timing is stable under its drift model.
