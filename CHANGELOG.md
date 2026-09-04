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
- Adds Rust-native `cargo xtask` support-matrix, workflow-policy, and
  compatibility checks in an unpublished workspace tool.

### Changed

- Deprecates `FileExt::duplicate` in favor of `File::try_clone` while retaining
  the inheritable fs2 0.4 runtime behavior for compatibility.
- Moves to Rust 2024 with Rust 1.88.0 as the minimum supported Rust version.
- Replaces legacy Windows bindings with focused `windows-sys` features and
  keeps Unix and Windows implementations in responsibility-specific modules.

### Fixed

- Protects Windows command-capture directories and consumes captured output
  through retained handles.

- Preserves verified allocation, locking, path, filesystem-counter, provider
  fallback, duplicated-handle, and cross-platform error-handling fixes from the
  accepted v0.7 development line.

- Rejects malformed Windows provider results where caller-available space
  exceeds caller-visible total or actual free space, while preserving valid
  quota-limited totals.
- Rejects malformed Windows and Unix provider tuples whose available counters
  exceed their corresponding free or total domains.
