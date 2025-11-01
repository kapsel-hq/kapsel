# Kapsel Code Style Guide

## Philosophy

Code is written once and read hundreds of times. Kapsel prioritizes clarity, type safety, and performance without sacrificing maintainability. Our style builds on standard Rust idioms documented in the [Rust API Guidelines](https://rust-lang.github.io/api-guidelines/), filling gaps specific to webhook reliability engineering.

**Guiding principles:**

- **Types prevent bugs** - Use the type system to make illegal states unrepresentable
- **Errors are values** - All failure paths return `Result`, never panic in production
- **Performance by design** - Zero-copy operations, connection pooling, lock-free concurrency
- **Clarity over cleverness** - Code should be immediately understandable to competent Rust developers

## Naming Conventions

Kapsel follows standard Rust naming with specific patterns for webhook reliability concerns.

### Functions: `verb_object` Pattern

Functions describe actions using active verbs followed by the object they operate on:

```rust
// GOOD: Clear action + target
fn create_webhook_event(payload: &[u8]) -> WebhookEvent
fn validate_signature(payload: &[u8], secret: &str) -> Result<()>
fn claim_pending_events(pool: &PgPool) -> Result<Vec<WebhookEvent>>
fn deliver_webhook(event: &WebhookEvent) -> Result<()>

// BAD: Unclear, passive, or non-standard
fn webhookCreation(payload: &[u8]) -> WebhookEvent  // camelCase
fn webhook_deliver(event: &WebhookEvent) -> Result<()>  // object_verb
fn doValidation(payload: &[u8]) -> Result<()>  // generic verb
```

### Boolean Predicates: `is_`, `has_`, `can_`

Boolean functions use clear predicate prefixes:

```rust
// GOOD: Clear boolean predicates
fn is_retryable(&self) -> bool
fn has_expired(&self, now: DateTime<Utc>) -> bool
fn can_retry(&self, max_attempts: u32) -> bool
fn is_terminal_state(&self) -> bool

// BAD: Missing predicates
fn retryable(&self) -> bool
fn expired(&self) -> bool
```

### Avoid JavaBean-style `get_`/`set_` Prefixes for Field Accessors

Rust convention omits boilerplate `get_` and `set_` prefixes for methods that simply access the fields of a struct. Name the accessor method after the field itself.

**For direct field access:**

```rust
pub struct WebhookEvent {
    id: EventId,
    body: Vec<u8>,
}

impl WebhookEvent {
    // GOOD: Simple accessor, same name as field
    pub fn id(&self) -> EventId {
        self.id
    }

    // GOOD: Mutable accessor
    pub fn body_mut(&mut self) -> &mut Vec<u8> {
        &mut self.body
    }
}

// BAD: Unnecessary prefixes for struct fields
fn get_id(&self) -> EventId
fn set_body(&mut self, body: Vec<u8>)
```

**Exception: Use `get()` for Collection Lookups**

The `get()` method name is idiomatic and correct when performing a fallible lookup on a collection or container type, typically using a key or index. This follows standard library conventions.

```rust
// GOOD: Using get() for keyed lookup in a collection
let event = event_cache.get(&event_id); // Returns Option<&Event>
let item = my_vec.get(index);           // Returns Option<&T>

// GOOD: Descriptive names for complex retrieval operations
let resource = HEAVY_RESOURCE.get_or_init(|| compute_resource());
```

**For computed values:**

When computation is involved, use `verb_object` pattern:

```rust
// GOOD: Computation clear from verb
fn formatted_id(&self) -> String {
    format!("evt_{}", self.id.0)
}

fn body_bytes(&self) -> Bytes {
    Bytes::from(self.body.as_slice())
}

fn compute_signature(&self, secret: &str) -> String {
    // HMAC computation
}

// BAD: Looks like simple field access
fn signature(&self, secret: &str) -> String {
    // Expensive computation hidden
}
```

### Type Conversions: `as_*`, `into_*`, `to_*`

Follow standard library conventions:

```rust
// GOOD: Standard conversion patterns
fn as_bytes(&self) -> &[u8]           // Cheap reference cast
fn into_bytes(self) -> Vec<u8>        // Consuming conversion
fn to_string(&self) -> String         // Expensive allocation

// GOOD: Type-specific wrappers
fn as_event_id(&self) -> EventId      // Cheap newtype wrap
fn into_uuid(self) -> Uuid            // Extract inner value
fn to_json(&self) -> Value            // Serialize
```

### Fallible Operations

**The `Result<T, E>` return type is the indicator of fallibility.** Function names describe the action, not error handling:

```rust
// GOOD: Name describes action, Result indicates fallibility
fn deliver_webhook(event: &WebhookEvent) -> Result<()>
fn parse_endpoint_url(url: &str) -> Result<Url>
fn connect_database(url: &str) -> Result<PgPool>

// BAD: Redundant try_ prefix
fn try_deliver_webhook(event: &WebhookEvent) -> Result<()>
fn try_parse_endpoint_url(url: &str) -> Result<Url>
```

**Exception**: Use `try_` prefix only when providing a non-panicking alternative to a function that would panic:

```rust
// This pattern is rare in Kapsel (we forbid panics in production)
fn parse_id(s: &str) -> EventId {
    // Panics on invalid input
}

fn try_parse_id(s: &str) -> Result<EventId, ParseError> {
    // Non-panicking alternative
}
```

## Error Handling

### Library Code: Use `thiserror`

For reusable libraries (`kapsel-core`, `kapsel-delivery`), use `thiserror` for structured errors:

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

impl DeliveryError {
    pub fn is_retryable(&self) -> bool {
        matches!(self, Self::NetworkError { .. } | Self::Timeout { .. })
    }
}
```

### Application Code: Use `anyhow`

For application code (`kapsel-api`), use `anyhow::Result` with `.context()`:

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
// BAD: Can panic
let value = map.get(&key).unwrap();
let number = string.parse::<u32>().expect("invalid number");

// GOOD: Explicit error handling
let value = map.get(&key).ok_or_else(|| anyhow!("key not found: {key}"))?;
let number = string.parse::<u32>()
    .with_context(|| format!("failed to parse number: {string}"))?;
```

**Exception**: Test code may use `unwrap()` and `expect()` for clarity.

## Type Safety

### Newtype Wrappers for IDs

Prevent ID confusion with type-safe wrappers:

```rust
// GOOD: Derive traits for ergonomics
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash,
    serde::Serialize, serde::Deserialize,
    sqlx::Type,
)]
#[serde(transparent)]  // Serialize as the inner type
pub struct EventId(pub Uuid);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct TenantId(pub Uuid);

impl EventId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

// Convenience conversions
impl From<Uuid> for EventId {
    fn from(uuid: Uuid) -> Self {
        Self(uuid)
    }
}

impl From<EventId> for Uuid {
    fn from(id: EventId) -> Self {
        id.0
    }
}

// Type safety prevents bugs:
fn process_event(event_id: EventId, tenant_id: TenantId) {
    // Compiler prevents swapping arguments
}
```

### Make Illegal States Unrepresentable

Use enums to enforce state machines:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventStatus {
    Received,
    Pending,
    Delivering,
    Delivered,    // Terminal
    Failed,       // Terminal
    DeadLetter,   // Terminal
}

impl EventStatus {
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Delivered | Self::Failed | Self::DeadLetter)
    }

    pub fn can_transition_to(&self, target: EventStatus) -> bool {
        use EventStatus::*;
        matches!(
            (self, target),
            (Received, Pending)
                | (Pending, Delivering)
                | (Delivering, Delivered)
                | (Delivering, Failed)
                | (Failed, DeadLetter)
        )
    }
}
```

## Performance Guidelines

### Zero-Copy Operations with `Bytes`

`Bytes` enables cheap cloning through reference counting. The key is to receive `Bytes` at system boundaries and pass it through:

```rust
// GOOD: Receive Bytes and pass it along
async fn ingest_webhook(payload: Bytes) -> anyhow::Result<()> {
    // Clone is cheap (atomic refcount increment, no data copy)
    let payload_for_analytics = payload.clone();
    tokio::spawn(async move {
        process_analytics(payload_for_analytics).await;
    });

    // Original payload continues processing
    let event = WebhookEvent::new(payload)?;
    persist_event(&event).await
}

