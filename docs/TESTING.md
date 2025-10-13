# Testing Guide

Kapsel uses a comprehensive testing strategy designed for both local development and CI environments.

## Test Categories

### Unit Tests

Fast, isolated tests with no external dependencies.

```bash
cargo test --lib
```

### Integration Tests

Complete system tests using PostgreSQL database.

```bash
cargo test --test health_check_test
```

## Database Backend

### PostgreSQL (Docker)

- Full PostgreSQL compatibility
- Requires Docker daemon running
- Used for all tests (unit and integration)
- Ensures tests run against production database type

## Environment Setup

### Local Development

```bash
# Ensure PostgreSQL test container is running
docker ps | grep kapsel-postgres-test

# Run all tests
cargo test

# Run specific test
cargo test test_environment_initializes

# Run with tracing enabled
RUST_LOG=debug cargo test
```

### CI/Production Testing

```bash
# Tests use DATABASE_URL environment variable
# Defaults to port 5432 for CI
cargo test
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

- `DATABASE_URL` - PostgreSQL connection string (auto-detected from container)
- `RUST_LOG=debug` - Enable debug logging
- `CI=true` - CI mode (uses default port 5432)

### Features

- `docker` - PostgreSQL with testcontainers (required for integration tests)

## Troubleshooting

### "Connection refused" errors

Check Docker daemon is running and PostgreSQL test container is up:

```bash
docker info
docker ps | grep kapsel-postgres-test
```

### Slow test startup

PostgreSQL tests typically complete in ~300-400ms per test. If slower:
- Check Docker resources
- Ensure no other heavy processes are running
- Consider increasing Docker memory allocation

### Database schema mismatches

Tests create fresh schemas automatically. No manual cleanup needed.

## Performance Guidelines

- PostgreSQL tests: < 500ms per test
- Use `#[ignore]` for slow integration tests
- Prefer unit tests for business logic
- Use integration tests for API contracts
- Database tests include schema creation and cleanup

## CI Configuration

Tests run with PostgreSQL backend for all test types.

Example GitHub Actions:

```yaml
- name: Run unit tests
  run: cargo test --lib

- name: Run integration tests
  run: cargo test
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

PostgreSQL tests use persistent container - connect directly for inspection:

```bash
psql -h localhost -U postgres -d kapsel_test
```

### Test timing issues

Use `env.advance_time()` instead of `tokio::time::sleep()` for deterministic timing.
