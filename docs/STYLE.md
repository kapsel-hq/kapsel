# Kapsel Code Style Guide

Last Updated: 2025-01-03

## Overview

This document establishes coding standards for Kapsel development. The goal is consistent, maintainable, and performant code that follows Rust idioms while incorporating battle-tested patterns from high-performance systems.

## Naming Conventions (TIGERSTYLE)

### Functions: `verb_object` Pattern

Functions should describe what they do using active verbs followed by the object they operate on:

```rust
// GOOD: Clear action + target
fn create_webhook_event(payload: &[u8]) -> WebhookEvent
fn validate_signature(payload: &[u8], secret: &str) -> Result<bool>
fn claim_pending_events(pool: &PgPool) -> Result<Vec<WebhookEvent>>
fn deliver_webhook(event: &WebhookEvent) -> Result<()>

// BAD: Unclear, passive, or non-standard patterns
fn webhookCreation(payload: &[u8]) -> WebhookEvent  // camelCase
fn webhook_deliver(event: &WebhookEvent) -> Result<()>  // object_verb
fn doValidation(payload: &[u8]) -> Result<bool>  // generic verb
fn signature_check(payload: &[u8]) -> Result<bool>  // noun instead of verb
```

### Predicates: `is_`, `has_`, `can_`

Boolean functions should use clear predicate prefixes:

```rust
// GOOD: Clear boolean predicates
fn is_retryable(&self) -> bool
fn has_expired(&self, now: DateTime<Utc>) -> bool
fn can_retry(&self, max_attempts: u32) -> bool
fn is_terminal_state(&self) -> bool

// BAD: Missing predicates or unclear
fn retryable(&self) -> bool
fn expired(&self) -> bool
fn check_terminal(&self) -> bool
```

### NO get*/set* Prefixes

Rust convention omits get*/set* prefixes:

```rust
// GOOD: Rust idiomatic accessors
fn status(&self) -> EventStatus
fn update_status(&mut self, status: EventStatus)
fn body(&self) -> &[u8]
fn body_bytes(&self) -> Bytes

// BAD: Unnecessary get_/set_ prefixes
fn get_status(&self) -> EventStatus
fn set_status(&mut self, status: EventStatus)
fn get_body(&self) -> &[u8]
```

### Type Conversions: `as_*`, `into_*`, `to_*`

Follow Rust std conventions for conversions:

```rust
// GOOD: Standard conversion patterns
fn as_bytes(&self) -> &[u8]           // Cheap reference conversion
fn into_bytes(self) -> Vec<u8>        // Owned conversion
fn to_string(&self) -> String         // Expensive conversion

// Type-specific conversions
fn as_event_id(&self) -> EventId      // Cheap wrapper
fn into_uuid(self) -> Uuid            // Extract inner value
fn to_json(&self) -> serde_json::Value // Serialize
```

### Optional Types: `maybe_*`

Use `maybe_` prefix for functions that return `Option<T>`:

```rust
// GOOD: Clear optional semantics
fn maybe_parse_header(value: &str) -> Option<DateTime<Utc>>
fn maybe_extract_tenant_id(headers: &HeaderMap) -> Option<TenantId>

// ALSO ACCEPTABLE: Standard try_ prefix for Result
fn try_parse_header(value: &str) -> Result<DateTime<Utc>, ParseError>
```

### Result Types: `try_*`

Use `try_` prefix for fallible operations:

```rust
// GOOD: Clear fallible operations
fn try_deliver_webhook(event: &WebhookEvent) -> Result<()>
fn try_parse_endpoint_url(url: &str) -> Result<Url>
fn try_connect_database(url: &str) -> Result<PgPool>

// Exception: Standard new() constructors can be fallible
fn new(config: Config) -> Result<Self, Error>
```

## Error Handling

### Library Code: Use `thiserror`

For libraries (`kapsel-core`, `kapsel-delivery`), use `thiserror` for structured errors:

```rust
#[derive(Debug, thiserror::Error)]
pub enum DeliveryError {
    #[error("network connection failed: {message}")]
    NetworkError { message: String },

    #[error("request timeout after {timeout_seconds}s")]
    Timeout { timeout_seconds: u64 },

    #[error("webhook delivery failed after {attempts} attempts")]
    RetriesExhausted { attempts: u32 },
}
```

### Application Code: Use `anyhow`

For applications (`kapsel-api`), use `anyhow::Result` and `.context()`:

```rust
async fn ingest_webhook(payload: Bytes) -> anyhow::Result<EventId> {
    let event = parse_webhook_payload(&payload)
        .context("failed to parse webhook payload")?;

    persist_event(&event)
        .await
        .context("failed to persist event to database")?;

    Ok(event.id)
}
```

### Never Panic in Production

Production code must never use `unwrap()`, `expect()`, or `panic!()`:

```rust
// BAD: Can panic in production
let value = map.get(&key).unwrap();
let number = string.parse::<u32>().expect("invalid number");

// GOOD: Handle errors gracefully
let value = map.get(&key).ok_or_else(|| anyhow!("key not found"))?;
let number = string.parse::<u32>()
    .with_context(|| format!("failed to parse number: {string}"))?;
```

**Exception**: Test code may use `unwrap()` and `expect()` for clarity.

### Retry Classification

Errors must implement retry logic:

```rust
impl DeliveryError {
    pub fn is_retryable(&self) -> bool {
        match self {
            // Retryable: temporary failures
            Self::NetworkError { .. }
            | Self::Timeout { .. }
            | Self::ServerError { .. } => true,

            // Non-retryable: permanent failures
            Self::ClientError { .. }
            | Self::RetriesExhausted { .. } => false,
        }
    }
}
```

## Documentation Standards

### Module-Level Documentation

Every public module needs comprehensive documentation:

````rust
//! Webhook delivery engine with reliability guarantees.
//!
//! This module orchestrates webhook delivery using a pool of async workers
//! that claim events from PostgreSQL and deliver them to configured endpoints.
//! Integrates circuit breakers, retry logic, and graceful shutdown.
//!
//! # Architecture
//!
//! ```text
//! ┌────────────────┐   ┌──────────────┐   ┌─────────────┐
//! │ DeliveryEngine │──▶│ Worker Pool  │──▶│ HTTP Client │
//! └────────────────┘   └──────────────┘   └─────────────┘
//! ```
//!
//! # Example
//!
//! ```rust
//! let engine = DeliveryEngine::new(pool, config)?;
//! engine.start().await?;
//! ```
````

### Function Documentation

Document WHY, not WHAT:

```rust
/// Claims pending events from the database for processing.
///
/// Uses `FOR UPDATE SKIP LOCKED` to enable lock-free work distribution
/// across multiple workers. This PostgreSQL-specific feature allows
/// concurrent workers to claim different events without blocking each other.
///
/// Returns up to `batch_size` events ordered by `received_at` to ensure
/// FIFO processing within each endpoint.
async fn claim_pending_events(&self) -> Result<Vec<WebhookEvent>>
```

### Explain Non-Obvious Optimizations

```rust
// PostgreSQL's SKIP LOCKED allows multiple workers to claim
// rows without blocking each other, giving us free work distribution
let query = sqlx::query!(
    "SELECT * FROM webhook_events
     WHERE status = 'pending'
     ORDER BY received_at ASC
     LIMIT $1
     FOR UPDATE SKIP LOCKED"
);
```

### Database Constraints

Document critical constraints that affect application logic:

```rust
/// Size of the payload in bytes.
///
/// Must be between 1 and 10MB (10485760 bytes) to satisfy database
/// CHECK constraint. Even empty payloads are stored as size 1.
pub payload_size: i32,
```

## Type Safety

### Newtype Wrappers for IDs

Prevent ID confusion with type-safe wrappers:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EventId(pub Uuid);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TenantId(pub Uuid);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EndpointId(pub Uuid);

