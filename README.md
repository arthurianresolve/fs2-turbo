# fs2-turbo

**The fs2 API, maintained for modern Rust.**

[![actively-developed](https://img.shields.io/badge/actively%20developed%3F-yes-brightgreen.svg)](https://github.com/arthurianresolve/fs2-turbo/graphs/commit-activity)
[![Maintainer](https://img.shields.io/badge/maintainer-arthurianresolve-brightgreen)](https://github.com/arthurianresolve/)
[![dependency status](https://deps.rs/repo/github/arthurianresolve/fs2-turbo/status.svg)](https://deps.rs/repo/github/arthurianresolve/fs2-turbo) ![GitHub Issues](https://img.shields.io/github/issues/arthurianresolve/fs2-turbo)
[![Release gates (dev)](https://img.shields.io/github/actions/workflow/status/arthurianresolve/fs2-turbo/release-gates.yml?branch=dev&label=Release%20gates)](https://github.com/arthurianresolve/fs2-turbo/actions/workflows/release-gates.yml?query=branch%3Adev)
[![CI (dev)](https://img.shields.io/github/actions/workflow/status/arthurianresolve/fs2-turbo/ci.yml?branch=dev&label=CI)](https://github.com/arthurianresolve/fs2-turbo/actions/workflows/ci.yml?query=branch%3Adev)
![Codecov](https://img.shields.io/codecov/c/github/arthurianresolve/fs2-turbo)
[![Rust:_1.98.1](https://img.shields.io/badge/Rust%201.98.1-black?logo=rust)](#rust-version)
[![Rust_Edition:2024](https://img.shields.io/badge/Rust-Edition%202024-orange?logo=rust)](#rust-edition)
[![MSRV: Rust 1.88.0](https://img.shields.io/badge/MSRV-1.88.0-blue?logo=rust)](#platforms-and-validation)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue)](#license) 

  [Why](#why-fs2-turbo) · [Migration](#how-to-migrate) · [API and performance](#what-the-api-provides) · [Compatibility](#compatibility-and-operating-safety) · [Security](#security-and-contributing)
  

-----
Cross-platform file locking, preallocation, descriptor duplication, and filesystem
statistics for Rust. Extends `std::fs::File` on Unix and Windows.


**Package:** `fs2-turbo` | **Library:** `fs2`
## Why fs2-turbo?

The original [fs2][upstream] is no longer actively maintained; its upstream source
history has been inactive since 2018. 

`fs2-turbo` continues that work from the
fs2 v0.4.3 baseline, preserving its public API while addressing modern Rust and
filesystem requirements:

- **Modernized implementation:** Rust 2024, reviewed and evolved to modern file system operations and state of the art optimized dependency selection updates, including native
  `windows-sys` bindings, and focused Unix, Windows, allocation, and statistics modules.
- **Correctness and security hardening:** validated filesystem counters, safer
  failure handling, and explicit contracts around locking, allocation, and inheritance.
- **Targeted performance improvements:** lower latency for selected allocation
  and filesystem queries including the historical Windows comparisons below. Overall churn reduction and no performance regressions (1% tollerance)

[PR #1][evolution] traces the evolution from fs2 v0.4.3 to fs2-turbo v1.0.0,
including implementation owners, per-API forwarding, OS-specific commits, and
benchmark evidence.

## How to migrate

Version 1.0.0 is being prepared and is not yet published to crates.io. To evaluate
the development branch, replace your direct `fs2` dependency with:

```toml
[dependencies]
fs2 = { package = "fs2-turbo", git = "https://github.com/arthurianresolve/fs2-turbo", branch = "1.0.0" }
```

After the 1.0 release is published, use the registry dependency instead:

```toml
[dependencies]
fs2 = { package = "fs2-turbo", version = "1.0" }
```

The dependency key and library name remain `fs2`, so existing `use fs2::...`
imports remain valid. Public v0.4.3 methods remain available, subject to the
[compatibility notes](#compatibility-and-operating-safety) below. This replaces
your direct dependency, not upstream `fs2` used transitively by other crates.

### Locking on Rust 1.89 or higher

Rust 1.89 added [inherent locking methods to `std::fs::File`][std-locks]. Calls such
as `file.lock_shared()`, `file.try_lock_shared()`, and `file.unlock()` can therefore
select the standard library rather than this crate, with different error types
and API contracts.

**Prefer the explicit `fs2_*` methods.** They select this crate unambiguously,
retain its `io::Result` interface, and use the same implementation as the legacy
methods. All five aliases also work at the Rust 1.88.0 MSRV.

```rust
use fs2::FileExt;
use std::fs::File;
use std::io;

fn main() -> io::Result<()> {
    let file = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open("data.bin")?;

    file.fs2_lock_exclusive()?;
    file.allocate(1024 * 1024)?;
    file.fs2_unlock()?;

    let stats = fs2::statvfs(".")?;
    println!(
        "Available: {} / {} bytes",
        stats.available_space(),
        stats.total_space()
    );
    Ok(())
}
```

The file is opened without truncation before acquiring the lock. All participants
must cooperate with the locking protocol when changing its length.

For existing code, fully qualified calls such as `FileExt::lock_exclusive(&file)?`
and `FileExt::unlock(&file)?` also explicitly select this crate.

## What the API provides

All sizes are in bytes. File methods belong to `FileExt`; legacy locking names
appear in parentheses. Statistics getters return values from an existing snapshot.

The performance column reports **historical paired median latency changes on
Windows x86_64/MSVC**, relative to fs2 v0.4.3 unless marked `*`. Negative means
lower latency; these are not percentages calculated from independently collected
timing medians.

| API | Purpose | Paired latency change |
| --- | --- | ---: |
| `fs2_lock_shared()` (`lock_shared`) | Wait for a shared whole-file lock. | Parity * |
| `fs2_lock_exclusive()` (`lock_exclusive`) | Wait for an exclusive whole-file lock. | Parity * |
| `fs2_try_lock_shared()` (`try_lock_shared`) | Attempt a shared lock without waiting. | Parity * |
| `fs2_try_lock_exclusive()` (`try_lock_exclusive`) | Attempt an exclusive lock without waiting. | Parity * |
| `fs2_unlock()` (`unlock`) | Release this file's lock. | Parity * |
| `allocate(len)` | Reserve backing storage and ensure a length of at least `len`, subject to platform guarantees. | **-48.56%** when already satisfied |
| `allocated_size()` | Query allocated storage rather than logical file length. | -0.00% |
| `duplicate()` | Duplicate the file with shared cursor and inheritable descriptor/handle semantics. Deprecated; see below. | +0.02% |
| `fs2::lock_contended_error()` | Obtain the platform's lock-contention error. | Not measured |
| `fs2::statvfs(path)` | Obtain one validated `FsStats` snapshot. | +0.23% |
| `fs2::free_space(path)` | Query actual free filesystem space. | **-95.13%** |
| `fs2::available_space(path)` | Query space available to the caller, accounting for restrictions such as quotas. | **-95.17%** |
| `fs2::total_space(path)` | Query total capacity as reported by the platform provider. | -0.25% |
| `fs2::allocation_granularity(path)` | Query the filesystem's allocation unit. | **-88.94%** |
| `FsStats::{free_space, available_space, total_space, allocation_granularity}()` | Read the four counters from a snapshot without another filesystem query. | Not timed independently |
| `FsStatsQuery::new(path)` | Prepare a reusable filesystem-statistics query. | Not measured |
| `FsStatsQuery::snapshot()` | Obtain fresh counters through a prepared query. | Not measured |

`*` The five `fs2_*` aliases were compared with their legacy methods **within
fs2-turbo**, not with upstream fs2. They met the configured 2% equivalence and A/A
control gates: evidence of parity, not an alias speedup. The aliases solve the
Rust 1.89+ method-selection ambiguity described above.

**Reading the results:** near-zero differences do not establish a meaningful
speedup or regression. Calling all four scalar convenience queries separately
showed a paired 69.74% latency reduction. [PR #1][benchmarks] contains the full
timings, workload variants, A/B and A/A ratio ranges, and limitations. The benchmark
harness has since been removed from `dev`; these are retained measurements, not
fresh current-head results or guarantees for other machines. No Unix,
contended-lock, or `FsStatsQuery` performance claim is made.

### Choose the right statistics query

- **One counter:** use the matching scalar function, such as `available_space(path)`.
- **Several counters together:** call `statvfs(path)` once, then use the `FsStats`
  getters instead of issuing four independent queries.
- **Repeated snapshots:** prepare an `FsStatsQuery`, then call `snapshot()` as
  needed. Path preparation is reused; the counters are not cached. Recreate the
  query when current-directory or mount, junction, or symlink changes should
  change the path's meaning.

For the full API documentation, run
`cargo doc --package fs2-turbo --lib --no-deps --open` from a checkout.

## Compatibility and operating safety

The v0.4.3 API remains available, but compatibility is not a promise that compiler
requirements, diagnostics, or invalid-input behavior never change.

- **Compiler floor:** Rust 1.88.0 is required. Older compilers supported by
  upstream fs2 are outside this fork's support contract.
- **Locks:** treat them as a coordination mechanism for cooperating processes,
  not an access-control boundary. Avoid mixing lock APIs on the same file or
  assuming identical behavior for duplicated handles across platforms.
- **Duplication:** `FileExt::duplicate()` deliberately preserves the original
  inheritable descriptor/handle behavior and shared cursor. It is deprecated;
  use `File::try_clone()` when inheritance is not required. Consumers that deny
  deprecation warnings need to migrate or explicitly allow this compatibility call.
- **Allocation:** callers must exclusively own file-length changes. A platform
  operation may set an exact length; advisory locks protect only cooperating
  participants. Unsupported reservation guarantees can return
  `ErrorKind::Unsupported`. Sparse and compressed files have platform-specific
  restrictions documented in the [API contract][file-api].
- **Statistics:** malformed or inconsistent counters are rejected. On Windows,
  the modern provider reports physical total capacity; a legacy fallback may
  report a quota-limited total. Actual free space can therefore exceed a
  quota-limited total, while caller-available space cannot exceed either.

## Platforms and validation

Native CI is configured for Linux x86_64, Windows x86_64/MSVC, and macOS x86_64
and aarch64, with Rust 1.88.0 and stable lanes. Platform adapters use `libc` on
Unix and `windows-sys` on Windows.

**Rust 1.88.0 is the exact MSRV; Rust 1.98.1 is the recommended development and
pinned release-validation toolchain.** The [support matrix][support] distinguishes
native runtime evidence from compile-only coverage. Historical 32-bit and GNU
Windows targets are not covered by the native test matrix; cross-compilation
alone does not establish runtime support.

Useful development checks:

```sh
cargo +1.88.0 test --workspace --locked
cargo +1.98.1 fmt --all -- --check
cargo +1.98.1 clippy --workspace --all-targets --locked -- -D warnings
cargo xtask compatibility
```

The v0.4.3 compatibility oracle exercises the retained API. Release gates also
cover documentation, future-compatibility diagnostics, dependency audits, package
contents, and building the extracted package. These describe the configured
checks, not a claim that every current run has passed.

`tools/fs2-dev` is unpublished repository tooling, excluded from the library
package. Run it only in a trusted checkout with trusted tools: it launches
processes with your account's permissions, not inside a security sandbox.

## Security and contributing

For vulnerability reports, supported versions, and security boundaries, follow
[SECURITY.md][security]. Report suspected vulnerabilities privately using the
process there rather than opening a public issue.

For bugs and improvements, [open an issue][issues] with the platform, Rust version,
and a minimal reproduction. [PR #1][evolution] is the implementation and migration
reference for the 1.0.0 release.

## License

Licensed under [MIT][mit] or [Apache-2.0][apache], at your option.

Copyright (c) 2015 Dan Burkert.
Copyright (c) 2026 George Dietrichsbruckner (MacArthur).

-----

## ⭐ Found this useful? Star this repo to help others find it!

[upstream]: https://github.com/danburkert/fs2-rs
[evolution]: https://github.com/arthurianresolve/fs2-turbo/pull/1
[benchmarks]: https://github.com/arthurianresolve/fs2-turbo/pull/1#benchmarks
[std-locks]: https://doc.rust-lang.org/stable/std/fs/struct.File.html#method.lock_shared
[file-api]: https://github.com/arthurianresolve/fs2-turbo/blob/dev/src/lib.rs
[support]: https://github.com/arthurianresolve/fs2-turbo/blob/dev/support-matrix.json
[security]: https://github.com/arthurianresolve/fs2-turbo/blob/dev/SECURITY.md
[issues]: https://github.com/arthurianresolve/fs2-turbo/issues
[mit]: https://github.com/arthurianresolve/fs2-turbo/blob/dev/LICENSE-MIT
[apache]: https://github.com/arthurianresolve/fs2-turbo/blob/dev/LICENSE-APACHE
[maintainer]: https://github.com/arthurianresolve
