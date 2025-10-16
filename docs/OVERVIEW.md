# System Overview

Kapsel is a webhook reliability service providing guaranteed at-least-once delivery with cryptographically verifiable audit trails. We ensure webhooks never get lost while providing immutable proof of delivery for compliance and dispute resolution.

## Core Value Proposition

**The Problem**: Webhook failures are silent killers. Network timeouts, 5xx errors, rate limits, and cascading failures during load spikes lead to:

- Lost revenue from missed payment notifications
- Data integrity issues from dropped inventory updates
- Poor user experience from failed status updates
- Compliance violations from untracked events
- Disputes without proof of delivery attempts

**Our Solution**: The definitive webhook reliability service providing:

1. **Guaranteed Acceptance** - Reliable HTTP ingestion with cryptographic validation
2. **Zero Duplication** - Built-in deduplication preventing duplicate processing
3. **Guaranteed Delivery** - At-least-once delivery with intelligent retry logic
4. **Cryptographic Attestation** - Merkle tree-based proof of delivery with Ed25519 signatures
5. **Complete Observability** - Full request lifecycle visibility and debugging

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

### Data Flow

#### Phase 1: Guaranteed Acceptance

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

#### Phase 2: Guaranteed Delivery

3. **Reliability Engine**
   - Worker pool claiming events for distributed processing
   - HTTP delivery client with timeout and error handling
   - Exponential backoff retry logic with jitter
   - Circuit breaker protection against cascade failures
   - Complete delivery attempt audit trail

#### Phase 3: Cryptographic Attestation

4. **Merkle Tree Attestation**
   - Every delivery attempt becomes a Merkle tree leaf
   - Periodic commitment into append-only log (every 10s or 100 events)
   - Ed25519 signed tree roots providing non-repudiation
   - Inclusion proofs for verifying specific deliveries
   - Consistency proofs ensuring append-only property

## Design Philosophy

### Correctness by Construction

- **Type-driven design** - Illegal states are unrepresentable
- **Structured concurrency** - No orphaned tasks, graceful shutdown
- **Bounded resources** - Natural backpressure via bounded channels
- **Cryptographic integrity** - Tamper-proof audit trails

### Performance Without Compromise

- **Zero-copy operations** - `Bytes` for payload handling
- **Lock-free async
  ** - Message passing over shared state
- **Data-oriented design** - Hot/cold data separation
- **Batch attestation** - Amortized cryptographic operations

### Observable by Default

- **Structured logging** - Every event has a correlation ID
- **Distributed tracing** - Full request lifecycle visibility
- **Real-time metrics** - Prometheus-compatible instrumentation
- **Verifiable proof** - Client-side verification tools

## Reliability Guarantees

### At-Least-Once Delivery

We guarantee that every accepted webhook will be delivered at least once to its destination endpoint, or marked as permanently failed after exhausting all retry attempts. This is achieved through:

- Persistent retry state in PostgreSQL
- Idempotency keys to prevent duplicate processing
- Worker pool with claim-based processing
- Exponential backoff retry logic
- Reconciliation loops for crash recovery

### Cryptographic Attestation

Every delivery attempt is cryptographically attested using:

- **Merkle Tree Inclusion** - Proof that delivery attempt exists in log
- **Ed25519 Signatures** - Service authenticity and non-repudiation
- **Append-Only Log** - Immutable history of all delivery attempts
- **Client Verification** - Independent proof validation without trusting Kapsel

### Consistency Model

Our reliability guarantees build on proven patterns:

1. **No data loss** - PostgreSQL ACID compliance with write-before-acknowledge
2. **Exactly-once processing** - Database-enforced idempotency preventing duplicates
3. **Complete audit trail** - Full event lifecycle tracking with delivery attempts
4. **Cryptographic proof** - Merkle tree attestation for immutable delivery records
5. **End-to-end tracing** - Request correlation from ingestion through delivery

### Failure Modes

| Failure Type      | Detection                  | Response                                  | Attestation                    |
| ----------------- | -------------------------- | ----------------------------------------- | ------------------------------ |
| Network partition | Connection timeout         | Exponential backoff with jitter           | Failed attempt recorded        |
| Endpoint overload | 5xx responses              | Circuit breaker activation                | Response status in leaf        |
| Malformed payload | Parse error                | Dead letter queue with diagnostics        | Error details in attestation   |
| Database failure  | Connection pool exhaustion | Graceful degradation, in-memory buffering | Batch commit on recovery       |
| System overload   | Channel saturation         | Backpressure, 503 to sources              | Delayed but guaranteed logging |

## Security Model

### Security Architecture

