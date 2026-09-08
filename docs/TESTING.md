# Testing

This page owns proof strategy, test placement, deterministic inputs, hostile-input coverage, and
crash recovery expectations. [Build and test](BUILD.md) owns runnable commands; direct contracts own
exact behavior and evidence limits.

## Test through the owning interface

Place each test at the lowest layer whose interface states the behavior. Moving a test outward must
not require widening a production seam. A higher layer should add a composition or external-contract
assertion rather than repeat an implementation matrix.

| Location                              | Owns                                                                                             |
| ------------------------------------- | ------------------------------------------------------------------------------------------------ |
| Implementation-local `#[cfg(test)]`   | Pure parsing, classification, SQL and filesystem invariants, and private adapter or fault seams. |
| Root package `tests/application_*.rs` | Exported `Application` behavior with the product compiled without `cfg(test)`.                   |
| Root package `tests/e2e_*.rs`         | Production binaries, machine output, exit classes, restart, and operator workflows.              |
| `crates/<crate>/tests/`               | Exported interfaces of independently meaningful workspace packages.                              |
| `fuzz/`                               | Hostile bytes entering only through production interfaces.                                       |
| Ignored simulation targets            | Seeded lifecycle schedules, repeated recovery, and invariant checks.                             |
| Explicit live-kind scripts and tests  | Disposable-cluster behavior and real process termination.                                        |

The root is both workspace root and product package, so `application_` and `e2e_` prefixes
distinguish package integration from binary end-to-end tests. A test-support crate or public
provider seam requires multiple real consumers; one production Kubernetes adapter does not justify
either.

Assert pure implementation rules exhaustively once at their owner. At higher layers, assert
authority separation, durable outcomes, composition, observable output, and non-disclosure. Prefer
table-driven cases with shared setup, and use separate precise assertions when distinct contract
facts matter.

## Core effect-gateway proof matrix

| Layer                | Required proof                                                                                          |
| -------------------- | ------------------------------------------------------------------------------------------------------- |
| Request validation   | Bounds for identity, namespace, Deployment, container, digest, and authorization.                       |
| Authorization        | Signed grant, configured trust, exact tuple, and rejection before persistence.                          |
| Journal transition   | Deterministic fault injection at every durable state.                                                   |
| Target disposition   | Permanent invalid targets are pre-attempt `NOT_ATTEMPTED`; transient reads defer fairly.                |
| Provider attempt     | Safe GET precedes atomic target identity and `apply_started`; mutation follows that commit.             |
| Recovery             | Every injected window and process kill reopens without a blind second mutation.                         |
| Receiver observation | Request acceptance, timeout, transport completion, and rollout result remain distinct.                  |
| Classification       | Timeout and unresolved evidence are `UNKNOWN`, never false success or failure.                          |
| Receipt/inspection   | Canonical vectors carry all classifier inputs; inspection recomputes under explicit trust and limits.   |
| Publication          | Bytes, path, digest, and key ID freeze before collision-safe publication; kill recovery preserves them. |
| Migration            | Legacy self-asserted authorization fails closed rather than becoming trusted provenance.                |
| Hostile input        | Malformed, oversized, duplicate, reordered, unknown, and trailing records fail closed.                  |
| Disclosure           | Secrets and unbounded provider bodies stay out of SQLite, receipts, reports, errors, and logs.          |

`INSPECTED` means authenticated bytes and classifier consistency under supplied trust. It is not
receiver truth, causation, complete capture, compliance, or `VERIFIED`.

## Determinism and crash proof

Default semantic tests do not depend on wall-clock time, random keys, live services, ambient trust,
locale, or filesystem order. Use fixed keys, explicit evaluation time, private temporary
directories, seeded inputs, and sorted output. A subprocess test may use a bounded monotonic
coordination deadline; result meaning must not depend on polling order or timing.

