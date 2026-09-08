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

The v0.4 `FileExt` methods and their behavior remain available. Rust 1.89 and
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

## Filesystem statistics usage

`statvfs` acquires and validates one consistent set of filesystem counters.
When several counters are needed, read one snapshot rather than calling the
individual convenience functions, which each acquire a new snapshot.
For repeated fresh snapshots of the same filesystem, construct `FsStatsQuery`
once and call `snapshot`. It reuses path preparation without caching counters.

## Development validation

Repository validation is implemented by the unpublished Rust `fs2-dev` tool.
Run it only from a trusted checkout with a trusted host Git/Rust toolchain:
repository code executes with the invoking user's ambient authority.

The supported local entry points are:

```text
cargo +1.88.0 fmt --all -- --check
cargo +1.88.0 test --workspace --locked
cargo +1.88.0 clippy --workspace --all-targets --locked -- -D warnings
cargo +1.98.1 test --workspace --locked
cargo xtask matrix
cargo xtask compatibility
```

Release CI also builds documentation, checks future
incompatibilities, audits locked dependencies, validates package contents, and
builds the extracted package. Repository tooling, policies, compatibility
fixtures are excluded from the published crate.

## License

`fs2-turbo` is primarily distributed under the terms of both the MIT license and the
Apache License (Version 2.0).

See [LICENSE-APACHE](LICENSE-APACHE), [LICENSE-MIT](LICENSE-MIT) for details.

Copyright (c) 2015 Dan Burkert.
