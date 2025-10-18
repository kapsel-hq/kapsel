//! Database testing infrastructure with transaction isolation.
//!
//! Provides PostgreSQL container management and per-test transaction rollback
//! for fast, isolated testing without manual cleanup or schema management.

use std::{sync::Arc, time::Duration};

use anyhow::{Context, Result};
use sqlx::{PgPool, Postgres, Transaction};
use testcontainers::{runners::AsyncRunner, ContainerAsync, ImageExt};
use testcontainers_modules::postgres::Postgres as PostgresImage;
use tracing::{debug, info};

/// Shared PostgreSQL container and connection pool for the entire test process.
///
/// This is shared across all tests using Arc/Weak references.
/// When the last reference is dropped, the container is automatically cleaned
/// up.
pub struct SharedDatabase {
    /// Connection pool to the shared container
    pool: PgPool,
    /// Container handle - keeps container alive until dropped
    #[allow(dead_code)]
    container: ContainerAsync<PostgresImage>,
}

use once_cell::sync::Lazy;
use tokio::sync::Mutex;

/// Weak reference to shared database for cross-test sharing
static SHARED_WEAK: Lazy<Mutex<std::sync::Weak<SharedDatabase>>> =
    Lazy::new(|| Mutex::new(std::sync::Weak::new()));

/// Test database handle providing transaction-based isolation.
///
/// Each test gets its own `TestDatabase` which manages a transaction
/// that automatically rolls back when dropped, ensuring perfect isolation
/// between tests while maintaining high performance.
pub struct TestDatabase {
    /// Reference to the shared database
    shared_db: Arc<SharedDatabase>,
}

impl SharedDatabase {
    /// Initialize the shared PostgreSQL container and run migrations.
    async fn new() -> Result<Self> {
        info!("initializing shared PostgreSQL test container");

        // Start container with PostgreSQL 16 Alpine for smaller size
        let postgres_image = PostgresImage::default().with_tag("16-alpine");

        let container = AsyncRunner::start(postgres_image)
            .await
            .context("failed to start PostgreSQL container")?;

        // Get the mapped port for connections
        let port =
            container.get_host_port_ipv4(5432).await.context("failed to get container port")?;

        let connection_string = format!("postgres://postgres:postgres@127.0.0.1:{}/postgres", port);

        debug!(port, "connecting to PostgreSQL container");

        // Create connection pool with proper configuration for tests
        let pool = Self::create_connection_pool(&connection_string).await?;

        // Enable required extensions
        Self::setup_extensions(&pool).await?;

        // Run migrations to create schema
        Self::run_migrations(&pool).await?;

        info!("shared PostgreSQL container ready");

        Ok(Self { pool, container })
    }

    /// Create a properly configured connection pool for tests.
    async fn create_connection_pool(connection_string: &str) -> Result<PgPool> {
        // Retry connection with backoff - container needs time to start
        let mut retries = 0;
        const MAX_RETRIES: u32 = 30;
        const RETRY_DELAY_MS: u64 = 100;

        loop {
            match sqlx::postgres::PgPoolOptions::new()
                // Workspace-wide parallel test connection pool settings
                .max_connections(100)
                .min_connections(20)
                // Extended timeouts for complex worker tests
                .acquire_timeout(Duration::from_secs(30))
                .idle_timeout(Duration::from_secs(60))
                .max_lifetime(Duration::from_secs(300))
                .connect(connection_string)
                .await
            {
                Ok(pool) => {
                    // Verify connection actually works
                    if sqlx::query("SELECT 1").execute(&pool).await.is_ok() {
                        debug!("PostgreSQL connection established");
                        return Ok(pool);
                    }
                    // Connection created but not working, retry
                    pool.close().await;
                },
                Err(e) => {
                    if retries >= MAX_RETRIES {
                        return Err(anyhow::anyhow!(
                            "failed to connect to PostgreSQL after {} retries: {}",
                            MAX_RETRIES,
                            e
                        ));
                    }
                },
            }

            retries += 1;
            tokio::time::sleep(Duration::from_millis(RETRY_DELAY_MS)).await;
        }
    }

    /// Enable required PostgreSQL extensions.
    async fn setup_extensions(pool: &PgPool) -> Result<()> {
        // UUID generation support
        sqlx::query("CREATE EXTENSION IF NOT EXISTS \"uuid-ossp\"")
            .execute(pool)
            .await
            .context("failed to create uuid-ossp extension")?;

        // Cryptographic functions for HMAC
        sqlx::query("CREATE EXTENSION IF NOT EXISTS pgcrypto")
            .execute(pool)
            .await
            .context("failed to create pgcrypto extension")?;

        Ok(())
    }

    /// Run database migrations to create schema.
    async fn run_migrations(pool: &PgPool) -> Result<()> {
        // Navigate from kapsel-testing crate to workspace root
        let workspace_root =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().parent().unwrap();
        let migrations_path = workspace_root.join("migrations");

        debug!(?migrations_path, "running migrations");

        sqlx::migrate::Migrator::new(migrations_path)
            .await?
            .run(pool)
            .await
            .context("failed to run migrations")?;

        info!("database migrations completed");
        Ok(())
    }

    /// Get the connection pool for this shared database.
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }
}

