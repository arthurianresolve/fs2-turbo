# fs2-dev

Repository-only validation tooling for fs2-turbo. These tools are excluded from
the published crate.

## Native coverage reproducibility

Set `CARGO_INCREMENTAL=0` explicitly for every native coverage build, including
local runs. Use a fresh `CARGO_LLVM_COV_TARGET_DIR` for each compiler and profile
(combined, unit, integration), and set `FS2_COVERAGE_REQUIRE_NATIVE_FIXTURES=1`
when reproducing CI. Export JSON, LCOV, and text from the same profile before
starting another build. Record the source revision and local changes, target,
`rustc -vV`, `cargo llvm-cov --version`, and compilation environment with the
evidence.

Incremental compilation changes which unused-function coverage mappings rustc
emits. On Windows with Rust 1.98.1 at `c53c05e`, switching only
`CARGO_INCREMENTAL` from `0` to `1` changed combined instantiations from
`348/406` to `348/375`; the executed count stayed at 348. The smaller denominator
does not establish improved test coverage. CI comparisons use the non-incremental
profile.

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

### Pending Windows fixture experiments

The annotations do not implement or claim successful native experiments.
Once sufficient disk headroom is available, use this bounded investigation:

1. Capture fresh, isolated combined, unit, and integration profiles for the same
   source revision on Rust 1.88.0 and Rust 1.98.1, using the settings above.
2. For legacy fallback, select an explicitly approved local/provider fixture.
   Demonstrate that public filesystem-stat queries genuinely reach the legacy
   provider. Do not assume that SMB, FAT, or exFAT forces fallback, provision a
   network share automatically, or downgrade the operating system to force it.
3. For pending I/O, reuse the public sparse-allocation scenario on an approved
   fixture and inspect whether the completion closure, wait helper, and private
   overlapped state actually execute. The existing ordinary overlapped test is
   not evidence of pending completion unless those mappings execute.
4. Predeclare at most three attempts per fixture, at most 2 MiB of data per
   attempt, and a 120-second wall-clock deadline per attempt. Run under an
   owned-process timeout with reliable teardown; keep buffers and private event
   handles live until native completion. Preserve private directory permissions,
   IOCP isolation, error propagation, and all failed or inconclusive evidence.
5. Report observed mappings and API assertions, including file length, preserved
   contents, and failure behavior. A fixture that never exercises the intended
   route is inconclusive, not passing. Do not retry indefinitely for a higher
   percentage or promote an unstable fixture to required CI.

No production bypass flags, exposed private APIs, weakened allocation checks,
or removal of defensive error paths are justified solely by these experiments.

## MC/DC diagnostic

`mcdc-diagnostic` performs a fresh, isolated engineering diagnostic with the
Rust-MCDC baseline
`e57cec60d416d49dc9d5bdb9b23ea14f20d4a49e`. It does not participate in CI or
release gates and does not replace the native line, region, function, or
instantiation coverage workflow.

The command requires:

- a completely clean Rust-MCDC checkout at the exact baseline;
- a rustup-linked patched Rust 1.98.1 toolchain built from rustc commit
  `48a229ceaefd4985c50990b14116b6d856af0985` with LLVM 22.1.8;
- `cargo-llvm-cov 0.8.7` in `PATH`;
- new absolute work and report paths outside both source repositories; and
- at least 8 GiB free on the work volume unless a different explicit floor is
  supplied.

```text
cargo xtask mcdc-diagnostic \
  --rust-mcdc-root <absolute-rust-mcdc-checkout> \
  --toolchain <rustup-linked-patched-toolchain> \
  --work-dir <new-absolute-disposable-directory> \
  --report <new-absolute-report.json>
```

The runner removes ambient Rust instrumentation flags, uses a fresh isolated
Cargo target, probes the patched compiler's MC/DC flag, runs fs2-turbo tests
serially, and accepts only LLVM JSON export type
`llvm.coverage.json.export` version `3.1.0`. Version `3.0.1` and stale output are
rejected rather than translated.

The report retains exact source/tree, Rust-MCDC, rustc binary, compiler/LLVM,
tool, and export identities. It reconciles LLVM's file and function MC/DC
projections and reports decision, condition-pair, executed-vector, and
`not_evaluated` counts. LLVM's condition-pair flags remain transport-level
diagnostics: the command does not construct an independent unique-cause or
masking proof.

The pinned compiler slice covers root-expansion, non-async, non-generic
free-function `if` expressions with nested short-circuit Boolean leaves, up to
16 conditions. Negation and unsupported constructs are not silently counted as
covered. The semantic census remains incomplete, so the output cannot support a
100% MC/DC, qualification, certification, source/object-equivalence, or release
claim.
