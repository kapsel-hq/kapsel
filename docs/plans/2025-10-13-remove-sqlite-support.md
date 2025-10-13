# Remove SQLite Support Implementation Plan

> **For Claude:** Use `${SUPERPOWERS_SKILLS_ROOT}/skills/collaboration/executing-plans/SKILL.md` to implement this plan task-by-task.

**Goal:** Remove SQLite database support from test-harness, simplifying to PostgreSQL-only for all tests.

**Architecture:** Currently test-harness maintains dual database support (SQLite for fast local tests, PostgreSQL for integration tests). This creates maintenance burden with duplicate schemas, dual query implementations, and abstraction layers. We'll remove SQLite entirely, use PostgreSQL for all tests, and delete ~300 lines of abstraction code.

**Tech Stack:** Rust, sqlx (PostgreSQL only), tokio, Docker PostgreSQL

**Rationale:**
- Eliminates schema drift (TEXT vs TIMESTAMPTZ, JSON vs JSONB, String vs UUID)
- Removes dual query implementations at every database interaction
- Tests run against production database type, catching Postgres-specific bugs
- Single test iteration cost: 370ms (acceptable for development workflow)
- Simplifies codebase before building more features on top

---

## Task 1: Update dependencies to remove SQLite

**Files:**
- Modify: `crates/test-harness/Cargo.toml:19`

**Step 1: Remove SQLite feature from sqlx dependency**

In `crates/test-harness/Cargo.toml`, change line 19:

```toml
# Before:
sqlx = { workspace = true, features = ["sqlite", "runtime-tokio-rustls"] }

# After:
sqlx = { workspace = true, features = ["runtime-tokio-rustls"] }
```

**Step 2: Verify dependency change compiles**

Run: `cargo check --package test-harness`