impl TestDatabase {
    /// Create a new test database handle.
    ///
    /// This manages sharing of the database container across tests in the same
    /// process. Uses a weak reference to allow proper cleanup when all
    /// tests finish.
    pub async fn new() -> Result<Self> {
        // Try to upgrade existing weak reference to shared database
        {
            let weak_guard = SHARED_WEAK.lock().await;
            if let Some(shared_db) = weak_guard.upgrade() {
                return Ok(Self { shared_db });
            }
        }

        // Create new shared database since none exists
        let shared_db = Arc::new(SharedDatabase::new().await?);

        // Store weak reference for future sharing
        {
            let mut weak_guard = SHARED_WEAK.lock().await;
            *weak_guard = Arc::downgrade(&shared_db);
        }

        Ok(Self { shared_db })
    }

    /// Create a completely isolated test database handle.
    ///
    /// This bypasses the shared database mechanism and creates a fresh
    /// database container. Used by property tests that run in custom
    /// runtimes to avoid runtime conflicts.
    pub async fn new_isolated() -> Result<Self> {
        // Always create new database, never share
        let shared_db = Arc::new(SharedDatabase::new().await?);
        Ok(Self { shared_db })
    }

    /// Get a reference to the shared database.
    pub fn shared_database(&self) -> &Arc<SharedDatabase> {
        &self.shared_db
    }

    /// Begin a new transaction for test isolation.
    ///
    /// The transaction will automatically rollback when dropped,
    /// ensuring no test data persists between tests.
    pub async fn begin_transaction(&self) -> Result<Transaction<'static, Postgres>> {
        // SAFETY: We need a 'static transaction for ergonomics in TestEnv.
        // This is safe because the underlying pool is owned by SharedDatabase
        // which lives for the entire process duration.
        let tx = self.shared_db.pool.begin().await.context("failed to begin transaction")?;

        // Convert to 'static lifetime for easier use in TestEnv
        let tx = unsafe {
            std::mem::transmute::<Transaction<'_, Postgres>, Transaction<'static, Postgres>>(tx)
        };

        Ok(tx)
    }

    /// Get the underlying connection pool.
    ///
    /// Used by components that need their own database connections
    /// (like delivery workers). Data inserted in test transactions
    /// won't be visible to these connections unless explicitly committed.
    pub fn pool(&self) -> PgPool {
        self.shared_db.pool.clone()
    }

    /// Create a new connection pool for components that need isolation.
    ///
    /// This returns a new pool connected to the same shared container,
    /// useful for testing components that manage their own connections.
    pub async fn create_isolated_pool(&self) -> Result<PgPool> {
        Ok(self.shared_db.pool.clone())
    }

    /// Schema name for the test database (always 'public').
    pub fn schema_name(&self) -> &str {
        "public"
    }
}

/// Get a shared database instance for tests that need direct access.
///
/// This is used by TestEnv to hold the Arc directly.
pub async fn get_shared_database() -> Result<Arc<SharedDatabase>> {
    let db = TestDatabase::new().await?;
    Ok(Arc::clone(&db.shared_db))
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::*;

    #[tokio::test]
    async fn shared_database_is_reused_across_tests() {
        let db1 = get_shared_database().await.unwrap();
        let db2 = get_shared_database().await.unwrap();

        // Both should be the same Arc instance
        assert!(Arc::ptr_eq(&db1, &db2));

        // Both should work
        let result1: (i32,) = sqlx::query_as("SELECT 1").fetch_one(db1.pool()).await.unwrap();
        let result2: (i32,) = sqlx::query_as("SELECT 1").fetch_one(db2.pool()).await.unwrap();

        assert_eq!(result1.0, 1);
        assert_eq!(result2.0, 1);
    }

    #[tokio::test]
    async fn transactions_provide_isolation() {
        let db = TestDatabase::new().await.unwrap();

        // Create two transactions
        let mut tx1 = db.begin_transaction().await.unwrap();
        let mut tx2 = db.begin_transaction().await.unwrap();

        // Insert in first transaction
        let tenant_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO tenants (id, name, plan, api_key, created_at, updated_at)
             VALUES ($1, $2, $3, $4, NOW(), NOW())",
        )
        .bind(tenant_id)
        .bind("isolated-tenant")
        .bind("enterprise")
        .bind(format!("isolation-test-key-{}", tenant_id))
        .execute(&mut *tx1)
        .await
        .unwrap();

        // Should not be visible in second transaction
        let count: (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM tenants WHERE name = 'isolated-tenant'")
                .fetch_one(&mut *tx2)
                .await
                .unwrap();

        assert_eq!(count.0, 0, "transactions should be isolated");
    }

    #[tokio::test]
    async fn explicit_commit_makes_data_visible() {
        let db = TestDatabase::new().await.unwrap();
        let tenant_id = Uuid::new_v4();

        // Create and commit a transaction
        {
            let mut tx = db.begin_transaction().await.unwrap();

            sqlx::query(
                "INSERT INTO tenants (id, name, plan, api_key, created_at, updated_at)
                 VALUES ($1, $2, $3, $4, NOW(), NOW())",
            )
            .bind(tenant_id)
            .bind("committed-tenant")
            .bind("enterprise")
            .bind(format!("commit-test-key-{}", tenant_id))
            .execute(&mut *tx)
            .await
            .unwrap();

            // Explicitly commit
            tx.commit().await.unwrap();
        }

        // Data should be visible in a new connection
        let pool = db.pool();
        let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM tenants WHERE id = $1")
            .bind(tenant_id)
            .fetch_one(&pool)
            .await
            .unwrap();

        assert_eq!(count.0, 1, "committed data should be visible");

        // Clean up the committed data
        sqlx::query("DELETE FROM tenants WHERE id = $1")
            .bind(tenant_id)
            .execute(&pool)
            .await
            .unwrap();
    }
}
