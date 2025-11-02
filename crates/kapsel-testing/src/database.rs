//! Database management for deterministic testing.
//!
//! Provides isolated test databases with startup cleanup and deterministic
//! behavior. Optimized for nextest's process-per-test model.

use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use sqlx::{
    postgres::{PgConnectOptions, PgPoolOptions},
    PgPool, Postgres, Transaction,
};
use tokio::sync::Semaphore;
use tracing::{debug, info, warn};
use uuid::Uuid;

const TEMPLATE_DB_NAME: &str = "kapsel_test_template";
const MAIN_TEST_DB_NAME: &str = "kapsel_test";

// Singleton pools for connection reuse
static SHARED_POOL: tokio::sync::OnceCell<PgPool> = tokio::sync::OnceCell::const_new();
static ADMIN_POOL: tokio::sync::OnceCell<PgPool> = tokio::sync::OnceCell::const_new();

// Semaphore to limit concurrent database creation operations
static DB_CREATION_SEMAPHORE: Semaphore = Semaphore::const_new(3);

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
        // Check if pool exists and is healthy
        if let Some(pool) = SHARED_POOL.get() {
            if !pool.is_closed() {
                return Ok(pool.clone());
            }
        }

        // Atomic initialization - only one thread will create the pool
        let pool = SHARED_POOL
            .get_or_try_init(|| async {
                let pool = create_shared_pool().await?;
                anyhow::Ok(pool)
            })
            .await?;

        Ok(pool.clone())
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
pub fn ensure_template_database_exists() {}

/// Create database from template.
pub async fn create_database_from_template(admin_pool: &PgPool, database_name: &str) -> Result<()> {
    let start_time = Instant::now();

    // Acquire semaphore permit to limit concurrent database operations
    let permit_start = Instant::now();
    #[allow(clippy::expect_used)]
    let _permit = DB_CREATION_SEMAPHORE
        .acquire()
        .await
        .expect("failed to acquire database creation semaphore");
    let permit_duration = permit_start.elapsed();

    if permit_duration > Duration::from_millis(100) {
        warn!(
            "Database creation semaphore wait took {}ms for {}",
            permit_duration.as_millis(),
            database_name
        );
    }

    debug!("creating database {} from template {}", database_name, TEMPLATE_DB_NAME);

    let db_create_start = Instant::now();
    sqlx::query(&format!(
        "CREATE DATABASE \"{database_name}\" WITH TEMPLATE \"{TEMPLATE_DB_NAME}\""
    ))
    .execute(admin_pool)
    .await
    .with_context(|| format!("failed to create database {database_name} from template"))?;

    let db_create_duration = db_create_start.elapsed();
    let total_duration = start_time.elapsed();

    if total_duration > Duration::from_millis(500) {
        warn!(
            "Database creation took {}ms (DB create: {}ms, permit wait: {}ms) for {}",
            total_duration.as_millis(),
            db_create_duration.as_millis(),
            permit_duration.as_millis(),
            database_name
        );
    } else {
        debug!(
            "successfully created database {} from template in {}ms",
            database_name,
            total_duration.as_millis()
        );
    }

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

    if sqlx::query(&format!("DROP DATABASE IF EXISTS \"{database_name}\" WITH (FORCE)"))
        .execute(admin_pool)
        .await
        .is_err()
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
    let start_time = Instant::now();

    // Check if pool exists and is healthy
    if let Some(pool) = ADMIN_POOL.get() {
        if !pool.is_closed() {
            debug!("reusing existing admin pool ({}ms)", start_time.elapsed().as_millis());
            return Ok(pool.clone());
        }
    }

    // Atomic initialization - only one thread will create the pool
    let pool = ADMIN_POOL
        .get_or_try_init(|| async {
            let database_url = std::env::var("DATABASE_URL")
                .context("DATABASE_URL environment variable is required")?;

            let opts = database_url
                .parse::<PgConnectOptions>()
                .context("failed to parse DATABASE_URL")?
                .database("postgres");

            let pool_create_start = Instant::now();
            let pool = PgPoolOptions::new()
                .max_connections(2)
                .min_connections(0)
                .max_lifetime(Duration::from_secs(300))
                .acquire_timeout(Duration::from_secs(3))
                .connect_with(opts)
                .await
                .context("failed to connect to admin database")?;

            let pool_duration = pool_create_start.elapsed();
            if pool_duration > Duration::from_millis(200) {
                warn!("Admin pool creation took {}ms", pool_duration.as_millis());
            }

            anyhow::Ok(pool)
        })
        .await?;

    let total_duration = start_time.elapsed();

    if total_duration > Duration::from_millis(200) {
        warn!("Admin pool creation took {}ms", total_duration.as_millis());
    } else {
        debug!("created admin connection pool in {}ms", total_duration.as_millis());
    }

    Ok(pool.clone())
}

/// Create connection pool for specific database.
pub async fn create_database_pool(database_name: &str) -> Result<PgPool> {
    let start_time = Instant::now();
    let database_url =
        std::env::var("DATABASE_URL").context("DATABASE_URL environment variable is required")?;

    let opts = database_url
        .parse::<PgConnectOptions>()
        .context("failed to parse DATABASE_URL")?
        .database(database_name);

    let pool = PgPoolOptions::new()
        .max_connections(2)
        .min_connections(0)
        .max_lifetime(Duration::from_secs(300))
        .idle_timeout(Duration::from_secs(10))
        .acquire_timeout(Duration::from_secs(3))
        .connect_with(opts)
        .await
        .with_context(|| {
            format!("failed to create connection pool for database: {database_name}")
        })?;

    let duration = start_time.elapsed();
    if duration > Duration::from_millis(200) {
        warn!("Database pool creation took {}ms for {}", duration.as_millis(), database_name);
    } else {
        debug!("created database pool for {} in {}ms", database_name, duration.as_millis());
    }

    Ok(pool)
}

/// Create shared pool optimized for nextest process-per-test model.
async fn create_shared_pool() -> Result<PgPool> {
    let start_time = Instant::now();
    let database_url =
        std::env::var("DATABASE_URL").context("DATABASE_URL environment variable is required")?;

    let opts = database_url
        .parse::<PgConnectOptions>()
        .context("failed to parse DATABASE_URL")?
        .database(MAIN_TEST_DB_NAME);

    let pool = PgPoolOptions::new()
        .max_connections(10)
        .min_connections(0)
        .max_lifetime(Duration::from_secs(300))
        .idle_timeout(Duration::from_secs(30))
        .acquire_timeout(Duration::from_secs(30))
        .connect_with(opts)
        .await
        .context("failed to create shared connection pool")?;

    let duration = start_time.elapsed();
    if duration > Duration::from_millis(200) {
        warn!("Shared pool creation took {}ms", duration.as_millis());
    } else {
        debug!("created shared pool in {}ms", duration.as_millis());
    }

    Ok(pool)
}
