# kapsel

[![CI](https://github.com/kapsel-cloud/kapsel/actions/workflows/ci.yml/badge.svg?branch=master)](https://github.com/kapsel-cloud/kapsel/actions/workflows/ci.yml)
[![Developer beta](https://img.shields.io/badge/developer_beta-v0.2.0-orange)](https://github.com/kapsel-cloud/kapsel/releases/tag/v0.2.0)

**Controlled execution for autonomous systems. Kubernetes first.**

Kapsel lets an AI agent or automated workflow request one tightly bounded Kubernetes change without
receiving cluster credentials. It records state before attempting the effect, recovers from crashes
without a blind second mutation, and returns a durable, inspectable `SUCCEEDED`, `FAILED`, or
`UNKNOWN` result based on bounded Kubernetes observations.

> [!WARNING]
>
> Kapsel 0.2.0 is a developer beta. It is not production-ready. Do not use it for consequential
> production changes.

## Why Kapsel exists

As infrastructure is increasingly operated by agents that observe, propose changes, and evaluate
results, the systems deciding what to do remain fallible. More capable reasoning does not remove the
need for a dependable boundary around consequential actions.

Kapsel's direction is to be that small execution component beside operator-owned authority. The
caller proposes an action. The operator authorizes it. Kapsel controls the bounded effect and
preserves what it can honestly establish, even when execution breaks.

Three questions must stay separate:

- **May this action happen?** Exact authorization determines permission, not an agent's confidence.
- **What happened?** Durable state and receiver observations support a bounded execution outcome.
- **Was it a good decision?** Application evaluation and operational judgment remain outside Kapsel.
  An available rollout does not establish that the application behaves correctly.

Kubernetes is the first proving ground, not the permanent product identity. It gives us real
asynchronous effects, concurrent changes, and observable outcomes. The same execution boundary is
useful to conventional automation and human callers. It should not depend on a particular model or
agent framework; MCP is just one adapter.

This is a direction to grow through concrete operations, not a shipped general platform. Today
Kapsel supports only the image change below. It is not a planner, an AI SRE, or a general Kubernetes
reliability service. [Technical scope](docs/SCOPE.md) owns current capabilities and limits.

## The problem is not the patch

Changing a Deployment image is easy. The difficult part starts when the process disappears between
sending the request and recording what happened:

- Did Kubernetes receive the request?
- Did the Deployment roll out the requested image?
- Is retrying safe, or could that create a second effect?
- What can the next automated step honestly conclude?

Kapsel treats the attempt and the conclusion as separate facts. It refuses to turn request
acceptance, transport completion, process exit, or timeout into a rollout result.

## One operation

```text
kubernetes.set_deployment_image(namespace, deployment, container, immutable_image_digest)
```

The caller supplies the operation identity and those four bounded values. The operator separately
owns the exact signed grant, trusted grant key, Kubernetes credentials, journal, receipt key, and
private paths. None of that authority enters caller input.

```text
bounded request
  -> exact operator-owned authorization
  -> durable target identity and attempt marker
  -> one conditional Kubernetes patch opportunity
  -> bounded rollout observation
  -> SUCCEEDED / FAILED / UNKNOWN
  -> frozen signed receipt
```

A missing or invalid target can stop earlier as `NOT_ATTEMPTED`. That is a local pre-attempt
disposition, not a Kubernetes receiver result.

## Honest outcomes

| Result      | What Kapsel established                                                                                                                      |
| ----------- | -------------------------------------------------------------------------------------------------------------------------------------------- |
| `SUCCEEDED` | The observed Deployment reached the requested generation and satisfied Kapsel's available-rollout classifier for the requested image.        |
| `FAILED`    | The same Deployment UID and requested image were observed at the requested generation with the defined `ProgressDeadlineExceeded` condition. |
| `UNKNOWN`   | Bounded reconciliation could establish neither defined outcome.                                                                              |

`UNKNOWN` does not mean failed, unchanged, or safe to retry. It is the strongest honest conclusion
when the evidence is incomplete or ambiguous.

The receipt authenticates Kapsel's frozen request, attempt, observation, result, and explicit
non-claims under supplied trust. It does not prove causation, exactly-once mutation, complete
cluster health, or universal Kubernetes truth.

## The crash boundary

Before crossing the mutation seam, Kapsel commits `apply_started` with the target Deployment UID,
resource version, and write strategy. If the process later restarts from that state, recovery only
observes. It does not blindly patch again.

That ordering is the core mechanism: durable state before the effect, observation after ambiguity.
The [technical tour](docs/TOUR.md) follows one operation through the complete path.

## Try the published beta

The [v0.2.0 release](https://github.com/kapsel-cloud/kapsel/releases/tag/v0.2.0) contains an
authenticated x86-64 GNU/Linux archive and a disposable-`kind` crash-recovery demonstration. The
[evaluation guide](docs/EVALUATOR.md) shows how to authenticate the artifact and run it.

For source development, follow [Build and test](docs/BUILD.md) to install the toolchain and run the
deterministic gate:

```sh
./scripts/ci-local.sh
```

With Docker, kind 0.32+, kubectl 1.30+, and Python 3.11+:

```sh
./scripts/demo-kind-crash-recovery.sh
```

The demo exercises healthy, failed-rollout, and process-loss paths, restarts without a blind second
mutation, inspects the frozen receipt, and removes the cluster it created.

## Go deeper

- [Technical tour](docs/TOUR.md) — follow one request from authority to receipt.
- [Effect-gateway contract](docs/EFFECT_GATEWAY.md) — exact lifecycle, recovery, result, and receipt
  semantics.
- [Evaluator commands](docs/COMMANDS.md) and [MCP adapter](docs/MCP.md) — supported v0.2.x
  interfaces.
- [Documentation map](docs/INDEX.md) — find architecture, security, release, and contributor docs.

Repository HEAD also contains unpublished service and installer work. It is not part of v0.2.0 and
is not a supported installation path. [Technical scope](docs/SCOPE.md) owns the exact current
boundary.

Licensed under the [Apache License, Version 2.0](LICENSE).
