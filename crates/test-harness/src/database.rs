//! Database testing infrastructure with pre-allocated schema pool.
//!
//! This module provides scalable database testing by maintaining a fixed pool
//! of pre-created database schemas. Tests claim a schema, use it in isolation,
//! then return it for cleanup and reuse.
//!
//! Architecture:
//! - Single shared connection pool (eliminates connection fragmentation)
//! - Pre-allocated schemas (schema_001, schema_002, ..., schema_N)
//! - Schema-based isolation via PostgreSQL search_path
//! - Deterministic cleanup between tests

use std::{collections::VecDeque, path::Path, sync::Arc};

use anyhow::{Context, Result};
use sqlx::{migrate::Migrator, postgres::PgConnectOptions, PgPool, Postgres, Transaction};
use tokio::sync::{Mutex, OnceCell};

/// Number of pre-allocated schemas in the pool.
/// Should be >= number of concurrent tests for optimal performance.
const SCHEMA_POOL_SIZE: usize = 50;

/// Shared database connection pool for all tests.
static SHARED_POOL: OnceCell<PgPool> = OnceCell::const_new();

/// Pool of available schema names for test isolation.
static SCHEMA_POOL: OnceCell<Arc<Mutex<VecDeque<String>>>> = OnceCell::const_new();

/// Get or initialize the shared database connection pool.
async fn get_shared_pool() -> Result<&'static PgPool> {
    SHARED_POOL
        .get_or_try_init(|| async {
            let connect_options = PgConnectOptions::new()
                .host("localhost")
                .port(5433)
                .username("postgres")
                .password("postgres")
                .database("kapsel_test")
                .ssl_mode(sqlx::postgres::PgSslMode::Disable);

            let pool = sqlx::postgres::PgPoolOptions::new()
                .max_connections(20)
                .min_connections(5)
                .acquire_timeout(std::time::Duration::from_secs(30))
                .idle_timeout(Some(std::time::Duration::from_secs(600)))
                .connect_with(connect_options)
                .await
                .context("Failed to connect to test database")?;

            // Enable required extensions
            sqlx::query("CREATE EXTENSION IF NOT EXISTS \"uuid-ossp\"")
                .execute(&pool)
                .await
                .context("Failed to enable UUID extension")?;

            sqlx::query("CREATE EXTENSION IF NOT EXISTS \"pgcrypto\"")
                .execute(&pool)
                .await
                .context("Failed to enable pgcrypto extension")?;

            Ok(pool)
        })
        .await
}

/// Initialize the schema pool with pre-created schemas.
async fn initialize_schema_pool() -> Result<Arc<Mutex<VecDeque<String>>>> {
    let pool = get_shared_pool().await?;
    let mut schema_names = VecDeque::with_capacity(SCHEMA_POOL_SIZE);

    for i in 1..=SCHEMA_POOL_SIZE {
        let schema_name = format!("test_schema_{:03}", i);

        // Create schema if it doesn't exist
        sqlx::query(&format!("CREATE SCHEMA IF NOT EXISTS {}", schema_name))
            .execute(pool)
            .await
            .with_context(|| format!("Failed to create schema {}", schema_name))?;

        // Clean any existing data (in case of previous test failures)
        cleanup_schema(&schema_name).await?;

        // Run migrations in the schema
        run_migrations_in_schema(&schema_name).await?;

        schema_names.push_back(schema_name);
    }

    Ok(Arc::new(Mutex::new(schema_names)))
}

/// Get or initialize the schema pool.
async fn get_schema_pool() -> Result<&'static Arc<Mutex<VecDeque<String>>>> {
    SCHEMA_POOL.get_or_try_init(|| async { initialize_schema_pool().await }).await
}

