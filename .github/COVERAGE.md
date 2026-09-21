# Coverage evidence policy

The public headline is measured fs2-turbo library line coverage on the three primary
native targets with Rust 1.98.1. It is not a claim of complete repository, branch,
MC/DC, or raw combined compiler-instantiation coverage. The unit-only profile has a
separate strict instantiation gate.

## Runner ownership and acceptance

Library and tooling validation run as separate GitHub-hosted jobs, not as two
package test suites sharing one runner. Native CI retains Linux x86_64, Windows
x86_64 MSVC, and both macOS architectures on Rust 1.88.0 and stable. Release
validation retains exact Rust 1.88.0 and Rust 1.98.1 jobs for each package.

| Responsibility | Jobs | Evidence ownership |
| --- | --- | --- |
| Library tests, formatting, Clippy and legacy compatibility | CI `check`; Release gates `library` | `fs2-turbo`, whose library is named `fs2` |
| Tooling tests, formatting, Clippy and workflow policy | CI `tooling_check`; Release gates `tooling` | Unpublished `fs2-dev` |
| Primary and supplemental library coverage | CI `coverage` and `coverage_extended` | `primary` and `extended` receipts |
| MSRV library coverage | MSRV coverage evidence `coverage-msrv` | `msrv` receipts |
| Nightly library branch coverage | Nightly branch coverage gate `branch_coverage` | `branch` receipts |
| Native Rust tooling coverage | CI `coverage_tooling` | `tooling` receipts, never merged into library totals |

Library jobs may invoke `fs2-dev` as a compatibility or coverage validator; that
does not run its test suite or instrument it into library coverage. Full tooling
tests include the coverage-policy regressions, so focused library mutation jobs
no longer rebuild and rerun that tooling subset. Both mutation workflows remain.

The existing `Complete primary coverage evidence` check also requires successful
native tooling checks. The existing release `Rust 1.88.0` and `Rust 1.98.1` check
names aggregate the separate library and tooling matrices. Failed or unexpectedly
skipped prerequisites fail these checks; separation must not create a green gate
with missing package validation. Release acceptance still requires all relevant
workflows to pass on the same commit, including mutation checks when applicable.

Coverage producers retain separate combined, unit, integration, tooling, MSRV,
and branch build/profile directories. No instrumented build or report cache is
shared across these profiles, packages, targets, or compiler versions. Runner
labels, image revisions, source hashes, compiler identity, and run attempts remain
recorded with the reports. Each collector checks its own run's complete artifact
set; cross-workflow acceptance does not authorize mixing report sets.

The cleanup changes orchestration, not coverage calculations or thresholds. It
removes duplicate tooling tests from the matrix preflight and mutation runners,
preserves native test coverage, and bounds previously unbounded job durations.
Separate hosted jobs have setup overhead; a faster total CI time is not claimed
without measurements. Workflow edits require fresh exact-SHA CI before acceptance,
even when the measured Rust source and reviewed baseline are unchanged.

## Required primary evidence

- Linux x86_64, Windows x86_64 MSVC, and macOS ARM64 each require every reported
  executable source line to be covered. Cross-platform merging cannot rescue a miss.
- The collector requires all artifacts from the same commit, tree, run, and attempt.
  Each producer seals its report set after the source-identity check; digests, native
  compiler identity, source manifests, and tracked source inventories are checked.
- Source hashes accept the exact checkout bytes or equivalent LF/CRLF text. This
  accommodates Git checkout line endings without accepting arbitrary source changes.
- LLVM LF/LH summaries are retained separately from unique LCOV DA coordinates.
  Summary differences are diagnostics, not a reason to rewrite the measured source.
- The reviewed baseline in `coverage-policy.json` records the existing exact-SHA
  report scope and raw metrics. Raw LLVM misses may not increase, and exact coverage
  ratios may not decrease. Inventory or baseline changes require explicit review.
- The nonempty unit-only profile requires 100% of raw LLVM instantiations. Its
  denominator may not shrink below the reviewed per-target baseline. Combined raw
  instantiations remain visible compiler-sensitive diagnostics; their misses and
  compiler-asymmetric definition groups may not grow beyond the reviewed baseline.
- Combined and unit source-definition and source-location groups must remain fully
  exercised. Integration-only residuals are accepted only when the exact topology
  is unit-covered and its source location has a target-specific review annotation.
  New unowned or unclassified gaps fail closed.
- Source files without line records are listed explicitly. Test files, cfg-disabled
  modules, and files containing no executable definitions are not labelled covered.

## Separate evidence

- Rust 1.88.0 keeps its own native profiles and existing coverage policy. Its reports
  are never combined with Rust 1.98.1 to mask compiler-specific gaps.
- fs2-dev Rust tooling coverage has separate native reports and no invented 100%
  baseline. Shell and JavaScript repository scripts are outside that Rust metric.
- macOS Intel and Linux ARM64 are supplemental measurements, not extensions of the
  existing three-target 100% claim until their exact-SHA results are reviewed.
