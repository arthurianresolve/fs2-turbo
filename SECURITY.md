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
- unpublished `fs2-dev` tooling and benchmark harnesses;
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
- repository or selected benchmark source reaching Cargo compilation and
  execution;
- temporary and staged benchmark files reaching executable launch or final
  evidence publication;
- repository-controlled inputs reaching GitHub Actions runners.

Selected benchmark code executes with the invoking user's ambient filesystem,
credential, process, environment, and network authority. Trust acknowledgement
is not a sandbox.

The current checkout and the host Git, Cargo, and Rust toolchain are trusted
bootstrap inputs. `cargo xtask` compiles repository tooling before an in-process
check can run; `--trust-selected-code` acknowledges secondary benchmark subjects
and does not make an untrusted checkout or host toolchain safe.

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
- Mutable or selected source must not reach ambient-authority execution without
  the required trust acknowledgement.
- Strict benchmark executables and evidence must resist lower-trust filesystem
  modification, link or reparse traversal, and destination replacement.
- Diagnostic parsers must bound aggregate computational work. Independent file
  or record limits are insufficient when an algorithm multiplies input dimensions.
- Markdown derived from retained benchmark reports must preserve report status,
  overall decision, strict or exploratory mode, diagnostic-only state, and A/A
  disposition. Missing historical fields remain unknown rather than establishing
  strict evidence.
- Retained benchmark reports must contain only normalized, non-secret host and
  toolchain facts. Raw inherited environment values and full-environment
  digests remain process-private and are used only for in-run drift detection.
- CI permissions must remain minimal, checkout credentials unpersisted, and
  third-party actions commit-pinned.

## Reportable Findings and Severity Context

Reportable issues include:

- memory unsafety, invalid handle ownership, or unintended capability transfer;
- data corruption or truncation while documented API requirements are met;
- acceptance or projection of inconsistent filesystem-counter relationships
  that can misstate caller-available or physical capacity;
- bypass of selected-code trust acknowledgement;
- retained benchmark-executable or evidence substitution by a lower-trust
  local identity;
- path, symlink, junction, or reparse-point attacks crossing an intended
  filesystem boundary;
- overwrite or provenance failures that can make invalid evidence appear valid;
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
- Ambient-authority behavior of explicitly trusted selected code is expected.
  A bypass of the acknowledgement remains reportable.
- Unix process-group containment is lifecycle cleanup, not hostile-code
  isolation; session or process-group escape by trusted selected code is known.
- Inheritable `FileExt::duplicate` behavior is a deprecated v0.4 compatibility
  risk. Reports should establish a new impact, bypass, or concrete affected
  consumer rather than only restating the documented behavior.
- Performance-only regressions are not security findings unless they create a
  realistic resource-exhaustion or availability attack.

## Known Limitations and Compensating Controls

- Consumers should use `File::try_clone` when inheritance is unwanted and close
  or clear legacy duplicates before spawning less-trusted children.
- Benchmark subjects that are not fully trusted should run under a separate
  low-privilege account, container, or disposable virtual machine.
- Strict benchmark evidence assumes the current checkout and host Git, Cargo,
  Rust compiler, and executable search paths are trusted and protected from
  lower-trust modification. It records environment and version evidence but
  does not authenticate a compromised host toolchain.
- Historical benchmark measurements are performance evidence only. They do not
  prove security-control effectiveness or cover implementations changed after
  the measured commit.
- Windows ancestry validation reflects the current security descriptor. Strict
  runs require lower-trust reparse-capable handles opened under an earlier,
  subsequently revoked grant to be closed first; a DACL review cannot revoke
  access already granted to an open handle. The current user, SYSTEM, and
  Administrators remain trusted authorities rather than same-user sandbox
  boundaries.
- Windows benchmark paths whose ancestry grants reparse-capable rights to an
  application-capability SID or ordinary token group are rejected. Use a
  protected output root and fixture path rather than a permissive host temp path.
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

### Windows benchmark path authority

