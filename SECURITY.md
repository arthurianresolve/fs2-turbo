# Security Policy

## Supported Versions

| Version | Security support |
| --- | --- |
| 1.0.0 / `dev` | Active |
| Published 0.4.3 | Compatibility-preserving fixes considered case by case |
| Earlier versions | Unsupported |

## Reporting a Vulnerability

Use GitHub's private vulnerability-reporting feature when the repository's
Security page offers it. Include the affected version and platform, realistic
prerequisites, impact, and a minimal reproducer when safe.

If private reporting is unavailable, open a public issue containing only a
request for a private contact channel. Do not include vulnerability details,
secrets, exploit code, or sensitive environment information in that issue.

Maintainers will make a good-faith effort to acknowledge reports promptly,
assess severity, and coordinate remediation and disclosure. No fixed response
or remediation SLA is promised.

## System and Scope

This policy covers:

- the published `fs2` library;
- Unix and Windows native filesystem implementations;
- compatibility fixtures and tests;
- unpublished `fs2-dev` validation tooling;
- repository CI and release-validation workflows.

The library operates with filesystem authority already held by its caller. It
does not provide authentication, authorization, mandatory locking, path
confinement, or a network service.

## Repository Hosting and Release Controls

Tracked workflow files cannot enforce GitHub-hosted repository settings. Before
releasing an exact commit, maintainers must independently verify and retain
evidence that:

- release and development branches have appropriate rulesets or branch
  protection, including required review and status checks and restrictions on
  deletion and force pushes;
- default workflow-token permissions remain read-only and workflows cannot
  approve pull-request reviews;
- permitted third-party actions are restricted and full commit-SHA pinning is
  enforced where GitHub supports it;
- vulnerability alerts, dependency security updates, secret scanning, and push
  protection are configured as intended;
- private vulnerability-reporting availability matches the instructions above;
- self-hosted runners, repository and environment secrets, caches, environments,
  and artifact visibility and retention have been reviewed for the release.

These settings can change without a source commit. Their state must be bound to
the exact release SHA and verification time rather than inferred from this file
or from the workflow source alone.

## Threat Model and Trust Boundaries

Security-relevant boundaries include:

- caller-supplied files, paths, allocation lengths, and lock operations passed
  to native operating-system APIs;
- descriptors or handles crossing into a less-trusted child process;
- repository source reaching Cargo compilation and execution;
- temporary command-capture files reaching validation results;
- repository-controlled inputs reaching GitHub Actions runners.

Repository tooling executes with the invoking user's ambient filesystem,
credential, process, environment, and network authority. The current checkout
and host Git, Cargo, Rust compiler, and executable search paths are trusted
bootstrap inputs. `cargo xtask` compiles repository tooling before an
in-process check can run; it is not a sandbox for untrusted checkouts.

## Security Invariants

- Native output storage, handle ownership, asynchronous operations, integer
  conversions, and error handling must remain memory-safe and fail closed.
- Allocation must not unexpectedly truncate data when callers satisfy the
  documented exclusive logical-length ownership requirement.
- Except for the documented Apple compatibility boundary, every nonempty
  allocation request must either establish requested-range coverage or return
  `Unsupported`. A file-wide allocated-byte total is not proof of that coverage.
- `File::try_clone` must produce non-inheritable descriptors or handles.
- Legacy `FileExt::duplicate` inheritance must remain explicit and documented
  while compatibility behavior is retained.
- Advisory locks must not be represented as authorization or mandatory
  isolation.
- Filesystem statistics must reject arithmetic overflow and invalid native
  domains. Caller-available space must not exceed caller-visible total or actual
  free space. Modern physical free space must not exceed physical total; legacy
  physical free space may exceed a quota-limited caller-visible total.

- Command capture must resist lower-trust link, reparse, namespace, and
  captured-output replacement. Retain authoritative handles through consumption.
- CI permissions must remain minimal, checkout credentials unpersisted, and
  third-party actions commit-pinned.

## Reportable Findings and Severity Context

Reportable issues include:

- memory unsafety, invalid handle ownership, or unintended capability transfer;
- data corruption or truncation while documented API requirements are met;
- acceptance or projection of inconsistent filesystem-counter relationships
  that can misstate caller-available or physical capacity;

- path, symlink, junction, or reparse-point attacks crossing an intended
  filesystem boundary;

- workflow injection or credential exposure through repository-controlled
  inputs.

Severity must account for realistic reachability and prerequisites. Arbitrary
code execution or capability transfer may have high impact while receiving a
lower overall severity when exploitation also requires local workspace access,
an explicitly exploratory operation, or a later inheritance-capable spawn.

## Out of Scope, Exclusions, and Accepted Risk

- Non-cooperating concurrent logical-length changes during allocation are
  outside the current API contract.
- Advisory-lock bypass by processes that do not participate in the locking
  protocol is not a security defect.

- Unix process-group containment is lifecycle cleanup, not hostile-code
  isolation; session or process-group escape by child processes is known.
- Inheritable `FileExt::duplicate` behavior is a deprecated v0.4 compatibility
  risk. Reports should establish a new impact, bypass, or concrete affected
  consumer rather than only restating the documented behavior.
- Performance-only regressions are not security findings unless they create a
  realistic resource-exhaustion or availability attack.

## Known Limitations and Compensating Controls

- Consumers should use `File::try_clone` when inheritance is unwanted and close
  or clear legacy duplicates before spawning less-trusted children.

- Windows ancestry validation reflects the current security descriptor. Capture
  operations require lower-trust reparse-capable handles opened under an earlier,
  subsequently revoked grant to be closed first; a DACL review cannot revoke
  access already granted to an open handle. The current user, SYSTEM, and
  Administrators remain trusted authorities rather than same-user sandbox
  boundaries.

- On Windows, sparse allocation may materialize all holes through the current
  logical EOF before restoring the sparse attribute. Compressed files return
  Unsupported because Windows exposes no equivalent full-reservation proof.
- On macOS and iOS, `FileExt::allocate` retains the v0.4-compatible
  `F_PREALLOCATE` physical-EOF behavior. Apple's public interface provides
  file-level reservation rather than a portable proof for every previously
  sparse extent; callers requiring extent-by-extent assurance need a
  filesystem-specific verification protocol.
- GitHub repository and account settings, branch protection, rulesets, action
  policy, vulnerability alerts, private reporting, secrets, runner hardening,
  caches, environments, and artifact visibility and retention are outside
  repository-source verification. Release evidence must verify them separately.
- Solaris uses process-associated `fcntl` advisory records because it lacks a
  native handle-scoped `flock` primitive. Independent handles in one process do
  not contend with the same lifetime semantics as other supported platforms.

### Windows command-capture authority

Command-capture roots must be on local fixed volumes. UNC, mapped remote,
removable, optical, RAM-disk, unknown, and unavailable roots are rejected.
The tooling retains directory handles, validates ancestry ownership and DACLs,
and rejects reparse traversal or lower-trust namespace mutation. Directory-backed
DOS drive aliases retain their physical ancestry and bind the target by volume
serial and full file identity. The exact TrustedInstaller service SID is trusted
only at volume roots, not ordinary ancestors or private directories.

Capture directories are created randomly beneath the first accepted private
root selected from the user profile, local application data, or executable
parent. Private directories require current-user ownership and a protected DACL
limited to that user and SYSTEM. Output is consumed through retained handles,
not reopened by pathname; no-delete-share directory handles remain live during
capture. These controls protect capture integrity, not against trusted-user
code execution or an already-compromised host toolchain.
