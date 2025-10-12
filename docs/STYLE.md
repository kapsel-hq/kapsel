# Style Guide

This document defines our coding standards. Every line follows these rules. No exceptions.

## Philosophy

Code is written once and read hundreds of times. Optimize for the reader, not the writer. In 5 years, the code should still be obvious.

- **Names tell the story** - No comments needed if names are clear
- **Consistency over perfection** - Same patterns everywhere
- **Explicit over clever** - Boring code is good code
- **WHY not WHAT** - Comments explain decisions, not mechanics

## Naming

### Functions

```rust
// Actions use verb_object pattern (TIGERSTYLE)
pub fn deliver_webhook(webhook: &Webhook) -> Result<()>
pub fn validate_signature(payload: &[u8]) -> Result<()>
pub fn insert_event(event: Event) -> Result<EventId>

// Predicates use is_, has_, can_
pub fn is_valid(&self) -> bool
pub fn has_expired(&self) -> bool
pub fn can_retry(&self) -> bool

// Conversions follow Rust idioms
pub fn as_bytes(&self) -> &[u8]        // Borrowed view
pub fn to_string(&self) -> String      // Allocating conversion
pub fn into_inner(self) -> T           // Consuming conversion

// Fallible operations use try_
pub fn try_parse(input: &str) -> Result<Self>
pub fn try_deliver(&self) -> Result<()>

// Optional operations that require logic use maybe_
pub fn maybe_extract_key(&self) -> Option<Key>
pub fn maybe_next_retry(&self) -> Option<Instant>

// NO get_/set_ prefix nonsense
// BAD:  get_status(), set_status()
// GOOD: status(), update_status() or with_status()
```

### Types

```rust
// Types are PascalCase and descriptive
pub struct WebhookEvent { }
pub struct RetryPolicy { }

// Newtypes prevent primitive obsession
pub struct EventId(Uuid);
pub struct TenantId(Uuid);

// Enums have clear, simple variants
pub enum Status {
    Pending,
    Delivered,
    Failed,
}
```

### Variables

```rust
// Descriptive, no abbreviations
let webhook_event = ...;  // NOT: evt, wh_evt, webhookEvt
let retry_count = ...;     // NOT: cnt, retries
let endpoint_url = ...;    // NOT: url, ep_url

// Loop indices only when necessary
for (index, item) in items.iter().enumerate() { }
```

## Error Handling

```rust
// Library errors with thiserror
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("webhook {id} not found")]
    NotFound { id: EventId },

    #[error("delivery failed after {attempts} attempts")]
    DeliveryExhausted { attempts: u32 },
}

// Application errors with context
use anyhow::{Context, Result};

let webhook = fetch_webhook(id)
    .await
    .context("failed to fetch webhook")?;

// Never panic in production
// No unwrap(), expect(), panic!(), unreachable!()
// No array indexing without bounds check
```

## Documentation

### When to Document

Document:

- Module purpose and architecture
- Public API functions
- Complex algorithms
- Non-obvious decisions
- Performance trade-offs

Don't document:

- Private helper functions (unless complex)
- Obvious getters/setters
- Standard trait implementations
- What the code already says clearly

### Module Documentation

```rust
//! Webhook delivery engine.
//!
//! Handles reliable delivery with exponential backoff and circuit breakers.
//! Workers claim events using PostgreSQL's SKIP LOCKED for distributed processing.

pub mod delivery;
```

### Function Documentation

```rust
/// Delivers a webhook to its configured endpoint.
///
/// Applies exponential backoff between retries. Opens circuit breaker
/// after repeated failures to prevent cascade failures.
///
/// # Errors
///
/// Returns `Error::NetworkTimeout` if the request times out.
/// Returns `Error::CircuitOpen` if the endpoint's circuit breaker is open.
pub async fn deliver(webhook: &Webhook) -> Result<Response
> {
    // Implementation
}
```

### Inline Comments

```rust
// Comments explain WHY, not WHAT

// BAD: Increment counter
counter += 1;

// GOOD: Account for zero-indexing in user display
counter += 1;

// GOOD: Jitter prevents thundering herd when multiple
// workers retry simultaneously
let jitter = rng.gen_range(0..base_delay / 4);

// Complex sections get a brief explanation
// PostgreSQL's SKIP LOCKED allows multiple workers to claim
// rows without blocking each other, giving us free work distribution
let query = sqlx::query!(
    "SELECT * FROM events
     WHERE status = 'pending'
     LIMIT $1
     FOR UPDATE SKIP LOCKED",
    batch_size
);
```

