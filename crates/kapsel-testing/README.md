# Test Harness

Comprehensive testing infrastructure for Kapsel webhook reliability service. Provides deterministic testing, invariant validation, and property-based testing capabilities.

## Philosophy

The test suite IS the system specification. Every test represents an invariant that must hold true. We don't test to find bugs—we test to prove correctness through exhaustive invariant validation, deterministic simulation, and property-based exploration.

## Features

### Deterministic Time Control

```rust
use kapsel_testing::{TestEnv, Clock};
use std::time::Duration;

#[tokio::test]
async fn test_with_time_control() {
    let env = TestEnv::new().await?;

    // Advance time deterministically
    env.clock.advance(Duration::from_secs(60));

    // Sleep advances the test clock, not real time
    env.clock.sleep(Duration::from_secs(30)).await;
}
```

### Mock HTTP Server

```rust
use kapsel_testing::http::{MockServer, MockEndpoint};

#[tokio::test]
async fn test_webhook_delivery() {
    let mock = MockServer::start().await;

    // Configure mock to fail 3 times, then succeed
    mock.mock_sequence()
        .respond_with(503, "Service Unavailable")
        .respond_with(503, "Service Unavailable")
        .respond_with(503, "Service Unavailable")
        .respond_with_json(200, &json!({"status": "ok"}))
        .build()
        .await;
}
```

### Test Fixtures

```rust
use kapsel_testing::fixtures::{WebhookBuilder, scenarios};

// Build custom webhook
let webhook = WebhookBuilder::with_defaults()
    .json_body(&json!({"event": "payment.completed"}))
    .header("X-Stripe-Signature", "sig_123")
    .build();

// Use pre-built scenarios
let stripe_webhook = scenarios::stripe_webhook();
let (original, duplicate) = scenarios::duplicate_webhook();
```

### Invariant Testing

```rust
use kapsel_testing::{invariant_assertions, Invariants};

// Verify at-least-once delivery
invariant_assertions::assert_all_terminal(&events)?;

// Verify idempotency
invariant_assertions::assert_idempotent(&response1, &response2)?;

// Verify retry bounds
invariant_assertions::assert_retry_bounded(&event, max_retries)?;
```

### Property-Based Testing

```rust
use proptest::prelude::*;
use kapsel_testing::invariants::strategies;

proptest! {
    #[test]
    fn idempotency_always_holds(
        webhook in strategies::webhook_event_strategy()
    ) {
        let response1 = process(&webhook);
        let response2 = process(&webhook);
        prop_assert_eq!(response1.id, response2.id);
    }
}
```

### Scenario Testing

```rust
use kapsel_testing::ScenarioBuilder;

#[tokio::test]
async fn exponential_backoff_scenario() {
    let env = TestEnv::new().await?;

    ScenarioBuilder::new("exponential backoff")
        .ingest("endpoint_1", webhook)
        .inject_failure(FailureKind::Http500)
        .advance_time(Duration::from_secs(1))
        .expect_delivery(Duration::from_secs(5))
        .check_invariant(|env| {
            // Custom invariant check
            Ok(())
        })
        .run(&env)
        .await?;
}
```

## Core Components

### TestEnv

The main test environment containing all test infrastructure:

- `db`: PostgreSQL connection pool
- `clock`: Deterministic time control
- `http_mock`: Mock HTTP server
- `config`: Test configuration

### TestClock

Provides deterministic time control:

- `advance(duration)`: Advance test time
- `now()`: Get current test time
- `sleep(duration)`: Async sleep that advances test time

### MockServer

HTTP mock server for simulating external endpoints:

- `mock_endpoint()`: Configure single response
- `mock_sequence()`: Configure response sequence
- `received_requests()`: Get all received requests

### Fixtures

Pre-built test data generators:

- `WebhookBuilder`: Create custom webhooks
- `EndpointBuilder`: Create test endpoints
- `scenarios`: Common test scenarios (Stripe, GitHub, etc.)

### Invariants

System invariant validators:

- At-least-once delivery
- Idempotency guarantees
- Retry bounds
- Exponential backoff
- Circuit breaker correctness
- Tenant isolation
- FIFO ordering

## Database Testing

Tests use PostgreSQL in Docker with transaction-based isolation:

