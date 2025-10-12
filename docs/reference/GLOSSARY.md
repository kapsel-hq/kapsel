# Glossary

This glossary defines domain-specific terms used throughout the Hooky project. Understanding these terms is essential for working with the codebase and documentation.

## A

### At-Least-Once Delivery
A delivery guarantee ensuring that every accepted webhook will be delivered to its destination at least once. May result in duplicate deliveries, which recipients must handle idempotently.

### Attestation
A cryptographically signed proof that a webhook was received and stored. Generated using JWT tokens with our private key, providing non-repudiation for compliance and audit purposes.

### Audit Log
An immutable, append-only record of all webhook events stored in TigerBeetle. Provides cryptographic verification and chain of custody for compliance requirements.

## B

### Backpressure
A flow control mechanism where downstream components signal upstream components to slow down when overwhelmed. Implemented through bounded channels and rate limiting.

### Bounded Channel
An in-memory queue with a fixed maximum capacity. When full, it applies backpressure by rejecting new items rather than growing unbounded.

## C

### Chain of Custody
Complete, verifiable record of a webhook's journey from ingestion through delivery, including all processing steps, timestamps, and state changes.

### Circuit Breaker
A fault tolerance pattern that prevents cascading failures. Opens (stops traffic) after repeated failures, half-opens to test recovery, then closes when healthy. Implemented per endpoint.

### Correlation ID
A unique identifier (UUID) that follows a webhook through the entire system, enabling distributed tracing and log correlation across components.

## D

### Dead Letter Queue (DLQ)
Storage for webhooks that have permanently failed delivery after exhausting all retry attempts. Allows manual inspection and potential reprocessing.

### Delivery Attempt
A single try to deliver a webhook to its destination endpoint. Includes the HTTP request, response (if any), timing information, and error details.

### Deterministic Testing
Testing approach where all non-determinism (time, randomness, I/O) is controlled, allowing perfect reproduction of test failures and systematic exploration of failure scenarios.

### Dispatcher
The component responsible for reading events from the in-memory channel and persisting them to both PostgreSQL and TigerBeetle in the two-phase persistence model.

## E

### Endpoint
A destination URL where webhooks should be delivered. Each endpoint has its own configuration including retry policy, timeout, and circuit breaker state.

### Event
A webhook that has been received and stored in the system. Includes the original headers, payload, and metadata about its processing state.

### Exponential Backoff
Retry strategy where the delay between attempts increases exponentially (1s, 2s, 4s, 8s...). Prevents overwhelming failed endpoints while allowing recovery.

## F

### FOR UPDATE SKIP LOCKED
PostgreSQL feature for implementing work queues. Workers can claim rows without blocking each other, enabling efficient parallel processing.

## H

### HMAC-SHA256
Cryptographic signature algorithm using a shared secret to verify webhook authenticity. Prevents forgery and tampering of webhook payloads.

### Hot Path
Code execution path that runs frequently and impacts performance. Optimized for minimal allocations and maximum throughput.

## I

### Idempotency
Property where multiple identical requests produce the same result. Critical for handling retries without causing duplicate side effects.

### Idempotency Key
Unique identifier for a webhook event used to detect and prevent duplicate processing. Can be provided by the source or generated from payload content.

### Ingestion
The process of receiving, validating, and storing incoming webhooks. Must complete quickly (< 10ms) to prevent source timeouts.

### Immutable Log
Append-only data structure that cannot be modified after writing. TigerBeetle provides this for audit trail integrity.

## J

### Jitter
Random variation added to retry delays to prevent synchronized retries from multiple workers (thundering herd problem).

### JWT (JSON Web Token)
Signed tokens used for API authentication and signed attestations. Provides cryptographic proof of authenticity.

## M

### Multi-tenancy
System design where multiple customers (tenants) share infrastructure while maintaining complete data isolation.

## O

### Operational State
The mutable, queryable state of webhooks stored in PostgreSQL. Includes delivery status, retry counts, and scheduling information.

## P

