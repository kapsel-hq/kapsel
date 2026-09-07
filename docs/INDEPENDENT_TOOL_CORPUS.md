# Independent kubectl failure corpus

Status: bounded client experiment, run with kubectl v1.33.9. Not a product-direction decision.

## Question and result

Can the failure cases learned from Kapsel expose a useful recovery or reporting hazard in an
independent tool, without requiring that tool to adopt Kapsel?

The experiment runs the real `kubectl set image` and `kubectl rollout status` executables against a
small loopback HTTP fixture. It reproduces actionable integration risks: an image update does not
establish rollout completion, a new status process observes the current named Deployment rather than
the original action, and a successful non-watching status command can describe a pending rollout.
None is claimed as an upstream kubectl bug. They are ordinary client-contract tests.

**The failure-kit thesis is rejected for this experiment.** The cases travel without Kapsel, but do
not establish practical value beyond ordinary adapter tests. My recommendation is to retain only a
compact reference implementation rather than make a failure kit primary or deepen the resident
broker on this evidence alone. That recommendation combines this negative result with the earlier
[workflow experiment](RECONNECTABLE_AGENT_ACTION.md), which found no reduction in operator effort.
It does not remove any implementation, change a contract, or establish that a broker cannot be
useful in another concrete workflow. [Technical scope](SCOPE.md) remains authoritative until an
explicit direction decision is accepted.

## Reproduce

Requirements: Python 3.11+ and the upstream kubectl v1.33.9 build at commit
`69220b617523ac1ba5d070e74c12b5daf5e6c572` on `PATH`. The script refuses other version/commit pairs.
It needs neither Cargo nor Kapsel binaries, Kubernetes, Docker, model calls, or provider
credentials.

```sh
python3 scripts/test-independent-kubectl.py > /tmp/kubectl-corpus.json
python3 scripts/test-independent-kubectl.py --case recreated-same-revision \
  > /tmp/kubectl-recreation.json
```

The second command is the reduced replay for the strongest attribution hazard. Both commands exit
nonzero on an unexpected client response, patch shape, retry, or control result. This is a separate
CI-capable experiment, not an added kubectl dependency in the default deterministic gate.

Each case has a new temporary kubeconfig, discovery cache, home, and loopback port. Every command
receives an explicit context, namespace, server, and kubeconfig. The child environment excludes
ambient credentials and proxies. No real cluster is contacted. Request and process deadlines bound
failure waits. The kill case waits for fixture persistence, sends SIGKILL to the actual caller, then
starts a new read-only status process. There is no caller retry loop.

## What is measured

The JSON separates:

- the intended UID, original opaque resourceVersion, and immutable requested image;
- actual caller argv, captured stdout/stderr, exit status, kill flag, and elapsed time;
- HTTP requests observed by the fixture, including the exact PATCH bytes as decoded JSON;
- fixture-owned image changes, writer interventions, and explicit status updates;
- retained receiver state and the caller's retained argv/output, with no action journal; and
- the status text reported to the reconstructed caller.

The intended facts belong to the test oracle. Kubectl is not represented as having frozen or
approved them. The caller processes do not share a durable action handle. Their discovery cache
contains no action outcome.

