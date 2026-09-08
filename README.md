# fs2-turbo

`fs2-turbo` provides cross-platform file locking, allocation, duplication, and
filesystem statistics. The package exports the `fs2` library crate, so version
1.0 preserves the public fs2 0.4 API while retaining the correctness and safety
fixes developed in this maintained fork. It uses Rust 2024 and requires Rust
1.88.0 or newer.

The original implementation is from
[danburkert/fs2-rs](https://github.com/danburkert/fs2-rs). This maintained fork
lives at
[github.com/arthurianresolve/fs2-turbo](https://github.com/arthurianresolve/fs2-turbo).

[![Documentation](https://docs.rs/fs2-turbo/badge.svg)](https://docs.rs/fs2-turbo)
[![Crate](https://img.shields.io/crates/v/fs2-turbo.svg)](https://crates.io/crates/fs2-turbo)

## Installation

```toml
[dependencies]
fs2 = { package = "fs2-turbo", version = "1" }
```

## Features

- File descriptor duplication.
- Shared and exclusive file locks.
- File preallocation and allocated-size queries.
- Filesystem snapshots and scalar space queries.
- Prepared `FsStatsQuery` values for repeated fresh snapshots.

On Unix and Windows, `FileExt::duplicate` retains the original crate's
inheritable duplicate semantics. Use `File::try_clone` when the duplicate must
not be inherited by a child process.

`FileExt::allocate` may use an exact-size platform operation. Callers must
exclusively own file-length changes while it runs; advisory locks provide that
exclusion only when every participant follows the same protocol.

## Compatibility

The v0.4 `FileExt` methods and their behavior remain available. Rust 1.97 and
newer also provide inherent locking methods on `std::fs::File`; use fully
qualified calls when the fs2 implementation must be selected explicitly:

```rust
use fs2::FileExt;
use std::fs::File;
use std::io;

fn locked(file: &File) -> io::Result<()> {
    FileExt::lock_exclusive(file)?;
    FileExt::unlock(file)
}
```

The `fs2_lock_shared`, `fs2_lock_exclusive`, `fs2_try_lock_shared`,
`fs2_try_lock_exclusive`, and `fs2_unlock` forwarding methods are retained for
collision-safe migration code.

## Platforms

The `fs2` library supports the Unix and Windows targets implemented by the platform
adapters in this repository. Unix support uses
[`libc`](https://github.com/rust-lang/libc); Windows support uses
[`windows-sys`](https://github.com/microsoft/windows-rs).

On Windows, filesystem snapshots report physical total capacity when the
modern disk-space provider is available. On systems that require the legacy
fallback, the reported total may be limited by the calling user's disk quota.
All providers reject inconsistent results where caller-available space exceeds
caller-visible total or actual free space. Valid quota behavior remains
supported: physical free space may exceed a caller-visible, quota-limited total.

The CI matrix continuously tests the native `x86_64` targets on Linux, macOS,
and Windows, plus native `aarch64` macOS, with Rust 1.88.0 and stable. The
historical 32-bit and GNU Windows targets are not currently covered by the native test matrix. The
`armv7-unknown-linux-uclibceabihf` target is compile-checked separately with
nightly `build-std`; runtime tests require a target-specific emulator and
uClibc sysroot.

The target evidence and allocation capability claims are recorded in the
repository-only `support-matrix.json` registry. CI uses literal native and
cross-target job matrices, and validation rejects drift between those entries
and the registry before runtime tests or compile-checks run. Compile-only
evidence does not imply runtime support.

## Benchmarking

Stable Criterion benchmarks are provided for the public APIs in the separate
`fs2-benchmarks` workspace member. From a repository checkout, run them with
`cargo bench --manifest-path benchmarks/Cargo.toml`. When benchmarking files,
account for the filesystem backing the temporary directory; `/tmp` is often a
`tmpfs` mount.

`statvfs` is the snapshot-first interface: it acquires and validates one
consistent set of filesystem counters. When several counters are needed, prefer
one `statvfs` snapshot and read its accessors rather than calling the individual
convenience functions, which each acquire a new snapshot.
When an application needs fresh snapshots repeatedly for the same filesystem,
construct `FsStatsQuery` once and call `snapshot`; it reuses platform path
preparation without caching counter values. The `stats_snapshot` and
`prepared_stats` benchmark groups measure both usage patterns. On Windows, the
`windows_root_stats` group also measures exact drive-root preparation and scalar
queries.

Repository operations are implemented by the unpublished Rust `fs2-dev` tool:

> [!WARNING]
> `bench refs`, `bench crates`, and `bench stats` compile and execute the
> selected revisions or checkouts with the current user's ambient filesystem,
> environment, credential, process, and network authority. They are not
> sandboxed and require `--trust-selected-code`. Strict reports establish
> provenance and statistical rigor only for trusted subjects.

`cargo xtask` is itself compiled from the current checkout. Run repository
tooling only from a trusted checkout with a trusted host Git/Rust toolchain;
`--trust-selected-code` covers secondary benchmark subjects, not that bootstrap
environment. Strict paired-statistics evidence also requires an existing
directory fixture under ancestry the invoking user can prove resistant to
lower-trust namespace replacement.

On Unix, strict benchmark operations also require provable directory authority
for workspaces, staging, and publication. Linux accepts recognized direct local
filesystems; DrvFS/9p, FUSE, CIFS, OverlayFS, network, and unknown filesystems
fail closed. macOS accepts recognized local filesystems without extended ACLs,
and other Unix platforms fail closed until they have an equivalent authority
check. Use storage inside the WSL Linux filesystem rather than `/mnt/c`, and
publish into a private directory rather than a shared sticky directory.

```text
cargo xtask matrix
cargo xtask compatibility
cargo xtask policy
cargo xtask bench refs --help
cargo xtask bench crates --help
cargo xtask bench lock --help
cargo xtask bench stats --help
```

The compatibility command compiles one frozen v0.4 consumer against exact fs2
0.4.3 and the current checkout across Rust editions 2015 through 2024. The
benchmark commands stage subjects independently, retain typed process outcomes
and artifacts, and apply the versioned policy in
`benchmarks/measurement-policy.json`.

Every benchmark subject receives one explicit unmeasured priming invocation in
each fresh process before warm-up. Criterion ref and cross-crate comparisons
also use a separate single-execution process for cold-start evidence; paired
lock and statistics harnesses record their prime observation inside each
combined process. None of these observations enter runtime estimates, medians,
ratios, confidence bounds, or Criterion samples. Stable comparisons use exact,
distribution-free one-sided 95% median bounds and reject any affected workload
whose upper candidate-to-baseline ratio exceeds `1.02`. Windows filesystem-stat
comparisons additionally use same-process alternating calls and an A/A control
to detect host and fixture-order drift.

The checked-in [comparative benchmark report](benchmark-report.md) distinguishes
the exact measured candidate from the current dev reference. Rows whose
implementation changed after measurement are explicitly excluded from
exact-SHA performance claims until rerun.

## Runner benchmark results

[![Latest accepted dev runner benchmark](https://arthurianresolve.github.io/fs2-turbo/dev.svg)](https://arthurianresolve.github.io/fs2-turbo/dev.html)

Latest accepted hosted-runner measurements for **dev** against fs2 v0.4.3.
Pushes to dev refresh this display after all pilot acceptance gates pass.
The displayed SHA and date identify the measured revision; this is not a
current-head status badge. The three-workload Windows VM pilot is separate
from local benchmark evidence and does not represent the full API suite.

## Development validation

The supported local entry points are:

```text
cargo +1.88.0 fmt --all -- --check
cargo +1.88.0 test --workspace --locked
cargo +1.88.0 clippy --workspace --all-targets --locked -- -D warnings
cargo +1.98.1 test --workspace --locked
cargo xtask matrix
cargo xtask compatibility
cargo xtask policy
```

Release CI also builds documentation and benchmarks, checks future
incompatibilities, audits locked dependencies, validates package contents, and
builds the extracted package. Repository tooling, policies, compatibility
fixtures, and benchmarks are excluded from the published crate.

## License

`fs2-turbo` is primarily distributed under the terms of both the MIT license and the
Apache License (Version 2.0).

See [LICENSE-APACHE](LICENSE-APACHE), [LICENSE-MIT](LICENSE-MIT) for details.

Copyright (c) 2015 Dan Burkert.