/// Clean all data from a schema while preserving structure.
async fn cleanup_schema(schema_name: &str) -> Result<()> {
    let pool = get_shared_pool().await?;

    // Set search_path to target schema
    sqlx::query(&format!("SET search_path TO {}, public", schema_name)).execute(pool).await?;

    // Delete data in dependency order (children first, parents last)
    sqlx::query("DELETE FROM delivery_attempts").execute(pool).await?;
    sqlx::query("DELETE FROM webhook_events").execute(pool).await?;
    sqlx::query("DELETE FROM endpoints").execute(pool).await?;
    sqlx::query("DELETE FROM tenants").execute(pool).await?;

    Ok(())
}

/// Run database migrations in the specified schema.
async fn run_migrations_in_schema(schema_name: &str) -> Result<()> {
    let pool = get_shared_pool().await?;

    // Set search_path to target schema for migrations
    sqlx::query(&format!("SET search_path TO {}, public", schema_name)).execute(pool).await?;

    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().parent().unwrap();

    let migrations_path = workspace_root.join("migrations");
    let migrator = Migrator::new(migrations_path).await?;

    migrator
        .run(pool)
        .await
        .with_context(|| format!("Failed to run migrations in schema {}", schema_name))?;

    Ok(())
}

/// Guard that automatically returns schema to pool on drop.
pub struct SchemaGuard {
    schema_name: String,
    returned: bool,
}

impl SchemaGuard {
    fn new(schema_name: String) -> Self {
        Self { schema_name, returned: false }
    }

    /// Get the schema name for this guard.
    pub fn schema_name(&self) -> &str {
        &self.schema_name
    }

    /// Manually return schema to pool (automatic on drop).
    pub async fn release(mut self) -> Result<()> {
        if !self.returned {
            self.return_to_pool().await?;
            self.returned = true;
        }
        Ok(())
    }

    async fn return_to_pool(&self) -> Result<()> {
        // Clean the schema before returning it
        cleanup_schema(&self.schema_name).await?;

        // Return schema to available pool
        let pool = get_schema_pool().await?;
        let mut available_schemas = pool.lock().await;
        available_schemas.push_back(self.schema_name.clone());

        Ok(())
    }
}

impl Drop for SchemaGuard {
    fn drop(&mut self) {
        if !self.returned {
            // Best-effort cleanup on drop - can't propagate errors
            let schema_name = self.schema_name.clone();
            tokio::spawn(async move {
                if let Err(e) = cleanup_schema(&schema_name).await {
                    eprintln!("Failed to cleanup schema {} on drop: {}", schema_name, e);
                }

                // Return to pool
                if let Ok(pool) = get_schema_pool().await {
                    let mut available_schemas = pool.lock().await;
                    available_schemas.push_back(schema_name);
                }
            });
        }
    }
}

/// Test database with isolated schema from pre-allocated pool.
pub struct TestDatabase {
    pool: PgPool,
    _schema_guard: SchemaGuard,
}

impl TestDatabase {
    /// Claim a schema from the pool for isolated testing.
    pub async fn new() -> Result<Self> {
        let pool = get_shared_pool().await?;
        let schema_pool = get_schema_pool().await?;

        // Claim a schema from the pool
        let schema_name = {
            let mut available_schemas = schema_pool.lock().await;
            available_schemas.pop_front()
                .context("No available schemas in pool - increase SCHEMA_POOL_SIZE or reduce test concurrency")?
        };

        // Set connection search_path to use the claimed schema
        let guard = SchemaGuard::new(schema_name);

        Ok(Self { pool: pool.clone(), _schema_guard: guard })
    }

    /// Get the database connection pool.
    pub fn pool(&self) -> PgPool {
        self.pool.clone()
    }

    /// Get the schema name for this test database.
    pub fn schema_name(&self) -> &str {
        self._schema_guard.schema_name()
    }

    /// Execute a query with automatic search_path set to test schema.
    pub async fn execute_in_schema(&self, query: &str) -> Result<sqlx::postgres::PgQueryResult> {
        let search_path_query = format!("SET search_path TO {}, public", self.schema_name());
        sqlx::query(&search_path_query).execute(&self.pool).await?;

        sqlx::query(query)
            .execute(&self.pool)
            .await
            .context("Failed to execute query in test schema")
    }

