# fs2-rs comparative benchmark

Baseline: origin/0.4.3 at 9a340454a8292df025de368fc4b310bb736f382f
Measured candidate: dev at a10f82678eebf5b9235908b42c4378de57a37c6b
Current dev reference: 79f16d0b528030dc38b803b3cf1ae677d71d8cef
Current tracked-worktree snapshot: 46aae5b5fa131458f84d74f1d6c83838693f7c57
Host: Windows x86_64, Rust 1.97.1 MSVC, measurements pinned to CPU 0

Protocol: 8 independent ABBA blocks; 50 Criterion samples per case; 2 s warm-up; 5 s measurement; 10 s cooldown; serial execution.
Execution: 32 of 32 suite invocations exited zero; 19 common cases; 608 current estimate files.
Scope: the common legacy benchmark surface only. Dev-only APIs such as FsStatsQuery and the modern fs2_* methods have no 0.4.3 counterpart and are not included in the direct comparison.

The `lock_unlock` row was refreshed for `ca58b4e` using eight same-process A/B replicates against the exact synchronous v0.4.3 lock sequence; the other rows retain the original full-suite measurements.

| Case | 0.4.3 p50 | dev p50 | Paired median delta | 8-block ratio range | Outliers |
| --- | ---: | ---: | ---: | ---: | ---: |
| allocated_size | 1.98 us | 1.85 us | -4.61% | 82.48% to 121.41% | 138 / 1600 (8.62%) |
| available_space | 3,058.34 us | 72.15 us | -97.75% | 1.81% to 2.49% | 140 / 1600 (8.75%) |
| available_space_file_fallback | 3,403.12 us | 222.02 us | -93.74% | 5.04% to 7.09% | 130 / 1600 (8.12%) |
| duplicate | 1.24 us | 1.23 us | -1.99% | 77.14% to 118.61% | 100 / 1600 (6.25%) |
| file_allocate_already_satisfied | 8.03 us | 1.94 us | -75.48% | 17.94% to 38.16% | 107 / 1600 (6.69%) |
| file_create_delete | 856.36 us | 822.77 us | +1.87% | 68.42% to 113.25% | 37 / 1600 (2.31%) |
| file_open_allocate_delete | 1,561.11 us | 1,528.66 us | -0.11% | 71.97% to 155.09% | 265 / 1600 (16.56%) |
| file_open_truncate_delete | 1,589.65 us | 1,644.28 us | -0.99% | 85.65% to 163.98% | 268 / 1600 (16.75%) |
| free_space | 3,097.55 us | 69.92 us | -97.77% | 1.70% to 2.59% | 144 / 1600 (9.00%) |
| free_space_file_fallback | 3,432.54 us | 222.18 us | -93.67% | 4.38% to 7.70% | 140 / 1600 (8.75%) |
| lock_unlock | 2.72 us | 2.72 us | -0.02% | 99.08% to 101.28% | 40 / 400 (10.00%) |
| stats_snapshot/four_convenience_queries | 12,255.53 us | 6,076.80 us | -50.86% | 39.08% to 53.82% | 122 / 1600 (7.62%) |
| stats_snapshot/one_snapshot | 3,096.03 us | 2,940.25 us | -1.89% | 73.07% to 101.19% | 122 / 1600 (7.62%) |
| total_space | 3,063.47 us | 2,916.75 us | -2.51% | 76.54% to 110.61% | 139 / 1600 (8.69%) |
| windows_root_stats/allocation_granularity | 98.64 us | 39.95 us | -60.30% | 31.15% to 45.02% | 127 / 1600 (7.94%) |
| windows_root_stats/available_space | 99.68 us | 39.88 us | -59.92% | 32.62% to 48.97% | 121 / 1600 (7.56%) |
| windows_root_stats/free_space | 100.18 us | 40.05 us | -59.52% | 29.72% to 44.02% | 168 / 1600 (10.50%) |
| windows_root_stats/one_top_level_snapshot | 99.68 us | 40.47 us | -59.14% | 33.39% to 42.78% | 90 / 1600 (5.62%) |
| windows_root_stats/total_space | 98.68 us | 39.62 us | -60.53% | 29.66% to 44.85% | 133 / 1600 (8.31%) |

Interpretation: negative delta means dev is faster; positive delta means dev is slower. The clearest improvements are space queries, already-satisfied allocation, batched stats convenience queries, and Windows-root stats. `lock_unlock` now matches v0.4.3 within the 2% non-inferiority margin. Small changes should be treated as noisy where the paired range crosses 100% or the run-to-run spread is large.