1. **Cryptographic Validation** - HMAC-SHA256 preventing webhook spoofing
2. **Input Validation** - Comprehensive payload and header validation
3. **SQL Injection Prevention** - Parameterized queries throughout
4. **Sensitive Data Protection** - No secrets in logs or error responses
5. **TLS Everywhere** - Encrypted communication in production
6. **Tenant Isolation** - Row-level security preventing data leakage
7. **Audit Integrity** - Merkle tree cryptographic delivery proof
8. **Non-Repudiation** - Ed25519 signed tree heads
9. **Rate Limiting** - Per-tenant throttling preventing resource exhaustion

### Attestation Security

- **Key Management** - Ed25519 keys stored securely (HSM/KMS in production)
- **Signature Verification** - Public key available for independent verification
- **Tamper Evidence** - Merkle tree structure reveals any modification attempts
- **Append-Only Guarantee** - Consistency proofs between successive tree heads

## Operational Excellence

### Observability Stack

- **Logs**: Structured JSON with correlation IDs (`tracing`)
- **Metrics**: RED method (Rate, Errors, Duration) via Prometheus
- **Traces**: OpenTelemetry for distributed tracing
- **Proofs**: Downloadable attestation packages for client verification

### Testing Strategy

1. **Unit tests** - Pure logic validation including crypto operations
2. **Integration tests** - Component interaction with real databases
3. **Property tests** - Invariant verification with `proptest`
4. **Chaos tests** - Deterministic failure injection
5. **Attestation tests** - Merkle proof generation and verification

### Production Readiness

- **Graceful shutdown** - Multi-phase connection draining with attestation flush
- **Zero-downtime deployment** - Blue-green with health checks
- **Automatic recovery** - Self-healing with supervisor trees
- **Resource limits** - CPU/memory quotas with monitoring
- **Proof availability** - 99.9% SLA for attestation proof generation

## Performance Targets

| Metric             | Target SLO       | Design Basis                                  |
| ------------------ | ---------------- | --------------------------------------------- |
| Ingestion latency  | p99 < 10ms       | PostgreSQL write performance                  |
| Delivery latency   | p50 < 100ms      | HTTP client + retry logic                     |
| Attestation batch  | < 100ms          | Batch processing every 10s or 100 events      |
| Proof generation   | p99 < 50ms       | In-memory Merkle tree with caching            |
| STH commitment     | < 500ms          | Ed25519 signing + database write              |
| Throughput         | 10K webhooks/sec | Connection pool + async workers               |
| Retry success rate | > 99.9%          | Exponential backoff + circuit breakers        |
| Proof verification | < 10ms           | Client-side Ed25519 + Merkle path validation  |
| Availability       | 99.95%           | PostgreSQL reliability + graceful degradation |

These targets reflect production webhook reliability service requirements with cryptographic attestation overhead.

## Technology Stack

### Core Technology Stack

- **Language**: Rust for memory safety and zero-cost abstractions
- **Web Framework**: Axum providing type-safe async HTTP handling
- **Database**: PostgreSQL for ACID compliance and advanced concurrency
- **Cryptography**: Ed25519-dalek for signatures, rs-merkle for tree operations
- **Testing**: Property-based testing with deterministic simulation
- **Observability**: Structured logging with distributed tracing
- **Metrics**: Prometheus for reliability monitoring and alerting
- **Security**: Comprehensive fuzzing and property-based security testing

### Attestation Stack

- **Merkle Trees**: rs-merkle with SHA-256 (RFC 6962 compatible)
- **Signatures**: Ed25519 for speed and security
- **Proof Format**: JSON with hex-encoded hashes for portability
- **Verification**: Standalone Rust library and CLI tool

Every technology choice prioritizes webhook delivery reliability with verifiable proof.

## Compliance Benefits

### Verifiable Delivery Receipts

The Merkle tree attestation provides legally defensible proof of:

- **What** was delivered (payload hash)
- **When** it was delivered (timestamp)
- **Where** it was delivered (endpoint URL)
- **Result** of delivery (status code, response)
- **Authenticity** (Ed25519 signature from Kapsel)

### Use Cases

1. **Financial Services** - Prove notification delivery for regulatory compliance
2. **Healthcare** - HIPAA audit trails with tamper-proof records
3. **Legal Tech** - Dispute resolution with cryptographic proof
4. **E-commerce** - Verify order confirmations were sent
5. **Government** - Immutable records for FOIA compliance

## Next Steps

- [Technical Specification](./SPECIFICATION.md) - Detailed requirements including attestation
- [Attestation Architecture](./ATTESTATION.md) - Deep dive into Merkle tree implementation
- [Getting Started](../DEVELOPMENT.md) - Set up local development
- [Implementation Status](./IMPLEMENTATION_STATUS.md) - Current development progress
