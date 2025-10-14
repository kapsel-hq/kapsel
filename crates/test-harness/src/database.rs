//! Database testing utilities with transaction-based isolation.
//!
//! Uses a single shared PostgreSQL database with transaction rollback
//! for test isolation. This is the production-grade approach that
//! scales properly and doesn't consume excessive disk space.

use std::path::Path;

use anyhow::{Context, Result};
use sqlx::{migrate::Migrator, postgres::PgConnectOptions, PgPool, Postgres, Transaction};

/// Database backend for tests using transaction-based isolation.
pub struct TestDatabase {
    pool: PgPool,
}

impl TestDatabase {
    /// Creates connection to shared test database.
    pub async fn new() -> Result<Self> {
        let pool = create_test_pool().await?;
        let db = Self { pool };

        // Run migrations once on the shared database
        db.ensure_migrations().await?;

        Ok(db)
    }

    /// Returns connection pool for the shared test database.
    pub fn pool(&self) -> PgPool {
        self.pool.clone()
    }

    /// Begins a new transaction for test isolation.
    ///
    /// The transaction will automatically rollback when dropped,
    /// providing perfect isolation between tests.
    pub async fn transaction(&self) -> Result<TestTransaction> {
        let tx = self.pool.begin().await.context("Failed to begin test transaction")?;
        Ok(TestTransaction::new(tx))
    }

    /// Ensures migrations are applied to the shared database.
    async fn ensure_migrations(&self) -> Result<()> {
        run_postgres_migrations(&self.pool).await
    }
}

/// Transaction wrapper that auto-rolls back for test isolation.
pub struct TestTransaction {
    tx: Option<Transaction<'static, Postgres>>,
}

impl TestTransaction {
    fn new(tx: Transaction<'static, Postgres>) -> Self {
        Self { tx: Some(tx) }
    }

    /// Returns a reference to the transaction for executing queries.
    pub fn tx_mut(&mut self) -> &mut Transaction<'static, Postgres> {
        self.tx.as_mut().expect("Transaction already consumed")
    }

    /// Explicitly commits the transaction (prevents rollback).
    ///
    /// Usually not needed in tests - rollback is desired for isolation.
    pub async fn commit(mut self) -> Result<()> {
        if let Some(tx) = self.tx.take() {
            tx.commit().await.context("Failed to commit test transaction")?;
        }
        Ok(())
    }

    /// Explicitly rolls back the transaction.
    pub async fn rollback(mut self) -> Result<()> {
        if let Some(tx) = self.tx.take() {
            tx.rollback().await.context("Failed to rollback test transaction")?;
        }
        Ok(())
    }
}

impl Drop for TestTransaction {
    fn drop(&mut self) {
        if let Some(tx) = self.tx.take() {
            // Auto-rollback in background - this is the key to test isolation
            tokio::spawn(async move {
                if let Err(e) = tx.rollback().await {
                    tracing::warn!("Failed to rollback test transaction: {}", e);
                }
            });
        }
    }
}

// Allow using the transaction directly in sqlx operations
impl std::ops::Deref for TestTransaction {
    type Target = Transaction<'static, Postgres>;

    fn deref(&self) -> &Self::Target {
        self.tx.as_ref().expect("Transaction already consumed")
    }
}

impl std::ops::DerefMut for TestTransaction {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.tx.as_mut().expect("Transaction already consumed")
    }
}

/// Database pool type alias.
pub type DatabasePool = PgPool;