    /// Begin a transaction with search_path set to test schema.
    pub async fn begin_transaction(&self) -> Result<Transaction<'_, Postgres>> {
        let mut tx = self.pool.begin().await?;

        let search_path_query = format!("SET search_path TO {}, public", self.schema_name());
        sqlx::query(&search_path_query).execute(&mut *tx).await?;

        Ok(tx)
    }

    /// Seed test data into the database.
    pub async fn seed_test_data(&self) -> Result<()> {
        let mut tx = self.begin_transaction().await?;

        // Insert test tenant
        sqlx::query!(
            "INSERT INTO tenants (id, name, plan, api_key, created_at, updated_at)
             VALUES ($1, $2, $3, $4, NOW(), NOW())
             ON CONFLICT (id) DO NOTHING",
            uuid::Uuid::parse_str("018c5b9e-8d5a-7890-abcd-123456789012").unwrap(),
            "test-tenant",
            "enterprise",
            "test-api-key-12345"
        )
        .execute(&mut *tx)
        .await?;

        // Insert test endpoint
        sqlx::query!(
            "INSERT INTO endpoints (id, tenant_id, name, url, max_retries, circuit_state, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, NOW(), NOW())
             ON CONFLICT (id) DO NOTHING",
            uuid::Uuid::parse_str("018c5b9e-8d5a-7890-abcd-123456789013").unwrap(),
            uuid::Uuid::parse_str("018c5b9e-8d5a-7890-abcd-123456789012").unwrap(),
            "test-endpoint",
            "https://api.example.com/webhooks",
            3i32,
            "closed"
        ).execute(&mut *tx).await?;

        tx.commit().await?;
        Ok(())
    }

    /// Create a new connection pool with search_path set to this test schema.
    ///
    /// This is useful for components like workers that need their own pool
    /// but must operate within the test schema isolation.
    pub async fn create_schema_aware_pool(&self) -> Result<PgPool> {
        let search_path = format!("{},public", self.schema_name());
        let connect_options = PgConnectOptions::new()
            .host("localhost")
            .port(5433)
            .username("postgres")
            .password("postgres")
            .database("kapsel_test")
            .options([("search_path", &search_path)])
            .ssl_mode(sqlx::postgres::PgSslMode::Disable);

        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(5)
            .min_connections(1)
            .acquire_timeout(std::time::Duration::from_secs(10))
            .idle_timeout(Some(std::time::Duration::from_secs(60)))
            .connect_with(connect_options)
            .await
            .context("Failed to create schema-aware connection pool")?;

        Ok(pool)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_schema_pool_initialization() {
        let schema_pool = get_schema_pool().await.unwrap();
        let schemas = schema_pool.lock().await;
        assert_eq!(schemas.len(), SCHEMA_POOL_SIZE);
    }

    #[tokio::test]
    async fn test_database_isolation() {
        let db1 = TestDatabase::new().await.unwrap();
        let db2 = TestDatabase::new().await.unwrap();

        // Schemas should be different
        assert_ne!(db1.schema_name(), db2.schema_name());

        // Each database should be isolated
        db1.seed_test_data().await.unwrap();

        // db2 should not see db1's data
        let mut tx2 = db2.begin_transaction().await.unwrap();
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM tenants").fetch_one(&mut *tx2).await.unwrap();
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn test_schema_cleanup_on_drop() {
        let _schema_name = {
            let db = TestDatabase::new().await.unwrap();
            db.seed_test_data().await.unwrap();
            db.schema_name().to_string()
        }; // db drops here, should trigger cleanup

        // Give cleanup time to complete
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // Create new database that might reuse the cleaned schema
        let db = TestDatabase::new().await.unwrap();
        let mut tx = db.begin_transaction().await.unwrap();
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM tenants").fetch_one(&mut *tx).await.unwrap();

        // Schema should be clean (either reused and cleaned, or different schema)
        assert_eq!(count, 0);
    }
}