## Code Organization

### Imports

```rust
// Standard library
use std::collections::HashMap;
use std::time::Duration;

// External crates
use anyhow::{Context, Result};
use tokio::sync::mpsc;

// Internal modules
use crate::models::{Webhook, Event};
use crate::error::Error;
```

### Module Structure

Keep modules focused. One concept per module. Deep modules hide complexity.

```
src/
├── lib.rs          // Public API only
├── config.rs       // Configuration
├── error.rs        // Error types
├── models/         // Domain models
├── handlers/       // HTTP handlers
├── delivery/       // Delivery engine
└── persistence/    // Database layer
```

## Patterns

### Builder Pattern

Use when construction is complex:

```rust
let webhook = Webhook::builder()
    .endpoint(endpoint_id)
    .payload(bytes)
    .build()?;
```

### Newtype Pattern

Prevent primitive obsession:

```rust
pub struct EventId(Uuid);

impl EventId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}
```

### Repository Pattern

Isolate data access:

```rust
#[async_trait]
pub trait EventStore {
    async fn insert(&self, event: Event) -> Result<EventId>;
    async fn find(&self, id: EventId) -> Result<Option<Event>>;
    async fn claim_pending(&self, limit: usize) -> Result<Vec<Event>>;
}
```

## Testing

### Test Names

Test names describe behavior:

```rust
#[test]
fn expired_webhook_cannot_be_retried() { }

#[test]
fn circuit_breaker_opens_after_threshold() { }

// NOT: test_webhook(), test_1(), webhook_test()
```

### Test Organization

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn webhook_without_endpoint_fails_validation() {
        // Test focused on one behavior
    }
}
```

## Performance

### Zero-Cost Principles

```rust
// Use borrows over clones
fn process(data: &str) -> Result<()>  // NOT: (data: String)

// Use Bytes for payloads
pub struct Webhook {
    payload: Bytes,  // Cheap to clone, reference counted
}

// Avoid allocations in hot paths
// Pre-allocate collections when size is known
let mut events = Vec::with_capacity(100);
```

### Async Patterns

```rust
// Always use biased select! for shutdown
tokio::select! {
    biased;

    _ = shutdown.recv() => break,
    event = queue.recv() => process(event).await?,
}

// Track spawned tasks
let mut tasks = JoinSet::new();
tasks.spawn(async move { worker.run().await });

// Clean shutdown
while let Some(result) = tasks.join_next().await {
    result?;
}
```

## Things We Don't Do

### No Noise

```rust
// NO separator comments
// ============
// TESTS
// ============

// NO obvious comments
let count = 0; // Initialize count to zero

// NO commented-out code
// let old_way = do_something();

// NO get_/set_ prefixes
impl Webhook {
    // BAD
    pub fn get_status(&self) -> Status
    pub fn set_status(&mut self, status: Status)

    // GOOD
    pub fn status(&self) -> Status
    pub fn update_status(&mut self, status: Status)
}
```

### No Premature Abstractions

```rust
// Don't create traits for single implementations
// Don't make everything generic
// Don't abstract until you have 2+ use cases
```

### No Clever Code

```rust
// BAD: Clever one-liner
let x = a.zip(b).fold(0, |a, (x, y)| a + x * y);

// GOOD: Clear intent
let mut sum = 0;
for (val_a, val_b) in a.iter().zip(b.iter()) {
    sum += val_a * val_b;
}
```

## Enforcement

These rules are enforced by:

1. **rustfmt** - Formatting (no config debates)
2. **clippy** - Linting with pedantic mode
3. **CI pipeline** - Automated checks
4. **Code review** - Human verification

Non-conforming code is not merged. Fix it or delete it.

## Final Word

Good code looks boring. It's obvious what it does, why it does it, and how it handles failure. If you need to think hard to understand a piece of code, it's wrong.

Write code for the maintainer who will curse your name at 3 AM when the system is down. That maintainer might be you.
