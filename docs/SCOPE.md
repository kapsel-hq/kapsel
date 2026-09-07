# Technical scope

> Kapsel gives an automated workflow one tightly bounded Kubernetes operation without giving it
> cluster credentials, then returns the strongest honest result supported by bounded receiver
> observations: `SUCCEEDED`, `FAILED`, or `UNKNOWN`.

Status: active development. The published release is the v0.2.0 developer beta; current source also
contains unreleased service and receipt-storage changes.

This page owns Kapsel's current product boundary, maturity, and technical non-goals. The
[effect-gateway contract](EFFECT_GATEWAY.md) owns exact authorization, lifecycle, recovery, result,
and receipt semantics.

## Direction and current boundary

The [public technical vision](../README.md#why-kapsel-exists) is controlled execution beneath
fallible autonomous systems. Kubernetes is the first concrete proving ground. That motivation does
not make Kapsel a general Kubernetes reliability service or extend the implemented boundary below.

Kapsel separates permission from execution evidence. Choosing a useful action and evaluating
application quality belong to the caller's surrounding system. Neither model confidence nor an
application evaluator can replace the exact grant or redefine a receiver result.

The current local journal and conditional Kubernetes patch do not coordinate independent agents
across journals or hosts. They provide no fleet ordering, distributed transaction, or system-wide
invariant guarantee. Broader coordination and additional operations require their own concrete
semantics and evidence, not a generic interface added in anticipation.

## One capability

```text
kubernetes.set_deployment_image(namespace, deployment, container, immutable_image_digest)
```

The caller supplies one stable operation identity plus the namespace, Deployment, container, and
immutable digest-bound image. The operator separately supplies the exact signed grant and trusted
key, Kubernetes authority, receipt signing material, journal, and private paths.

Caller input cannot contain credentials, trust, grants, shell commands, `kubectl`, manifests,
arbitrary patches, tags, wildcards, paths, or lifecycle controls.

## One durable path

```text
bounded request
  -> exact operator-owned authorization
  -> durable pre-attempt rejection or mutation marker
  -> one conditional Kubernetes patch opportunity
  -> bounded rollout observation
  -> SUCCEEDED / FAILED / UNKNOWN
  -> frozen signed receipt
```

Kapsel commits target identity and `apply_started` before attempting the patch. Recovery from that
state observes rather than blindly mutating again.

`SUCCEEDED` and `FAILED` are defined classifications over bounded observations of the same target,
image, and generation. `UNKNOWN` means reconciliation established neither result. It does not mean
failure, no effect, safety, or permission to retry.

A permanent missing or invalid target may finish as `NOT_ATTEMPTED` before the mutation marker. That
is a local disposition with no receiver result or effect receipt.

An inspected receipt authenticates frozen bytes and classifier consistency under separately supplied
trust. It does not prove causation, exactly-once effects, complete cluster health, complete capture,
compliance, or Kubernetes truth.

## What is published

The [v0.2.0 developer beta](V0.2.md) provides:

- one authenticated x86-64 GNU/Linux archive;
- a local CLI and fixed-schema stdio MCP adapter;
- signed exact grants and classifier-complete receipts;
- SQLite-backed crash recovery;
- a disposable-`kind` crash-recovery demonstration; and
- bounded v0.1.1 journal, grant, and receipt continuity.

Only the named v0.2.x CLI, MCP, grant, receipt, archive, and journal-upgrade surfaces have the
bounded compatibility described by their direct contracts. Public Rust APIs, another platform, and
production support do not.

Repository HEAD also contains an unpublished customer-resident [Kapsel service](KAPSEL_SERVICE.md)
and partial installer work. The installer implements its foundation plus the two fixed groups and
two fixed users, but no assets, Kubernetes mutation, credential, activation, refresh, or uninstall.
They add no v0.2.0 promise and are not currently a supported installation path.

## Maturity

Kapsel is a developer beta, not production software. It has finite proof for one operation, one
Kubernetes adapter, one release target, and named crash windows. It has no production availability,
remediation, support, high-availability, backup, or platform promise.

## Non-goals

Kapsel does not provide:

- a Kubernetes operation suite or arbitrary administration;
- a generic provider SDK, capability system, policy language, or workflow engine;
- runtime plugins, a public Rust SDK, or a stable package ecosystem;
- a hosted control plane, dashboard, fleet manager, or managed authority;
- a generic receipt, witnessing, compliance, or audit product;
- exactly-once mutation, universal capture, causal proof, or universal Kubernetes truth;
- production support, high availability, or a second platform target; or
- a second capability without its own concrete semantics and evidence.

The [architecture](ARCHITECTURE.md) describes the current composition. Accepted
[decisions](decisions/README.md) explain why it remains deliberately narrow.
