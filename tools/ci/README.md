# Windows benchmark pilot

This pilot runs after pushes to dev or the repository's current default branch,
or after a trusted manual dispatch on either branch. It evaluates whether a
standard GitHub-hosted Windows VM produces usable paired measurements. It is not the full comparison
campaign, release clearance, or a replacement for desktop benchmark evidence.

## Frozen scope

- Runner: windows-2022, x64, four guest vCPUs, serialized per branch. The two
  branches use independent hosted VMs, not shared local build or evidence roots.
- Baseline: upstream v0.4.3, 9a340454a8292df025de368fc4b310bb736f382f.
- Candidate: the exact event SHA on dev or the current default branch, descended
  from the baseline and still reachable from that remote branch. There is no
  arbitrary candidate SHA or repository URL input.
- Harness: the protected dev checkout. On a dev push this is the event SHA; on
  the default branch it is resolved at checkout and recorded independently of
  the candidate. The default branch does not need development tooling or a
  library merge merely to be benchmarked.
- Primary compiler: Rust 1.98.1; preparatory library tests use exact Rust 1.88.0.
- Order: duplicate-single-refs, file-create-delete-refs, lock-refs.
- Every profile includes paired A/B and upstream A/A. There is no candidate B/B
  or full reverse-direction campaign in this pilot.
- No changed timed bodies, timing overrides, batching substitutions, skipped
  controls, outlier filtering, replacement windows, or retries.
- The isolated file-create-delete profile uses paired_common.rs unchanged and
  the original measurement-policy.json; only its metric list is narrowed.

Duplicate retains its 16 replicates, 50 samples, 20-second measurement policy
and 1% A/A margin. The other two profiles retain the common policy with eight
replicates, 50 samples and five-second measurements. All preserve their existing
warm-up, cooldown, 2% non-inferiority margin and outlier rules. Each profile also
requires the existing 60-second settle and 30-interval host observation, with
5% mean and 20% peak guest sibling-load limits. Bounds are 70 minutes per
profile, 270 minutes for the worker, and 330 minutes for the GitHub job.

## Security and execution

Pushes automatically execute trusted protected-branch code. Manual dispatch
requires an explicit trust acknowledgement. A job-level guard restricts actual
runner allocation to dev and the repository's current default branch. The broad
push event subscription follows default-branch renames without granting other
branches a runner; other branches can have skipped workflow entries only. The
workflow has read-only repository permissions,
SHA-pinned actions, no persisted checkout credential and no automatic PR writes.
It refuses self-hosted runners and does not register this computer with Actions.

The administrator provisions a fresh local standard account and a private
directory on the fixed NTFS system volume, not the permissive checkout volume.
The system volume and private root ACLs are recorded without changing either
volume's permissions. A non-admin worker installs isolated Rust
toolchains, prefetches dependencies, runs preparatory checks, builds fs2-dev and
invokes its existing strict Rust benchmark machinery. PowerShell only provisions
the Windows account, records VM context and manages process launch and receipts.
The benchmark subject still executes unsandboxed as the temporary standard user.

The worker removes inherited Actions credentials from its environment. Its
private Cargo home does not use shared Actions caches. Dependency downloads
finish before measurements; benchmark builds are locked and offline. Existing
filesystem ancestry, DACL, reparse, process containment and evidence checks are
not bypassed. The account is removed after the owned worker exits; evidence is
retained until artifact retention or ephemeral VM teardown.

CPU placement is derived from the guest's native core topology before the
pilot: select the highest-numbered guest core, place the measured process on
its highest logical CPU, and keep the launcher off the entire sibling set.
No core shopping based on observed load is permitted. This does not establish
physical host isolation. The CI mode asserts a noninteractive, non-admin worker,
not a locked interactive desktop. No services, antivirus, security controls,
power plans or process priorities are changed, and tracing is not enabled.

## Publication and interpretation

Before dispatch, validate the new launcher and profile, then obtain approval to
publish the workflow. GitHub requires a workflow_dispatch workflow to exist on
the repository's default branch before it can be dispatched. Do not change the
default branch or silently add the workflow there to work around registration.
Both workflows must be registered on the default branch. Workflow entrypoints
are literal repository script paths; the pilot uses checkout's recorded commit
output rather than inline command substitution. Pushes then select
that branch's exact event SHA automatically; manual dispatch is restricted to
dev or the current default branch.

The artifact includes the plan, VM/image/volume context, tool and source hashes,
native stdout/stderr/exit receipts, and the Rust reports and raw observations.
It deliberately excludes the Cargo home, toolchains and user profile. Upload is
attempted even on failure; forced job cancellation or VM loss can prevent upload.
Fourteen-day artifact retention is not a permanent evidence archive.

Admission/setup failure stops the pilot. Completed statistical failures remain
visible while subsequent predeclared profiles run; no failed profile is retried.
A nonzero profile exit leaves the workflow failing. Read each canonical report
to distinguish execution failure, invalid A/A, excessive outliers and a valid
A/B non-regression failure.

Passing all three profiles justifies considering a separate full hosted campaign.
It does not establish that other workloads are stable, that all future runners
will be equivalent, or that the candidate is faster on physical Windows hosts.
Do not merge CI and local timings into one dataset or update PR #1 from this
pilot. The selected workflow image label can receive provider updates, so retain
the actual image version rather than treating the label as an immutable image.

## Branch-specific README results

The accepted-results publisher runs only after a successful eligible pilot and
checks that its candidate is still the corresponding branch head before rendering.
It never executes downloaded artifacts. The protected dev checkout supplies the
Rust renderer; that checkout is independent of the recorded measurement harness.
The renderer requires all three complete strict A/B and A/A reports, unchanged
acceptance margins, matching run/source identity and successful process outcomes.

GitHub Pages uses workflow artifact deployment, not a results commit or branch.
The dev README embeds dev.svg; the default-branch README embeds default.svg.
Each displays the last accepted measurement, its exact SHA, date and runner image.
A failed, inconclusive, diagnostic, superseded or incomplete attempt never
replaces a README display. Such attempts remain visible in Actions and retained
runner artifacts, not in either README. Until first acceptance, the display says
that no accepted measurements have been published.

The two accepted datasets are retained separately in accepted.json. Publications
are serialized, reject older run identities, restore the other branch's data,
and regenerate escaped static SVG/HTML plus downloadable canonical JSON. A 404
can initialize empty state only when no prior Pages deployment exists; other
restore failures leave the existing site unchanged. The measurement job has no
Pages write permission. The workflow policy permits actions:read only in this
publisher's read-only render job, and permits the pinned Pages actions only in
this named, success-gated workflow. The deployment job has no shell steps or
checkout. Only the isolated deployment job receives pages:write
and id-token:write; repository contents remain read-only throughout.

Enable GitHub Pages with the Actions build type before publishing. README images
may be cached; the linked accepted report carries the measurement SHA and time.
Do not describe the last accepted result as a current-head CI or release status.
If the default branch later merges dev's README, retain the default.svg/default.html
links there rather than replacing them with the dev links.