// This prevents bugs like:
fn process_event(event_id: EventId, tenant_id: TenantId) {
    // Compiler error if we swap the arguments:
    // process_event(tenant_id, event_id); // ❌ Won't compile
}
```

### Make Illegal States Unrepresentable

Use the type system to enforce invariants:

```rust
#[derive(Debug, Clone)]
pub enum EventStatus {
    Received,
    Pending,
    Delivering,
    Delivered,    // Terminal state
    Failed,       // Terminal state
    DeadLetter,   // Terminal state
}

impl EventStatus {
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Delivered | Self::Failed | Self::DeadLetter)
    }

    pub fn can_retry(&self) -> bool {
        matches!(self, Self::Pending | Self::Failed)
    }
}
```

## Performance Guidelines

### Zero-Copy Operations

Use `Bytes` for payload handling:

```rust
// GOOD: Zero-copy byte handling
pub struct WebhookEvent {
    pub body: Vec<u8>,  // Database storage format
}

impl WebhookEvent {
    /// Get the body as Bytes for zero-copy operations.
    pub fn body_bytes(&self) -> Bytes {
        Bytes::from(self.body.clone())
    }
}
```

### Database Query Patterns

Use prepared statements and connection pooling:

```rust
// GOOD: Prepared statement with parameter binding
sqlx::query!(
    "INSERT INTO webhook_events (id, tenant_id, body) VALUES ($1, $2, $3)",
    event.id.0,
    event.tenant_id.0,
    event.body
)
.execute(&pool)
.await?;

// BAD: String interpolation (SQL injection risk + no statement reuse)
let query = format!(
    "INSERT INTO webhook_events (id, tenant_id) VALUES ('{}', '{}')",
    event.id, event.tenant_id
);
```

### Async Best Practices

Prefer message passing over shared state:

```rust
// GOOD: Channel-based communication
let (tx, mut rx) = mpsc::channel::<WebhookEvent>(100);

// Worker sends events
tx.send(event).await?;

// Consumer processes events
while let Some(event) = rx.recv().await {
    process_event(event).await?;
}

// AVOID: Shared mutable state with locks
let events = Arc<Mutex<Vec<WebhookEvent>>>::new(Vec::new());
```

## Testing Standards

### Test Naming

Tests should describe the behavior being tested:

```rust
#[tokio::test]
async fn webhook_ingestion_returns_200_with_event_id() {}

#[tokio::test]
async fn webhook_ingestion_rejects_invalid_hmac_signature() {}

#[tokio::test]
async fn delivery_worker_respects_circuit_breaker_open_state() {}
```

### Use Test Fixtures

Create reusable test helpers:

```rust
impl TestEnv {
    /// Creates a test tenant with default settings.
    pub async fn create_tenant(&self, name: &str) -> Result<TenantId> {
        let tenant_id = TenantId::new();
        sqlx::query!(
            "INSERT INTO tenants (id, name, plan) VALUES ($1, $2, $3)",
            tenant_id.0,
            name,
            "enterprise"
        )
        .execute(&self.db.pool())
        .await?;

        Ok(tenant_id)
    }
}
```

### Deterministic Testing

Use controlled time for deterministic tests:

```rust
#[tokio::test]
async fn exponential_backoff_timing_is_deterministic() {
    let env = TestEnv::new().await?;

    // Use test clock, not real time
    env.clock.advance(Duration::from_secs(1));

    let retry_at = env.clock.now() + Duration::from_secs(2);
    assert_eq!(retry_at, expected_time);
}
```

### Property-Based Testing

Verify invariants with generated inputs:

```rust
proptest! {
    #[test]
    fn retry_count_is_bounded(
        failures in 0u32..100,
        max_retries in 1u32..20
    ) {
        let attempts = simulate_retries(failures, max_retries);
        prop_assert!(attempts <= max_retries + 1); // +1 for initial attempt
    }
}
```

## Module Organization

### File Naming

Files should match their primary responsibility:

```
crates/kapsel-delivery/src/
├── worker.rs           # Individual worker logic
├── worker_pool.rs      # Pool management
├── circuit.rs          # Circuit breaker implementation
├── retry.rs            # Retry logic and backoff
├── client.rs           # HTTP delivery client
└── error.rs            # Error types
```

### Re-exports

Keep public APIs clean with selective re-exports:

```rust
// lib.rs
pub use error::{DeliveryError, Result};
pub use worker::{DeliveryConfig, DeliveryEngine};
pub use worker_pool::WorkerPool;