The fixture records the receiver state independently of kubectl. It does not import Kapsel, call its
classifier, parse a receipt, or reproduce its journal. It accepts only one exact image-patch shape
and serves the small discovery, GET, LIST, and WATCH responses the commands need. Status updates are
explicit fixture inputs, not simulated controllers. Its effect log establishes only fixture
persistence and request behavior. It establishes no real Kubernetes admission ordering, storage
conflicts, ReplicaSet creation, or application health. The earlier
[live recovery comparison](decisions/0011-retain-observation-only-recovery.md#evidence) owns those
separate Kubernetes observations.

## Cases and controls

Every case sends exactly one PATCH. Reconstruction sends only reads. The fixture does not provoke or
assert unsafe automatic retries that kubectl does not perform.

| Case                          | Caller behavior and retained evidence                                                                               | Independent fixture state                                                                   | Reported conclusion and integration risk                                                                                           |
| ----------------------------- | ------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------- |
| Healthy control               | `set image` exits 0. Fresh status and revision-2 status exit 0.                                                     | Requested image on original UID, explicitly settled revision 2.                             | `image updated`, then `successfully rolled out`. Positive control.                                                                 |
| Lost response                 | Fixture persists PATCH, closes connection without a response. `set image` exits 1. Fresh status exits 0.            | One image change persists. Fixture later settles it.                                        | Error is not proof of no effect. Fresh status describes the current rollout, not proof of original causation.                      |
| Caller loss                   | Actual caller is killed after persistence and before the response. Exit is -9. New status process exits 0.          | One image change persists, then fixture settles it.                                         | Reconstruction can inspect current state without re-sending. The process has no durable original-action record.                    |
| Stale GET snapshot            | Fixture captures GET response, changes target state before delivering it, then receives PATCH. `set image` exits 0. | PATCH has no UID/resourceVersion preconditions and changes the writer's image.              | Client-generated patch does not carry snapshot approval. This fixture result is not a claim about Kubernetes conflict enforcement. |
| Intervening writer            | Writer changes image and revision after PATCH. Fresh default status exits 0. Revision-2 status exits 1.             | Current revision 3 runs a different image.                                                  | Default status reports the later rollout. Revision pin is a useful negative control.                                               |
| Recreated name, same revision | Writer replaces UID and image but uses revision 2. Fresh default and revision-2 status exit 0.                      | Replacement UID, different image, settled revision 2.                                       | A revision number is not an object identity or original-action handle. Both status calls describe the replacement.                 |
| Pending, no watch             | `set image` exits 0. Fresh `rollout status --watch=false` exits 0 with waiting text.                                | Requested image is recorded but generation is not observed and available replicas are zero. | Neither exit zero nor `image updated` means rollout completion. The waiting text is honest.                                        |

An adapter can turn these into regressions without adopting Kapsel: preserve original identity and
intent in workflow history, compare them before attributing later status, and keep command
completion separate from receiver outcome. This experiment does not implement a new adapter or claim
those checks solve atomic authorization, ambiguous mutation, or causation.

## Upstream contract

The version-pinned primary sources were read before implementing the fixture:

- [`set_image.go`](https://github.com/kubernetes/kubectl/blob/v0.33.9/pkg/cmd/set/set_image.go)
  calculates a strategic merge diff and prints the returned object after PATCH. It does not wait for
  rollout or add UID/resourceVersion conditions to that image-only diff.
- [`rollout_status.go`](https://github.com/kubernetes/kubectl/blob/v0.33.9/pkg/cmd/rollout/rollout_status.go)
  documents following the latest rollout by default and the optional revision pin. Its watch loop
  prints status and returns successfully when watching is disabled, even when `done` is false. A
  deletion event during an existing watch is an error. That protection does not bind a fresh process
  to an object observed by an earlier process.
- [Deployment status viewer](https://github.com/kubernetes/kubectl/blob/v0.33.9/pkg/polymorphichelpers/rollout_status.go)
  compares the optional revision number and current generation/replica status. It takes no expected
  UID or image parameter and makes no original-action attribution claim.

These sources explain why the observations are expected. They are pinned, not claims about the
latest kubectl. The script checks the reported build identity, not a binary signature or checksum.

## Cost and limits

The seven-case run on macOS arm64 completed in 4.502 seconds, including fresh subprocesses and
fixture cleanup. The suite uses one Python standard-library file, no new dependency or production
interface, seven cases, and one exact patch shape. Its separate replay requires only the same
kubectl build. Port numbers, request scheduling, and timings vary. Assertions depend on the
coordinated state changes, not exact request order or elapsed time.

The maintenance cost is version-specific discovery/list/watch fixtures and patch/output assertions.
An upgrade requires reviewing upstream behavior and rerunning controls. It does not require
maintaining a Kubernetes simulator, corpus interchange format, or general tool adapter. That low
integration cost still buys ordinary adapter knowledge, not evidence for a new maintained product.

Two concrete approaches were considered. Extending the live kind lane would establish actual
receiver behavior but add cluster, image, credential, and controller setup to a client-reporting
question. The selected loopback fixture executes the independent client and isolates the reporting
contract in seconds. Its deliberately narrower evidence cannot replace live kind proof. No new live
cluster experiment was run here.

The recommendation remains limited to this one independently maintained client and these fixed
failure cases. No agent model was run and no second independently maintained agent wrapper was
qualified. No public Rust, CLI, MCP, grant, lifecycle, receipt, or recovery contract changed.
