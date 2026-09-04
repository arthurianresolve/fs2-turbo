# fs2-rs Domain Context

## File allocation

File allocation reserves physical filesystem space for a file and ensures the
file length reaches the requested size. `FileExt::allocate` owns the capacity
and length postcondition, and the allocation module owns the public allocated
size projection. Platform adapters provide the current allocation state and
reservation primitive through the allocation seam. A platform without a
reservation primitive must return `Unsupported` rather than claiming the
physical-space guarantee. The caller owns exclusive logical-length changes for
the duration of `FileExt::allocate`; exact-size platform operations cannot
preserve a concurrent, non-cooperating resize, and advisory locks are effective
only when all participants follow the same protocol.

## File locks

File locks provide shared or exclusive advisory access, either blocking or
non-blocking, plus release. `FileExt` routes the requested mode and blocking
behavior through the private platform seam; Unix and Windows adapters translate
that request into operating-system locking calls while preserving
platform-specific behavior. Shared lock-contract tests exercise the
cross-platform interface; adapter tests retain OS-specific semantics.

## Filesystem statistics

Filesystem statistics report free, available, and total space plus allocation
granularity. The stats module owns the named `FilesystemCounters` seam, checked
counter conversion, invariants, the snapshot-first `FsStats` interface, and the
prepared `FsStatsQuery` interface for repeated fresh snapshots; Unix and Windows
adapters acquire the raw counters. Convenience queries remain compatibility
projections and each acquire a new snapshot. The Windows adapter uses a
one-query full snapshot when the operating system supports it, retains the
legacy fallback, uses the narrowest correct query for scalar projections, and
recognizes exact drive roots without repeating volume-root discovery. A guarded
metadata-only handle query accelerates free and available space for online
regular files. Paths that are ineligible for a narrow query retain canonical
volume-root resolution. On Windows, modern snapshots report physical total
space while the legacy fallback may report a quota-limited total for the calling
user; scalar queries preserve the physical-free and caller-available domains and
fall back to the canonical provider when an optimized query is unavailable or
invalid. Every Windows provider requires caller-available space to be no greater
than caller-visible total or actual free space. The modern provider additionally
requires physical free space to be no greater than physical total space; the
legacy provider may report physical free space above a quota-limited
caller-visible total. Platform adapters construct private raw counters through
source-specific constructors; the stats module alone owns their representation,
projection, conversion, and validation. Windows provider attempts use one typed
internal outcome for values and classified fallbacks; the modern provider owns
its dynamic symbol cache, and scalar routing resolves it only after narrow
direct and handle queries are unavailable.

## Support evidence

The support matrix owns target evidence levels, allocation capability claims,
and the CI job metadata that produces that evidence. Runtime evidence and
compile-only evidence remain distinct claims; the validator parses the workflow
and rejects drift between registry job references and the workflow's actual
matrix consumption. JSON is validated once into an immutable support registry;
workflow validation and matrix generation consume only that model.

## Compatibility and performance evidence

The compatibility oracle compiles one frozen v0.4 consumer against exact fs2
0.4.3 and the current checkout across supported Rust editions, then exercises
the shared legacy behavior contract through both adapters. Legacy source shape
and stable behavior come from the v0.4 reference; verified correctness and
safety fixes remain canonical. Rust 1.97 collision fixtures use fully qualified
`FileExt` calls so standard-library inherent lock methods cannot change the API
being tested.

The unpublished Rust `fs2-dev` workspace tool owns support-matrix validation,
compatibility checks, measurement policy, process execution, atomic reports,
and benchmark orchestration. Typed configuration models reject unknown fields
and carry explicit schema versions. One native command runner captures stdout,
stderr, duration, working directory, command-local environment, and a typed
process outcome. Tooling dependencies do not enter the published package or
production dependency graph.

Mutable benchmark paths are security boundaries. Unix tooling traverses them
without following untrusted links, retains directory descriptors, validates
ownership and rename protection, and requires private final staging and
publication directories. Strict Linux operations accept only recognized direct
local filesystems; macOS additionally rejects extended ACLs. Unrecognized,
network, userspace, layered, or otherwise unsupported Unix storage fails closed.

Ref, cross-crate, and paired-stats subjects are trusted executable input. The
tool requires an explicit `--trust-selected-code` acknowledgement before it
builds them, but it does not sandbox their filesystem, environment, credential,
process, or network access. Process containment provides lifecycle cleanup
only. Strict evidence means provenance and statistical rigor under this trusted
subject assumption, not adversarial authenticity or hostile-code isolation.
The current checkout and host Git, Cargo, Rust compiler, and executable search
paths are trusted bootstrap inputs rather than independently authenticated
evidence. Strict paired-statistics runs retain the complete ancestry of an
existing directory fixture throughout measurement.

Performance comparisons independently stage both subjects and their target
directories, use identical harness and lockfile inputs, and retain typed process
outcomes, estimates, dispersion, outliers, disk state, and artifact paths. Every
fresh subject process performs and records one explicit priming invocation
before warm-up and timed work. Criterion comparisons use a separate
single-execution process for cold-start evidence; paired lock and statistics
harnesses record the prime observation inside their combined process. None of
these observations enter runtime statistics.

General ref comparisons use at least eight A-B-B-A blocks and reject blocks
whose directional-pair spread exceeds 20%. Each accepted block contributes one
geometric-mean ratio. Windows filesystem-stat comparisons use eight independent
same-process alternating repetitions plus an A/A control because separate
processes cannot reliably cancel abrupt filesystem-state changes. Exact,
distribution-free one-sided 95% median bounds enforce the shared 2%
non-regression margin. Mixed, unstable, or inconclusive evidence rejects a
production candidate.
