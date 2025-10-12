# System Overview

Kapsel is a webhook reliability service that guarantees at-least-once delivery with cryptographically verifiable audit trails. We solve the critical problem of dropped webhooks in mission-critical integrations by acting as a highly reliable intermediary between webhook sources and destination endpoints.

## Core Value Proposition

**The Problem**: Webhook failures are silent killers. Network timeouts, 5xx errors, rate limits, and cascading failures during load spikes lead to:

- Lost revenue from missed payment notifications
- Data integrity issues from dropped inventory updates
- Poor user experience from failed status updates
- Compliance violations from untracked events

**Our Solution**: A managed service providing:

1. **Guaranteed Delivery** - At-least-once delivery with configurable exponential backoff
2. **Idempotency** - Built-in deduplication preventing duplicate processing
3. **Verifiable Integrity** - Cryptographic attestations via TigerBeetle's immutable log
4. **Full Observability** - Complete audit trail with structured tracing and metrics

## Architecture

### System Components

```
┌─────────────┐       ┌──────────────┐       ┌────────────┐
│   Webhook   │──────▶│ HTTP Receiver│──────▶│  Channel   │
│   Sources   │       │    (Axum)    │       │   (mpsc)   │
└─────────────┘       └──────────────┘       └────────────┘
                             │                      │
                             ▼                      ▼
                      ┌──────────────┐       ┌────────────┐
                      │   Signature  │       │ Dispatcher │
                      │  Validation  │       │   (Task)   │
                      └──────────────┘       └────────────┘
                                                    │
                                    ┌───────────────┴───────────────┐
                                    ▼                               ▼
                            ┌──────────────┐              ┌────────────────┐
                            │  PostgreSQL  │              │  TigerBeetle   │
                            │ (Operational)│              │  (Audit Log)   │
                            └──────────────┘              └────────────────┘
                                    │
                                    ▼
                            ┌──────────────┐
                            │ Worker Pool  │
                            │(Retry Logic) │
                            └──────────────┘
                                    │
                                    ▼
                            ┌──────────────┐
                            │  Destination │
                            │  Endpoints   │
                            └──────────────┘
```

### Data Flow

1. **Ingestion (<10ms response)**
   - Webhook received at unique URL
   - HMAC signature validated
   - Event pushed to bounded channel
   - 200 OK returned immediately

2. **Persistence (Two-Phase Model)**
   - **Phase 1**: Insert into PostgreSQL with `received` status (operational durability)
   - **Phase 2**: Append to TigerBeetle log (cryptographic audit trail)
   - Update PostgreSQL status to `pending` for delivery

3. **Delivery**
   - Workers claim events using `FOR UPDATE SKIP LOCKED`
   - Exponential backoff with jitter (1s, 2s, 4s, 8s, ...)
   - Circuit breaker per endpoint to prevent cascade failures
   - Update delivery status and metrics

## Design Philosophy

### Correctness by Construction

- **Type-driven design** - Illegal states are unrepresentable
- **Structured concurrency** - No orphaned tasks, graceful shutdown
- **Bounded resources** - Natural backpressure via bounded channels

### Performance Without Compromise

- **Zero-copy operations** - `Bytes` for payload handling
- **Lock-free async** - Message passing over shared state
- **Data-oriented design** - Hot/cold data separation

### Observable by Default

- **Structured logging** - Every event has a correlation ID
- **Distributed tracing** - Full request lifecycle visibility
- **Real-time metrics** - Prometheus-compatible instrumentation

## Reliability Guarantees

### At-Least-Once Delivery

We guarantee that every accepted webhook will be delivered at least once to its destination endpoint, or marked as permanently failed after exhausting all retry attempts. This is achieved through:

- Persistent retry state in PostgreSQL
- Idempotency keys to prevent duplicate processing
- Reconciliation loops for crash recovery

### Consistency Model

Our two-phase persistence ensures:

1. **No data loss** - PostgreSQL write before acknowledgment
2. **Audit integrity** - Eventual consistency with TigerBeetle
3. **Exactly-once processing** - Idempotency enforcement at ingestion

### Failure Modes

| Failure Type      | Detection                  | Response                                  |
| ----------------- | -------------------------- | ----------------------------------------- |
| Network partition | Connection timeout         | Exponential backoff with jitter           |
| Endpoint overload | 5xx responses              | Circuit breaker activation                |
| Malformed payload | Parse error                | Dead letter queue with diagnostics        |
| Database failure  | Connection pool exhaustion | Graceful degradation, in-memory buffering |
| System overload   | Channel saturation         | Backpressure, 503 to sources              |

## Security Model

### Defense in Depth

1. **HMAC-SHA256 validation** - Cryptographic proof of origin
2. **TLS everywhere** - Encrypted in transit
3. **Tenant isolation** - Row-level security in PostgreSQL
4. **Secrets management** - HSM integration for sensitive data

### Audit & Compliance

- **Immutable audit log** - TigerBeetle provides cryptographic proof
- **Signed attestations** - JWT tokens proving event receipt
- **Data retention** - Configurable per subscription tier
- **GDPR compliance** - Right to deletion, data portability

## Operational Excellence

### Observability Stack

- **Logs**: Structured JSON with correlation IDs (`tracing`)
- **Metrics**: RED method (Rate, Errors, Duration) via Prometheus
- **Traces**: OpenTelemetry for distributed tracing
- **Synthetic monitoring**: Continuous end-to-end verification

### Testing Strategy

1. **Unit tests** - Pure logic validation
2. **Integration tests** - Component interaction with real databases
3. **Property tests** - Invariant verification with `proptest`
4. **Chaos tests** - Deterministic failure injection
5. **Fuzz tests** - Security vulnerability discovery

### Production Readiness

- **Graceful shutdown** - Multi-phase connection draining
- **Zero-downtime deployment** - Blue-green with health checks
- **Automatic recovery** - Self-healing with supervisor trees
- **Resource limits** - CPU/memory quotas with monitoring

## Performance Targets

| Metric             | SLO              | Notes                  |
| ------------------ | ---------------- | ---------------------- |
| Ingestion latency  | p99 < 10ms       | Time to return 200 OK  |
| Delivery latency   | p50 < 100ms      | First delivery attempt |
| Throughput         | 10K webhooks/sec | Per instance           |
| Retry success rate | > 99.9%          | Within 24 hours        |
| Availability       | 99.95%           | Monthly uptime         |

## Technology Stack

- **Language**: Rust (performance, safety, correctness)
- **Web Framework**: Axum (tokio-native, type-safe)
- **Database**: PostgreSQL (ACID, battle-tested)
- **Audit Log**: TigerBeetle (cryptographic verification)
- **Observability**: OpenTelemetry + Prometheus
- **Testing**: proptest, cargo-fuzz, deterministic simulation

## Next Steps

- [Technical Specification](./SPECIFICATION.md) - Detailed requirements
- [Component Architecture](./architecture/COMPONENTS.md) - Deep dive into each component
- [Getting Started](./development/GETTING_STARTED.md) - Set up local development