Fault tests, simulations, process recovery, and compile-time demonstration controls cross the same
private operation-selected provider and publication implementations used by `Application`.
Queue-oriented helpers may select one identity but own no lifecycle transition. Process-kill proof
must cross both the ambiguous mutation seam and the receipt-publication seam, establish one provider
attempt, and prove prepared receipt bytes are neither re-signed nor relocated.

A live `kind` lane is explicit, environment-owning evidence. It complements but never replaces
fault-injection around every journal window.

## Evidence classes

### Deterministic suite

The default suite contains implementation-local tests, package integration tests, binary tests
needing no external service, and documentation tests. It owns repeatable semantic and hostile-input
proof. Source coverage is an informational review aid only; no percentage establishes crash safety,
Kubernetes semantics, release integrity, or production readiness.

The MCP subprocess lane proves bounded newline-delimited framing, one five-field tool, operator
configuration outside caller input, typed `SUCCEEDED`, `FAILED`, `UNKNOWN`, and `NOT_ATTEMPTED`
vocabulary, restart, protocol-only standard output, bounded hostile input, and secret-free failures.
Cancellation, EOF, or transport completion never determines receiver outcome.

The v0.1.1 fixture lane covers every historical lifecycle state, migration and restore interruption,
repeated reopen, provider-call counts, and frozen receipt bytes without Kubernetes or network
access. The [upgrade contract](UPGRADE.md) owns compatibility meaning.

### Robustness

Fuzzing calls production hostile-input interfaces from canonical corpus vectors without network or
ambient authority. Failures retain a minimized artifact and exact replay information.

Long simulations generate bounded lifecycle schedules, crash windows, retry deferrals, and reopen
operations from an explicit seed. Every step checks durable state, provider-call count, terminal
state, and frozen-receipt invariants. The seed is always replayable; wall-clock duration may change
only how many cases run, not their semantics.

### Live Kubernetes and demonstration

The live-kind gate owns real Kubernetes success, defined failed rollout, bounded `UNKNOWN`, and
process loss against a uniquely owned disposable cluster. It must show no blind second patch and
must clean up or export bounded failure evidence. Its recovery-policy case uses an instrumented
mutating webhook to separate PATCH requests and admission effects from persisted Deployment and
controller effects when an identical stale patch is replayed.

The exact-snapshot case separately acquires the operator target through the production adapter. It
proves one matching PATCH, preflight stale rejection for both version drift and same-name recreation
without a PATCH, and a changed receiver version between preflight and the gateway PATCH. That final
conflict remains `apply_started`, not a pre-attempt conclusion; deterministic fault tests own the
exhaustive restart and receipt-projection matrix around it.

The public demonstration adds an observable evaluator path through healthy,
`ProgressDeadlineExceeded`, mutation-loss, and receipt-publication-loss cases. Compile-time harness
controls remain outside caller input and the ordinary executable. A visual demonstration is finite
evidence, not exhaustive recovery proof.

#### Frozen JSON PATCH receiver comparison

JSON Patch can reject stale values before mutating admission. It does not make replay safe. This
comparison retains the production strategic adapter and observation-only recovery. Both arms use the
same unreleased sequential dispatch and format-4 receipt baseline. Experimental requests bypass
fresh dispatch permission only inside the test module. No production replay path is added.

The JSON document freezes the original independently read UID, opaque resourceVersion, and container
index. It tests UID, version, and the name at that index before replacing the image. Both strategies
set the same operation annotation and preserve unrelated annotations and the untargeted container.
No later observation refreshes either document. The JSON pointer escapes `/` as `~1`, and an absent
annotations map requires adding the parent map rather than just its member.

The boundary follows the pinned receiver source:

- [RFC 6902 sections 4.6 and 5](https://www.rfc-editor.org/rfc/rfc6902#section-4.6) define ordered
  tests and patch failure.
- The
  [v1.33.12 PATCH handler](https://github.com/kubernetes/kubernetes/blob/v1.33.12/staging/src/k8s.io/apiserver/pkg/endpoints/handlers/patch.go#L388-L425)
  returns `422` when JSON Patch application fails. Its transformer sequence applies the patch before
  mutating admission. Strategic metadata preconditions are checked later by
  [registry update](https://github.com/kubernetes/kubernetes/blob/v1.33.12/staging/src/k8s.io/apiserver/pkg/registry/generic/registry/store.go#L649-L733).
- [GuaranteedUpdate](https://github.com/kubernetes/kubernetes/blob/v1.33.12/staging/src/k8s.io/apiserver/pkg/storage/etcd3/store.go#L436-L520)
  may start with a cached object and re-evaluate the transformers. A cached original can pass JSON
  tests even after a writer has persisted. Earlier tests therefore do not guarantee zero admission
  for every request sent after a writer. One strategic request can invoke admission repeatedly.

`kind_tests::patch_experiment` compares the production strategic document builder with a test-only
JSON builder. The HTTP client disables automatic server-response retries. Dispatch counters are
cross-checked against independent API-server `RequestReceived` audit events, identified by a fixed
experimental user-agent. The webhook records AdmissionReview UIDs in a ledger and flushes one
out-of-band log effect for each invocation. Tests cross-check that ledger against pod logs.

GETs establish desired spec, annotation, UID, version, and generation changes separately. ReplicaSet
counts and controller-observed generation and replicas provide bounded controller evidence. HTTP
statuses are reported separately as `not-classified`, never promoted into rollout results. The
existing gateway cases retain responsibility for recovery, classification, and receipt evidence.

The overlap case holds both requests in admission after their log effects. Both futures remain
pending while a GET proves the original version and spec remain unchanged. The test releases the
first request, awaits its response, then releases the second. The unpersisted case allows admission
but returns a webhook mutation setting `replicas: -1`. Built-in validation rejects the candidate.
After the response and an unchanged-version/spec check, the fixture allows an exact replay. This
exercises failure after mutating admission, not an injected etcd outage. Barriers establish
ordering. Polling only retrieves facts. Limits are 20 seconds per barrier, 32 configured cases, 16
invocations per case, and 240 seconds for the matrix.

A complete run on arm64 macOS with Docker 29.4.0, kind 0.32.0, kubectl 1.33.9, and the pinned
Kubernetes v1.33.12 node image measured these counts. Each admission produced one log effect. Counts
are observations of this run, not universal admission cardinalities. A second complete run with
stronger rollout-readiness and full-spec/annotation-preservation assertions measured one rather than
two strategic admissions in the preflight-writer row. Other counts below matched. The server
reported gitCommit `1f348c8e82cf0f170df4ac2b1e859ea0d398ff09`.

| Scenario or checkpoint                              | PATCHes per strategy | Strategic admissions / logs | JSON admissions / logs | Experimental persisted updates / new ReplicaSets |
| --------------------------------------------------- | -------------------- | --------------------------- | ---------------------- | ------------------------------------------------ |
| Persisted response discarded, then exact replay     | 2                    | 2 / 2                       | 1 / 1                  | 1 / 1                                            |
| Writer between preflight and PATCH                  | 1                    | 2 / 2                       | 0 / 0                  | 0 / 0                                            |
| Same-name recreation before PATCH                   | 1                    | 1 / 1                       | 0 / 0                  | 0 / 0                                            |
| Container reordering before PATCH                   | 1                    | 1 / 1                       | 0 / 0                  | 0 / 0                                            |
| Both overlapping requests held in admission         | 2                    | 2 / 2                       | 2 / 2                  | 0 / not sampled at barrier                       |
| Overlapping requests after ordered release          | 2                    | 3 / 3                       | 2 / 2                  | 1 / 1                                            |
| Admitted but invalid first candidate, then replay   | 2                    | 2 / 2                       | 2 / 2                  | 1 / 1                                            |
| Before-send fault and journal reopen, before replay | 0                    | 0 / 0                       | 0 / 0                  | 0 / not sampled before replay                    |
| Counterfactual replay after the unsent fault        | 1                    | 1 / 1                       | 1 / 1                  | 1 / 1                                            |

Audit confirmed 20 experimental PATCHes across 14 cases. Stale strategic requests returned `409`,
while stale JSON requests returned `422`. Both invalid first candidates returned `422` without
changing the original version. Exact replay then returned `200` with a second admission effect. Both
overlapping JSON requests also reached admission before either persisted. These are live
counterexamples to treating JSON tests as receiver-wide deduplication.

Successful image changes advanced generation and ReplicaSet count from 1 to 2, with controller
observed generation 2 and one available and updated replica. The annotation writer created no
ReplicaSet. The reorder writer created its own ReplicaSet, not attributed to the rejected PATCH.
Recreation changed UID. The matrix records before/after controller counts, not an exhaustive history
of controller actions or workload correctness.

Fault placement matters. The before-send case injects an error at `ApplyStartedCommitted`, closes
and reopens the real journal, and proves attempted state with no caller result and zero applies.
This is not SIGKILL or power loss. Its subsequent raw replay is counterfactual, not gateway
recovery. The response-discard case receives a successful response in the harness before discarding
it for comparison purposes. It is not TCP loss. Existing process and loopback transport tests remain
separate evidence, not an executed JSON adapter integration.

Recommendation: retain strategic merge and no-replay. JSON tests reduced stale admission exposure in
these traces but did not eliminate pending or unpersisted ambiguity. Replacement adds original index
binding, annotation-parent handling, and write-strategy compatibility work without removing recovery
machinery. Earlier rejection may still justify future adoption on its own merits. This experiment
adopts nothing and adds no production dependency. Arbitrary webhook behavior, proxies, other
Kubernetes versions, actual storage failure, and power-loss durability remain unproved.

The measured baseline is `59b4f04f513f1dfd4131f722b6c44b210d73b453`, with the test-only changes in
this section's owning files. Obtain a checkout containing those changes and run:

```sh
cargo test --locked -p kapsel --lib \
  kind_tests::patch_experiment::frozen_json_document -- --nocapture
./scripts/format.sh
./scripts/ci-local.sh
TMPDIR=/tmp ./scripts/test-kind-effect-gateway.sh
```

The launcher records the base revision and binary working-tree diff SHA-256. Untracked source is
refused rather than omitted from evidence. The pinned node image is
`kindest/node:v1.33.12@sha256:3f5c8443c620245e4d355cfe09e96a91ead32ceaa569d3f1ca9edf0cb2fe2ff4`. The
audit policy and unauthenticated webhook control port are disposable test infrastructure only. The
launcher creates a uniquely named cluster and removes only its owned cluster, image, and workspace.
Local uncommitted evidence is not a remotely retrievable commit or a published release.

### Release artifact

Artifact proof crosses extracted `x86_64-unknown-linux-gnu` bytes rather than a Cargo test binary.
Two isolated assemblies must produce identical archive, checksum, SBOM, and digest-manifest bytes.
Hostile archive validation precedes extraction; smoke uses only extracted files to prove identity,
grant provisioning, operation and restart, offline inspection, MCP equivalence, demonstration-binary
separation, cleanup, and uninstall. It kills the extracted demonstration executable at both owned
seams and preserves one provider attempt and frozen receipt bytes under rotated settings. The
Sigstore bundle receives identity and failure checks rather than a false reproducibility
requirement.

The live artifact demonstration is a separate environment-owning gate. Exact layout, publisher
authentication, provenance, and evidence limits belong to [Release artifacts](RELEASE.md).

### Kapsel service

The unpublished service evidence remains layered around `Application`:

- projection reads status and frozen receipts without Kubernetes access or lifecycle advancement;
- Unix-socket tests cover effective-group peer credentials, framing, allocation, hostile fields,
  disclosure, one in-flight submission, and no queue;
- process tests cover `ACCEPTED` as process ownership only, immediate `BUSY`, caller disconnect,
  concurrent status, one provider attempt, and one journal;
- process-loss tests require startup reconciliation before bind and preserve frozen receipt bytes;
- startup and asset tests freeze fixed roots, no-follow file rules, exact argv, stale-socket
  handling, systemd, sysusers, and namespaced RBAC bytes; and
- deterministic root-substitution tests rename and replace state, receipt, and runtime names after
  validation, then prove journal creation, receipt access, and socket bind stay with the retained
  directory identities.

Service-client tests freeze its three-command grammar, bounded framing, receipt digest verification,
exclusive mode-`0600` output, and refusal to replace an existing file. `kapsel-authority` tests
freeze shared grant/trust vectors and consistency without turning that package into a public SDK.
The [Kapsel service contract](KAPSEL_SERVICE.md) owns the complete unpublished boundary.

### Installer

The partial, unpublished installer has four current evidence layers:

- private platform-neutral `identity` and `transaction` modules own fixed GID/UID selection, exact
  observation classification, canonical transaction validation, pending-effect transitions, identity
  blocking, reopening, reverse rollback rules, and exact staged/published host-file evidence. Their
  implementation-local tests run under ordinary macOS package tests and workspace Clippy. Portable
  CLI, bundle-refusal, kubeconfig, and fixed-authority tests remain beside their owners;
- Linux-only implementation-local tests own bounded process execution, exact creation modes, and
  descriptor-relative `O_TMPFILE`, xattr, link, rename, fsync, and recovery behavior;
- ignored, named Linux Rust integration tests cross the staged bundle, exact descriptor-relative
  operator input, a Rust TLS Kubernetes fixture, authority consistency, hostile filesystem and
  metadata refusal, durable lock/transaction recovery, group/user process-kill seams, terminal
  conflict and ambiguous-user reopening, ownership-safe pre-user rollback, and one native
  Debian-tool composition. Rust owns fake host state, recovery classification, transaction parsing,
  and assertions. Process-kill helpers assert the SIGKILL-equivalent status 137 without a shell
  diagnostic. The outer Python and shell remain bundle-staging, cache-mount, and
  disposable-container launch code only; and
- `./scripts/test-debian12-installer-identities.sh` runs the exact approved groupadd and useradd
  argv against a pinned Debian 12 `linux/amd64` image. It records all changed account files, passwd
  and shadow rows, NSS name and numeric visibility, lock state, home, shell, GECOS, hostile
  defaults, duplicate name and UID, timeout, process loss, injected partial state, and the exact
  sudo effective GID path without supplementary membership.

The direct identity experiment must classify only exactly absent, exactly complete, conflict, or
ambiguous/partial. It must derive no-effect or completion from command status. The container is
always disposable; conflict or ambiguous/partial evidence permits no repair or continuation.

The Linux container keeps build caches separate from evidence. Its cache key includes the pinned
builder, toolchain, target, and lockfile. Rustup state, the Cargo registry, and compiled
dependencies may survive. The staged payload, operator directory, fake host, Kubernetes server,
transactions, locks, and observations never do. Native Linux workspace Clippy remains authoritative
for the thin Linux adapters; Docker is not part of pre-commit or the deterministic local gate.

Implemented execution ends after read-only host and Kubernetes clean-install preflight, durable
`installing`, and recoverable creation of both fixed groups and users. Private Linux tests also
prove the regular-host-file staging, marker, inode, no-replace publication, and recovery foundation,
including the required destination-filesystem operations, but no production asset order invokes it.
Group rollback remains available only before a user effect. No test proves installed production
assets, Kubernetes mutation, credential issuance, activation, refresh, uninstall, real payload
provenance, final metadata or size bounds, runnable installation, candidate assembly, or candidate
qualification. The default payload-free build stops at `bundle_unavailable`; staged test builds stop
at `implementation_incomplete` after the implemented identity boundary.

The service and installer are absent from v0.2.0 and remain unpublished.