- Supplemental jobs run for `dev` and `1.0.0` pushes and CI manual/monthly canary
  runs. GitHub schedules run from the default `1.0.0` branch.
- The manual-only release-head Codecov validation retains region-aware JSON on the
  isolated logical `coverage-validation` branch, separately from release line
  coverage. Its partial-line presentation is not a replacement
  for LLVM region totals, branch or MC/DC measurement, or instantiation coverage.
  See the publication rules below for upload and ingestion requirements.
- Existing broad and focused mutation workflows are retained. Unviable mutations
  are not killed mutants; timeouts and missing outcomes are not successful evidence.
- The pinned nightly branch gate measures reviewed branch outcomes independently on
  all three native targets. MC/DC remains unmeasured; LLVM branch instrumentation
  does not cover every Rust construct and is not complete instantiation coverage.

## Publication

Release-headline Codecov publication remains restricted to trusted events on 1.0.0.
Three separate uploads carry primary-linux, primary-windows, and primary-macos flags.
The combined and per-platform project checks, plus measured patch checks, require
100% with zero tolerance. Carryforward is disabled and all three uploads are required.
Only line reports enter these checks. The strict YAML source remains branch 1.0.0,
where the reviewed release settings are enforced.

An explicit manual CI run on `1.0.0` may also exercise real Codecov ingestion.
This validation-only job requires all primary measurements and the completeness
gate, rechecks the exact report receipts, and uploads only those three region-aware
JSON reports using region-pilot-linux, region-pilot-windows, and region-pilot-macos
flags. Automatic file fixes are disabled. The pilot does not also upload LCOV for
that commit; flags alone would not isolate a combined commit score. Pushes and pull
requests cannot trigger this upload. It uses the existing GitHub secret without
changing permissions, branch protections, or the release uploader. Server-side
processed reports and per-platform totals must be checked separately; an uploader
exit code alone is not ingestion evidence. The governing YAML still comes from
1.0.0, while `override_branch: coverage-validation` prevents validation uploads
from changing the release-head score. No Git branch named `coverage-validation`
is required.

CI permissions remain contents: read; checkout credentials are not persisted.
Collectors do not execute artifact content. Missing reports, provenance mismatches,
test failures, and invalid diagnostics fail the evidence check rather than becoming
zero-denominator 100% results.

## Local validation

Run `node --test .github/scripts/coverage-audit.test.cjs .github/scripts/coverage-relocate.test.cjs` for parser, provenance,
integrity, source-inventory, and per-platform merge fixtures. Existing fs2-dev matrix
and coverage tests remain responsible for repository workflow and native gate policy.
No previous artifact is relabelled as a fresh measurement after a source change.

## Reruns and failed collections

Use **Re-run all jobs**, not **Re-run failed jobs** or an individual collector.
Artifact names include the producer run attempt. Collectors download only the exact
expected target names for that attempt and still verify the receipt's commit, tree,
run, and attempt. A partial rerun intentionally fails closed; earlier successful
artifacts are not silently reused. Start a new full run if earlier artifacts expired.

Collectors continue after artifact-download failures, inspect each expected target,
and preserve a rejected JSON/Markdown summary before exiting nonzero. Summary upload
runs even when collection fails, unless the run was cancelled. A rejected summary
contains no publishable coverage percentage. Filesystem or checkout failures that
prevent the collector itself from executing can still prevent summary creation.

## Missed-location review

The primary baseline also records individual zero-count LLVM JSON entries and
zero-count code regions per emitted instance, including their symbols, filenames,
coordinates, and multiplicities. These diagnostic inventories supplement numeric
ratchets; they do not change LLVM totals or the library line-coverage denominator.
A newly missed location cannot be compensated for by covering a different location.
Closing existing gaps is allowed.

The location inventory is derived from the same historical exact-SHA reports as
the numeric baseline; input JSON hashes and exporter versions are retained. It is
not a fresh measurement of subsequent changes. Source relocation, compiler symbol
churn, and export-schema changes require explicit review rather than fuzzy matching
or automatic baseline refresh. External/compiler filenames are retained as opaque
diagnostic identities and are never opened as filesystem paths.

The review-only relocation helper can propose whole-line moves of an unchanged,
uniquely identified named-function body. It requires the historical JSON digest
recorded in the primary policy, a sealed current native artifact set, unchanged
compiler/exporter identities, and source bytes from explicit immutable Git commits.
Changed bodies, ambiguous duplicate bodies, new misses, symbol churn, and external
source relocation are rejected. The ordinary CI gate remains strict and never loads
these proposals automatically; adopting a proposal still requires explicit review.

```text
node .github/scripts/coverage-relocate.cjs TARGET CANDIDATE_SHA BASELINE_JSON CANDIDATE_ARTIFACT_DIRECTORY
```

The artifact directory contains the normal `coverage-TARGET` subdirectory and its
receipt. The command prints a proposal with report/source digests, not a new baseline
or accepted CI verdict, and does not modify repository files.

