# Retain observation-only recovery after an ambiguous patch

Status: accepted.

Kind: decision. Date: 2026-09-05.

Owns: Whether recovery after `apply_started` observes, replays the frozen conditional patch, or
hands retry scheduling to a durable-workflow runtime.

Does not own: Receiver-result classification, a workflow-engine integration, another capability, or
receipt bytes.

## Context

A process can die after Kapsel commits `apply_started` but before any network send. Observation-only
recovery abandons that still-authorized action. Replaying the exact frozen operation could complete
it while the original Deployment UID and resource version remain current.

The same journal state also covers a request accepted by Kubernetes whose response was lost. A
recovery policy cannot distinguish those windows. UID and resource-version preconditions bound
persistence, but they do not protect all earlier Kubernetes request processing.

The comparison used one immutable operation tuple:

```text
operation ID + Deployment UID + resourceVersion + immutable image +
conditional-strategic-merge-patch
```

Every replay used those original values. It never refreshed authorization, target identity, or a
precondition from later state.

## Evidence

The deterministic test
`recovery_policy_tests::frozen_recovery_policy_matrix_separates_requests_admission_and_effects` uses
an independent receiver model rather than Kapsel's classifier. It compares:

1. current observation-only recovery;
2. exactly one replay of the frozen patch; and
3. a projection of a Temporal Activity with explicit `MaximumAttempts: 2`.

The Temporal row is a semantic projection of documented at-least-once Activity execution. The test
does not claim to execute Temporal.

Each cell below is
`caller invocations / PATCH requests / mutating admission invocations / out-of-band admission effects / persisted Deployment changes / controller effects / authorized actions left unsent / caller conclusions`.

| Scenario                                         | Observe only                      | One frozen replay                 | Temporal projection               |
| ------------------------------------------------ | --------------------------------- | --------------------------------- | --------------------------------- |
| Death immediately before send                    | `1/0/0/0/0/0/1/UNKNOWN`           | `1/1/1/0/1/1/0/SUCCEEDED`         | `1/1/1/0/1/1/0/SUCCEEDED`         |
| Accepted mutation, response lost                 | `1/1/1/0/1/1/0/SUCCEEDED`         | `1/2/2/0/1/1/0/SUCCEEDED`         | `1/2/2/0/1/1/0/SUCCEEDED`         |
| Two callers, identical original preconditions    | `2/2/2/0/1/1/0/SUCCEEDED+UNKNOWN` | `2/2/2/0/1/1/0/SUCCEEDED+UNKNOWN` | `2/2/2/0/1/1/0/SUCCEEDED+UNKNOWN` |
| Intervening writer before recovery               | `1/0/0/0/1/1/1/UNKNOWN`           | `1/1/1/0/1/1/0/UNKNOWN`           | `1/1/1/0/1/1/0/UNKNOWN`           |
| Target deletion and recreation                   | `1/0/0/0/1/1/1/UNKNOWN`           | `1/1/1/0/1/1/0/UNKNOWN`           | `1/1/1/0/1/1/0/UNKNOWN`           |
| Later template change retaining marker/image     | `1/1/1/0/2/2/0/SUCCEEDED*`        | `1/2/2/0/2/2/0/SUCCEEDED*`        | `1/2/2/0/2/2/0/SUCCEEDED*`        |
| Admission out-of-band effect after response loss | `1/1/1/1/1/1/0/SUCCEEDED`         | `1/2/2/2/1/1/0/SUCCEEDED`         | `1/2/2/2/1/1/0/SUCCEEDED`         |

The concurrent row has two separately authorized operation IDs with the same frozen Deployment
identity, resource version, image, and strategy. Both initial requests run, one persists, and the
other returns a conflict. No ambiguous result is injected in that row, so none of the three recovery
policies adds a request.

`SUCCEEDED*` is only a conclusion from the later observed state. A writer changed the template after
the original patch while retaining its image and marker. The current classifier can use that later
current generation as the requested generation. Neither replay nor Temporal identifies the original
patch generation, so this row is not evidence that the original effect caused the later rollout. The
receipt's no-causation claim remains material.