Limitations: this is controlled exploratory historical comparison evidence. The full-suite rows use the revisions' historical dependency/lockfile graphs, while the refreshed lock row uses a focused same-process reference protocol and does not measure contended throughput. No repository source files were changed by the benchmark.

## Current Windows statistics non-regression

A strict same-process paired run on 2026-09-03 compared the last
pre-hardening production implementation at
`81e704ae378fcaa04e8ed82d7575728ed5478d44` with the current production
implementation at `35436b1c515bedd1338de101124b42a1cde3976a`. The run used
Rust 1.97.1, CPU affinity mask `1/ff`, eight A/B process replicates, eight A/A
control replicates, 50 samples, two seconds of warm-up, five seconds of
measurement per workload, and the repository's 2% non-regression margin.

| Workload | Median candidate/baseline | Exact upper ratio | Disposition |
| --- | ---: | ---: | --- |
| free_space | 0.999336 | 1.005784 | Non-inferior |
| available_space | 1.003000 | 1.012621 | Non-inferior |
| total_space | 0.999076 | 1.004943 | Non-inferior |
| allocation_granularity | 0.999483 | 1.008896 | Non-inferior |
| stats_snapshot/one_snapshot | 0.999079 | 1.002233 | Non-inferior |
| prepared_stats/construct_query | 0.993839 | 1.007687 | Non-inferior |
| prepared_stats/one_prepared_snapshot | 1.003326 | 1.007455 | Non-inferior |

Every A/B upper bound remained below `1.02`; every A/A control was balanced;
the report contained no environment or execution anomalies. The decision was
`strict-non-regression-pass`. The policy, harness, paired-stats protocol,
lockfile, and binary SHA-256 values were respectively
`f56e729354203964e2a03a2e8c60606fea600fd6b69455a345b39b6ec0e5449d`,
`9eee6389fb21740f0b67de2eaaf57ebd7ec1143fa48061f682e75264517f5fc5`,
`dfd415abc941dacf498680646f5827a23d81ab0854ba6ca74280dba94b4dbf29`,
`82b89083f08d50b452d13bfb35c37ccfdacdcba692c7a717abd9f34655651168`,
and `cc7392b999bd74df88403ff3ab2b115a61bd2a01be7e4e2eb7e6b3762a16a38d`.

This supplemental run isolates the Windows statistics hardening. It does not
compose its ratios with the older v0.4.3 run or turn those historical rows into
a fresh direct v0.4.3 comparison.

## Current worktree statistics versus v0.4.3

A fresh strict same-process paired run on 2026-09-03 compared v0.4.3 at
`9a340454a8292df025de368fc4b310bb736f382f` with the immutable tracked-worktree
snapshot `46aae5b5fa131458f84d74f1d6c83838693f7c57`, whose parent is the current
committed dev reference `c320a4c8cf597361c05a6ca599afbf7d5aeb1110`.
The run used Rust 1.97.1 MSVC, CPU affinity mask `1/ff`, eight A/B process
replicates, eight A/A control replicates, 50 samples, two seconds of warm-up,
five seconds of measurement per workload, ten seconds of cooldown, and the
repository's 2% non-regression margin.

v0.4.3 predates `FsStatsQuery`, so the seven-workload harness cannot compile
against that baseline. This run used a retained and hashed disposable adapter
limited to the five statistics workloads implemented by both revisions. The
adapter was not included in either benchmarked source revision.

| Workload | Median candidate/baseline | Median delta | Exact upper ratio | Disposition |
| --- | ---: | ---: | ---: | --- |
| free_space | 0.049941 | -95.01% | 0.050699 | Non-inferior |
| available_space | 0.050080 | -94.99% | 0.050986 | Non-inferior |
| total_space | 0.999009 | -0.10% | 1.002223 | Non-inferior |
| allocation_granularity | 0.100128 | -89.99% | 0.100735 | Non-inferior |
| stats_snapshot/one_snapshot | 0.998938 | -0.11% | 1.006137 | Non-inferior |

Every A/B exact upper bound remained below `1.02`; every A/A control was
balanced; and the report contained no environment, execution, or observation
anomalies. The requested confidence was 95%; the exact A/B bounds achieved
96.484375%, and the simultaneous A/A bounds achieved at least 99.21875%.
The decision was `strict-non-regression-pass`.

For `allocation_granularity`, the exact candidate/baseline ratio interval was
`0.099004` to `0.100735`. The candidate's median duration was about 10.01% of
v0.4.3, equivalent to an approximately 9.99-fold speedup for this workload.