Fixtures cover collection profiles, failed and partial collections,
tampering, compiler/exporter mismatches, swapped gaps, Windows junctions, and
pathname replacement. The primary native matrix runs these fixtures on each OS.
Passing fixtures is not a substitute for exact-SHA native measurements and actual
Codecov service validation.

## Nightly branch regression gate

The separate `coverage-branch-policy.json` records the reviewed source inventory,
per-target branch locations, denominators, baseline JSON digests, and exact nightly
compiler/exporter identities. Each native target independently requires 100% of its
measured outcomes. LLVM JSON physical-outcome unions, file/aggregate totals, and
LCOV branch records must agree. Zero denominators, changed inventories, or new
uncovered outcomes fail rather than being rounded or merged away.

Producers seal JSON, LCOV, text, provenance, and source manifests. The complete
branch collector requires all three targets from one SHA, tree, run, and attempt;
failed producers, tampering, and missing evidence fail closed. Download failures
still permit a rejected diagnostic summary, never a publishable partial result.
These branch artifacts are not uploaded to Codecov or mixed with stable coverage.

Primary, tooling, MSRV, and nightly native coverage use explicit OS generations:
Ubuntu 24.04, Windows Server 2025 with VS 2026, and macOS 26 ARM64. Image revisions
and Node versions are recorded because hosted images can still receive updates.
Ordinary compatibility lanes retain their existing runner policy.

Focused mutation validation also exercises the legacy/direct byte-counter guards
and short drive-root validator, and cancels superseded runs to prioritize current
evidence. Broad mutation testing remains retained. Its weekly schedule runs from
the default `1.0.0` branch; focused validation runs for relevant release-branch
pushes and manual dispatches. No Codecov publication rule or branch protection
change is implied here.

MSRV and nightly coverage run on pushes and pull requests targeting `dev` and
`1.0.0`, and on manual dispatches for those branches. Neither workflow
uses path filters, so documentation-only changes still receive coverage checks.
The genuine Rust 1.88.0 jobs retain the required `Coverage / <target> / Rust 1.88.0`
check names; primary Rust 1.98.1 coverage remains separately identified.

Tooling and supplemental coverage use the same push and pull-request branch scope
while retaining their manual and scheduled runs. Collectors use their producers'
event eligibility with `always()`, so an unexpectedly skipped producer still
reaches evidence auditing rather than silently skipping the collector. These
workflow changes do not modify branch protections or Codecov publication rules.
Fixture and local replay validation are not fresh native measurements of an
unpublished candidate.

## Separate Codecov library and tooling reporting

Library and developer-tooling coverage remain separate measurements. The existing
native runners produce both report sets; Codecov publication does not add another
coverage test matrix.

| Scope | Sources | Release flags | Candidate pilot flags |
| --- | --- | --- | --- |
| fs2 library | `src/**` | `primary-linux`, `primary-windows`, `primary-macos` | `region-pilot-linux`, `region-pilot-windows`, `region-pilot-macos` |
| fs2-dev tooling | `tools/fs2-dev/src/**` | `tooling-linux`, `tooling-windows`, `tooling-macos` | `tooling-pilot-linux`, `tooling-pilot-windows`, `tooling-pilot-macos` |

Both publication jobs require successful library and tooling evidence before the
first upload. They download the three exact-attempt native artifacts for each
scope and rerun the existing collectors against the current SHA, source tree,
run, attempt, report digests, source inventories, and coverage policies. Library
and tooling collection summaries are retained separately. An absent, stale,
modified, or incomplete report set prevents publication; no carryforward or
additional ignore rules are used.

The library keeps its existing upload format, including region-aware JSON in the
candidate pilot. Tooling uploads its package-scoped LCOV for physical-line
coverage. Codecov line totals are not LLVM region, function, instantiation,
source-definition, branch, MC/DC, or mutation-testing results. Those dimensions
remain governed by their existing independent native policies and artifacts.
The two scopes must not be combined into a library-only coverage claim.

The `fs2-library` and `fs2-dev` components intersect source paths with the
appropriate flags. Components supply views; flags own the status checks to avoid
duplicate gates. Release policy retains the library check names and adds tooling
project, per-platform project, and patch checks at 100%, with zero tolerance.
The six-upload notification threshold complements, but does not replace, the
collectors' exact platform and provenance checks.

### Policy validation

`strict_yaml_branch: "1.0.0"` remains intentional: only the protected release
branch controls production coverage policy. Validation uploads remain isolated and
must not be presented as production-policy enforcement.

Before changing policy, validate workflow and Codecov YAML and manually dispatch
CI on `1.0.0`. Confirm that all six expected validation uploads are merged for that
SHA and run attempt on the logical `coverage-validation` branch. Compare each tooling
flag with its native LCOV, compare each library flag using its existing format,
and reject nonzero data outside the corresponding source scope. Zero-total files
from another platform are not measured coverage. Establish any new required
status names only after they have been observed on the intended release/PR path.

Official references: [flags](https://docs.codecov.com/docs/flags),
[components](https://docs.codecov.com/docs/components), and
[YAML authority and notifications](https://docs.codecov.com/docs/codecovyml-reference).
