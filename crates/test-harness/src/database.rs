//! Database testing infrastructure using Testcontainers for perfect isolation.
//!
//! Each test gets its own PostgreSQL container, providing complete isolation
//! and eliminating connection pool exhaustion issues.

use anyhow::{Context, Result};
use sqlx::{postgres::PgConnectOptions, PgPool};
use testcontainers::{runners::AsyncRunner, ContainerAsync, ImageExt};
use testcontainers_modules::postgres::Postgres;

/// Test database with its own PostgreSQL container.
///
/// Each instance creates its own PostgreSQL container, providing complete
/// isolation. The container is automatically cleaned up when dropped.
pub struct TestDatabase {
    pool: PgPool,
    _container: ContainerAsync<Postgres>,
    port: u16,
}

impl TestDatabase {
    /// Create a new test database with its own PostgreSQL container.
    pub async fn new() -> Result<Self> {
        // Start PostgreSQL container
        let postgres_image = Postgres::default().with_tag("16-alpine");
        let container: ContainerAsync<Postgres> = AsyncRunner::start(postgres_image)
            .await
            .context("Failed to start PostgreSQL container")?;

        let port =
            container.get_host_port_ipv4(5432).await.context("Failed to get PostgreSQL port")?;

        // Connect to container
        let connect_options = PgConnectOptions::new()
            .host("127.0.0.1")
            .port(port)
            .username("postgres")
            .password("postgres")
            .database("postgres")
            .ssl_mode(sqlx::postgres::PgSslMode::Disable);

        let pool = Self::wait_and_connect(connect_options).await?;

        // Enable extensions
        sqlx::query("CREATE EXTENSION IF NOT EXISTS \"uuid-ossp\"")
            .execute(&pool)
            .await
            .context("Failed to enable UUID extension")?;

        sqlx::query("CREATE EXTENSION IF NOT EXISTS \"pgcrypto\"")
            .execute(&pool)
            .await
            .context("Failed to enable pgcrypto extension")?;

        // Run migrations
        Self::run_migrations(&pool).await?;

        Ok(Self { pool, _container: container, port })
    }

    /// Wait for PostgreSQL to be ready and establish connection.
    async fn wait_and_connect(options: PgConnectOptions) -> Result<PgPool> {
        let mut retries = 30;
        let mut last_error = None;

        while retries > 0 {
            match sqlx::postgres::PgPoolOptions::new()
                .max_connections(20)
                .min_connections(5)
                .acquire_timeout(std::time::Duration::from_secs(10))
                .connect_with(options.clone())
                .await
            {
                Ok(pool) => {
                    // Verify connection works
                    if sqlx::query("SELECT 1").execute(&pool).await.is_ok() {
                        return Ok(pool);
                    }
                    pool.close().await;
                },
                Err(e) => last_error = Some(e),
            }

            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            retries -= 1;
        }

        Err(last_error
            .map(|e| anyhow::anyhow!("Failed to connect to PostgreSQL: {}", e))
            .unwrap_or_else(|| anyhow::anyhow!("Failed to connect to PostgreSQL after 30 retries")))
    }

    /// Run database migrations.
    async fn run_migrations(pool: &PgPool) -> Result<()> {
        let workspace_root =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().parent().unwrap();
        let migrations_path = workspace_root.join("migrations");

        sqlx::migrate::Migrator::new(migrations_path)
            .await?
            .run(pool)
            .await
            .context("Failed to run migrations")?;

        Ok(())
    }

    /// Database connection pool for this test database.
    pub fn pool(&self) -> PgPool {
        self.pool.clone()
    }

    /// Schema name (always 'public' for isolated containers).
    pub fn schema_name(&self) -> &str {
        "public"
    }

    /// Begin a transaction.
    pub async fn begin_transaction(&self) -> Result<sqlx::Transaction<'_, sqlx::Postgres>> {
        self.pool.begin().await.context("Failed to begin transaction")
    }

    /// Execute a query in this database.
    pub async fn execute_in_schema(&self, query: &str) -> Result<sqlx::postgres::PgQueryResult> {
        sqlx::query(query).execute(&self.pool).await.context("Failed to execute query")
    }

    /// Create a new connection pool for components that need their own.
    ///
    /// Essential for testing DeliveryWorker and similar components.
    pub async fn create_schema_aware_pool(&self) -> Result<PgPool> {
        let connect_options = PgConnectOptions::new()
            .host("127.0.0.1")
            .port(self.port)
            .username("postgres")
            .password("postgres")
            .database("postgres")
            .ssl_mode(sqlx::postgres::PgSslMode::Disable);

        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(10)
            .min_connections(2)
            .acquire_timeout(std::time::Duration::from_secs(10))
            .connect_with(connect_options)
            .await
            .context("Failed to create additional connection pool")?;

        Ok(pool)
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
        )
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_database_creation() {
        let db = TestDatabase::new().await.unwrap();

        // Verify we can execute queries
        let result: (i32,) =
            sqlx::query_as("SELECT 1 as test").fetch_one(&db.pool()).await.unwrap();
        assert_eq!(result.0, 1);
    }

    #[tokio::test]
    async fn test_complete_isolation() {
        // Create two databases - each with its own container
        let db1 = TestDatabase::new().await.unwrap();
        let db2 = TestDatabase::new().await.unwrap();

        // Seed data in db1
        db1.seed_test_data().await.unwrap();

        // db2 should only have the system tenant (different container)
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tenants")
            .fetch_one(&db2.pool())
            .await
            .unwrap();
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn test_worker_pool_creation() {
        let db = TestDatabase::new().await.unwrap();

        // Seed data
        db.seed_test_data().await.unwrap();

        // Create worker pool
        let worker_pool = db.create_schema_aware_pool().await.unwrap();

        // Worker pool should see system tenant + seeded tenant
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tenants")
            .fetch_one(&worker_pool)
            .await
            .unwrap();
        assert_eq!(count, 2);

        worker_pool.close().await;
    }

    #[tokio::test]
    async fn test_seed_data() {
        let db = TestDatabase::new().await.unwrap();

        // Initially has system tenant
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM tenants").fetch_one(&db.pool()).await.unwrap();
        assert_eq!(count, 1);

        // After seeding has system tenant + test tenant
        db.seed_test_data().await.unwrap();
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM tenants").fetch_one(&db.pool()).await.unwrap();
        assert_eq!(count, 2);
    }
}