The benchmark tooling retains directory handles and rejects link or reparse
traversal around mutable workspaces, private staging, and evidence publication.
Strict evidence requires repository, workspace, fixture, staging, and publication
paths to reside on local fixed volumes. UNC, mapped remote, removable, optical,
RAM-disk, unknown, and unavailable roots are rejected before descendant access;
exploratory runs remain explicitly non-strict on those roots.
Selected repository ancestry is retained before and after canonicalization and
held through revision resolution and source materialization.
Directory-backed DOS drive aliases are resolved through retained handles. Strict
admission retains both lexical and physical ancestry and binds the alias target
by volume serial and full file identity before descendant use.
Output preflight retains the complete destination ancestry before collision or
free-space probes. Collision checks do not follow the final entry, and strict
headroom checks query only the deepest retained existing parent. Missing output
parents are not traversed or created by preflight; publication creates them under
the existing private-directory policy.
Private workspace and staging directories must be owned by the current user and
use a protected DACL limited to that user and SYSTEM. Publication validates its
ancestry, confines destinations beneath the trusted benchmark root, and rejects
intermediate ancestors that grant lower-trust destructive namespace control or
rights capable of converting an existing directory into a reparse point. Current
user ownership does not bypass this DACL review. The immediate publication parent
must use the same private DACL, and publication retains no-replace destination
semantics. Add-subdirectory-only rights remain compatible. Application-capability
SIDs and ordinary token groups are not trust proxies for persistent evidence
ancestry. Transient capture, fixture, and workspace ancestry applies the same DACL
review and retains no-delete-share handles while private final directories protect
their contents. The exact TrustedInstaller service SID is accepted only for a
direct volume root, including a fixture that is itself that root; it is not a
trusted owner for ordinary ancestors or private directories. On Windows,
command-capture directories are created randomly beneath the first accepted
private root selected from the user profile, local application data, or the
executable parent, hardened immediately, and consumed through retained handles
rather than being reopened by pathname; the no-delete-share directory handle
remains live.
The statistics runner accepts an explicit `--output-root` when the source
checkout cannot itself serve as that protected publication root. A missing root
is created with the platform's private-directory policy; an existing root must
already satisfy it. The requested output must remain beneath the selected root,
and every component naming that root is retained for staging and publication.
The same ancestry, DACL, and no-replace checks continue to apply. Strict
paired-statistics runs also require an existing directory fixture whose complete
ancestry can be validated and retained for the measurement campaign.
Paired-lock runs create their measured file inside the retained private workspace,
outside the frozen source tree. Every A/B and A/A child receives that explicit
native directory path and uses it without falling back to ambient temporary
storage. Both environment snapshots describe this measured fixture, not the
separate publication volume. Direct lock-harness invocations must also supply
the fixture directory before the measurement arguments; only the parent runner
can establish strict admission and evidence.
Cross-crate Criterion runs use one private statistics fixture retained for the
campaign and bind it explicitly for every priming and measurement child;
inherited `FS2_BENCH_STATS_PATH` cannot select an unvalidated fixture. Use
`bench stats --fixture` for an explicitly selected fixture. Windows statistics
require `bench stats` for strict evidence; cross-crate and ref-to-ref statistics
remain exploratory on Windows.

These controls protect benchmark staging and publication namespaces. They do
not sandbox selected code or reduce its ambient authority.

### Unix benchmark path authority

The benchmark tooling treats mutable workspaces and evidence publication as
security boundaries. Unix ancestry is retained by descriptor and rejected when
ownership or mode permits lower-trust namespace replacement. Protected symlinks
are resolved only after their namespace edge is secured, and the target ancestry
is validated independently. Evidence destinations remain beneath the explicitly
trusted benchmark root. Sticky shared directories may be ancestors, but the final
mutable workspace, staging directory, and publication parent must be private.

Strict Linux paths are limited to recognized direct local filesystems; unknown,
network, userspace, and layered filesystems fail closed because reported mode
bits may not prove enforcement. Linux 9p/WSL DrvFs is therefore rejected. On
macOS, only recognized local filesystems without extended ACL entries are
accepted. Other Unix platforms fail closed until an equivalent authority check
is implemented. These are evidence constraints, not claims that every rejected
path is exposed. Use native Linux storage inside WSL or run the tooling natively
on Windows.