Expected: Will fail with compilation errors about missing Sqlite types (expected, we'll fix in next task)

**Step 3: Commit dependency change**

```bash
git add crates/test-harness/Cargo.toml
git commit -m "refactor(test-harness): remove sqlite dependency from sqlx"
```

---

## Task 2: Remove SQLite schema and enum variants

**Files:**
- Modify: `crates/test-harness/src/database.rs:1-610`

**Step 1: Remove SQLite-specific imports**

Remove from imports (around line 8):
```rust
// Remove this:
use sqlx::{postgres::PgConnectOptions, PgPool, Pool, Postgres, Sqlite};

// Replace with:
use sqlx::{postgres::PgConnectOptions, PgPool, Postgres};
```

**Step 2: Remove TestDatabase::Sqlite variant**

In `TestDatabase` enum (line 12-15), remove the Sqlite variant:

```rust
// Before:
pub enum TestDatabase {
    Sqlite { pool: Pool<Sqlite> },
    Postgres { pool: PgPool, database_name: String },
}

// After:
pub struct TestDatabase {
    pool: PgPool,
    database_name: String,
}
```

**Step 3: Simplify TestDatabase::new() method**

Replace the `new()` method (lines 18-28):

```rust
impl TestDatabase {
    /// Creates new test database using PostgreSQL.
    pub async fn new() -> Result<Self> {
        Self::new_postgres().await
    }
```

**Step 4: Remove new_sqlite() method entirely**

Delete lines 30-40 (the entire `new_sqlite()` method).

**Step 5: Update new_postgres() to return struct instead of enum**

Replace `new_postgres()` method (lines 42-72):

```rust
    /// Creates PostgreSQL database connection using existing postgres-test
    /// container.
    pub async fn new_postgres() -> Result<Self> {
        let database_name = "kapsel_test".to_string();

        // Read port from DATABASE_URL or default to 5432 (CI default)
        let port = std::env::var("DATABASE_URL")
            .ok()
            .and_then(|url| {
                url.split(':')
                    .nth(4)
                    .and_then(|port_str| port_str.split('/').next())
                    .and_then(|port_str| port_str.parse::<u16>().ok())
            })
            .unwrap_or(5432);

        let connect_options = PgConnectOptions::new()
            .host("127.0.0.1")
            .port(port)
            .username("postgres")
            .password("postgres")
            .database(&database_name);

        let pool = sqlx::PgPool::connect_with(connect_options)
            .await
            .context("Failed to connect to PostgreSQL test database")?;

        let db = Self { pool, database_name };
        db.run_migrations().await?;
        Ok(db)
    }
```

**Step 6: Simplify pool() method**

Replace `pool()` method (lines 74-80):

```rust
    /// Returns connection pool for the underlying database.
    pub fn pool(&self) -> PgPool {
        self.pool.clone()
    }
```

**Step 7: Simplify run_migrations() method**

Replace `run_migrations()` method (lines 82-88):

```rust
    /// Runs database migrations for PostgreSQL.
    async fn run_migrations(&self) -> Result<()> {
        run_postgres_migrations(&self.pool).await
    }
```

**Step 8: Simplify seed_test_data() method**

Replace `seed_test_data()` method (lines 90-96):

```rust
    /// Seeds test data into database.
    pub async fn seed_test_data(&self) -> Result<()> {
        let pool = self.pool();
        seed_endpoints(&pool).await?;
        seed_webhook_events(&pool).await?;
        Ok(())
    }
}
```

**Step 9: Verify changes compile so far**

Run: `cargo check --package test-harness`

Expected: Still fails, but with fewer errors (DatabasePool enum still needs fixing)

**Step 10: Commit structural changes**

```bash
git add crates/test-harness/src/database.rs
git commit -m "refactor(test-harness): convert TestDatabase from enum to struct"
```

---

## Task 3: Remove DatabasePool abstraction enum

**Files:**
- Modify: `crates/test-harness/src/database.rs:99-142`

**Step 1: Replace DatabasePool enum with type alias**

Replace the entire `DatabasePool` enum and impl (lines 99-142):

```rust
/// Database pool type alias.
pub type DatabasePool = PgPool;
```

**Step 2: Remove DatabasePoolRef enum**

Delete the entire `DatabasePoolRef` enum (lines 144-148).

**Step 3: Verify changes compile**

Run: `cargo check --package test-harness`

Expected: Still fails (DatabaseTransaction enum needs fixing)

**Step 4: Commit DatabasePool simplification**

```bash
git add crates/test-harness/src/database.rs
git commit -m "refactor(test-harness): replace DatabasePool enum with type alias"
```

---

## Task 4: Remove DatabaseTransaction abstraction enum

**Files:**
- Modify: `crates/test-harness/src/database.rs:150-172`

**Step 1: Replace DatabaseTransaction enum with type alias**

Replace the entire `DatabaseTransaction` enum and impl (lines 150-172):

```rust
/// Transaction type alias.
pub type DatabaseTransaction = sqlx::Transaction<'static, Postgres>;
```

**Step 2: Verify changes compile**

Run: `cargo check --package test-harness`

Expected: Still fails (helper functions need fixing)

**Step 3: Commit DatabaseTransaction simplification**

```bash
git add crates/test-harness/src/database.rs
git commit -m "refactor(test-harness): replace DatabaseTransaction enum with type alias"
```

---

## Task 5: Update setup_test_database() helper

**Files:**
- Modify: `crates/test-harness/src/database.rs:174-183`

**Step 1: Simplify setup_test_database() function**

Replace function (lines 174-183):

```rust
/// Sets up test database and returns connection pool.
pub async fn setup_test_database() -> Result<PgPool> {
    let db = TestDatabase::new().await?;
    let pool = db.pool();

    #[allow(clippy::disallowed_methods)]
    Box::leak(Box::new(db));

    Ok(pool)
}
```

**Step 2: Verify changes compile**

Run: `cargo check --package test-harness`

Expected: Still fails (migration and seed functions need fixing)

**Step 3: Commit helper function update**

```bash
git add crates/test-harness/src/database.rs
git commit -m "refactor(test-harness): simplify setup_test_database helper"
```

---

## Task 6: Delete SQLite migration function

**Files:**
- Modify: `crates/test-harness/src/database.rs:185-285`

**Step 1: Delete entire run_sqlite_migrations() function**

Delete lines 185-285 (the entire `run_sqlite_migrations()` function with all table creation and indexes).

**Step 2: Verify deletion**

Run: `cargo check --package test-harness`

Expected: Still fails (seed functions still have dual implementations)

**Step 3: Commit SQLite migration deletion**

```bash
git add crates/test-harness/src/database.rs
git commit -m "refactor(test-harness): delete sqlite migration function"
```

---

## Task 7: Simplify seed_endpoints() function

**Files:**
- Modify: `crates/test-harness/src/database.rs:395-432`

**Step 1: Replace seed_endpoints() with PostgreSQL-only implementation**

Replace entire function (lines 395-432):

```rust
/// Seeds test endpoints.
async fn seed_endpoints(pool: &PgPool) -> Result<()> {
    let tenant_id = Uuid::new_v4();

    sqlx::query(
        r#"
        INSERT INTO endpoints (tenant_id, name, url, signing_secret, max_retries)
        VALUES
            ($1, 'test-endpoint-1', 'https://example.com/webhook', 'secret123', 3),
            ($1, 'test-endpoint-2', 'https://example.org/hook', 'secret456', 5)
        ON CONFLICT DO NOTHING
        "#,
    )
    .bind(tenant_id)
    .execute(pool)
    .await?;

    Ok(())
}
```

**Step 2: Verify changes compile**

Run: `cargo check --package test-harness`

Expected: Still fails (seed_webhook_events needs fixing)

**Step 3: Commit seed_endpoints simplification**

```bash
git add crates/test-harness/src/database.rs
git commit -m "refactor(test-harness): simplify seed_endpoints to postgres-only"
```

---

## Task 8: Simplify seed_webhook_events() function

**Files:**
- Modify: `crates/test-harness/src/database.rs:434-487`

**Step 1: Replace seed_webhook_events() with PostgreSQL-only implementation**

Replace entire function (lines 434-487):

```rust
/// Seeds test webhook events.
async fn seed_webhook_events(pool: &PgPool) -> Result<()> {
    let endpoint: (Uuid, Uuid) =
        sqlx::query_as("SELECT id, tenant_id FROM endpoints LIMIT 1")
            .fetch_one(pool)
            .await?;

    sqlx::query(
        r#"
        INSERT INTO webhook_events (
            tenant_id, endpoint_id, source_event_id, idempotency_strategy,
            status, headers, body, content_type
        )
        VALUES
            ($1, $2, 'test-event-1', 'header', 'pending', '{}', 'test body 1', 'text/plain'),
            ($1, $2, 'test-event-2', 'header', 'delivered', '{}', 'test body 2', 'text/plain')
        ON CONFLICT DO NOTHING
        "#,
    )
    .bind(endpoint.1)
    .bind(endpoint.0)
    .execute(pool)
    .await?;

    Ok(())
}
```

**Step 2: Verify changes compile**

Run: `cargo check --package test-harness`

Expected: Still fails (assertion helpers need fixing)

**Step 3: Commit seed_webhook_events simplification**

```bash
git add crates/test-harness/src/database.rs
git commit -m "refactor(test-harness): simplify seed_webhook_events to postgres-only"
```

---

## Task 9: Simplify database assertion helpers

**Files:**
- Modify: `crates/test-harness/src/database.rs:489-564`

**Step 1: Simplify assert_event_status() function**

Replace entire function (lines 495-528):

```rust
    /// Asserts event exists with expected status.
    pub async fn assert_event_status(
        pool: &PgPool,
        event_id: &str,
        expected_status: &str,
    ) -> Result<(), String> {
        let event_uuid =
            Uuid::parse_str(event_id).map_err(|e| format!("Invalid UUID: {}", e))?;

        let status: Option<String> =
            sqlx::query_scalar("SELECT status FROM webhook_events WHERE id = $1")
                .bind(event_uuid)
                .fetch_optional(pool)
                .await
                .map_err(|e| format!("Database query failed: {}", e))?;

        match status {
            Some(actual) if actual == expected_status => Ok(()),
            Some(actual) => Err(format!(
                "Event {} has status '{}', expected '{}'",
                event_id, actual, expected_status
            )),
            None => Err(format!("Event {} not found", event_id)),
        }
    }
```

**Step 2: Simplify assert_delivery_attempts() function**

Replace entire function (lines 530-563):

```rust
    /// Asserts delivery attempt count for event.
    pub async fn assert_delivery_attempts(
        pool: &PgPool,
        event_id: &str,
        expected_count: i64,
    ) -> Result<(), String> {
        let event_uuid =
            Uuid::parse_str(event_id).map_err(|e| format!("Invalid UUID: {}", e))?;

        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM delivery_attempts WHERE event_id = $1")
                .bind(event_uuid)
                .fetch_one(pool)
                .await
                .map_err(|e| format!("Database query failed: {}", e))?;

        if count != expected_count {
            return Err(format!(
                "Event {} has {} delivery attempts, expected {}",
                event_id, count, expected_count
            ));
        }

        Ok(())
    }
}
```

**Step 3: Update module imports**

At the top of the assertions module (around line 490-493), update:

```rust
/// Database test assertions.
pub mod assertions {
    use uuid::Uuid;

    use super::PgPool;
```

**Step 4: Verify changes compile**

Run: `cargo check --package test-harness`

Expected: Still fails (test module needs fixing)

**Step 5: Commit assertion helper simplification**

```bash
git add crates/test-harness/src/database.rs
git commit -m "refactor(test-harness): simplify assertion helpers to postgres-only"
```

---

## Task 10: Update database.rs test module

**Files:**
- Modify: `crates/test-harness/src/database.rs:566-611`

**Step 1: Simplify database_setup_succeeds test**

Replace test (lines 570-584):

```rust
    #[tokio::test]
    async fn database_setup_succeeds() {
        let pool = setup_test_database().await.unwrap();

        let result = sqlx::query_scalar::<_, i32>("SELECT 1")
            .fetch_one(&pool)
            .await
            .unwrap();

        assert_eq!(result, 1);
    }
```

**Step 2: Simplify migrations_create_tables test**

Replace test (lines 586-610):

```rust
    #[tokio::test]
    async fn migrations_create_tables() {
        let pool = setup_test_database().await.unwrap();

        let tables: Vec<String> = sqlx::query_scalar(
            "SELECT table_name FROM information_schema.tables
             WHERE table_schema = 'public' ORDER BY table_name",
        )
        .fetch_all(&pool)
        .await
        .unwrap();

        assert!(tables.contains(&"webhook_events".to_string()));
        assert!(tables.contains(&"delivery_attempts".to_string()));
        assert!(tables.contains(&"endpoints".to_string()));
        assert!(tables.contains(&"tenants".to_string()));
    }
}
```

**Step 3: Run test-harness tests to verify**

Run: `cargo test --package test-harness`

Expected: All tests pass

**Step 4: Commit test module updates**

```bash
git add crates/test-harness/src/database.rs
git commit -m "refactor(test-harness): simplify database tests to postgres-only"
```

---

## Task 11: Update lib.rs public API

**Files:**
- Modify: `crates/test-harness/src/lib.rs`

**Step 1: Check lib.rs for DatabasePool exports**

Run: `cat crates/test-harness/src/lib.rs`

**Step 2: Verify the exported types are correct**

The `DatabasePool` type alias should now be exported correctly. No changes needed if it was already re-exported correctly.

**Step 3: Run full test-harness check**

Run: `cargo check --package test-harness`

Expected: Clean compilation with no errors

**Step 4: Run full test-harness tests**

Run: `cargo test --package test-harness`

Expected: All tests pass

**Step 5: Commit if any changes made**

```bash
# Only if lib.rs was modified
git add crates/test-harness/src/lib.rs
git commit -m "refactor(test-harness): update public API exports"
```

---

## Task 12: Update integration tests to use PgPool directly

**Files:**
- Modify: `tests/test_infrastructure.rs`
- Modify: `tests/webhook_ingestion_test.rs`

**Step 1: Check test files for DatabasePool usage**

Run: `grep -n "DatabasePool" tests/*.rs`

**Step 2: Update test_infrastructure.rs imports**

If the file imports `DatabasePool`, ensure it understands it's now `PgPool`:

```rust
use test_harness::{setup_test_database, TestEnv};
// DatabasePool is now just PgPool, so no special import needed
```

**Step 3: Update webhook_ingestion_test.rs imports**

Check and update imports similarly to use `PgPool` type where needed.

**Step 4: Run integration tests**

Run: `cargo test --test test_infrastructure`

Expected: All tests pass

Run: `cargo test --test webhook_ingestion_test`

Expected: All tests pass

**Step 5: Commit integration test updates**

```bash
git add tests/
git commit -m "refactor(tests): update integration tests for postgres-only database"
```

---

## Task 13: Update documentation

**Files:**
- Modify: `crates/test-harness/src/database.rs:1-6` (module doc comment)
- Modify: `docs/TESTING.md`

**Step 1: Update database.rs module documentation**

Replace module doc comment (lines 1-6):

```rust
//! Database testing utilities.
//!
//! Provides isolated test databases using PostgreSQL.
//! Requires Docker with kapsel-postgres-test container running.
//!
//! Tests automatically connect to PostgreSQL on the port specified in
//! DATABASE_URL environment variable (defaults to 5432 for CI).
```

**Step 2: Read current TESTING.md**

Run: `cat docs/TESTING.md`

**Step 3: Update TESTING.md to remove SQLite references**

Find and update sections mentioning SQLite:
- Remove "SQLite (in-memory)" references
- Update to say "PostgreSQL only"
- Ensure Docker setup instructions are clear
- Update environment variable documentation

**Step 4: Verify documentation reads correctly**

Run: `cat docs/TESTING.md | grep -i sqlite`

Expected: No matches (all SQLite references removed)

**Step 5: Commit documentation updates**

```bash
git add crates/test-harness/src/database.rs docs/TESTING.md
git commit -m "docs: update testing documentation for postgres-only approach"
```

---

## Task 14: Run full test suite and verify

**Files:**
- None (verification only)

**Step 1: Clean build**

Run: `cargo clean`

**Step 2: Run full workspace check**

Run: `cargo check --workspace`

Expected: Clean compilation with no errors

**Step 3: Run full test suite**

Run: `cargo test --workspace`

Expected: All tests pass

**Step 4: Verify test timing**

Run: `time cargo test --package test-harness`

Expected: Completes in ~3-4 seconds (similar to before)

**Step 5: Run CI checks locally**

Run: `cargo fmt --all --check`

Expected: Pass

Run: `cargo clippy --all-targets --all-features -- -D warnings`

Expected: Pass

**Step 6: Commit verification if any issues found and fixed**

```bash
# Only if fixes were needed
git add .
git commit -m "fix: address clippy warnings after sqlite removal"
```

---

## Task 15: Update Cargo.toml metadata

**Files:**
- Modify: `crates/test-harness/Cargo.toml:32`

**Step 1: Update package description comment**

In `crates/test-harness/Cargo.toml`, update the comment at line 32:

```toml
# Before:
# Database testing - supports both SQLite (in-memory) and PostgreSQL (Docker)

# After:
# Database testing - PostgreSQL via Docker
```

**Step 2: Verify Cargo.toml is valid**

Run: `cargo check --package test-harness`

Expected: Clean compilation

**Step 3: Commit Cargo.toml metadata update**

```bash
git add crates/test-harness/Cargo.toml
git commit -m "docs(test-harness): update cargo metadata for postgres-only"
```

---

## Task 16: Final verification and summary

**Files:**
- None (verification only)

**Step 1: Count deleted lines**

Run: `git diff --stat master..HEAD`

Expected: Significant net negative line count (~300 lines deleted)

**Step 2: Review commit history**

Run: `git log --oneline master..HEAD`

Expected: 15+ commits with clear messages

**Step 3: Run full test suite one final time**

Run: `cargo test --workspace --no-fail-fast`

Expected: All tests pass

**Step 4: Create summary of changes**

The refactoring:
- Removed SQLite dependency from test-harness
- Deleted ~300 lines of abstraction code
- Converted DatabasePool from enum to type alias
- Converted DatabaseTransaction from enum to type alias
- Removed dual query implementations in seed and assertion functions
- Updated documentation to reflect PostgreSQL-only approach
- All tests still passing with ~370ms per individual test

**Step 5: Push changes**

Ready for final review. When approved:

```bash
git push origin master
```

---

## Success Criteria

- [ ] `cargo check --workspace` passes
- [ ] `cargo test --workspace` passes with all tests green
- [ ] `cargo clippy --all-targets --all-features -- -D warnings` passes
- [ ] No references to SQLite in test-harness crate
- [ ] DatabasePool is simple type alias, not enum
- [ ] All tests run against PostgreSQL
- [ ] Documentation updated
- [ ] ~300 lines of code deleted
- [ ] Individual test timing ~370ms (acceptable)
- [ ] Full test suite timing ~3-4 seconds (similar to before)