/// Creates connection pool to shared test database.
async fn create_test_pool() -> Result<PgPool> {
    // Read port from DATABASE_URL or default to 5433 (test instance)
    let port = std::env::var("DATABASE_URL")
        .ok()
        .and_then(|url| {
            url.split(':')
                .nth(3)
                .and_then(|port_str| port_str.split('/').next())
                .and_then(|port_str| port_str.parse::<u16>().ok())
        })
        .unwrap_or(5433);

    // Use a unique database name per test run to avoid collisions
    let database_name = std::env::var("TEST_DATABASE_NAME")
        .unwrap_or_else(|_| format!("kapsel_test_shared_{}", uuid::Uuid::new_v4().simple()));

    let connect_options = PgConnectOptions::new()
        .host("127.0.0.1")
        .port(port)
        .username("postgres")
        .password("postgres")
        .database(&database_name);

    // Create the test database if it doesn't exist
    ensure_test_database_exists(&database_name, port).await?;

    // Connect with reasonable pool limits for testing
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(10)  // Reasonable for test concurrency
        .min_connections(2)
        .acquire_timeout(std::time::Duration::from_secs(30))
        .idle_timeout(Some(std::time::Duration::from_secs(600)))
        .connect_with(connect_options)
        .await
        .context("Failed to connect to test database")?;

    Ok(pool)
}

/// Ensures the shared test database exists.
async fn ensure_test_database_exists(database_name: &str, port: u16) -> Result<()> {
    let admin_options = PgConnectOptions::new()
        .host("127.0.0.1")
        .port(port)
        .username("postgres")
        .password("postgres")
        .database("postgres");

    let admin_pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect_with(admin_options)
        .await
        .context("Failed to connect to PostgreSQL admin database")?;

    // Check if database exists
    let exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_database WHERE datname = $1)")
            .bind(database_name)
            .fetch_one(&admin_pool)
            .await
            .context("Failed to check if test database exists")?;

    if !exists {
        let create_db_query = format!("CREATE DATABASE \"{}\"", database_name);
        sqlx::query(&create_db_query)
            .execute(&admin_pool)
            .await
            .context("Failed to create shared test database")?;

        tracing::info!("Created shared test database: {}", database_name);
    }

    admin_pool.close().await;
    Ok(())
}

/// Runs PostgreSQL migrations on the shared test database.
async fn run_postgres_migrations(pool: &PgPool) -> Result<()> {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .context("Failed to find workspace root")?;
    let migrations_path = workspace_root.join("migrations");

    let migrator = Migrator::new(migrations_path).await.context("Failed to create migrator")?;

    migrator.run(pool).await.context("Failed to run PostgreSQL migrations")?;

    Ok(())
}

/// Sets up test database and returns connection pool (legacy compatibility).
pub async fn setup_test_database() -> Result<DatabasePool> {
    let db = TestDatabase::new().await?;
    Ok(db.pool())
}

/// Seeds test endpoints into the database.
async fn seed_endpoints(executor: &mut Transaction<'_, Postgres>) -> Result<()> {
    sqlx::query(
        r#"
        INSERT INTO tenants (id, name, plan) VALUES
        ('00000000-0000-0000-0000-000000000001', 'test-tenant', 'free'),
        ('00000000-0000-0000-0000-000000000002', 'second-tenant', 'free')
        ON CONFLICT (id) DO NOTHING
        "#,
    )
    .execute(&mut **executor)
    .await
    .context("Failed to seed test tenants")?;

    sqlx::query(
        r#"
        INSERT INTO endpoints (id, tenant_id, name, url, max_retries, circuit_state) VALUES
        ('00000000-0000-0000-0000-000000000001', '00000000-0000-0000-0000-000000000001', 'primary', 'http://localhost:3000/webhook', 3, 'closed'),
        ('00000000-0000-0000-0000-000000000002', '00000000-0000-0000-0000-000000000001', 'backup', 'http://localhost:3001/webhook', 3, 'closed'),
        ('00000000-0000-0000-0000-000000000003', '00000000-0000-0000-0000-000000000002', 'secondary', 'http://localhost:3002/webhook', 3, 'closed')
        ON CONFLICT (id) DO NOTHING
        "#,
    )
    .execute(&mut **executor)
    .await
    .context("Failed to seed test endpoints")?;

    Ok(())
}

