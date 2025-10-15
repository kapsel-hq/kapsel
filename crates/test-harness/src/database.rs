//! Database testing utilities with shared containerized PostgreSQL and schema
//! isolation.
//!
//! Uses a single shared PostgreSQL container with per-test schema isolation.
//! This approach provides perfect test isolation while avoiding resource
//! exhaustion from creating containers per test.

use std::path::Path;

use anyhow::{Context, Result};
use sqlx::{migrate::Migrator, postgres::PgConnectOptions, PgPool, Postgres, Transaction};
use tokio::sync::OnceCell;

/// Shared test database connection pool for static postgres-test container
static SHARED_ADMIN_POOL: OnceCell<PgPool> = OnceCell::const_new();

async fn get_admin_pool() -> Result<&'static PgPool> {
    SHARED_ADMIN_POOL
        .get_or_try_init(|| async {
            let connect_options = PgConnectOptions::new()
                .host("localhost")
                .port(5433)
                .username("postgres")
                .password("postgres")
                .database("kapsel_test")
                .ssl_mode(sqlx::postgres::PgSslMode::Disable);

            let pool = sqlx::postgres::PgPoolOptions::new()
                .max_connections(50)
                .min_connections(5)
                .acquire_timeout(std::time::Duration::from_secs(60))
                .idle_timeout(Some(std::time::Duration::from_secs(300)))
                .connect_with(connect_options)
                .await
                .context("Failed to connect to static test database")?;

            // Enable extensions globally
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

async fn create_test_schema(schema_name: &str) -> Result<PgPool> {
    let admin_pool = get_admin_pool().await?;

    // Create the schema
    sqlx::query(&format!("CREATE SCHEMA IF NOT EXISTS {}", schema_name))
        .execute(admin_pool)
        .await
        .context("Failed to create test schema")?;

    // Create connection pool for this specific schema
    let search_path = format!("{},public", schema_name);
    let connect_options = PgConnectOptions::new()
        .host("localhost")
        .port(5433)
        .username("postgres")
        .password("postgres")
        .database("kapsel_test")
        .options([("search_path", &search_path)])
        .ssl_mode(sqlx::postgres::PgSslMode::Disable);

    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(20)
        .min_connections(2)
        .acquire_timeout(std::time::Duration::from_secs(60))
        .idle_timeout(Some(std::time::Duration::from_secs(300)))
        .connect_with(connect_options)
        .await
        .context("Failed to connect to test schema")?;

    Ok(pool)
}

async fn cleanup_schema(schema_name: &str) -> Result<()> {
    let admin_pool = get_admin_pool().await?;
    sqlx::query(&format!("DROP SCHEMA IF EXISTS {} CASCADE", schema_name))
        .execute(admin_pool)
        .await
        .with_context(|| format!("Failed to cleanup schema {}", schema_name))?;
    Ok(())
}

/// Database backend for tests using shared containerized PostgreSQL with schema
/// isolation.
pub struct TestDatabase {
    pool: PgPool,
    schema_name: String,
    _cleanup_guard: SchemaCleanupGuard,
}

/// Ensures schema cleanup when TestDatabase is dropped
struct SchemaCleanupGuard {
    schema_name: String,
}

impl Drop for SchemaCleanupGuard {
    fn drop(&mut self) {
        let schema_name = self.schema_name.clone();
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                let _: Result<(), _> = cleanup_schema(&schema_name).await;
            });
        }
        // If no runtime is available, skip cleanup (acceptable for test
        // schemas)
    }
}

impl TestDatabase {
    /// Creates a new test database with isolated schema.
    pub async fn new() -> Result<Self> {
        let schema_name = format!("test_{}", uuid::Uuid::new_v4().simple());
        Self::new_with_schema(&schema_name).await
    }

    /// Creates a test database with a specific schema name.
    pub async fn new_with_schema(schema_name: &str) -> Result<Self> {
        let pool = create_test_schema(schema_name).await?;

        let db = Self {
            pool,
            schema_name: schema_name.to_string(),
            _cleanup_guard: SchemaCleanupGuard { schema_name: schema_name.to_string() },
        };

        // Run migrations in the isolated schema
        db.run_migrations().await?;

        Ok(db)
    }

