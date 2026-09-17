# fs2-dev

Repository-only validation tooling for fs2-turbo. These tools are excluded from
the published crate.

## Coverage workflows

- `ci.yml`: Rust 1.98.1 on Linux x86_64, Windows x86_64, and macOS arm64.
- `coverage-msrv-validation.yml`: Rust 1.88.0 on the same native targets.
- `coverage-mutation-validation.yml`: focused cross-platform mutations on both
  compilers, with baseline and outcome validation.
- `mutation-testing.yml`: scheduled or manually dispatched broader mutation tests.

Native jobs retain isolated combined, unit, and integration profiles, existing
coverage gates, and Codecov publication policy. They record exact source/tree,
native target, compiler/LLVM, tool, run, and environment identities and verify
SHA-256 manifests of tracked source. Reports and available failure diagnostics are
retained for 14 days; failed jobs are not eligible Codecov uploads. Experimental
nightly branch coverage and MC/DC runners are not part of this tooling.

## Native coverage reproducibility

Set `CARGO_INCREMENTAL=0` explicitly for every native coverage build, including
local runs. Use a fresh `CARGO_LLVM_COV_TARGET_DIR` for each compiler and profile
(combined, unit, integration), and set `FS2_COVERAGE_REQUIRE_NATIVE_FIXTURES=1`
when reproducing CI. Export JSON, LCOV, and text from the same profile before
starting another build. Record the source revision and local changes, target,
`rustc -vV`, `cargo llvm-cov --version`, and compilation environment with the
evidence.

Incremental compilation can reduce the denominator without executing additional
code; a smaller denominator does not establish improved test coverage. Stable
coverage does not measure branch coverage or MC/DC.

Raw zero-count entries can include library mappings whose concrete downstream
instances execute. Keep these entries in raw totals and inspect their source
locations and symbols alongside separate unit and integration profiles. Private
error paths still require meaningful tests; matching a covered source location
alone does not prove that every instance has been exercised.

## Source-location execution diagnostics

Coverage diagnostics schema version 4 adds `source_location_execution_union`
to each profile. Existing source-definition groups, raw JSON entries, LLVM
totals, ownership records, and coverage gates remain unchanged.

The supplemental metric groups workspace definitions by normalized source
filename, start line, and start column. A location counts as executed when any
member topology has an executed entry. It is informational only: matching a
location neither proves every instantiation executed nor establishes semantic
equivalence between different region mappings.

Each location retains zero-based `definition_indexes` into the same profile's
original `definitions` array, plus `uncovered_definition_indexes`. Multi-topology
locations and uncovered groups with an executed location peer are reported
separately. Unit ownership is checked against each exact original topology, not
inferred from the supplemental union. `null` ownership means there is no
uncovered topology or no unit profile for comparison; it is not a passing check.

Integration gap annotations record their review baseline,
`e59918183b2789ba6986fbfe9df5c607150cb9a8`. An annotation requires the reviewed
target, filename, line, column, matching owner-symbol fragments for every member,
and unit ownership of every uncovered topology. Unmatched gaps stay
unclassified. These are historical review hints, not current reachability
proofs, waivers, or exclusions. Re-review annotations after changes to function
bodies, backend policy, source coordinates, or symbol formats.

| Review category | Meaning and next action |
| --- | --- |
| `legacy-provider-candidate` | Windows native fallback routing may be exercised by a suitable controlled provider fixture. Existing crate-internal tests already call the legacy native queries. |
| `pending-io-candidate` | Windows completion handling needs an operation that actually returns `ERROR_IO_PENDING`; an overlapped handle alone is insufficient. |
| `backend-inapplicable` | At the reviewed Linux revision, nonempty allocation reserves the range and extends length before separate finalization. Keep unit coverage and the backend contract intact. |
| `defensive-boundary` | Invalid native data, overflow, or an already-rejected input reaches this path. Prefer deterministic boundary tests rather than dangerous native inputs. |

Do not replace the intended integration inventory with a blanket 100% union
gate. Removing platform-inapplicable mappings reduces a denominator; grouping
mapping variants changes presentation. Neither establishes new test execution.

### Native fixture follow-up

Prior bounded NTFS experiments did not reach the legacy-provider or pending-I/O
candidates and did not close those gaps. Keep local evidence separate from new
measurements; passing API assertions alone does not close a mapping gap.

Use fresh isolated profiles on both compilers for a newly approved fixture, at
most three attempts per fixture/compiler, at most 2 MiB per attempt, and a
120-second owned-process deadline with reliable teardown. Report observed
mappings, file length, preserved contents, and error behavior. Retain failed and
inconclusive evidence rather than retrying indefinitely for a higher percentage.

Do not automatically provision network shares, downgrade the OS, assume a
filesystem forces fallback, or promote unstable fixtures to required CI. Preserve
private-directory permissions, IOCP isolation, native buffer/event lifetimes,
allocation checks, and error propagation. No production bypasses, exposed private
APIs, or removal of defensive paths are justified by a coverage target.