The policy, disposable harness, paired-stats protocol, lockfile, manifest, and
report SHA-256 values were respectively
`f56e729354203964e2a03a2e8c60606fea600fd6b69455a345b39b6ec0e5449d`,
`eb7057149953db7ade290ced5cb4cd09500276434fc6365f1dd5e712f7407778`,
`1e5c366532450d9fd596d57def3958515a21e32e52be652294e1bf205d0cc4c2`,
`b009066781ece9ab17e7d72d91e2238d970b303fceeb732480f4c2071bc1f63e`,
`5b97c7d43a4902945dbdb3efe2705aa7c3ca334c62c365629cd4916e3b684f4e`,
and `2a440123438baba5d484929d32f4a35c1b64902676722d9a41cd03d116adbd53`.

The retained local report is
`C:\Users\georg\f2b-46aae5b5-r2\results-allocation-46aae5b5-cpu0-strict-r2\report.json`.
This result establishes non-inferiority within the policy's 2% margin for all
five common functions and a substantial measured improvement for
`allocation_granularity`. It is not proof of zero possible slowdown and does
not provide a direct v0.4.3 comparison for candidate-only prepared-query APIs.

## Benchmark harness dependency validation

Commit `79f16d0b528030dc38b803b3cf1ae677d71d8cef` replaces the benchmark-only
`page_size` 0.6.0 Windows binding from `winapi` 0.3.9 to the workspace's
existing `windows-sys` 0.61.2 family. It does not change the published `fs2`
dependency graph, library source, public API, benchmark workloads, or
`page_size` API and caching behavior. The active workspace lockfile no longer
contains `winapi`, `winapi-i686-pc-windows-gnu`, or
`winapi-x86_64-pc-windows-gnu`; `winapi-util` remains independently reachable
through benchmark tooling.

The implementation was validated on Windows x86_64 from the pre-commit
working tree that became `79f16d0`; the only subsequent pre-commit edit removed
trailing whitespace from copied upstream documentation. Rust 1.88.0 passed all
`fs2` unit, integration, lock-contract, and upstream-compatibility tests,
warning-free clippy, and compilation of all five benchmark executables. Rust
1.98.1 passed the full workspace/all-target test set. A direct runtime probe of
the patched crate returned a 4,096-byte page size and a 65,536-byte Windows
allocation granularity. Temporary Socket active-graph scan
`cbafd3d0-5f6e-48ba-8409-1e61131d6d59` passed policy with no non-ignored alert
rows and no alert for `windows-sys` or `windows-link`.

A short unpinned Criterion smoke run exercised
`windows_root_stats/allocation_granularity` successfully and reported an
estimate interval of 34.068 us to 35.899 us. Criterion also displayed a 57.564%
regression against stale saved history. That comparison is not controlled or
revision-matched and is explicitly excluded from performance conclusions.

The controlled tables above were not regenerated and remain bound to their
recorded revisions, dependency graphs, and protocols. Future paired runs must
resolve both baseline and candidate through the same patched benchmark lock
graph before results are treated as comparable.

## Candidate reference provenance

The measured candidate reference is
`a10f82678eebf5b9235908b42c4378de57a37c6b`. The benchmark run itself was
executed at `ca58b4ee588c016fa82d39c6fa622ae50cf59ef8`; the intervening commits
changed reporting, tests, CI, and formatting in unpublished tooling without
changing the benchmarked `fs2` library implementation.

Post-measurement commits through
`81e704ae378fcaa04e8ed82d7575728ed5478d44` changed unpublished benchmark and
process tooling without changing the measured `fs2` implementation. Commit
`87ad22657f8661b3fade2c177ee9fcadbbf2d406` then hardened Windows
filesystem-counter validation and changed the direct query to collect
caller-visible total space. The strict paired run above directly covers that
production change through `35436b1c515bedd1338de101124b42a1cde3976a`.

The original v0.4.3 rows remain historical measurements rather than an
exact-SHA performance claim for the current implementation. The current direct
common-statistics conclusion is tied to snapshot `46aae5b5` under the paired
run's recorded host and policy; candidate-only prepared-query APIs remain
outside that direct v0.4.3 comparison.

Subsequent tracked worktree changes, including fail-closed provider-domain
checks for malformed Windows and Unix counter tuples and the guarded Windows
allocation-granularity handle fast path, were captured by immutable synthetic
snapshot `46aae5b5fa131458f84d74f1d6c83838693f7c57` and are covered by the direct
v0.4.3 common-statistics run above. The disposable five-workload adapter is not
part of that snapshot. Because the snapshot is not a published branch tip,
refresh the comparison against the eventual exact release SHA before making a
release-grade performance claim.
