# System Overview

Kapsel is a webhook reliability service foundation for building guaranteed at-least-once delivery systems. Currently operational as a webhook ingestion service with persistence and idempotency, with the delivery engine under active development.

## Core Value Proposition

**The Problem**: Webhook failures are silent killers. Network timeouts, 5xx errors, rate limits, and cascading failures during load spikes lead to:

- Lost revenue from missed payment notifications
- Data integrity issues from dropped inventory updates
- Poor user experience from failed status updates
- Compliance violations from untracked events

**Our Solution**: A service foundation providing:

1. **Webhook Ingestion** - Reliable HTTP endpoint with HMAC validation (✅ Complete)
2. **Idempotency** - Built-in deduplication with 24-hour window (✅ Complete)
3. **Guaranteed Delivery** - At-least-once delivery with retry logic (🚧 In Development)
4. **Observability** - Structured logging and request tracing (✅ Complete)
5. **Audit Trail** - TigerBeetle integration for cryptographic verification (📋 Planned)

## Architecture

### System Components

```
IMPLEMENTED:
┌─────────────┐       ┌──────────────┐
│   Webhook   │──────▶│ HTTP Receiver│──────▶ PostgreSQL
│   Sources   │       │    (Axum)    │
└─────────────┘       └──────────────┘
                             │
                             ▼
                      ┌──────────────┐
                      │   Signature  │
                      │  Validation  │
                      └──────────────┘

IN DEVELOPMENT:
                      ┌──────────────┐
                      │ Worker Pool  │
                      │  (Claiming)  │
                      └──────────────┘
                             │
                             ▼
                      ┌──────────────┐
                      │   Delivery   │
                      │    Client    │
                      └──────────────┘

PLANNED:
                      ┌──────────────┐              ┌────────────────┐
                      │ Retry Logic  │              │  TigerBeetle   │
                      │  & Backoff   │              │  (Audit Log)   │
                      └──────────────┘              └────────────────┘
                             │
                             ▼
                      ┌──────────────┐
                      │   Circuit    │
                      │   Breakers   │
                      └──────────────┘
```

### Data Flow

#### Currently Implemented:

1. **Ingestion** ✅
   - Webhook received at `/ingest/:endpoint_id`
   - HMAC signature validated (optional)
   - Event persisted to PostgreSQL
   - 200 OK with event ID returned

2. **Persistence** ✅
   - Insert into PostgreSQL with proper idempotency checking
   - Duplicate detection via `ON CONFLICT` handling
   - Status tracking through lifecycle

#### In Development:

3. **Delivery** 🚧
   - Workers can claim events using `FOR UPDATE SKIP LOCKED` ✅
   - HTTP delivery client (not implemented)
   - Retry logic with exponential backoff (planned)
   - Circuit breaker integration (types defined, not integrated)
   - Delivery attempt tracking (schema exists, logic pending)

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

### At-Least-Once Delivery (Target Design)

Once complete, we will guarantee that every accepted webhook will be delivered at least once to its destination endpoint, or marked as permanently failed after exhausting all retry attempts. This will be achieved through:

- Persistent retry state in PostgreSQL (✅ Schema ready)
- Idempotency keys to prevent duplicate processing (✅ Implemented)
- Worker pool with claim-based processing (✅ Claiming works)
- Exponential backoff retry logic (🚧 Not implemented)
- Reconciliation loops for crash recovery (📋 Planned)

### Consistency Model

Current implementation ensures:

1. **No data loss** - PostgreSQL write before acknowledgment ✅
2. **Exactly-once ingestion** - Idempotency enforcement via unique constraints ✅
3. **Audit trail** - Complete event history in PostgreSQL ✅

Future additions:

- TigerBeetle integration for cryptographic audit integrity
- Distributed tracing correlation across systems

### Failure Modes

| Failure Type      | Detection                  | Response                                  |
| ----------------- | -------------------------- | ----------------------------------------- |
| Network partition | Connection timeout         | Exponential backoff with jitter           |
| Endpoint overload | 5xx responses              | Circuit breaker activation                |
| Malformed payload | Parse error                | Dead letter queue with diagnostics        |
| Database failure  | Connection pool exhaustion | Graceful degradation, in-memory buffering |
| System overload   | Channel saturation         | Backpressure, 503 to sources              |

## Security Model

### Current Security Implementation

1. **HMAC-SHA256 validation** - Optional signature verification ✅
2. **Input validation** - 10MB payload limit, type checking ✅
3. **SQL injection prevention** - Parameterized queries throughout ✅
4. **No secrets in logs** - Sensitive data excluded from tracing ✅

### Planned Security Enhancements

- **TLS termination** - HTTPS in production deployments
- **Tenant isolation** - Row-level security in PostgreSQL
- **Secrets management** - Environment-based configuration
- **Audit log** - TigerBeetle integration for cryptographic proof
- **Rate limiting** - Per-tenant throttling

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

| Metric             | Target SLO       | Current Status        |
| ------------------ | ---------------- | --------------------- |
| Ingestion latency  | p99 < 10ms       | Not measured          |
| Delivery latency   | p50 < 100ms      | Not implemented       |
| Throughput         | 10K webhooks/sec | Not benchmarked       |
| Retry success rate | > 99.9%          | Delivery not complete |
| Availability       | 99.95%           | No production metrics |

Note: These are design targets. Benchmarks will be implemented once core features are complete.

## Technology Stack

### Currently Integrated:

- **Language**: Rust (performance, safety, correctness) ✅
- **Web Framework**: Axum (tokio-native, type-safe) ✅
- **Database**: PostgreSQL (ACID, battle-tested) ✅
- **Testing**: proptest, integration tests, test harness ✅
- **Logging**: tracing with structured output ✅

### Planned Integrations:

- **Audit Log**: TigerBeetle (cryptographic verification)
- **Metrics**: Prometheus exposition
- **Tracing**: OpenTelemetry
- **Fuzzing**: cargo-fuzz for security testing

## Next Steps

- [Technical Specification](./SPECIFICATION.md) - Detailed requirements
- [Component Architecture](./architecture/COMPONENTS.md) - Deep dive into each component
- [Getting Started](./development/GETTING_STARTED.md) - Set up local development
