# Evaluator commands

## Receipt completion

`operate` commits terminal receipt evidence in SQLite before optional filesystem export. It
preserves filename output by exporting the committed bytes after execution. Export errors fail the
command without reopening the terminal action. Repeating the command exports the same bytes under
current output configuration, without dispatch or re-signing. The export directory need not exist
during application startup or execution. Journal formats older than format 4 are rejected, not
upgraded.

Status: v0.2 beta command contract implemented by package version `0.2.0`.

Kind: contract. Authority: local evaluator command grammar, operator files, output, bounds,
diagnostics, and exit classes.

Owns: The exact `kapsel` command surface supported across v0.2.x.

Does not own: Gateway lifecycle/result semantics, receipt bytes, Kubernetes semantics, MCP,
packaging, or the release demonstration crash demonstration.

## Compatibility posture

The operation grammar, files, bounds, machine output, diagnostics, and exit classes below are
preserved from `v0.1.1` in package version `0.2.0` and define the supported v0.2 command surface.
v0.2 additionally adopts one read-only package identity form. Across v0.2.x, intentional
incompatible changes require an explicit owner update, migration or replacement guidance, and
release notes. This support does not extend to the Rust package, source layout, crates.io, docs.rs,
`cargo install`, another platform, or production use.

Canonical grant v1 and receipt/trust v2 continuity are owned only by
[effect-gateway](EFFECT_GATEWAY.md). This command contract adopts their bytes and result vocabulary
without defining another vector format.

## Command grammar

The Unix executable accepts exactly these local evaluator forms:

```text
kapsel --version
kapsel provision-grant --authorization <file> --signing-seed <file> --signing-key-id <id> --output <file>
kapsel operate --request <file> --operator-config <file>
kapsel inspect --receipt <file> --trust <file> --evaluation-time-unix-s <i64>
               [--receipt-bytes-max <usize>] [--statement-bytes-max <usize>]
               [--trust-bytes-max <usize>] [--text-bytes-max <usize>]
```

The same executable also has one separately owned MCP process form,
`kapsel mcp --operator-config <file>`. [MCP adapter](MCP.md) owns its protocol, lifecycle, tool,
responses, and bounds; this document continues to own the shared operator-file grammar.

`kapsel --version` accepts no additional argument, reads no configuration or environment, writes
`kapsel <Cargo package version>` plus one newline to standard output, writes no diagnostic, and
exits zero. The value identifies the running binary; it does not imply another workspace package,
platform, registry artifact, or production support.

Options may appear in any order, exactly once. Unknown options, duplicate options, positional
values, missing values, and additional arguments are command-input failures. There are no
environment or ambient defaults.

All named files must be regular, non-symlink files. Every command rejects a file larger than its
owned limit before reading it: JSON inputs are at most 16 KiB, signing seeds and public keys are
exactly 32 raw bytes, signed grants are at most 4 KiB, receipts are at most 16 KiB, and receipt
trust is at most 1 KiB. Output grant files are created owner-only and never replace an existing
path.

## Fixed JSON inputs

JSON documents are UTF-8 objects with exactly the fields listed below. Unknown, duplicate, missing,
non-string, and trailing content is rejected. String values remain subject to the effect-gateway
field grammars.

The authorization file contains operator intent:

```json
{
  "authorization_id": "auth-001",
  "operation_id": "op-001",
  "namespace": "demo",
  "deployment": "agent-api",
  "container": "api",
  "immutable_image_digest": "registry.example/agent-api@sha256:<64-lowercase-hex>"
}
```

The agent request contains no authority or configuration:

```json
{
  "operation_id": "op-001",
  "namespace": "demo",
  "deployment": "agent-api",
  "container": "api",
  "immutable_image_digest": "registry.example/agent-api@sha256:<64-lowercase-hex>"
}
```

The operator configuration contains paths to separately provisioned authority and exact public key
identities:

```json
{
  "signed_authorization_grant": "/absolute/grant.bin",
  "authorization_key_id": "owner-key",
  "authorization_public_key": "/absolute/owner.pub",
  "kubeconfig": "/absolute/kubeconfig.yaml",
  "journal": "/absolute/journal.sqlite3",
  "receipt_directory": "/absolute/receipts",
  "receipt_signing_seed": "/absolute/receipt.seed",
  "receipt_signing_key_id": "receipt-key"
}
```

Every operator path is absolute. Kapsel reads only the named kubeconfig and its selected
`current-context`; it does not infer a kubeconfig, context, credentials, proxy, or authority from
the environment. Certificate authority, client certificate, client key, and token data must be
embedded in that bounded kubeconfig. Path-based credential references, auth-provider plugins, and
exec plugins are rejected. The shared root composition limits each Kubernetes HTTP response body to
2 MiB before collection or deserialization, independent of content-length, chunked, or
close-delimited framing. Kubernetes authority and both signing seeds remain operator-owned and never
enter the agent request, journal, receipt, report, stdout, or stderr.