### Payload
The body content of a webhook. Stored as raw bytes to preserve exact content without interpretation.

### Property-Based Testing
Testing approach that verifies invariants hold across automatically generated input ranges rather than specific test cases.

## Q

### Queue Depth
Number of webhooks waiting to be processed at various stages. Key metric for system health and backpressure.

## R

### Rate Limiting
Throttling mechanism to prevent any single tenant from overwhelming the system. Implemented using token bucket algorithm.

### Reconciliation
Background process ensuring eventual consistency between PostgreSQL (operational state) and TigerBeetle (audit log).

### Retry Policy
Configuration determining how many times and with what delays to retry failed webhook deliveries.

## S

### Signed Attestation
JWT token containing cryptographic proof that a webhook was received, including timestamp, hash, and metadata. Used for compliance and dispute resolution.

### Signing Secret
Shared secret used to generate and verify HMAC signatures. Each webhook source has its own secret for security.

### SKIP LOCKED
PostgreSQL clause allowing workers to claim available work without waiting for locks held by other workers.

### Structured Concurrency
Programming pattern where every spawned task has a defined parent responsible for its lifecycle, preventing resource leaks.

## T

### Tenant
A customer or organization using the service. All data is isolated per tenant for security and privacy.

### TigerBeetle
Purpose-built database for financial accounting providing cryptographically verifiable, immutable audit logs with strict consistency guarantees.

### Two-Phase Persistence
Our consistency model where events are first written to PostgreSQL for operational durability, then to TigerBeetle for audit integrity.

## U

### Unique URL
Per-endpoint ingestion URL where webhook sources send their events. Format: `https://api.hooky.dev/ingest/{endpoint_id}`

## W

### Webhook
HTTP POST request sent by one system to notify another of an event. Core unit of work in the system.

### Webhook Provider
External service that sends webhooks (e.g., Stripe, GitHub, Shopify). Each has specific signature validation requirements.

### Worker Pool
Collection of asynchronous tasks that claim and deliver webhooks. Sized based on available resources and throughput requirements.

## Z

### Zero-Copy
Performance optimization technique avoiding unnecessary data copying. Achieved through careful use of references and reference-counted buffers (Bytes).

---

## Common Acronyms

| Acronym | Expansion | Description |
|---------|-----------|-------------|
| ADR | Architecture Decision Record | Document recording significant design decisions |
| DLQ | Dead Letter Queue | Storage for permanently failed messages |
| DOD | Data-Oriented Design | Structuring data for access patterns, not domain models |
| HSM | Hardware Security Module | Dedicated crypto hardware for key management |
| JWT | JSON Web Token | Signed tokens for authentication and attestations |
| MRR | Monthly Recurring Revenue | Key business metric for SaaS |
| OTLP | OpenTelemetry Protocol | Standard for telemetry data export |
| PLG | Product-Led Growth | Go-to-market strategy focused on self-service |
| RAII | Resource Acquisition Is Initialization | Automatic resource management pattern |
| RBAC | Role-Based Access Control | Authorization model using roles |
| RED | Rate, Errors, Duration | Key metrics methodology |
| RPS | Requests Per Second | Throughput measurement |
| SLO | Service Level Objective | Target reliability metric |
| UUID | Universally Unique Identifier | 128-bit unique identifier |

## Domain-Specific Patterns

### Event States

1. **received** - Persisted to PostgreSQL, awaiting TigerBeetle log
2. **pending** - Ready for delivery, in work queue
3. **delivering** - Currently being processed by a worker
4. **delivered** - Successfully delivered to endpoint
5. **failed** - Permanently failed after exhausting retries

### Failure Classifications

- **Transient** - Temporary failures that should be retried (network errors, 5xx)
- **Permanent** - Failures that won't succeed with retry (4xx, validation errors)
- **Rate-Limited** - Special case requiring backoff per Retry-After header

### Consistency Levels

- **Immediate** - PostgreSQL provides immediate operational consistency
- **Eventual** - TigerBeetle provides eventual audit consistency
- **External** - Webhook delivery provides eventual external consistency