// Store Bytes directly
pub struct WebhookEvent {
    pub body: Bytes,
}

impl WebhookEvent {
    pub fn body(&self) -> Bytes {
        self.body.clone()  // Cheap clone
    }
}

// BAD: Converting Vec to Bytes loses the zero-copy benefit
pub struct WebhookEvent {
    pub body: Vec<u8>,
}

impl WebhookEvent {
    pub fn body_bytes(&self) -> Bytes {
        Bytes::from(self.body.clone())  // Full data copy!
    }
}
```

### Database Query Patterns

Use prepared statements and connection pooling:

```rust
// GOOD: Parameterized query with type safety
sqlx::query!(
    "INSERT INTO webhook_events (id, tenant_id, body) VALUES ($1, $2, $3)",
    event.id.0,
    event.tenant_id.0,
    event.body.as_ref()
)
.execute(&pool)
.await?;

// BAD: String interpolation (SQL injection + no statement reuse)
let query = format!(
    "INSERT INTO webhook_events (id, tenant_id) VALUES ('{}', '{}')",
    event.id, event.tenant_id
);
```

### Work Distribution with PostgreSQL

Use `SKIP LOCKED` for lock-free concurrency:

```rust
// Claim events without blocking other workers
sqlx::query_as!(
    WebhookEvent,
    r#"
    UPDATE webhook_events
    SET status = 'delivering'
    WHERE id IN (
        SELECT id FROM webhook_events
        WHERE status = 'pending'
        ORDER BY received_at ASC
        LIMIT $1
        FOR UPDATE SKIP LOCKED
    )
    RETURNING *
    "#,
    batch_size
)
.fetch_all(&pool)
.await?
```

### Async Best Practices

Prefer message passing over shared state:

```rust
// GOOD: Channel-based communication
let (tx, mut rx) = mpsc::channel::<WebhookEvent>(100);

