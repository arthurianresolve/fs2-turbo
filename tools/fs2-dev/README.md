# fs2-dev

Repository-only validation tooling for fs2-turbo. These tools are excluded from
the published crate.

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
