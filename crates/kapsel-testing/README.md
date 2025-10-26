# kapsel-testing

Test infrastructure for deterministic database isolation and HTTP mocking.

## Quick Start

Start the test database:

```bash
docker-compose up -d postgres-test
```

Run tests:

```bash
cargo test
```

## Architecture

Two isolation patterns:

1. **Transaction rollback** (default) - Shared database with automatic rollback
2. **Isolated database** - Separate database per test with automatic cleanup

## Usage

### Transaction Isolation (Recommended)

```rust
#[tokio::test]
async fn test_webhook_delivery() {
    let env = TestEnv::new().await?;
    let mut tx = env.pool().begin().await?;

    let tenant_id = env.create_tenant_tx(&mut *tx, "test-tenant").await?;
    let endpoint_id = env.create_endpoint_tx(&mut *tx, tenant_id, "https://example.com").await?;

    // Test logic here

    // Automatic rollback on drop - no cleanup needed
}
```

### Isolated Database

For tests requiring committed data (API handlers, background workers):

```rust
#[tokio::test]
async fn test_api_endpoint() {
    let env = TestEnv::new_isolated().await?;
    let mut tx = env.pool().begin().await?;

    let tenant_id = env.create_tenant_tx(&mut *tx, "test-tenant").await?;
    tx.commit().await?; // Commit for API visibility

    let app = create_router(env.pool().clone());
    let response = app.oneshot(request).await?;

    // Automatic database cleanup on drop
}
```

## Helper Methods

### Database Setup

- `create_tenant_tx(executor, name)` - Create tenant within transaction
- `create_tenant_with_plan_tx(executor, name, plan)` - Create tenant with plan
- `create_endpoint_tx(executor, tenant_id, url)` - Create endpoint
- `create_endpoint_with_config_tx(...)` - Create endpoint with full config
- `create_api_key_tx(executor, tenant_id, name)` - Create API key

### Test Utilities

- `http_mock` - HTTP mock server for external APIs
- `clock` - Deterministic time control
- `pool()` - Database connection pool

## Configuration

Set in `.cargo/config.toml`:

```toml
[env]
DATABASE_URL = "postgresql://postgres:postgres@localhost:5433/kapsel_test"
```

Docker service in `docker-compose.yml`:

```yaml
postgres-test:
  image: postgres:16-alpine
  ports:
    - "5433:5432"
  tmpfs:
    - /var/lib/postgresql/data:size=2g
```

## Troubleshooting

**Connection refused**: Start postgres-test container with `docker-compose up -d postgres-test`

**Relation does not exist**: Migrations run automatically, but can be forced with:

```bash
sqlx migrate run --database-url $DATABASE_URL
```

**Too many connections**: Restart container with `docker-compose restart postgres-test`
