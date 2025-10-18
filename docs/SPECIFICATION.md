# Technical Specification

This document defines functional and non-functional requirements for Kapsel. Requirements use standard RFC 2119 keywords (MUST, SHOULD, MAY).

**Related Documents:**

- [Architecture](ARCHITECTURE.md) - System design and components
- [Current Status](STATUS.md) - Implementation progress
- [Testing Strategy](TESTING_STRATEGY.md) - Quality assurance

## Functional Requirements

### Core Capabilities

#### FR-1: Webhook Ingestion

- **FR-1.1**: MUST accept HTTP POST at `/ingest/:endpoint_id`
- **FR-1.2**: MUST support payloads up to 10MB
- **FR-1.3**: MUST preserve all request headers
- **FR-1.4**: MUST return 200 OK only after database commit
- **FR-1.5**: MUST support application/json, application/x-www-form-urlencoded, text/plain

#### FR-2: Idempotency

- **FR-2.1**: MUST detect duplicates via X-Idempotency-Key, content hash, or source event ID
- **FR-2.2**: MUST maintain 24-hour deduplication window
- **FR-2.3**: MUST return identical response for duplicates

#### FR-3: Signature Validation

- **FR-3.1**: MUST support HMAC-SHA256 validation (Stripe, GitHub, Shopify, generic)
- **FR-3.2**: MUST allow configurable signature header per endpoint
- **FR-3.3**: SHOULD support timestamp validation for replay protection

#### FR-4: Delivery

- **FR-4.1**: MUST deliver via HTTP POST to destination URL
- **FR-4.2**: MUST preserve original headers (configurable filtering)
- **FR-4.3**: MUST add X-Kapsel-Event-Id, X-Kapsel-Delivery-Attempt, X-Kapsel-Original-Timestamp
- **FR-4.4**: SHOULD support custom headers per endpoint

#### FR-5: Retry Logic

- **FR-5.1**: MUST use exponential backoff (1s, 2s, 4s, 8s, 16s, 32s, 64s, 128s, 256s, 512s) with ±25% jitter
- **FR-5.2**: MUST retry on network errors, 5xx responses, 429 with Retry-After
- **FR-5.3**: MUST NOT retry on 4xx responses (except 429) or validation failures
- **FR-5.4**: MUST support configurable max attempts per endpoint (default: 10)

#### FR-6: Circuit Breaker

- **FR-6.1**: MUST implement per-endpoint circuit breaker (Closed, Open, Half-Open states)
- **FR-6.2**: MUST open after 5 consecutive failures or 50% failure rate over 10 requests
- **FR-6.3**: MUST transition to half-open after 30s, close after 3 successes

#### FR-7: Cryptographic Attestation

- **FR-7.1**: MUST record all delivery attempts as Merkle tree leaves (RFC 6962 format)
- **FR-7.2**: MUST commit tree every 10s or 100 events with Ed25519-signed root
- **FR-7.3**: MUST generate inclusion proofs and consistency proofs on request
- **FR-7.4**: MUST provide public key for independent verification

## Non-Functional Requirements

### Performance

| Requirement                    | Target         | Measurement                |
| ------------------------------ | -------------- | -------------------------- |
| NFR-1.1: Ingestion latency     | p99 < 50ms     | Database write complete    |
| NFR-1.2: Delivery latency      | p50 < 100ms    | First attempt start        |
| NFR-1.3: Attestation batch     | p99 < 100ms    | Tree commit duration       |
| NFR-1.4: Proof generation      | p99 < 50ms     | API response time          |
| NFR-2.1: Throughput            | 10K events/sec | Per instance sustained     |
| NFR-2.2: Concurrent deliveries | 1K/instance    | Simultaneous HTTP requests |
| NFR-3.1: Memory                | < 2GB          | At 10K events/sec          |
| NFR-3.2: CPU                   | < 4 cores      | At 10K events/sec          |
| NFR-3.3: Database connections  | ≤ 100          | Per instance               |

### Reliability

- **NFR-4.1**: MUST achieve 99.95% monthly uptime (≤ 21.6 minutes downtime)
- **NFR-4.2**: MUST guarantee zero data loss for accepted webhooks
- **NFR-4.3**: MUST auto-recover from transient failures
- **NFR-5.1**: MUST support multi-zone PostgreSQL replication
- **NFR-5.2**: MUST support point-in-time recovery (7 days)
- **NFR-5.3**: MUST maintain immutable Merkle tree audit log
- **NFR-5.4**: SHOULD retain attestation records for 7 years