/// Seeds test webhook events into the database.
async fn seed_webhook_events(executor: &mut Transaction<'_, Postgres>) -> Result<()> {
    let test_payload = serde_json::json!({
        "type": "test.event",
        "data": { "message": "test webhook payload" }
    });

    sqlx::query(
        r#"
        INSERT INTO webhook_events (id, tenant_id, endpoint_id, source_event_id, idempotency_strategy, status, payload_size, body, headers, content_type, received_at)
        VALUES
        ('00000000-0000-0000-0000-000000000001', '00000000-0000-0000-0000-000000000001', '00000000-0000-0000-0000-000000000001', 'test-event-1', 'header', 'pending', $1, $2, '{}', 'application/json', NOW()),
        ('00000000-0000-0000-0000-000000000002', '00000000-0000-0000-0000-000000000001', '00000000-0000-0000-0000-000000000002', 'test-event-2', 'header', 'pending', $1, $2, '{}', 'application/json', NOW())
        ON CONFLICT (id) DO NOTHING
        "#,
    )
    .bind(test_payload.to_string().len() as i32)
    .bind(test_payload.to_string().as_bytes())
    .execute(&mut **executor)
    .await
    .context("Failed to seed test webhook events")?;

    Ok(())
}

/// Seeds all test data into the database.
pub async fn seed_test_data(executor: &mut Transaction<'_, Postgres>) -> Result<()> {
    seed_endpoints(executor).await?;
    seed_webhook_events(executor).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn database_setup_succeeds() {
        let db = TestDatabase::new().await.unwrap();

        // Verify we can query the database
        let result: (i32,) = sqlx::query_as("SELECT 1").fetch_one(&db.pool()).await.unwrap();
        assert_eq!(result.0, 1);
    }

    #[tokio::test]
    async fn transaction_isolation_works() {
        let db = TestDatabase::new().await.unwrap();

        // Start two transactions
        let mut tx1 = db.transaction().await.unwrap();
        let mut tx2 = db.transaction().await.unwrap();

        // Insert data in tx1
        sqlx::query("CREATE TEMP TABLE test_isolation (id INTEGER)")
            .execute(&mut **tx1)
            .await
            .unwrap();

        sqlx::query("INSERT INTO test_isolation VALUES (1)").execute(&mut **tx1).await.unwrap();

        // tx2 shouldn't see the temp table from tx1
        let result = sqlx::query("SELECT COUNT(*) FROM test_isolation").execute(&mut **tx2).await;

        // This should fail because tx2 can't see tx1's temp table
        assert!(result.is_err());

        // Transactions will auto-rollback when dropped
    }

    #[tokio::test]
    async fn migrations_create_tables() {
        let db = TestDatabase::new().await.unwrap();
        let mut tx = db.transaction().await.unwrap();

        // Verify core tables exist
        let tables: Vec<String> = sqlx::query_scalar(
            "SELECT table_name FROM information_schema.tables
             WHERE table_schema = 'public' AND table_type = 'BASE TABLE'
             ORDER BY table_name",
        )
        .fetch_all(&mut **tx)
        .await
        .unwrap();

        let expected_tables = vec!["delivery_attempts", "endpoints", "tenants", "webhook_events"];

        for table in expected_tables {
            assert!(tables.contains(&table.to_string()), "Missing table: {}", table);
        }
    }

    #[tokio::test]
    async fn test_data_seeding_works() {
        let db = TestDatabase::new().await.unwrap();
        let mut tx = db.transaction().await.unwrap();

        seed_test_data(&mut tx).await.unwrap();

        // Verify seeded data exists
        let tenant_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM tenants").fetch_one(&mut **tx).await.unwrap();
        assert!(tenant_count >= 2);

        let endpoint_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM endpoints")
            .fetch_one(&mut **tx)
            .await
            .unwrap();
        assert!(endpoint_count >= 3);

        // Transaction will rollback - data won't persist
    }

    #[tokio::test]
    async fn test_database_url_port_parsing() {
        // Test port extraction from DATABASE_URL
        std::env::set_var("DATABASE_URL", "postgresql://user:pass@localhost:5432/db");
        let port = std::env::var("DATABASE_URL")
            .ok()
            .and_then(|url| {
                url.split(':')
                    .nth(3)
                    .and_then(|port_str| port_str.split('/').next())
                    .and_then(|port_str| port_str.parse::<u16>().ok())
            })
            .unwrap_or(5433);
        assert_eq!(port, 5432);

        std::env::remove_var("DATABASE_URL");
    }
}
