# Changelog

Notable changes to `fs2-turbo`. Library changes and repository-only tooling are
listed separately.

## [1.0.0] - 2026-09-25

First release under the `fs2-turbo` package name, based on [fs2 v0.4.3][baseline].

### Migration and compatibility

The Cargo package changes from `fs2` to `fs2-turbo`; the library crate remains
`fs2`. After publication, replace the dependency entry while keeping existing
`use fs2::...` imports:

```toml
[dependencies]
fs2 = { package = "fs2-turbo", version = "1.0.0" }
```

- **MSRV:** Rust **1.88.0**. The library uses Rust 2024; consumers do not need to
  change their edition.
- **API compatibility:** the v0.4.3 `FileExt`, `FsStats`, `statvfs`,
  `lock_contended_error`, and scalar filesystem-statistics APIs remain available.
  The deprecation and stricter error behavior below are relevant when migrating.

### Added

- Five explicit locking aliases: `fs2_lock_shared`, `fs2_lock_exclusive`,
  `fs2_try_lock_shared`, `fs2_try_lock_exclusive`, and `fs2_unlock`. All support
  Rust 1.88.0. Prefer these names on **Rust 1.89 and later** to avoid collisions
  with inherent `File` locking methods.
- `FsStatsQuery` prepares a path once for repeated snapshots. Each snapshot
  reads fresh filesystem counters; counter values are not cached.

### Changed

- Refactored Unix, Windows, allocation, and statistics code into focused modules
  and specialized scalar filesystem-statistics queries.

### Deprecated

- `FileExt::duplicate()`. Shared file position and inheritable descriptor or
  handle semantics are intentionally preserved. Prefer `File::try_clone()`
  unless inheritance is required. Builds that deny deprecation warnings must
  migrate or explicitly allow this call.

### Security and error handling

- Validate native filesystem counters for overflow, zero allocation granularity,
  and inconsistent free, available, or total space. Valid quota relationships
  remain accepted; malformed native results return errors.
- Strengthen allocation checks and native I/O completion handling. Requests
  whose reservation guarantee cannot be established return errors, subject to
  the documented Apple compatibility behavior. See [SECURITY.md](SECURITY.md)
  for security boundaries and reporting guidance.

### Dependencies

Manifest requirements change as follows:

| Scope | fs2 v0.4.3 | fs2-turbo v1.0.0 |
| --- | --- | --- |
| Windows runtime | `winapi` 0.3 | `windows-sys` 0.61.2 with selected API features |
| Unix runtime | `libc` 0.2.30 | `libc` 0.2.189 |
| Shared runtime | No `cfg-if` dependency | `cfg-if` 1 |
| Tests | `tempdir` 0.3 | `tempfile` 3.27 |

### Repository tooling and validation

- Added `cargo xtask` support-matrix, workflow-policy, compatibility, and coverage
  checks. A frozen v0.4.3 consumer and consumer-edition checks cover editions
  2015, 2018, 2021, and 2024. Rust **1.98.1** is the pinned primary validation
  and release toolchain, separate from the MSRV.
- Added separate library and tooling coverage collection, artifact provenance
  validation, and mutation-testing workflows.
- Hardened Windows command-capture storage in `fs2-dev`, retaining file handles
  when consuming captured output.
- Added `clap`, `serde`, `serde_json`, `serde_yaml_ng`, `sha2`, and `wait-timeout`
  to the unpublished `fs2-dev` tool. These are not runtime dependencies of the
  published library.

[1.0.0]: https://github.com/arthurianresolve/fs2-turbo/pull/4
[baseline]: https://github.com/arthurianresolve/fs2-turbo/tree/9a340454a8292df025de368fc4b310bb736f382f