The live command `./scripts/test-kind-effect-gateway.sh` adds a pinned Kubernetes v1.33.12 proof. An
instrumented mutating webhook records each unique AdmissionReview UID and operation ID as an
out-of-band log effect. The first frozen strategic patch persists and creates one new ReplicaSet.
Replaying the identical stale patch invokes the webhook again, then returns Kubernetes API
`409 Conflict`. The post-replay Deployment UID, resource version, generation, complete desired spec,
operation annotation, both container images, and ReplicaSet count equal the post-first-patch state.

That ordering is expected from the pinned Kubernetes source. Strategic PATCH invokes mutating
admission while producing the updated object inside `GuaranteedUpdate`; storage checks the stale
resource version afterward:

- [PATCH handler admission and update, Kubernetes v1.33.12](https://github.com/kubernetes/kubernetes/blob/v1.33.12/staging/src/k8s.io/apiserver/pkg/endpoints/handlers/patch.go#L628-L704)
- [registry storage update checks, Kubernetes v1.33.12](https://github.com/kubernetes/kubernetes/blob/v1.33.12/staging/src/k8s.io/apiserver/pkg/registry/generic/registry/store.go#L649-L733)

Kubernetes'
[admission webhook good practices](https://kubernetes.io/docs/concepts/cluster-administration/admission-webhooks-good-practices/)
say webhooks should avoid out-of-band side effects and be idempotent. They also permit real-request
side effects through `sideEffects: NoneOnDryRun`. The frozen operation annotation is not a
receiver-enforced idempotency key, so Kapsel cannot assume every admission component deduplicates
it.

## Workflow baseline

Temporal provides durable Workflow Event History, Activity timeouts, and retry scheduling. Its
[Activity execution](https://docs.temporal.io/activity-execution) is at least once, so a worker loss
can execute the Activity again.
[Retry policies](https://docs.temporal.io/encyclopedia/retry-policies) must be explicitly bounded
for this comparison; the projected second attempt uses the original opaque Activity payload and
treats conflict as ambiguity before observation. Temporal does not make the Kubernetes effect atomic
with Activity completion.

An equivalent implementation still needs resident provider authority, a frozen versioned operation
payload, Kubernetes-aware conflict handling, read-only receiver classification, durable receipt
semantics, and admission-side-effect idempotence outside Temporal. Self-hosting also adds the
[Temporal service](https://docs.temporal.io/temporal-service), persistence, schema, worker, upgrade,
and backup operations. The official [SDK Core repository](https://github.com/temporalio/sdk-core)
states that its Rust SDK is under development, adding either an experimental SDK or another worker
language to this Rust repository.

## Decision

Retain observation-only recovery after `apply_started`.

No replay means death before send can abandon an authorized action. This is reported as `UNKNOWN`,
not hidden as success or failure. Exact replay improves that one completion window, but after a lost
response, conflict, or replacement it adds another mutating-admission invocation and can repeat an
out-of-band receiver effect that frozen UID and resource version cannot prevent. No-replay therefore
passes the decision test because it prevents a concrete receiver effect that frozen replay cannot
prevent.

Do not adopt Temporal for this operation. Its bounded Activity retry has the same receiver exposure
as exact replay and does not replace the capability-specific authority, receiver, or receipt logic.
It adds materially more implementation and operational machinery.

For reconnectable callers, continuation is exact: after `apply_started`, a new caller or process
resumes the same operation by observation only. It must not issue a new operation identity, refresh
authority or preconditions, or resend through caller or workflow retry. `UNKNOWN` stops dependent
automation and hands the frozen evidence to a human.

## Current sequential design

The adopted implementation keeps durable attempt, dispatch permission, observation-only recovery,
and receipt completion distinct. A durable attempt records that dispatch may have happened, so
recovery cannot derive permission from it. Dispatch permission is a private, one-use value issued
after a successful fresh attempt commit and consumed by the adapter. Observation-only recovery
determines what can be concluded without sending the mutation again. Receipt completion commits the
original signed evidence in SQLite, independently of export. The
[effect-gateway contract](../EFFECT_GATEWAY.md#fresh-dispatch-permission) owns the exact rules.

The sequential comparison below showed that the adapter interface can enforce part of the dispatch
discipline without an event machine. Binding the complete authorized snapshot inside the attempt
transaction prevents substitution of different facts under the same operation ID. Consuming the
permission removes ordinary repeat-dispatch and history-dispatch call sites. The inherited
client-retry correction prevents automatic server-response retries from expanding one permitted
dispatch into repeated PATCH requests.

Worker exclusion, conditional database writes, the driver's no-retry obligation, and honest UNKNOWN
remain necessary. The new deterministic and loopback HTTP evidence does not extend the pinned live
Kubernetes or durability claims below.

### Sequential boundary comparison

The original flow already enforced fresh dispatch through journal phases, conditional writes, worker
exclusion, and lexical control flow. Its adapter still accepted independently selected, clonable
request and target values. Correct callers did not resend, but the interface would accept an
accidental second call or arguments reconstructed from attempted history.

| Candidate                                      | Benefit                                                                         | Cost or reason not selected                                                                      |
| ---------------------------------------------- | ------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------ |
| Existing sequential flow plus retry correction | Correct existing ordering and observation-only recovery                         | Reusable apply arguments leave dispatch discipline to callers                                    |
| Private consumed dispatch permission, adopted  | Prevents ordinary repeat/history dispatch and binds the exact committed payload | One private type, one bounded row load, and a short transaction                                  |
| Extract only pure snapshot decisions           | Could move the existing comparison into a function                              | No second production comparison to delete. Does not establish commitment or prevent repeat apply |

Before, `begin_attempt` returned a clonable target and the gateway called
`adapter.apply(&request, &target).await`. Now it returns a `DispatchPermission` and the same
sequential call site uses `adapter.apply(permission).await`. The old adapter interface is removed,
not retained behind a wrapper. The journal's private constructor runs only after successful commit
acknowledgement. The concrete adapter consumes the permission into its bound request and target
before building the same strategic merge PATCH.

The complete authorized snapshot is checked in one immediate transaction before the conditional
attempt write. That prevents a phase-typed snapshot from another journal supplying different
request, approval, or authorization provenance under the same operation ID. A losing transition or
commit error returns no permission. No transaction spans a network call or await.

Snapshot comparison remains in `Journal::begin_attempt`. Classification, grant verification,
observation bounds, receipt bytes and format-4 completion retain their existing owners. The existing
seeded harness executes these same journal/gateway decisions. This is shared execution, not a new
pure approval/recovery kernel. The larger event machine remains rejected rather than becoming a
second production policy owner.

### Sequential boundary evidence

The comparison began at `21f0a2534e9555130025a4082e935194886a6cb2`, which preserves the rejected
event-machine prototype and separate retry correction atop the adopted, unreleased format-4 baseline
`b27d8e0a6c3dea91d2d4333ea111d32629a04f26`. The application retry correction is retained, not
attributed to dispatch permission. Grant v1 late binding, grant v2 exact approval, receipt v2/v3,
receiver classification and compatibility policy are unchanged.

| Trace                                                | Observed result                                                                                |
| ---------------------------------------------------- | ---------------------------------------------------------------------------------------------- |
| Two real connections race fresh commitment           | Exactly one permission with the frozen request and target                                      |
| Same ID, different action from another journal       | No permission or durable change                                                                |
| Commit succeeds, acknowledgement is lost             | No permission returned. Recovery only observes and freezes UNKNOWN for an unsent action        |
| Unused permission is dropped                         | Zero applies, no NOT_ATTEMPTED, honest UNKNOWN and preserved receipt bytes across reopen       |
| Stale Authorized snapshot is reused after commitment | No permission reminted                                                                         |
| Continuation is cancelled during target read         | Remains Authorized. A later fresh run may commit and dispatch once                             |
| Continuation is cancelled after dispatch             | Remains ApplyStarted. Recovery does not send again                                             |
| Process is killed while apply is pending             | Existing subprocess regression crosses the new interface and recovers without another mutation |

`tests/application_retry.rs` uses the ordinary operator-document client construction and a real
loopback HTTP receiver. Healthy execution, 429/503/504 responses, complete-request connection loss,
and cancellation after receipt of PATCH each count exactly one complete PATCH through restart,
reconciliation and repeat execution. The receiver remains available for the complete trace. Each
case checks two GETs and preserved receipt retrieval. The ambiguity matrix checks approved UID,
opaque resourceVersion and image in captured PATCH bodies. Success comes from separate receiver
observation, not the error response or request count.

Temporary internal API probes compiled against the actual private types reject permission reuse
(E0382), Clone (E0599), Copy (E0277), dispatch from `ApplyStartedOperation` (E0308), and
construction outside the journal's private fields (E0451). These were not external-crate privacy
failures. They can be reproduced in a temporary `#[cfg(test)]` child of `gateway` using `super::*`
and the invalid calls. Run the field-construction probe separately because earlier type errors can
prevent privacy checking. No compile-test dependency or source-rewriting harness is retained.

The focused dispatch group passed 4 tests, the gateway group 51 with 3 ignored, and the HTTP
group 3. The release-mode seeded lane passed 256 cases at seed `21182435914953528` with one shard,
alternating legacy and matching snapshot grants and including attempt-commit acknowledgement loss.
The full deterministic gate passed, with 114 root library tests passed and 9 ignored. A bounded
independent review found no concrete fix-worthy findings. Ignored live or extended lanes are not
counted as proved.

Find the local adoption revision and reproduce the evidence with:

```sh
git log -1 --format='%H %T %s' --grep='^gateway: adopt sequential dispatch permission$'
cargo test --locked -p kapsel --lib gateway::tests::dispatch -- --nocapture
cargo test --locked -p kapsel --test application_retry
KAPSEL_SIMULATION_SEED=21182435914953528 KAPSEL_SIMULATION_CASES=256 \
  KAPSEL_SIMULATION_SHARDS=1 ./scripts/test-simulation.sh
./scripts/ci-local.sh
```

Another checkout must obtain the identified commit from a repository containing it. Local
preservation does not imply a push, merge, published artifact or released behavior.

## Consequences and limits

- Dispatch permission is not lifetime-bound to `WorkerLock`. The driver must retain worker exclusion
  through I/O. The type does not constrain hostile code already holding raw credentials, or an
  adapter deliberately resending its extracted payload.
- The shared application client disables hidden server-response retries. Custom clients, proxies,
  HTTP/2 behavior and admission reinvocation remain separate obligations.
- Attempt-commit acknowledgement loss is injected after real commitment. This does not prove actual
  SQLite I/O failures, torn writes, power-loss or hardware durability, or unbounded schedules. The
  new cancellation tests drop futures. Existing process-kill tests provide separate finite evidence,
  not a new before-send or commit-acknowledgement process-kill proof.
- The seeded receiver fixture may report an independent failed rollout even for an unsent action.
  That tests classifier consistency, not causation. The dedicated unsent tests supply no receiver
  facts and require UNKNOWN.
- Kapsel prevents recovery-induced duplicate admission effects. It cannot prevent admission
  reinvocation internal to one API request or duplicates from independent pre-attempt callers.
- Frozen UID/resource-version replay does prevent overwriting an intervening writer or replacement
  Deployment in the tested races. It does not prevent the extra request or admission effect.
- The live result qualifies the pinned v1.33.12 kind receiver, not every Kubernetes version or
  admission implementation.
- Reconsider replay only with a receiver-enforced idempotency key covering the whole admission and
  persistence pipeline, or an enforced admission profile that excludes side effects.
- The later-generation attribution limitation is shared by all three policies and remains visible.
  It is not a reason to count replay as useful completion.
