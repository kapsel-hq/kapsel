# Technical Specification

This document defines the functional and non-functional requirements for Kapsel's webhook reliability service. These requirements guide all implementation decisions while leaving architectural flexibility.

**Related Documents:**

- [System Overview](OVERVIEW.md) - Architecture and design philosophy
- [Implementation Status](IMPLEMENTATION_STATUS.md) - Current development status
- [Testing Strategy](TESTING_STRATEGY.md) - Quality assurance approach

## Functional Requirements

### Core Capabilities

#### FR-1: Webhook Ingestion

- **FR-1.1**: Accept HTTP POST requests at unique, per-endpoint URLs
- **FR-1.2**: Support payloads up to 10MB in size
- **FR-1.3**: Preserve all headers from the original webhook
- **FR-1.4**: Return 200 OK only after PostgreSQL commit (p99 < 50ms)
- **FR-1.5**: Support Content-Type: application/json, application/x-www-form-urlencoded, text/plain

#### FR-2: Idempotency

- **FR-2.1**: Detect duplicate webhooks using configurable strategy:
  - Customer-provided key (X-Idempotency-Key header)
  - Content hash (SHA256 of normalized payload)
  - Source event ID (extracted from payload via JSONPath)
- **FR-2.2**: Deduplication window of 24 hours minimum
- **FR-2.3**: Return identical response for duplicate requests

#### FR-3: Signature Validation

- **FR-3.1**: HMAC-SHA256 validation for supported providers:
  - Stripe (stripe-signature header)
  - GitHub (X-Hub-Signature-256)
  - Shopify (X-Shopify-Hmac-Sha256)
  - Generic (X-Webhook-Signature)
- **FR-3.2**: Configurable signature header per endpoint
- **FR-3.3**: Optional timestamp validation to prevent replay attacks

#### FR-4: Delivery

- **FR-4.1**: HTTP POST delivery to configured destination URL
- **FR-4.2**: Preserve original headers (with configurable filtering)
- **FR-4.3**: Add delivery metadata headers:
  - X-Kapsel-Event-Id: unique event identifier
  - X-Kapsel-Delivery-Attempt: attempt number
  - X-Kapsel-Original-Timestamp: ISO8601 ingestion time
- **FR-4.4**: Support custom headers per endpoint

#### FR-5: Retry Logic

- **FR-5.1**: Exponential backoff with jitter:
  - Base: 1s, 2s, 4s, 8s, 16s, 32s, 64s, 128s, 256s, 512s
  - Jitter: ±25% randomization
  - Max attempts: 10 (configurable per endpoint)
- **FR-5.2**: Retry on:
  - Network errors (connection refused, timeout)
  - HTTP 5xx responses
  - HTTP 429 (rate limit) with Retry-After header support
- **FR-5.3**: Do not retry on:
  - HTTP 4xx responses (except 429)
  - Payload validation failures

#### FR-6: Circuit Breaker

- **FR-6.1**: Per-endpoint circuit breaker with three states:
  - Closed: Normal operation
  - Open: All requests fail immediately
  - Half-Open: Limited requests to test recovery
- **FR-6.2**: Thresholds:
  - Open after 5 consecutive failures or 50% failure rate over 10 requests
  - Half-open after 30 seconds
  - Close after 3 consecutive successes in half-open state

#### FR-7: Cryptographic Attestation

- **FR-7.1**: Merkle tree-based attestation for all delivery attempts:
  - Every delivery attempt becomes a Merkle tree leaf
  - Leaf contains: delivery_attempt_id, endpoint_url, payload_hash, timestamp, status
  - SHA-256 hashing with 0x00 prefix (RFC 6962 compliance)
- **FR-7.2**: Periodic tree commitment:
  - Batch events every 10 seconds or 100 events (configurable)
  - Compute new Merkle root using rs-merkle
  - Sign root with Ed25519 private key
  - Store Signed Tree Head (STH) in PostgreSQL
- **FR-7.3**: Proof generation:
  - Inclusion proof for any delivery attempt
  - Consistency proof between two STHs
  - Downloadable proof package for client verification