## Startup and restart

`operate` opens the configured `Application`, submits the exact request idempotently, and invokes
application-owned reconciliation. Starting the same command again with the same request and operator
configuration is ordinary restart. The configured grant selects the operation even when unrelated
journal rows exist. No command field selects a lifecycle state, transition, fault point, receipt
bytes, or provider action.

## Output and diagnostics

Each invocation writes exactly one newline-terminated JSON object to stdout and at most one
newline-terminated diagnostic to stderr. Provisioning and operation stdout and every stderr are at
most 4 KiB; classifier-complete inspection stdout is at most 64 KiB. JSON keys have the exact order
shown here; optional values use JSON `null`. No output contains input bytes, key material,
Kubernetes response bodies, or ambient values.

Successful grant provisioning:

```json
{ "command": "provision-grant", "status": "PROVISIONED" }
```

Successful operation/reconciliation:

```json
{
  "command": "operate",
  "operation_id": "op-001",
  "state": "FINALIZED",
  "result": "SUCCEEDED",
  "target_rejection": null,
  "receipt_file": "kap0038-op-001-<sha256>.receipt",
  "receipt_sha256": "<sha256>"
}
```

States and values use the effect-gateway vocabulary. A pre-attempt rejection reports
`NOT_ATTEMPTED`, a null result, and one of `DEPLOYMENT_NOT_FOUND`, `CONTAINER_NOT_FOUND`, or
`INVALID_TARGET`. `SUCCEEDED`, `FAILED`, and `UNKNOWN` are receiver outcomes and all are successful
command execution.

Offline inspection reports the classifier-complete signed statement in fixed field order:

```json
{
  "command": "inspect",
  "status": "INSPECTED",
  "operation_id": "op-001",
  "authorization_id": "auth-001",
  "authorization_signer_key_id": "owner-key",
  "authorization_grant_digest": "<sha256>",
  "namespace": "demo",
  "deployment": "agent-api",
  "container": "api",
  "immutable_image_digest": "registry.example/agent-api@sha256:<sha256>",
  "write_strategy": "conditional-strategic-merge-patch",
  "target_uid": "deployment-uid-1",
  "target_resource_version": "resource-version-0",
  "receiver_uid": "deployment-uid-1",
  "observed_image": "registry.example/agent-api@sha256:<sha256>",
  "observed_operation_marker": "op-001",
  "current_generation": 2,
  "requested_generation": 2,
  "observed_generation": 2,
  "observed_resource_version": "resource-version-2",
  "desired_replicas": 1,
  "updated_replicas": 0,
  "available_replicas": 0,
  "unavailable_replicas": 1,
  "rollout_condition_type": "Progressing",
  "rollout_condition_status": "False",
  "rollout_condition_reason": "ProgressDeadlineExceeded",
  "result": "FAILED",
  "non_claims": "no-exactly-once;no-causation;no-kubernetes-truth;no-complete-capture;no-witnessing;not-production"
}
```

For `STRUCTURE_REJECTED` and `SIGNATURE_REJECTED`, every statement and non-claims value is null. For
`UNTRUSTED_SIGNER`, authenticated statement fields and non-claims are present. The complete
maximum-sized escaped line remains within the 64 KiB inspection stdout bound. Inspection calls the
library inspector directly with the named bytes, explicit evaluation time, and explicit limits. It
constructs no Kubernetes client and performs no network, filesystem discovery, trust lookup,
environment lookup, or ambient clock read. It never emits `VERIFIED`.

A failed command emits this bounded stdout shape and a matching human diagnostic containing only the
class:

```json
{"command":"operate","status":"ERROR","error_class":"operator_configuration"}
Kapsel command failure: operator_configuration
```

The `command` value is the parsed subcommand, or `kapsel` when parsing did not identify one.

## Exit classes

| Exit | Class                    | Meaning                                                                   |
| ---- | ------------------------ | ------------------------------------------------------------------------- |
| 0    | completed                | Provisioning, operation outcome, or any bounded inspection status.        |
| 2    | `command_input`          | Invalid grammar, JSON, numeric value, bound, or agent/authorization data. |
| 3    | `operator_configuration` | Unsafe/missing operator file, authority, kubeconfig, signing, or path.    |
| 4    | `operation_failure`      | Durable, Kubernetes, reconciliation, or publication failure.              |

Every file and output shape above is a supported v0.2.x beta surface. Only the latest v0.2.x patch
receives best-effort security and correctness maintenance. There is no response-time, remediation,
availability, platform, or production-support SLA. Embedded contract text does not assert whether a
particular artifact has passed release acceptance or been published.