    /// Run database migrations in the test schema.
    async fn run_migrations(&self) -> Result<()> {
        let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(|p| p.parent())
            .context("Failed to find workspace root")?;
        let migrations_path = workspace_root.join("migrations");

        if !migrations_path.exists() {
            return Err(anyhow::anyhow!(
                "Migrations directory not found at {}",
                migrations_path.display()
            ));
        }

        let migrator =
            Migrator::new(migrations_path).await.context("Failed to load database migrations")?;

        migrator.run(&self.pool).await.context("Failed to run database migrations")?;

        Ok(())
    }

    /// Get a reference to the database connection pool.
    pub fn pool(&self) -> PgPool {
        self.pool.clone()
    }

    /// Get the schema name for this test database.
    pub fn schema_name(&self) -> &str {
        &self.schema_name
    }

    /// Begin a transaction that will be rolled back automatically.
    pub async fn begin_test_transaction(&self) -> Result<TestTransaction> {
        let tx = self.pool.begin().await.context("Failed to begin transaction")?;
        Ok(TestTransaction { tx: Some(tx) })
    }

    /// Execute raw SQL for test setup.
    pub async fn execute(&self, sql: &str) -> Result<()> {
        sqlx::query(sql).execute(&self.pool).await.context("Failed to execute SQL")?;
        Ok(())
    }

    /// Reset the schema by truncating all tables.
    pub async fn reset(&self) -> Result<()> {
        // Get all table names in this schema
        let table_names: Vec<String> = sqlx::query_scalar(
            "SELECT tablename FROM pg_tables WHERE schemaname = $1 ORDER BY tablename",
        )
        .bind(&self.schema_name)
        .fetch_all(&self.pool)
        .await
        .context("Failed to fetch table names")?;

        if table_names.is_empty() {
            return Ok(());
        }

        // Disable foreign key checks temporarily
        sqlx::query("SET session_replication_role = replica")
            .execute(&self.pool)
            .await
            .context("Failed to disable foreign key checks")?;

        // Truncate all tables
        for table_name in &table_names {
            sqlx::query(&format!("TRUNCATE TABLE {} RESTART IDENTITY CASCADE", table_name))
                .execute(&self.pool)
                .await
                .with_context(|| format!("Failed to truncate table {}", table_name))?;
        }

        // Re-enable foreign key checks
        sqlx::query("SET session_replication_role = DEFAULT")
            .execute(&self.pool)
            .await
            .context("Failed to re-enable foreign key checks")?;

        Ok(())
    }
}

/// Transaction wrapper that auto-rolls back for test isolation.
pub struct TestTransaction {
    tx: Option<Transaction<'static, Postgres>>,
}

impl TestTransaction {
    /// Get a reference to the underlying transaction.
    pub fn tx_mut(&mut self) -> &mut Transaction<'static, Postgres> {
        self.tx.as_mut().expect("Transaction should be available")
    }

    /// Commit the transaction (normally tests should let it roll back).
    pub async fn commit(mut self) -> Result<()> {
        if let Some(tx) = self.tx.take() {
            tx.commit().await.context("Failed to commit transaction")?;
        }
        Ok(())
    }

    /// Explicitly roll back the transaction.
    pub async fn rollback(mut self) -> Result<()> {
        if let Some(tx) = self.tx.take() {
            tx.rollback().await.context("Failed to rollback transaction")?;
        }
        Ok(())
    }
}

impl Drop for TestTransaction {
    fn drop(&mut self) {
        // Transaction will be automatically rolled back when dropped
        if self.tx.is_some() {
            // Note: Could log a warning if transaction wasn't explicitly
            // handled
        }
    }
}

impl std::ops::Deref for TestTransaction {
    type Target = Transaction<'static, Postgres>;

    fn deref(&self) -> &Self::Target {
        self.tx.as_ref().expect("Transaction should be available")
    }
}

impl std::ops::DerefMut for TestTransaction {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.tx.as_mut().expect("Transaction should be available")
    }
}
