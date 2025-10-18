# System Architecture

Kapsel provides webhook reliability through guaranteed at-least-once delivery with cryptographic attestation. This document describes the system design, components, and operational characteristics.

## Problem Statement

Webhook failures cause silent data loss. Network timeouts, server errors, and rate limits lead to missed events without visibility. Traditional solutions retry delivery but provide no proof that delivery occurred.

Kapsel solves this by combining persistent delivery guarantees with cryptographic proof through Merkle tree attestation and Ed25519 signatures.

## Core Components

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
                    ┌────────┴────────┐
                    ▼                 ▼
            ┌─────────────┐    ┌─────────────┐
            │ Destination │    │   Merkle    │
            │  Endpoints  │    │ Attestation │
            └─────────────┘    └─────────────┘
                                      │
                                      ▼
                               ┌─────────────┐
                               │ Signed Tree │
                               │ Heads (STH) │
                               └─────────────┘
```

## Data Flow

### Phase 1: Ingestion

Webhooks arrive at `/ingest/:endpoint_id` and are immediately persisted to PostgreSQL. The service returns 200 OK only after database commit, guaranteeing zero data loss. HMAC signature validation prevents spoofed webhooks.

Database-enforced idempotency detects duplicates using configurable strategies:

- Customer-provided key (X-Idempotency-Key header)
- Content hash (SHA256 of payload)
- Source event ID (JSONPath extraction)

### Phase 2: Delivery

Worker pool claims pending events using PostgreSQL `FOR UPDATE SKIP LOCKED`, enabling lock-free work distribution across multiple instances. HTTP delivery client handles timeouts and errors.

Exponential backoff with jitter schedules retries: 1s, 2s, 4s, 8s, 16s, 32s, 64s, 128s, 256s, 512s. Maximum attempts configurable per endpoint.

Circuit breakers protect against cascade failures with three states:

- **Closed**: Normal operation
- **Open**: Fast-fail after threshold (5 consecutive failures or 50% over 10 requests)
- **Half-Open**: Limited probes after 30s timeout

### Phase 3: Attestation

Every delivery attempt becomes a Merkle tree leaf containing:

- Delivery attempt ID
- Endpoint URL
- Payload hash (SHA-256)
- Timestamp
- HTTP status code
- Response hash

Batch processing commits leaves every 10 seconds or 100 events. Ed25519 signature on Merkle root provides cryptographic proof. Clients can independently verify delivery using inclusion proofs without trusting Kapsel.

## Technology Stack

| Component     | Technology                         | Rationale                              |
| ------------- | ---------------------------------- | -------------------------------------- |
| Language      | Rust 1.75+                         | Memory safety, zero-cost abstractions  |
| Web Framework | Axum 0.7+                          | Type-safe async HTTP                   |
| Database      | PostgreSQL 14+                     | ACID compliance, SKIP LOCKED           |
| Async Runtime | Tokio 1.35+                        | Production-grade async                 |
| Cryptography  | Ed25519-dalek 2.1+, rs-merkle 1.5+ | Fast signatures, RFC 6962 Merkle trees |
| Testing       | proptest, testcontainers           | Property-based + integration testing   |

## Performance Characteristics

| Metric                  | Target         | Design Basis                                  |
| ----------------------- | -------------- | --------------------------------------------- |
| Ingestion latency (p99) | < 50ms         | PostgreSQL write performance                  |
| Delivery latency (p50)  | < 100ms        | HTTP client + retry logic                     |
| Throughput              | 10K events/sec | Connection pool + async workers               |
| Attestation batch       | < 100ms        | rs-merkle batch processing                    |
| Proof generation (p99)  | < 50ms         | In-memory tree + caching                      |
| Availability            | 99.95%         | PostgreSQL reliability + graceful degradation |

## Reliability Guarantees

### At-Least-Once Delivery

Every accepted webhook will be delivered at least once or marked permanently failed after exhausting retries. Guaranteed through:

- PostgreSQL ACID compliance
- Worker pool with claim-based processing
- Exponential backoff retry logic
- Reconciliation loops for crash recovery

### Exactly-Once Processing

Database-enforced idempotency prevents duplicate processing even when sources retry. Deduplication window: 24 hours minimum.

### Cryptographic Attestation

Merkle tree with Ed25519 signatures provides tamper-proof proof of delivery:

- **Inclusion proofs** verify specific delivery attempts exist in log
- **Consistency proofs** verify append-only property (no history modification)
- **Client verification** enables independent validation without trusting Kapsel

## Failure Modes

| Failure           | Detection                  | Response                     | Attestation              |
| ----------------- | -------------------------- | ---------------------------- | ------------------------ |
| Network partition | Connection timeout         | Exponential backoff + jitter | Failed attempt recorded  |
| Endpoint overload | 5xx responses              | Circuit breaker activation   | Status in Merkle leaf    |
| Malformed payload | Parse error                | Dead letter queue            | Error in attestation     |
| Database failure  | Connection pool exhaustion | Graceful degradation         | Batch commit on recovery |
| System overload   | Channel saturation         | Backpressure, 503 to sources | Delayed but guaranteed   |

## Security Model

**Input Validation**

- HMAC-SHA256 signature validation
- Payload size limits (10MB)
- Header sanitization
- SQL injection prevention (parameterized queries)

**Data Protection**

- TLS 1.3+ for external communication
- Secrets excluded from logs and errors
- Row-level tenant isolation
- Merkle tree tamper evidence

**Cryptographic Integrity**

- Ed25519 key management (HSM/KMS in production)
- Public key distribution for verification
- Append-only log guarantees
- Non-repudiation through asymmetric signatures

## Observability

**Structured Logging**

- JSON format with correlation IDs
- Request/response tracing
- Error context preservation
- Configurable log levels

**Metrics** (Prometheus)

- RED method (Rate, Errors, Duration)
- Database connection pool utilization
- Circuit breaker state transitions
- Attestation batch sizes

**Distributed Tracing**

- OpenTelemetry integration
- Full request lifecycle visibility
- Cross-service correlation

## Operational Design

**Graceful Shutdown**

1. Stop accepting new webhooks
2. Drain in-flight deliveries (30s timeout)
3. Flush attestation batches
4. Close database connections
5. Exit with status 0

**Health Checks**

- `/health/live`: Process alive (Kubernetes liveness)
- `/health/ready`: Database connected (Kubernetes readiness)
- `/health/startup`: Initialization complete

**Configuration**
All configuration via environment variables:

- `DATABASE_URL`: PostgreSQL connection string
- `WORKER_POOL_SIZE`: Concurrent delivery workers
- `CHANNEL_CAPACITY`: Bounded channel size
- `LOG_LEVEL`: trace|debug|info|warn|error

## Deployment Constraints

**Stateless Application Tier**

- No local state (all in PostgreSQL)
- Horizontal scaling support
- Rolling deployments safe

**Database Requirements**

- PostgreSQL 14+ with pgcrypto
- Connection pooling (recommended: PgBouncer)
- Multi-zone replication for HA
- Point-in-time recovery enabled

**Resource Limits**

- Memory: < 2GB at 10K events/sec
- CPU: < 4 cores at 10K events/sec
- Database connections: ≤ 100 per instance

## Related Documentation

- [Technical Specification](SPECIFICATION.md) - Detailed requirements
- [Attestation Architecture](ATTESTATION.md) - Cryptographic proof system
- [Testing Strategy](TESTING_STRATEGY.md) - Quality assurance approach
- [Current Status](STATUS.md) - Implementation progress
