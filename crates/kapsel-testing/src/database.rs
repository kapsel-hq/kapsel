//! Database management for deterministic testing.
//!
//! Provides isolated test databases with startup cleanup and deterministic
//! behavior. Optimized for nextest's process-per-test model.

use std::time::Duration;

use anyhow::{Context, Result};
use sqlx::{
    postgres::{PgConnectOptions, PgPoolOptions},
    PgPool, Postgres, Transaction,
};
use tracing::{debug, info};
use uuid::Uuid;

const TEMPLATE_DB_NAME: &str = "kapsel_test_template";
const MAIN_TEST_DB_NAME: &str = "kapsel_test";

// Singleton pools for connection reuse
static SHARED_POOL: std::sync::OnceLock<PgPool> = std::sync::OnceLock::new();
static ADMIN_POOL: std::sync::OnceLock<PgPool> = std::sync::OnceLock::new();

/// Test database handle providing either shared or isolated database access.
#[derive(Debug)]
pub struct TestDatabase {
    pool: PgPool,
}

impl TestDatabase {
    /// Create new test database handle using shared pool.
    pub async fn new() -> Result<Self> {
        let pool = Self::shared_pool().await?;
        Ok(Self { pool })
    }

    /// Get shared database pool for transaction-based tests.
    pub async fn shared_pool() -> Result<PgPool> {
        if let Some(pool) = SHARED_POOL.get() {
            if !pool.is_closed() {
                return Ok(pool.clone());
            }
        }

        let pool = create_shared_pool().await?;
        let _ = SHARED_POOL.set(pool.clone());
        Ok(pool)
    }

    /// Create isolated test database for tests requiring production behavior.
    pub async fn new_isolated() -> Result<IsolatedTestDatabase> {
        IsolatedTestDatabase::new().await
    }

    /// Create transactional test database for automatic rollback.
    pub async fn new_transaction() -> Result<TransactionalTestDatabase> {
        TransactionalTestDatabase::new().await
    }

    /// Access to the underlying database pool.
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }
}

/// Transactional test database that automatically rolls back changes.
#[derive(Debug)]
pub struct TransactionalTestDatabase {
    pool: PgPool,
}

impl TransactionalTestDatabase {
    /// Create new transactional test database.
    pub async fn new() -> Result<Self> {
        let pool = TestDatabase::shared_pool().await?;
        Ok(Self { pool })
    }

    /// Begin a new transaction for test operations.
    pub async fn begin(&self) -> Result<Transaction<'_, Postgres>> {
        Ok(self.pool.begin().await?)
    }

    /// Access to the underlying database pool.
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }
}

/// Isolated test database with its own PostgreSQL database.
#[derive(Debug)]
pub struct IsolatedTestDatabase {
    pool: PgPool,
    database_name: String,
}

impl IsolatedTestDatabase {
    /// Create new isolated test database.
    pub async fn new() -> Result<Self> {
        let admin_pool = create_admin_pool().await?;

        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let database_name = format!("test_{}_{}", timestamp, Uuid::new_v4().simple());

        create_database_from_template(&admin_pool, &database_name).await?;

        let pool = create_database_pool(&database_name).await?;

        info!("created isolated test database: {}", database_name);

        Ok(Self { pool, database_name })
    }

    /// Access to the database pool.
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Get the database name.
    pub fn database_name(&self) -> &str {
        &self.database_name
    }
}

/// Ensure template database exists for isolated test creation.
pub async fn ensure_template_database_exists() -> Result<()> {
    Ok(())
}

/// Create database from template.
pub async fn create_database_from_template(admin_pool: &PgPool, database_name: &str) -> Result<()> {
    debug!("creating database {} from template {}", database_name, TEMPLATE_DB_NAME);

    sqlx::query(&format!(
        "CREATE DATABASE \"{database_name}\" WITH TEMPLATE \"{TEMPLATE_DB_NAME}\""
    ))
    .execute(admin_pool)
    .await
    .with_context(|| format!("failed to create database {database_name} from template"))?;

    debug!("successfully created database {} from template", database_name);
    Ok(())
}

/// Drop database immediately with connection termination.
pub async fn drop_database_immediate(admin_pool: &PgPool, database_name: &str) -> Result<()> {
    let _ = sqlx::query(&format!(
        "SELECT pg_terminate_backend(pid)
         FROM pg_stat_activity
         WHERE datname = '{database_name}'
         AND pid <> pg_backend_pid()"
    ))
    .execute(admin_pool)
    .await;

    if let Err(_) =
        sqlx::query(&format!("DROP DATABASE IF EXISTS \"{database_name}\" WITH (FORCE)"))
            .execute(admin_pool)
            .await
    {
        sqlx::query(&format!("DROP DATABASE IF EXISTS \"{database_name}\""))
            .execute(admin_pool)
            .await
            .with_context(|| format!("failed to drop database: {database_name}"))?;
    }

    Ok(())
}

/// Create or reuse admin connection pool for database management operations.
pub async fn create_admin_pool() -> Result<PgPool> {
    if let Some(pool) = ADMIN_POOL.get() {
        if !pool.is_closed() {
            return Ok(pool.clone());
        }
    }

    let database_url =
        std::env::var("DATABASE_URL").context("DATABASE_URL environment variable is required")?;

    let opts = database_url
        .parse::<PgConnectOptions>()
        .context("failed to parse DATABASE_URL")?
        .database("postgres");

    let pool = PgPoolOptions::new()
        .max_connections(10)
        .min_connections(0)
        .max_lifetime(Duration::from_secs(300))
        .acquire_timeout(Duration::from_secs(10))
        .connect_with(opts)
        .await
        .context("failed to connect to admin database")?;

    let _ = ADMIN_POOL.set(pool.clone());

    debug!("created admin connection pool");
    Ok(pool)
}

/// Create connection pool for specific database.
pub async fn create_database_pool(database_name: &str) -> Result<PgPool> {
    let database_url =
        std::env::var("DATABASE_URL").context("DATABASE_URL environment variable is required")?;

    let opts = database_url
        .parse::<PgConnectOptions>()
        .context("failed to parse DATABASE_URL")?
        .database(database_name);

    let pool = PgPoolOptions::new()
        .max_connections(3)
        .min_connections(0)
        .max_lifetime(Duration::from_secs(300))
        .idle_timeout(Duration::from_secs(10))
        .acquire_timeout(Duration::from_secs(3))
        .connect_with(opts)
        .await
        .with_context(|| {
            format!("failed to create connection pool for database: {database_name}")
        })?;

    Ok(pool)
}

/// Create shared pool optimized for nextest process-per-test model.
async fn create_shared_pool() -> Result<PgPool> {
    let database_url =
        std::env::var("DATABASE_URL").context("DATABASE_URL environment variable is required")?;

    let opts = database_url
        .parse::<PgConnectOptions>()
        .context("failed to parse DATABASE_URL")?
        .database(MAIN_TEST_DB_NAME);

    let pool = PgPoolOptions::new()
        .max_connections(5)
        .min_connections(0)
        .max_lifetime(Duration::from_secs(300))
        .idle_timeout(Duration::from_secs(30))
        .acquire_timeout(Duration::from_secs(3))
        .connect_with(opts)
        .await
        .context("failed to create shared connection pool")?;

    Ok(pool)
}