### Security

- **NFR-6.1**: MUST use TLS 1.3+ for all external connections
- **NFR-6.2**: MUST encrypt database storage at rest
- **NFR-6.3**: MUST store secrets in HSM/KMS (production)
- **NFR-7.1**: MUST enforce complete tenant isolation
- **NFR-7.2**: MUST implement per-tenant rate limiting
- **NFR-7.3**: SHOULD enforce resource quotas per tier

## Technical Constraints

| Constraint    | Requirement                               |
| ------------- | ----------------------------------------- |
| Language      | Rust 1.75+                                |
| Web framework | Axum 0.7+                                 |
| Database      | PostgreSQL 14+ with pgcrypto              |
| Cryptography  | Ed25519-dalek 2.1+, rs-merkle 1.5+        |
| Deployment    | Container-based, Kubernetes-ready         |
| Code safety   | 100% safe Rust, no panics in production   |
| Configuration | 12-factor app, environment variables only |

## Data Model

### Event Lifecycle

| State       | Description                    | Terminal |
| ----------- | ------------------------------ | -------- |
| Received    | Webhook accepted and persisted | No       |
| Pending     | Queued for delivery            | No       |
| Delivering  | Currently being delivered      | No       |
| Delivered   | Successfully delivered         | Yes      |
| Failed      | Exhausted all retries          | Yes      |
| Dead Letter | Manual intervention required   | Yes      |

### Storage Requirements

**Webhooks**: All accepted events with original payload and headers. Deduplication window: 24 hours.

**Delivery Attempts**: Complete history with timestamps, status codes, response data. Retention: 90 days minimum.

**Attestation**: Merkle leaves (append-only), signed tree heads, proof cache. Retention: 7 years.

**Configuration**: Endpoints, retry policies, circuit breaker state, signing secrets. Multi-tenant isolation enforced.

## Interface Requirements

### Webhook Ingestion API

**Endpoint Pattern**: `POST /ingest/{endpoint_id}`

**Requirements**:

- Accept any valid HTTP POST request
- Return 200 OK only after persistent storage
- Include unique event identifier in response
- Support standard webhook payload formats

### Management API

**Authentication**: Bearer token with tenant scoping

**Core Operations**:

- Endpoint lifecycle management (create, update, delete, list)
- Event status queries and filtering
- Delivery attempt inspection and retry triggering
- Configuration management and validation

### Webhook Delivery Interface

**Protocol**: HTTP POST to configured destination URLs

**Headers**: Preserve original headers plus delivery metadata

- `X-Kapsel-Event-Id`: Unique event identifier
- `X-Kapsel-Attempt`: Current attempt number
- `X-Kapsel-Timestamp`: Original ingestion time

**Behavior**: Follow HTTP semantics for success/failure determination

### Attestation API

#### GET /attestation/sth

Get latest Signed Tree Head.

Response:

```json
{
  "tree_size": 12345,
  "root_hash": "abc123...", // hex
  "timestamp": 1234567890,
  "signature": "def456...", // hex
  "public_key": "789abc..." // hex
}
```

#### GET /attestation/proof/{leaf_hash}

Get inclusion proof for delivery attempt.

Response:

```json
{
  "leaf_hash": "abc123...",
  "leaf_index": 5678,
  "tree_size": 12345,
  "proof_hashes": ["hash1", "hash2"],
  "root_hash": "root123..."
}
```

#### GET /attestation/download-proof/{delivery_attempt_id}

Download complete proof package for client verification.
}

````

#### POST /ingest/{endpoint_id}

Receive a webhook (no authentication required).

Headers:

- Any headers from source
- X-Idempotency-Key (optional)
- X-Webhook-Signature (if configured)

Response:

- 200 OK: Webhook accepted and persisted to PostgreSQL
- 400 Bad Request: Invalid payload or signature
- 413 Payload Too Large: Exceeds 10MB limit
- 429 Too Many Requests: Rate limit exceeded
- 503 Service Unavailable: System overloaded