```rust
#[tokio::test]
async fn test_with_database() {
    let env = TestEnv::new().await?;

    // Start transaction (auto-rollback)
    let tx = env.transaction().await?;

    // Test operations...

    // Optionally commit instead of rollback
    tx.commit().await?;
}
```

## Performance Benchmarks

Run benchmarks with:

```bash
cargo bench --package kapsel_testing
```

Key metrics tracked:

- Ingestion latency p99 < 50ms
- Delivery throughput > 1000/sec
- Database query p99 < 5ms
- Memory per 1K webhooks < 200MB

## Best Practices

### Test Organization

```rust
// Unit tests: Pure logic, no I/O
#[test]
fn backoff_calculation() {
    // Test exponential backoff math
}

// Integration tests: Component boundaries
#[tokio::test]
async fn database_operations() {
    let env = TestEnv::new().await?;
    // Test with real database
}

// Property tests: Invariant validation
proptest! {
    #[test]
    fn invariant_always_holds(input in strategy()) {
        // Verify invariant for all inputs
    }
}

// Scenario tests: Multi-step workflows
#[tokio::test]
async fn complete_workflow() {
    ScenarioBuilder::new("workflow")
        // Build complex scenario
        .run(&env).await?;
}
```

### Deterministic Testing

Always use `TestClock` for time-dependent operations:

```rust
// BAD: Non-deterministic
tokio::time::sleep(Duration::from_secs(1)).await;

// GOOD: Deterministic
env.clock.sleep(Duration::from_secs(1)).await;
```

### Invariant Validation

Add invariant checks to all tests:

```rust
ScenarioBuilder::new("test")
    .ingest(endpoint, webhook)
    .check_invariant(|env| {
        // Verify system invariants hold
        invariant_assertions::assert_no_lost_events(&ingested, &current)?;
        Ok(())
    })
    .run(&env).await?;
```

## Examples

### Golden Sample Test

```rust
#[tokio::test]
async fn golden_webhook_delivery() {
    let env = TestEnv::new().await?;
    let mock = MockServer::start().await;

    // Configure to fail 3 times then succeed
    mock.mock_sequence()
        .respond_with(503, "Unavailable")
        .respond_with(503, "Unavailable")
        .respond_with(503, "Unavailable")
        .respond_with(200, "OK")
        .build()
        .await;

    // Ingest webhook
    let event_id = env.ingest(webhook).await?;

    // Process with exponential backoff
    for delay in [1, 2, 4] {
        env.clock.advance(Duration::from_secs(delay));
        env.process_pending().await?;
    }

    // Verify delivery
    assert_eq!(env.get_status(event_id).await?, "delivered");

    // Verify timing
    let total = env.clock.elapsed();
    assert_eq!(total, Duration::from_secs(7)); // 1+2+4
}
```

### Property Test Example

```rust
proptest! {
    #[test]
    fn retry_never_exceeds_max(
        max_retries in 1u32..20,
        failures in 0u32..100
    ) {
        let mut event = create_event();

        for _ in 0..failures {
            if should_retry(&event, max_retries) {
                event.attempt_count += 1;
            }
        }

        prop_assert!(event.attempt_count <= max_retries + 1);
    }
}
```

## Configuration

### Environment Variables

- `RUST_LOG`: Set log level (default: `warn,kapsel=debug`)
- `TEST_DATABASE_URL`: Override test database URL
- `TEST_SEED`: Set deterministic seed for tests

### Test Features

- `docker`: Enable PostgreSQL via Docker (default)
- `chaos`: Enable chaos testing capabilities
- `bench`: Enable benchmark utilities

## Troubleshooting

### Connection Refused

Ensure Docker is running and PostgreSQL container is up:

```bash
docker ps | grep kapsel-postgres-test
```

### Slow Tests

PostgreSQL tests should complete in ~300-400ms. If slower:

- Check Docker resources
- Increase Docker memory allocation
- Use transaction-based tests for faster cleanup

### Flaky Tests

All tests must be deterministic. If experiencing flakes:

- Use `TestClock` instead of real time
- Mock all external dependencies
- Set deterministic seed: `TEST_SEED=12345`

## License

MIT OR Apache-2.0
