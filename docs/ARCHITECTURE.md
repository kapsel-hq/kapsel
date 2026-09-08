# Architecture

This page owns the current code composition and compile-time dependency direction. The
[effect-gateway contract](EFFECT_GATEWAY.md) owns exact authorization, lifecycle, recovery, result,
and receipt semantics. [Technical scope](SCOPE.md) owns release and support boundaries.

## Root product package

The repository root is the `kapsel` product package and workspace root. It composes one bounded
Kubernetes Deployment image operation through a deep application interface:

```text
local operate command or fixed stdio MCP adapter
  -> Application
       -> private Gateway
            -> exact grant authorization
            -> SQLite journal
            -> concrete Kubernetes adapter
            -> receiver-fact classification
            -> receipt signing and SQLite-owned terminal completion
            -> optional filesystem export

kapsel inspect
  -> offline receipt inspector
       <- receipt bytes + explicit trust + evaluation time + limits
```

`Application` is the caller-facing composition root. `OperatorConfiguration` supplies the exact
owner-signed grant, configured grant trust, concrete Kubernetes client, receipt signing material,
journal path, and receipt directory. The caller submits only `AgentRequest`, an alias for the
concrete `SetDeploymentImageRequest`; it cannot select authority, trust, credentials, paths, signing
material, or lifecycle controls.

`Application::execute` owns submission and lifecycle sequencing and returns an `OperationReport`.
`Application::reconcile` resumes the configured operation after restart and returns an optional
report when that operation exists. Application failures expose configuration, request-rejection, and
operation-failure classes instead of private gateway errors. Submission and snapshot helpers remain
private so callers cannot sequence durable states.

The private `Gateway` owns validation, authorization, journaling, conditional mutation,
observation-only recovery, receiver classification, and frozen receipt construction. It commits the
signed receipt and terminal state together in SQLite. Filesystem export is a separate adapter
operation. The crate-level offline inspector consumes receipt bytes directly without opening
`Application`, the journal, or a Kubernetes client. The journal provides one interface for rows,
snapshots, worker locking, capacity, and guarded transitions. Private `schema` and `opening`
children own exact layout and migration, and safe SQLite entry and owner-private pathname identity,
respectively.

## Concrete Kubernetes boundary

The Kubernetes adapter performs safe target reads, one conditional strategic merge patch, and
bounded rollout observation for the sole operation. Receiver facts and classification are separate
from transport and request-acceptance facts.