// Don't expose internal implementation details
pub(crate) use client::DeliveryClient;
```

### Dependency Direction

Follow dependency inversion principle:

```
┌─────────────────┐
│ Application     │ (kapsel-api)
├─────────────────┤
│ Domain          │ (kapsel-core)
├─────────────────┤
│ Infrastructure  │ (test-harness)
└─────────────────┘
```

Higher layers depend on lower layers, not vice versa.

## Database Patterns

### Idempotency

All database operations that might be retried must be idempotent:

```rust
// GOOD: ON CONFLICT handles duplicate inserts
sqlx::query!(
    "INSERT INTO webhook_events (...) VALUES (...)
     ON CONFLICT (tenant_id, endpoint_id, source_event_id) DO NOTHING"
)
.execute(&pool)
.await?;
```

### Work Distribution

Use PostgreSQL-specific features for lock-free concurrency:

```rust
// Claim work without blocking other workers
sqlx::query_as!(
    WebhookEvent,
    "UPDATE webhook_events
     SET status = 'delivering'
     WHERE id IN (
         SELECT id FROM webhook_events
         WHERE status = 'pending'
         ORDER BY received_at ASC
         LIMIT $1
         FOR UPDATE SKIP LOCKED
     )
     RETURNING *"
)
.bind(batch_size)
.fetch_all(&pool)
.await?
```

### Schema Constraints

Use database constraints to enforce invariants:

```sql
-- Payload size constraint
CHECK (payload_size > 0 AND payload_size <= 10485760)

-- Status transitions
CHECK (status IN ('received', 'pending', 'delivering', 'delivered', 'failed'))

-- Uniqueness for idempotency
UNIQUE(tenant_id, endpoint_id, source_event_id)
```

## Performance Considerations

### Memory Management

Avoid unnecessary allocations:

```rust
// GOOD: Reuse buffers
let mut buffer = Vec::with_capacity(1024);
for item in items {
    buffer.clear();
    serialize_into(&mut buffer, &item)?;
    process_buffer(&buffer)?;
}

// BAD: Allocate per iteration
for item in items {
    let buffer = serialize(&item)?;  // New allocation each time
    process_buffer(&buffer)?;
}
```

### Database Connection Pooling

Configure pools for your workload:

```rust
let pool = PgPoolOptions::new()
    .max_connections(20)                    // Adjust for your load
    .acquire_timeout(Duration::from_secs(5)) // Fail fast
    .idle_timeout(Duration::from_secs(300))  // Release unused connections
    .connect(&database_url)
    .await?;
```

### Batching

Process items in batches for efficiency:

```rust
const BATCH_SIZE: usize = 100;

let events = claim_events(&pool, BATCH_SIZE).await?;
for chunk in events.chunks(BATCH_SIZE) {
    process_batch(chunk).await?;
}
```

## Commit Message Format

Use conventional commits:

```
type(scope): description

feat(delivery): implement exponential backoff retry logic
fix(api): handle empty payload edge case
docs(style): update naming conventions
test(delivery): add property tests for retry bounds
refactor(core): extract event validation logic
perf(db): optimize event claiming query with index
```

## Code Review Checklist

Before submitting:

- [ ] All public functions have rustdoc comments
- [ ] Error handling uses Result types, no panics
- [ ] Tests cover the happy path and edge cases
- [ ] Performance considerations documented
- [ ] Database operations are idempotent
- [ ] Naming follows TIGERSTYLE conventions
- [ ] No clippy warnings with `-- -D warnings`
- [ ] No `unwrap()` in production code paths

## Enforcement

These standards are enforced through:

- **Pre-commit hooks**: Format and lint checks
- **CI pipeline**: Comprehensive test suite
- **Code review**: Manual verification of standards
- **clippy configuration**: Custom lints for project rules
- **Documentation**: This guide and inline comments

Follow these standards to maintain Kapsel's reputation for quality and reliability.
