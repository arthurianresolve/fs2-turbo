# Fix Report

- Scan: `c194ead6-9d31-4999-b098-473e42c75e73`
- Source finding export: `target/codex-security-remediation/scan-1de1cab/findings.json`
- Scan revision under correction: `1de1cabca99f914c08124260c08dedc6c3470a02`
- Scope: single bounded corrective implementation pass; no commit, push, or new security scan.

## Dispositions

| Finding ID | Severity | Disposition | Notes |
| --- | --- | --- | --- |
| `csf_b3bdc471d723dc241924cd10` | Medium | Remediated in current working patch; revalidated in this pass | Windows allocation now preserves an existing sparse-file tail when requested allocation is below EOF. |
| `csf_4c8858fca0f70d8e32dc06de` | High | Corrected in this pass | Windows blocking lock operations now use event-backed `OVERLAPPED` storage, wait for terminal completion on `ERROR_IO_PENDING`, and keep the manual-reset event plus `OVERLAPPED` alive through completion. Nonblocking try-lock behavior is preserved. |
| `csf_5d2b0f098324b279a0e2cf9d` | Low | Remediated in current working patch; not specifically changed in this correction pass | Apple `F_PREALLOCATE` mutable storage handling is carried forward from the in-progress remediation patch. No macOS/iOS runtime proof was produced in this Windows pass. |
| `csf_e38f3d276aba1aefcf2249a9` | Medium | Accepted residual compatibility risk | `FileExt::duplicate` retains v0.4 inheritable descriptor/handle semantics for compatibility and carries a deprecation/migration signal to `File::try_clone`. This is not claimed fixed by the compatibility mitigation. |
| `csf_130ce8c91e799d8e9c46c42f` | Low | Documented boundary; not adversarial sandbox closure | Unix process cleanup remains same-process-group lifecycle containment for `fs2-dev`, with the requested note that `setsid()` or other process-group changes can escape. No closure is claimed against hostile descendants. |
| `csf_ce059bc341ae0565301cc8ca` | Low | Accepted residual compatibility risk | Same disposition as `csf_e38f3d276aba1aefcf2249a9`: runtime inheritable duplicate semantics are retained; callers needing non-inheritable handles should use `File::try_clone`. |
| `csf_47d6a203bae845c6c7b6821c` | Medium | Corrected in this pass | Same Windows `LockFileEx`/`OVERLAPPED` remediation as `csf_4c8858fca0f70d8e32dc06de`, validated by the focused overlapped lock tests. |
| `csf_15b3b1454098788a9ed4ee11` | Medium | Remediated in current working patch; not specifically changed in this correction pass | Duplicate Apple `F_PREALLOCATE` finding carried forward from the in-progress remediation patch. No macOS/iOS runtime proof was produced in this Windows pass. |

## Verification

| Command | Result |
| --- | --- |
| `cargo fmt` | Passed; emitted the known sandbox warning `could not canonicalize path <host-path>`. |
| `cargo test -p fs2 windows::tests::lock` | Passed but matched 0 tests; this was the wrong module filter. |
| `cargo test -p fs2 windows::test::lock` | Passed 11 tests, including overlapped blocking lock and try-lock contention coverage. |
| `cargo test -p fs2 windows::test::allocation` | Passed 1 test: sparse EOF tail preservation. |
| `Get-PSDrive -Name C \| Select-Object Name,Free,Used` | Reported `Free = 25294708736` bytes, so workspace check was permitted. |

## Residual Risk

- The duplicate-handle findings remain accepted compatibility risk, not remediated vulnerabilities.
- Unix process containment is a lifecycle cleanup boundary for same-process-group descendants, not an adversarial sandbox and not a defense against `setsid` escapes.
- No new Codex Security scan was run in this pass.