// Producer
tx.send(event).await?;

// Consumer
while let Some(event) = rx.recv().await {
    process_event(event).await?;
}

// AVOID: Locks in async code
let events = Arc<Mutex<Vec<WebhookEvent>>>::new(Vec::new());
```

## Documentation Standards

### Module-Level Documentation

Every public module needs documentation explaining its purpose:

```rust
//! Webhook delivery engine with reliability guarantees.
//!
//! Orchestrates delivery using a pool of async workers that claim events
//! from PostgreSQL and deliver them to configured endpoints. Integrates
//! circuit breakers, retry logic, and graceful shutdown.
```

### Function Documentation

Document WHY, not WHAT. Focus on non-obvious behavior:

```rust
/// Claims pending events from the database for processing.
///
/// Uses `FOR UPDATE SKIP LOCKED` to enable lock-free work distribution.
/// This PostgreSQL-specific feature allows concurrent workers to claim
/// different events without blocking each other.
///
/// Returns up to `batch_size` events ordered by `received_at` to ensure
/// FIFO processing within each endpoint.
async fn claim_pending_events(&self, batch_size: usize) -> Result<Vec<WebhookEvent>>
```

### Database Constraints

Document constraints that affect application logic:

```rust
/// Size of the payload in bytes.
///
/// Must be between 1 and 10MB (10485760 bytes) per database CHECK constraint.
/// Even empty payloads are stored as size 1.
pub payload_size: i32,
```

## Testing Standards

### Test Naming

Tests describe behavior being verified:

```rust
#[tokio::test]
async fn webhook_ingestion_returns_200_with_event_id() {}

#[tokio::test]
async fn webhook_ingestion_rejects_invalid_hmac_signature() {}

#[tokio::test]
async fn delivery_worker_respects_circuit_breaker_open_state() {}
```

### Deterministic Testing

Always use `TestClock` for time-dependent operations:

```rust
#[tokio::test]
async fn exponential_backoff_timing_is_deterministic() {
    let env = TestEnv::new_shared().await?;

    // GOOD: Use test clock
    env.clock.advance(Duration::from_secs(1));

    // BAD: Non-deterministic real time
    // tokio::time::sleep(Duration::from_secs(1)).await;
}
```

### Property-Based Testing

Verify invariants with generated inputs:

```rust
proptest! {
    #[test]
    fn retry_count_never_exceeds_maximum(
        failures in 0u32..100,
        max_retries in 1u32..20
    ) {
        let attempts = simulate_retries(failures, max_retries);
        prop_assert!(attempts <= max_retries + 1);
    }
}
```

## Commit Message Format

Use Conventional Commits for automated changelog generation:

```
<type>(<scope>): <description>

[optional body]
```

**Types:**

- `feat`: New feature
- `fix`: Bug fix
- `perf`: Performance improvement
- `refactor`: Code change (no behavior change)
- `test`: Test changes
- `docs`: Documentation only
- `chore`: Build process, tools, dependencies

**Examples:**

```
feat(delivery): implement exponential backoff retry logic

fix(api): handle empty payload edge case

docs(style): update naming conventions

test(delivery): add property tests for retry bounds

perf(db): optimize event claiming query with composite index
```

A good commit message and description:

```
perf(db): optimize that thing again

Here is a short description of the commit. Problem was this,
solution was this.

- Changed x to y, because z
- Another relevant change
```