- **FR-7.4**: Signature management:
  - Ed25519 key generation and secure storage
  - Public key availability for verification
  - Key rotation support (yearly recommended)

## Non-Functional Requirements

### Performance

#### NFR-1: Latency

- **NFR-1.1**: Ingestion latency p99 < 50ms (includes database write)
- **NFR-1.2**: First delivery attempt p50 < 100ms
- **NFR-1.3**: Database query p99 < 5ms
- **NFR-1.4**: Attestation batch commit p99 < 100ms
- **NFR-1.5**: Proof generation p99 < 50ms
- **NFR-1.6**: STH signing p99 < 10ms

#### NFR-2: Throughput

- **NFR-2.1**: 10,000 webhooks/second per instance
- **NFR-2.2**: 1,000 concurrent deliveries per instance
- **NFR-2.3**: 100,000 queued events without degradation

#### NFR-3: Resource Usage

- **NFR-3.1**: Memory < 2GB at 10K webhooks/sec
- **NFR-3.2**: CPU < 4 cores at 10K webhooks/sec
- **NFR-3.3**: Database connections ≤ 100 per instance

### Reliability

#### NFR-4: Availability

- **NFR-4.1**: 99.95% monthly uptime (≤ 21.6 minutes downtime)
- **NFR-4.2**: Zero data loss for accepted webhooks
- **NFR-4.3**: Automatic recovery from transient failures

#### NFR-5: Durability

- **NFR-5.1**: Multi-zone replication for PostgreSQL
- **NFR-5.2**: Point-in-time recovery to any second in last 7 days
- **NFR-5.3**: Immutable Merkle tree audit log
- **NFR-5.4**: Cryptographic proof of append-only property
- **NFR-5.5**: 7-year retention for attestation records

### Security

#### NFR-6: Encryption

- **NFR-6.1**: TLS 1.2+ for all external connections
- **NFR-6.2**: Encrypted at rest for database storage
- **NFR-6.3**: Secure secret storage (HSM/Vault integration)

#### NFR-7: Isolation

- **NFR-7.1**: Complete tenant isolation (no data leakage)
- **NFR-7.2**: Rate limiting per tenant
- **NFR-7.3**: Resource quotas per subscription tier

## Technical Constraints

### TC-1: Technology Stack

- Language: Rust 1.75+
- Web framework: Axum 0.7+
- Async runtime: Tokio 1.35+
- Database: PostgreSQL 14+ with pgcrypto extension
- Cryptography: Ed25519-dalek 2.1+, rs-merkle 1.5+
- Serialization: serde with zero-copy where possible

### TC-2: Deployment

- Container-based (Docker/OCI compliant)
- Kubernetes-ready (health checks, graceful shutdown)
- 12-factor app principles
- Stateless application tier

### TC-3: Development

- 100% safe Rust (no unsafe blocks in application code)
- No panics in production paths (#![forbid(unwrap, expect)])
- All errors handled explicitly
- Structured logging only (no println!)

## Data Requirements

### Persistence Model

- **Durability**: All accepted webhooks must survive system failures
- **Idempotency**: Duplicate detection and prevention across 24-hour window
- **Audit Trail**: Complete history of delivery attempts with timing and outcomes
- **Multi-tenancy**: Strict isolation between tenant data and configurations

### Event Lifecycle States

- **Received**: Webhook accepted and persisted
- **Pending**: Queued for delivery processing
- **Delivering**: Currently being delivered by worker
- **Delivered**: Successfully delivered to destination
- **Failed**: Permanently failed after exhausting retries
- **Dead Letter**: Moved to manual intervention queue

### Configuration Storage

- **Endpoints**: Destination URLs, retry policies, circuit breaker thresholds
- **Security**: Signing secrets, signature validation headers
- **Tenant Settings**: Rate limits, retention policies, feature flags

### Attestation Storage

- **Merkle Leaves**: Append-only log of all delivery attempts
- **Signed Tree Heads**: Periodic commitments with Ed25519 signatures
- **Proof Cache**: Pre-computed inclusion proofs for common queries
- **Attestation Keys**: Ed25519 public/private key pairs for signing

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
