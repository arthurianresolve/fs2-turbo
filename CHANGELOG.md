# Changelog

All notable changes to this project are documented in this file.

## 1.0.0 - 2026-09-03

### Compatibility

- Preserves the fs2 0.4 `FileExt` method names, signatures, locking behavior,
  allocation behavior, error mapping, and handle ownership semantics.
- Retains collision-safe `fs2_*` forwarding methods for Rust 1.97 and newer.
- Compiles the frozen v0.4 consumer and the current API across Rust editions
  2015, 2018, 2021, and 2024.

### Added

- Adds `FsStats`, `FsStatsQuery`, `statvfs`, and validated scalar filesystem
  space queries.
- Adds Rust-native `cargo xtask` validation, compatibility, policy, and
  performance commands in an unpublished workspace tool.
- Adds versioned benchmark policy, explicit first-invocation evidence, exact
  non-regression bounds, and same-process Windows filesystem-stat controls.

### Changed

- Deprecates `FileExt::duplicate` in favor of `File::try_clone` while retaining
  the inheritable fs2 0.4 runtime behavior for compatibility.
- Moves to Rust 2024 with Rust 1.88.0 as the minimum supported Rust version.
- Replaces legacy Windows bindings with focused `windows-sys` features and
  keeps Unix and Windows implementations in responsibility-specific modules.
- Makes strict Unix benchmark workspaces, staging, and publication fail closed
  when ownership, rename protection, filesystem authority, or ACL state cannot
  be established.
- Excludes repository tooling, benchmarks, policies, compatibility fixtures,
  and CI metadata from the published crate.

### Fixed

- Preserves verified allocation, locking, path, filesystem-counter, provider
  fallback, duplicated-handle, and cross-platform error-handling fixes from the
  accepted v0.7 development line.
- Prevents lower-trust Unix namespace replacement around benchmark workspaces,
  private staging, and evidence publication, confines output beneath the trusted
  benchmark root, and retains no-replace output publication.
- Protects Windows command-capture directories, consumes captured output through
  retained handles, and confines evidence publication to mutation-safe ancestry
  beneath the trusted benchmark root before the no-replace move.
- Enforces explicit selected-code acknowledgement for every benchmark mode.
- Rejects malformed Windows provider results where caller-available space
  exceeds caller-visible total or actual free space, while preserving valid
  quota-limited totals.
- Rejects malformed Windows and Unix provider tuples whose available counters
  exceed their corresponding free or total domains.
