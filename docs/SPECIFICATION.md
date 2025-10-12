# Technical Specification

This document defines the complete technical requirements, constraints, and interfaces for the Kapsel webhook reliability service. All implementation must conform to these specifications.

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

## Non-Functional Requirements

### Performance

#### NFR-1: Latency

- **NFR-1.1**: Ingestion latency p99 < 50ms (includes database write)
- **NFR-1.2**: First delivery attempt p50 < 100ms
- **NFR-1.3**: Database query p99 < 5ms
- **NFR-1.4**: TigerBeetle commit p99 < 1ms

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
- **NFR-5.3**: Immutable audit log in TigerBeetle

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
- Audit log: TigerBeetle 0.15+
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

## Data Models

### Database Schema

```sql
-- Core webhook event table
CREATE TABLE webhook_events (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL,
    endpoint_id UUID NOT NULL,

    -- Idempotency
    source_event_id TEXT NOT NULL, -- Extracted based on strategy
    idempotency_strategy TEXT NOT NULL, -- 'header', 'content', 'source_id'

    -- Status tracking
    status TEXT NOT NULL, -- 'received', 'pending', 'delivering', 'success', 'failed'
    failure_count INTEGER NOT NULL DEFAULT 0,
    last_attempt_at TIMESTAMPTZ,
    next_retry_at TIMESTAMPTZ,

    -- Payload
    headers JSONB NOT NULL,
    body BYTEA NOT NULL, -- Raw bytes, not interpreted
    content_type TEXT NOT NULL,

    -- Metadata
    received_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    delivered_at TIMESTAMPTZ,
    failed_at TIMESTAMPTZ,

    -- Audit
    tigerbeetle_id UUID, -- Reference to immutable log

    UNIQUE(tenant_id, endpoint_id, source_event_id)
);

-- Delivery attempts for audit trail
CREATE TABLE delivery_attempts (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    event_id UUID NOT NULL REFERENCES webhook_events(id),
    attempt_number INTEGER NOT NULL,

    -- Request details
    request_url TEXT NOT NULL,
    request_headers JSONB NOT NULL,

    -- Response details
    response_status INTEGER,
    response_headers JSONB,
    response_body TEXT, -- Truncated to 1KB

    -- Timing
    attempted_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    duration_ms INTEGER NOT NULL,

    -- Error details
    error_type TEXT, -- 'network', 'timeout', 'http_error'
    error_message TEXT
);

-- Endpoint configuration
CREATE TABLE endpoints (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL,

    -- Configuration
    url TEXT NOT NULL,
    name TEXT NOT NULL,

    -- Security
    signing_secret TEXT, -- Encrypted
    signature_header TEXT,

    -- Retry policy
    max_retries INTEGER NOT NULL DEFAULT 10,
    timeout_seconds INTEGER NOT NULL DEFAULT 30,

    -- Circuit breaker state
    circuit_state TEXT NOT NULL DEFAULT 'closed',
    circuit_failure_count INTEGER NOT NULL DEFAULT 0,
    circuit_last_failure_at TIMESTAMPTZ,
    circuit_half_open_at TIMESTAMPTZ,

    -- Metadata
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),

    UNIQUE(tenant_id, name)
);

-- Indexes for performance
CREATE INDEX idx_webhook_events_status ON webhook_events(status, next_retry_at)
    WHERE status IN ('pending', 'delivering');
CREATE INDEX idx_webhook_events_tenant ON webhook_events(tenant_id, received_at DESC);
CREATE INDEX idx_delivery_attempts_event ON delivery_attempts(event_id, attempt_number);
```

### TigerBeetle Schema

```rust
pub struct WebhookAuditEntry {
    pub id: u128,                    // Unique event ID
    pub timestamp: u64,               // Nanoseconds since epoch
    pub tenant_id: u128,              // Tenant identifier
    pub event_hash: [u8; 32],         // SHA256 of canonical event
    pub flags: u16,                   // Bitflags for event properties
    pub reserved: [u8; 14],           // Future expansion
}
```

## API Specification

### Authentication

All API requests require Bearer token authentication:

```
Authorization: Bearer <api_key>
```

### Endpoints

#### POST /v1/endpoints

Create a new webhook endpoint.

Request:

```json
{
  "name": "production-orders",
  "url": "https://api.example.com/webhooks",
  "signing_secret": "whsec_abc123",
  "signature_header": "X-Webhook-Signature",
  "max_retries": 10,
  "timeout_seconds": 30
}
```

Response (201 Created):

```json
{
  "id": "ep_abc123",
  "ingestion_url": "https://hooks.example.com/ingest/ep_abc123",
  "name": "production-orders",
  "created_at": "2024-01-01T00:00:00Z"
}
```

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
```

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
- 100% coverage for critical paths (retry logic, idempotency)
- All error conditions tested

### Integration Tests

- Database transaction rollback scenarios
- Network failure simulation
- Concurrent request handling
- Circuit breaker state transitions

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

- Immutable event log for 7 years
- Cryptographic proof of receipt
- Chain of custody documentation

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
