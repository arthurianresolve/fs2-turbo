# Coverage evidence policy

The public headline is measured fs2-turbo library line coverage on the three primary
native targets with Rust 1.98.1. It is not a claim of complete repository, branch,
MC/DC, or raw combined compiler-instantiation coverage. The unit-only profile has a
separate strict instantiation gate.

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
- Supplemental jobs run for dev-coverage pushes and CI manual/monthly canary runs.
  GitHub schedules run from the default branch; no dev-coverage schedule is implied
  before these changes are approved and promoted there.
- The region-aware Codecov JSON export is retained and tested by a manual-only
  dev-coverage ingestion pilot. Fresh native reports from the pilot commit are
  uploaded with region-pilot-linux, region-pilot-windows, and region-pilot-macos
  flags, with automatic file fixes disabled to preserve the exported records.
  This commit receives JSON only, not LCOV; the earlier LCOV baseline remains on
  its original commit. Flags alone would not isolate the combined commit score.
  Codecov processing and per-platform totals must be checked after upload.
  Its partial-line presentation is not a replacement for LLVM region totals,
  branch or MC/DC measurement, or compiler-instantiation coverage.
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
so the staged settings do not take effect there until explicitly approved and merged.

An explicit manual CI run on dev-coverage may also exercise real Codecov ingestion.
This validation-only job requires all primary measurements and the completeness
gate, rechecks the exact report receipts, and uploads only those three LCOV reports
using validation-linux, validation-windows, and validation-macos flags. Pushes and
pull requests cannot trigger this upload. It uses the existing GitHub secret without
changing permissions, branch protections, or the release uploader. Server-side
processed reports must be checked separately; an uploader exit code alone is not
ingestion evidence. The governing YAML still comes from 1.0.0, so this test does not
activate the candidate's release-only status rules.

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

Fixtures cover all four collection profiles, failed and partial collections,
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
evidence. Broad mutation testing remains retained. Its weekly schedule is not active
for `dev-coverage` while the workflow is absent from the default branch; use the
existing push/manual triggers until a separately approved default-branch promotion.
No default-branch scheduler, Codecov publication rule, or branch protection changes
are implied here.

MSRV and nightly coverage run on pushes and pull requests targeting `dev-coverage`,
`dev`, and `1.0.0`, and on manual dispatches for those branches. Neither workflow
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