#### GET /v1/events/{event_id}

Retrieve event details.

Response:

```json
{
  "id": "evt_xyz789",
  "endpoint_id": "ep_abc123",
  "status": "delivered",
  "received_at": "2024-01-01T00:00:00Z",
  "delivered_at": "2024-01-01T00:00:01Z",
  "attempts": [
    {
      "attempt_number": 1,
      "attempted_at": "2024-01-01T00:00:00.100Z",
      "response_status": 200,
      "duration_ms": 150
    }
  ]
}
````

## Error Taxonomy

### Application Errors

| Code  | Name              | Description              | Retry |
| ----- | ----------------- | ------------------------ | ----- |
| E1001 | INVALID_SIGNATURE | HMAC validation failed   | No    |
| E1002 | PAYLOAD_TOO_LARGE | Exceeds 10MB limit       | No    |
| E1003 | INVALID_ENDPOINT  | Endpoint not found       | No    |
| E1004 | RATE_LIMITED      | Tenant quota exceeded    | Yes   |
| E1005 | DUPLICATE_EVENT   | Idempotency check failed | No    |

### Delivery Errors

| Code  | Name               | Description               | Retry |
| ----- | ------------------ | ------------------------- | ----- |
| E2001 | CONNECTION_REFUSED | Target unavailable        | Yes   |
| E2002 | CONNECTION_TIMEOUT | Exceeded timeout          | Yes   |
| E2003 | HTTP_CLIENT_ERROR  | 4xx response              | No    |
| E2004 | HTTP_SERVER_ERROR  | 5xx response              | Yes   |
| E2005 | CIRCUIT_OPEN       | Circuit breaker triggered | Yes   |

### System Errors

| Code  | Name                    | Description                  | Retry |
| ----- | ----------------------- | ---------------------------- | ----- |
| E3001 | DATABASE_UNAVAILABLE    | PostgreSQL connection failed | Yes   |
| E3002 | TIGERBEETLE_UNAVAILABLE | Audit log unreachable        | Yes   |
| E3003 | QUEUE_FULL              | Channel at capacity          | Yes   |
| E3004 | WORKER_POOL_EXHAUSTED   | No available workers         | Yes   |

## Testing Requirements

### Unit Test Coverage

- Minimum 80% line coverage
- 100% coverage for critical paths (retry logic, idempotency, attestation)
- All error conditions tested
- Cryptographic operations fully tested

### Integration Tests

- Database transaction rollback scenarios
- Network failure simulation
- Concurrent request handling
- Circuit breaker state transitions
- Merkle tree construction and verification
- Ed25519 signature generation and validation

### Chaos Tests

- Random network failures
- Database connection drops
- Worker crashes during delivery
- Clock jumps for timeout testing

### Performance Tests

- Load test: 10K webhooks/sec sustained for 1 hour
- Stress test: Increase load until failure, measure breaking point
- Soak test: 1K webhooks/sec for 24 hours

## Compliance Requirements

### Data Privacy

- GDPR Article 17: Right to erasure implementation
- GDPR Article 20: Data portability via API
- CCPA compliance for California users

### Audit Trail

- Immutable Merkle tree log for 7 years
- Cryptographic proof of receipt via Ed25519 signatures
- Chain of custody documentation with verifiable proofs
- Client-side verification tools for independent validation

### Security Standards

- OWASP Top 10 mitigation
- SOC 2 Type II preparation
- Regular penetration testing

## Operational Requirements

### Monitoring

- Prometheus metrics endpoint at /metrics
- OpenTelemetry trace export
- Structured JSON logging to stdout

### Health Checks

- /health/live: Kubernetes liveness probe
- /health/ready: Kubernetes readiness probe
- /health/startup: Initialization complete check

### Graceful Shutdown

1. Stop accepting new connections
2. Drain in-flight requests (30s timeout)
3. Flush channel to database
4. Wait for active workers
5. Close database connections
6. Exit with code 0

### Configuration

All configuration via environment variables:

- DATABASE_URL: PostgreSQL connection string
- TIGERBEETLE_ADDRESSES: Cluster addresses
- WORKER_POOL_SIZE: Number of delivery workers
- CHANNEL_CAPACITY: Bounded channel size
- LOG_LEVEL: trace|debug|info|warn|error