`Journal::begin_attempt` produces a private one-use dispatch permission after the fresh transaction
commits. The adapter consumes its bound request and target. Ordinary sequential control flow
remains, and loaded attempted history stays observation-only. The
[fresh dispatch contract](EFFECT_GATEWAY.md#fresh-dispatch-permission) owns the exact guarantees,
client-retry obligations and limits.

A private adapter seam supports deterministic provider-call and crash-recovery tests. One production
adapter does not establish a reusable provider model, public provider interface, or generic
Kubernetes abstraction.

## CLI and MCP composition

`src/lib.rs` is the workspace-visible interface map. The executable layers remain shallow:

- `transport_support` loads bounded operator files and projects typed application reports and errors
  for both adapters;
- `command` owns fixed CLI input, deterministic envelopes, and exit classes;
- `mcp` owns bounded stdio framing and protocol lifecycle; and
- `main.rs` owns process arguments, streams, and exit handling.

The evaluator CLI and fixed-schema stdio MCP adapter both convert their inputs into the same
`Application` interface. Neither sequences private durable states. MCP exposes only request fields;
operator configuration remains out of band. [Evaluator commands](COMMANDS.md) and [MCP](MCP.md) own
their exact external contracts.

## Receipt and export composition

The receipt module owns canonical classifier-complete bytes, signatures, bounded parsing,
recomputation, and explicit trust, time, and limit inputs. Inspection is offline. SQLite commits
signed receipt bytes, their digest, signer identity, and terminal state atomically. The export
module owns Unix descriptor-relative, owner-private, collision-safe installation of already frozen
bytes. Neither module appoints ambient trust or establishes receiver truth, causation, or complete
capture.

## Release composition

Release assembly packages the same compile-time root product for the sole release target. The
ordinary `kapsel` executable is feature-free. A separately named `libexec` demonstration executable
contains compile-time `demo-harness` controls and is used only by the bundled demonstration.
Checksums, metadata, SBOM, and smoke automation are distribution concerns; they add no runtime
plugin, provider interface, trust source, or result vocabulary. The [release contract](RELEASE.md)
owns the exact archive.

## Workspace packages

```text
kapsel (root product)
  -> kapsel-authority

kapseld (unpublished service)
  -> kapsel

kapsel-installer (partial, unpublished)
  -> kapsel-authority
```

`kapsel-authority` is a fixed-purpose source-composition seam. It owns the exact authorization-grant
codec, receipt-trust codec, and their combined operator-input consistency check. The root product
and installer consume the same implementation without giving the installer the Kubernetes, journal,
or gateway dependency graph. It is not an installed process, runtime package, public SDK, generic
validation library, or supported Rust interface.

The excluded `fuzz` package contains hostile-input proof targets.

## Unpublished service

Repository HEAD also composes an unpublished resident service:

```text
bounded local service client
  -> authenticated Linux Unix socket
       -> kapseld
            -> Application
                 -> sole SQLite effect journal
```

`kapseld` provides caller-independent process lifetime, startup reconciliation, read-only status,
and exact frozen-receipt retrieval across a separate OS identity. It accepts fixed operator and
socket arguments, validates fixed roots descriptor-relatively, then keeps journal and socket I/O
beneath the retained directory handles through verified Linux `/proc/self/fd` paths. It reconciles
before binding and removes only an exact inactive service-owned stale socket. Unavailable or
inconsistent procfs fails startup before durable or socket effects; SQLite's moved-database refusal
fails later journal writes rather than reopening a replaced state root. Systemd owns process
lifecycle, runtime-directory cleanup, health, and diagnostics. Static inputs define one service
identity and namespaced Kubernetes RBAC.

The service adapter composes `Application::execute`, `Application::reconcile`, non-mutating
exact-grant matching, projected status, and frozen-receipt reads. It does not query SQLite directly,
duplicate export rules, sequence lifecycle states, add another store, or create a queue. The
[Kapsel service contract](KAPSEL_SERVICE.md) owns its unpublished external and installation
boundary.

## Partial installer

The unpublished `kapsel-installer` package has a fixed three-command grammar and consumes
`kapsel-authority` directly. Default workspace builds carry no embedded service payload, so a
mutating invocation stops at `bundle_unavailable` before host access. The release-only
`KAPSEL_INSTALLER_STAGE` build seam accepts one structurally bounded fixed stage; the current Docker
smoke supplies test-only ELF fixtures.

Current implementation validates operator input and bootstrap kubeconfig, performs read-only host
and Kubernetes clean-install preflight, durably enters `installing`, and creates or recovers the
exact two groups and two fixed users. A private host-file foundation also owns exact staged-file
facts, inode-bound publication, and recovery under a fixed absolute destination. No production asset
order calls that foundation, so execution still stops at `implementation_incomplete` after identity
creation. The installer does not install assets, Kubernetes resources, credentials, activation,
refresh, or uninstall, and no candidate assembly command exists. [Build](BUILD.md) lists its
runnable gates; [Kapsel service](KAPSEL_SERVICE.md) owns the approved future contract and exact
current boundary.

## Dependency rule

Transport and service adapters depend inward on `Application`; `Application` depends inward on the
private gateway and concrete implementations. Authority codecs may be shared only through the narrow
fixed-purpose package. A new package or public interface requires a demonstrated deployment or
dependency boundary and real consumers, not a speculative reuse case.

Accepted [architecture decisions](decisions/README.md) explain why this composition exists without
overriding current contracts.
