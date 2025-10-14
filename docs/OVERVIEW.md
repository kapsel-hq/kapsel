# System Overview

Kapsel is a webhook reliability service foundation for building guaranteed at-least-once delivery systems. Currently operational as a webhook ingestion service with persistence and idempotency, with the delivery engine under active development.

## Core Value Proposition

**The Problem**: Webhook failures are silent killers. Network timeouts, 5xx errors, rate limits, and cascading failures during load spikes lead to:

- Lost revenue from missed payment notifications
- Data integrity issues from dropped inventory updates
- Poor user experience from failed status updates
- Compliance violations from untracked events

**Our Solution**: The definitive webhook reliability service providing:

1. **Guaranteed Acceptance** - Reliable HTTP ingestion with cryptographic validation
2. **Zero Duplication** - Built-in deduplication preventing duplicate processing
3. **Guaranteed Delivery** - At-least-once delivery with intelligent retry logic
4. **Complete Observability** - Full request lifecycle visibility and debugging
5. **Cryptographic Audit** - Immutable delivery proof for compliance and disputes

## Architecture

### System Components

```
┌─────────────┐       ┌───────────────┐       ┌────────────┐
│   Webhook   │──────▶│ HTTP Receiver │──────▶│ PostgreSQL │
│   Sources   │       │  + Validator  │       │ Durability │
└─────────────┘       └───────────────┘       └────────────┘
                             │                      │
                             ▼                      ▼
                      ┌───────────────┐       ┌────────────┐
                      │  Worker Pool  │◄──────│   Event    │
                      │ Distribution  │       │   Queue    │
                      └───────────────┘       └────────────┘
                             │
                             ▼
                      ┌───────────────┐       ┌────────────┐
                      │ HTTP Delivery │◄──────│  Circuit   │
                      │ + Retry Logic │       │  Breakers  │
                      └───────────────┘       └────────────┘
                             │
                             ▼
                      ┌───────────────┐       ┌─────────────┐
                      │  Destination  │       │ TigerBeetle │ <- Not yet implemented
                      │  Endpoints    │       │ Audit Trail │
                      └───────────────┘       └─────────────┘
```

### Data Flow

#### Phase 1: Guaranteed Acceptance (Complete)

1. **Ingestion Pipeline**
   - Webhook received at `/ingest/:endpoint_id` with immediate persistence
   - HMAC signature validation preventing spoofed webhooks
   - PostgreSQL ACID compliance ensuring zero data loss
   - Structured response enabling proper error handling

2. **Durability Foundation**
   - Database-first durability with connection pooling
   - Idempotency enforcement preventing duplicate processing
   - Event lifecycle tracking from ingestion to delivery
   - Lock-free work distribution using PostgreSQL SKIP LOCKED

#### Phase 2: Guaranteed Delivery (Implementation Required)

3. **Reliability Engine**
   - Worker pool claiming events for distributed processing
   - HTTP delivery client with timeout and error handling
   - Exponential backoff retry logic with jitter
   - Circuit breaker protection against cascade failures
   - Complete delivery attempt audit trail

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

Our reliability guarantees build on proven patterns:

1. **No data loss** - PostgreSQL ACID compliance with write-before-acknowledge
2. **Exactly-once processing** - Database-enforced idempotency preventing duplicates
3. **Complete audit trail** - Full event lifecycle tracking with delivery attempts
4. **Cryptographic proof** - TigerBeetle integration for immutable delivery records
5. **End-to-end tracing** - Request correlation from ingestion through delivery

### Failure Modes

| Failure Type      | Detection                  | Response                                  |
| ----------------- | -------------------------- | ----------------------------------------- |
| Network partition | Connection timeout         | Exponential backoff with jitter           |
| Endpoint overload | 5xx responses              | Circuit breaker activation                |
| Malformed payload | Parse error                | Dead letter queue with diagnostics        |
| Database failure  | Connection pool exhaustion | Graceful degradation, in-memory buffering |
| System overload   | Channel saturation         | Backpressure, 503 to sources              |

## Security Model

### Security Architecture

1. **Cryptographic Validation** - HMAC-SHA256 preventing webhook spoofing
2. **Input Validation** - Comprehensive payload and header validation
3. **SQL Injection Prevention** - Parameterized queries throughout
4. **Sensitive Data Protection** - No secrets in logs or error responses
5. **TLS Everywhere** - Encrypted communication in production
6. **Tenant Isolation** - Row-level security preventing data leakage
7. **Audit Integrity** - TigerBeetle cryptographic delivery proof
8. **Rate Limiting** - Per-tenant throttling preventing resource exhaustion

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

| Metric             | Target SLO       | Design Basis                                  |
| ------------------ | ---------------- | --------------------------------------------- |
| Ingestion latency  | p99 < 10ms       | PostgreSQL write performance                  |
| Delivery latency   | p50 < 100ms      | HTTP client + retry logic                     |
| Throughput         | 10K webhooks/sec | Connection pool + async workers               |
| Retry success rate | > 99.9%          | Exponential backoff + circuit breakers        |
| Availability       | 99.95%           | PostgreSQL reliability + graceful degradation |

These targets reflect webhook reliability service requirements. Architecture designed to exceed these thresholds with room for optimization based on production telemetry.

## Technology Stack

### Core Technology Stack

- **Language**: Rust for memory safety and zero-cost abstractions
- **Web Framework**: Axum providing type-safe async HTTP handling
- **Database**: PostgreSQL for ACID compliance and advanced concurrency
- **Testing**: Property-based testing with deterministic simulation
- **Observability**: Structured logging with distributed tracing
- **Audit**: TigerBeetle for cryptographically verifiable delivery records
- **Metrics**: Prometheus for reliability monitoring and alerting
- **Security**: Comprehensive fuzzing and property-based security testing

Every technology choice prioritizes webhook delivery reliability and operational excellence.

## Next Steps

- [Technical Specification](./SPECIFICATION.md) - Detailed requirements
- [Component Architecture](./architecture/COMPONENTS.md) - Deep dive into each component
- [Getting Started](./development/GETTING_STARTED.md) - Set up local development
