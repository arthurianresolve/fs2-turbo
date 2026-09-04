# fs2 benchmark measurement

The lock benchmarks are measurement-only and must not change library behavior.
The v0.4-compatible lock methods are called explicitly through `FileExt` because
newer Rust versions also provide inherent `File` lock methods:

```rust
FileExt::lock_exclusive(&file).unwrap();
FileExt::unlock(&file).unwrap();
```

## Controlled A/B run

`cargo xtask bench refs` creates isolated local checkouts for the baseline and candidate,
uses independent Cargo target directories, runs the requested benchmark in
alternating serialized `A-B-B-A` and `B-A-A-B` order for eight blocks, and retains logs plus Criterion
artifacts. It records commit IDs, harness SHA-256 hashes, the validated shared
measurement policy, toolchain, command
arguments, typed process outcomes, medians, MAD, standard deviation, confidence bounds,
outliers, paired ratios, and a conservative disposition.
Each logical schedule position has three fixed process replicates. The actual
median process estimate at that position enters the two directional ratios, so
one transient process cannot determine a block while every process remains in
the retained run inventory. Alternating block order balances the subjects across
the two center and two edge positions.
An ABBA or BAAB block whose two directional ratios differ by more than the policy's
`max_pair_spread` is retained but classified as environmentally unstable. Such
a report is invalid rather than a performance regression; consistent slowdowns
remain actionable because both directional ratios move together.
The two directional ratios are diagnostic observations, not independent
replicates. Their geometric mean forms one drift-corrected ratio per block, and
only those independent block ratios enter the exact-median acceptance gate.

Example from the repository root:

```text
cargo xtask bench refs --baseline 137e27c --candidate <candidate-commit>
```

Defaults are three process replicates per logical position, 50 samples, a
2-second warmup, a 5-second measurement window,
and the `fs2`, `fs2_legacy`, and `fs_compat` lock benchmarks. Use a unique
`--output`; existing output directories are never overwritten. A nonzero
benchmark exit or missing estimate invalidates that invocation while remaining
repetitions continue, and the failure remains observable in the retained logs.
Stable slowdowns and inconclusive paired results are recorded in
`report.json` and must not be accepted as an optimization. Transient filesystem
failures are retained in the logs as `[fs2-bench] FS2_BENCH_FAILURE` records and counted in
the run's `failures`; any nonempty failure list makes the report invalid even when the process
exit code is zero.
Controlled scalar-stat runs use one recorded `stats-fixture` directory for both
subjects. Ad-hoc Criterion runs keep the temporary-directory fallback, and all
fixture selection remains outside the measured loop.

The policy is validated by `cargo xtask policy` and has one
shared Criterion configuration. The `ref_to_ref` and
`cross_crate` sections retain separate replication units and pairing orders so
the existing workflows do not change protocol. The Rust-native
`cargo xtask bench crates` entry point is reserved for the cross-crate
fs2/fs4 workload and uses the same validated Criterion configuration. The
ref-to-ref gate above is the canonical production-optimization workflow.
Both Rust-native runners validate the policy before starting measurements and write a
versioned `report.json` for completed and invalid runs; invalid reports retain
the failure or orchestration error and must not be used for a performance
decision. Reports are written through a temporary sibling and persisted at the
final path without overwriting existing evidence, so an interrupted write cannot
masquerade as a completed result.

Each subject first runs in Criterion's single-execution test mode. Its process
duration, typed outcome, logs, and failures are retained as cold-start evidence
but never enter timed estimates. Every timed process performs one additional
reported prime before Criterion's excluded warm-up period.
The `file_create_delete` workload also performs one fixed untimed pass over its
4,096-path pool before Criterion starts. This conditions the process-private
directory and leaves the measured create/delete loop unchanged.

The acceptance screen uses the canonical exact distribution-free one-sided
median bound implemented by `fs2-dev`. The shared default allows a
2% non-inferiority margin; pass zero explicitly to require strict parity. A
claimed speedup must repeat across all eight blocks. Eight is the smallest
block count for which the exact 95% median upper bound does not reduce to the
single worst observation.
This screen is deliberately conservative: mixed or inconclusive results are
rejected rather than converted into a performance claim.

Do not interpret Criterion's `change` line from the default `target/criterion`
directory as a standalone regression result. It compares against historical
data that may have been collected under different host conditions. Use
`cargo xtask bench refs`, or set a fresh `CRITERION_HOME` and save a new baseline for each
ad-hoc run before comparing medians.

## Same-process lock compatibility

Use the Rust-native paired lock harness when separate benchmark processes show
directional drift or elevated outlier rates:

```text
cargo xtask bench lock --output target/paired-lock-evidence
```

The harness compares `fs2_lock_exclusive`/`fs2_unlock` with the v0.4-compatible
`FileExt::lock_exclusive`/`FileExt::unlock` path. Both subjects use one prepared
file and run adjacently inside every sample; their order alternates for every
pair. A separate A/A control uses the current path for both subjects. The
default command retains eight process replicates, exact median bounds, raw
stdout/stderr, typed exits, immutable source provenance, and retained policy,
lockfile, and harness artifacts with hashes. A dirty source can only produce
exploratory evidence. Any lock or unlock failure stops that process and invalidates
the report rather than leaving a potentially locked handle in later samples.

The explicitly reported first pair is cold-start evidence only. It is excluded
from warm-up, calibration, sample estimates, ratios, and acceptance bounds.

Each subject is run once as an explicit priming observation before retained
measurements. Priming estimates and Criterion warm-up samples never enter
runtime ratios or confidence bounds; typed process outcomes and wall time remain
in the report.

## Unix path stack buffer

The Unix path-conversion benchmark selected a 3584-byte inline buffer. In the
measured sweep, 3328 bytes gave the best gain per additional 256 bytes of stack,
3584 bytes gave the best raw result in the middle band, and 4096 bytes gave the
maximum gain at the maximum tested stack footprint. Keep these values in the
measurement record rather than the production path module.

## Unix directory authority

On Unix, benchmark workspaces, staging directories, and publication parents are
accepted only when descriptor-relative checks can establish POSIX ownership and
rename protection for every path component. Protected root/current-user symlinks
are resolved only after their containing edge is secured, and their target
ancestry is validated independently. Non-sticky group- or world-writable
components are rejected. A sticky shared ancestor such as `/tmp` is accepted
only when the actual workspace or output parent beneath it is private.

Strict Linux paths are limited to recognized direct local filesystems; unknown,
network, userspace, and layered filesystems fail closed because reported mode
bits may not prove enforcement. Linux 9p/WSL DrvFs is therefore rejected. On
macOS, the path must use a recognized local filesystem without an extended ACL.
Run the checkout, `CARGO_TARGET_DIR`, and evidence destination from the WSL Linux
filesystem, or run the benchmark natively on Windows. Publish into a private
directory rather than directly into a shared sticky directory.
