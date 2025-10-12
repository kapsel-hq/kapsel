# Testing Guide

Kapsel uses a multi-tier testing strategy designed for both local development and CI environments.

## Test Categories

### Unit Tests

Fast, isolated tests with no external dependencies.

```bash
cargo test --lib
```

### Integration Tests

Business logic tests using in-memory SQLite database.

```bash
cargo test --test health_check_test
```

### Full Integration Tests

Complete system tests using PostgreSQL containers.

```bash
cargo test --features docker --test health_check_test
```

## Database Backends

### SQLite (Default)

- In-memory database
- No Docker required
- Fast startup and execution
- Perfect for TDD and local development

### PostgreSQL (Docker)

- Full PostgreSQL compatibility
- Requires Docker daemon
- Used in CI and for database-specific features
- Activated with `--features docker`

## Environment Setup

### Local Development

```bash
# Run tests with SQLite (default)
cargo test

# Run specific test
cargo test test_environment_initializes

# Run with tracing enabled
RUST_LOG=debug cargo test
```

### CI/Production Testing

```bash
# Force PostgreSQL backend
KAPSEL_TEST_BACKEND=postgres cargo test --features docker

# Run in CI mode
CI=true cargo test --features docker
```

## Test Structure

Tests are organized in:

- `tests/` - Integration tests
- `crates/*/src/` - Unit tests alongside code
- `crates/test-harness/` - Shared testing infrastructure

## Writing Tests

### Basic Integration Test

```rust
use test_harness::TestEnv;

#[tokio::test]
async fn test_webhook_delivery() {
    let env = TestEnv::new().await.expect("Failed to create test environment");

    // Test logic here
}
```

### Using Test Fixtures

```rust
use test_harness::fixtures::WebhookBuilder;

let webhook = WebhookBuilder::with_defaults()
    .json_body(serde_json::json!({
        "event": "test.created"
    }))
    .build();
```

### Database Testing

```rust
// Transaction-based isolation
let tx = env.transaction().await?;
// Test operations - automatically rolls back
```

### Scenario Testing

```rust
use test_harness::ScenarioBuilder;

ScenarioBuilder::new("webhook retry scenario")
    .ingest("endpoint_1", payload)
    .inject_failure(FailureKind::Http500)
    .advance_time(Duration::from_secs(60))
    .expect_delivery(Duration::from_secs(5))
    .run(&env)
    .await?;
```

## Test Configuration

### Environment Variables

- `KAPSEL_TEST_BACKEND=postgres` - Force PostgreSQL
- `RUST_LOG=debug` - Enable debug logging
- `CI=true` - CI mode (uses PostgreSQL automatically)

### Features

- `sqlite` - SQLite backend (default)
- `docker` - PostgreSQL with testcontainers

## Troubleshooting

### "Docker feature not enabled"

Solution: Add `--features docker` or use SQLite mode (default).

### "Connection refused" errors

Check Docker daemon is running:

```bash
docker info
```

### Slow test startup

Use SQLite for development:

```bash
cargo test  # SQLite is default
```

### Database schema mismatches

Tests create fresh databases automatically. No manual cleanup needed.

## Performance Guidelines

- SQLite tests: < 100ms per test
- PostgreSQL tests: < 500ms per test
- Use `#[ignore]` for slow integration tests
- Prefer unit tests for business logic
- Use integration tests for API contracts

## CI Configuration

Tests run in three stages:

1. **Unit tests** - No database required
2. **SQLite integration** - Fast integration tests
3. **PostgreSQL integration** - Full database compatibility

Example GitHub Actions:

```yaml
- name: Run unit tests
  run: cargo test --lib

- name: Run SQLite integration tests
  run: cargo test

- name: Run PostgreSQL integration tests
  run: cargo test --features docker
```

## Best Practices

### Test Isolation

- Each test gets fresh database
- Transactions auto-rollback
- No test interdependencies

### Assertions

Use the provided assertion helpers:

```rust
use test_harness::database::assertions;

assertions::assert_event_status(&pool, event_id, "delivered").await?;
```

### Deterministic Testing

- Use `TestClock` for time manipulation
- Mock external HTTP calls
- Seed deterministic test data

### Error Handling

Tests should use `expect()` with descriptive messages:

```rust
let result = operation().await.expect("Operation should succeed");
```

## Debugging Tests

### Enable SQL logging

```bash
RUST_LOG=sqlx=debug cargo test
```

### Inspect test database

SQLite tests use in-memory databases - use logging for inspection.
PostgreSQL tests spin up containers - check Docker logs.

### Test timing issues

Use `env.advance_time()` instead of `tokio::time::sleep()` for deterministic timing.
