//! Database testing utilities.
//!
//! Provides isolated test databases using PostgreSQL.
//! Requires Docker with kapsel-postgres-test container running.
//!
//! Tests automatically connect to PostgreSQL on the port specified in
//! DATABASE_URL environment variable (defaults to 5432).

use anyhow::{Context, Result};
use sqlx::{postgres::PgConnectOptions, PgPool, Postgres};
use uuid::Uuid;

/// Database backend abstraction for tests.
pub struct TestDatabase {
    pool: PgPool,
}

impl TestDatabase {
    /// Creates new test database using PostgreSQL.
    pub async fn new() -> Result<Self> {
        Self::new_postgres().await
    }

    /// Creates PostgreSQL database connection using existing postgres-test
    /// container.
    pub async fn new_postgres() -> Result<Self> {
        // Generate unique database name for test isolation
        let database_name = format!("kapsel_test_{}", Uuid::new_v4().simple());

        // Read port from DATABASE_URL or default to 5432 (CI default)
        let port = std::env::var("DATABASE_URL")
            .ok()
            .and_then(|url| {
                url.split(':')
                    .nth(3)
                    .and_then(|port_str| port_str.split('/').next())
                    .and_then(|port_str| port_str.parse::<u16>().ok())
            })
            .unwrap_or(5432);

        // First connect to postgres database to create test database
        let admin_options = PgConnectOptions::new()
            .host("127.0.0.1")
            .port(port)
            .username("postgres")
            .password("postgres")
            .database("postgres");

        let admin_pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(2) // Minimal connections for admin operations
            .acquire_timeout(std::time::Duration::from_secs(10))
            .connect_with(admin_options)
            .await
            .context("Failed to connect to PostgreSQL admin database")?;

        // Create unique test database
        let create_db_query = format!("CREATE DATABASE \"{}\"", database_name);
        sqlx::query(&create_db_query)
            .execute(&admin_pool)
            .await
            .context("Failed to create test database")?;

        admin_pool.close().await;

        // Now connect to the test database with limited pool size for tests
        let connect_options = PgConnectOptions::new()
            .host("127.0.0.1")
            .port(port)
            .username("postgres")
            .password("postgres")
            .database(&database_name);

        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(5) // Limit connections for tests
            .min_connections(1)
            .acquire_timeout(std::time::Duration::from_secs(10))
            .idle_timeout(Some(std::time::Duration::from_secs(30)))
            .max_lifetime(Some(std::time::Duration::from_secs(300)))
            .connect_with(connect_options)
            .await
            .context("Failed to connect to PostgreSQL test database")?;

        let db = Self { pool };
        db.run_migrations().await?;
        Ok(db)
    }

    /// Returns connection pool for the underlying database.
    pub fn pool(&self) -> PgPool {
        self.pool.clone()
    }

    /// Runs database migrations for PostgreSQL.
    async fn run_migrations(&self) -> Result<()> {
        run_postgres_migrations(&self.pool).await
    }

    /// Seeds test data into database.
    pub async fn seed_test_data(&self) -> Result<()> {
        let pool = self.pool();
        seed_endpoints(&pool).await?;
        seed_webhook_events(&pool).await?;
        Ok(())
    }
}

/// Database pool type alias.
pub type DatabasePool = PgPool;

/// Transaction type alias.
pub type DatabaseTransaction = sqlx::Transaction<'static, Postgres>;

/// Test database instance that cleans up on drop.
pub struct TestDatabaseGuard {
    database: TestDatabase,
    database_name: String,
    port: u16,
}

impl TestDatabaseGuard {
    pub fn pool(&self) -> PgPool {
        self.database.pool()
    }
}

impl Drop for TestDatabaseGuard {
    fn drop(&mut self) {
        let database_name = self.database_name.clone();
        let port = self.port;

        tokio::spawn(async move {
            if let Err(e) = cleanup_test_database(&database_name, port).await {
                tracing::warn!("Failed to cleanup test database {}: {}", database_name, e);
            }
        });
    }
}

async fn cleanup_test_database(database_name: &str, port: u16) -> Result<()> {
    let admin_options = PgConnectOptions::new()
        .host("127.0.0.1")
        .port(port)
        .username("postgres")
        .password("postgres")
        .database("postgres");

    let admin_pool = sqlx::PgPool::connect_with(admin_options).await?;

    // Terminate all connections to the database
    let terminate_query = format!(
        "SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE datname = '{}' AND pid <> pg_backend_pid()",
        database_name
    );
    let _ = sqlx::query(&terminate_query).execute(&admin_pool).await;

    // Drop the database
    let drop_query = format!("DROP DATABASE IF EXISTS \"{}\"", database_name);
    sqlx::query(&drop_query).execute(&admin_pool).await?;

    admin_pool.close().await;
    Ok(())
}

/// Sets up test database and returns connection pool.
pub async fn setup_test_database() -> Result<DatabasePool> {
    let database_name = format!("kapsel_test_{}", Uuid::new_v4().simple());
    let port = std::env::var("DATABASE_URL")
        .ok()
        .and_then(|url| {
            url.split(':')
                .nth(4)
                .and_then(|port_str| port_str.split('/').next())
                .and_then(|port_str| port_str.parse::<u16>().ok())
        })
        .unwrap_or(5433);

    let db = TestDatabase::new().await?;
    let guard = TestDatabaseGuard { database: db, database_name, port };

    let pool = guard.pool();

    #[allow(clippy::disallowed_methods)]
    Box::leak(Box::new(guard));

    Ok(pool)
}

/// PostgreSQL schema migrations.
async fn run_postgres_migrations(pool: &PgPool) -> Result<()> {
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS tenants (
            id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
            name TEXT NOT NULL,
            plan TEXT NOT NULL,
            created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
            updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
        )
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS endpoints (
            id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
            tenant_id UUID NOT NULL,
            url TEXT NOT NULL,
            name TEXT NOT NULL,
            signing_secret TEXT,
            signature_header TEXT,
            max_retries INTEGER NOT NULL DEFAULT 10,
            timeout_seconds INTEGER NOT NULL DEFAULT 30,
            circuit_state TEXT NOT NULL DEFAULT 'closed',
            circuit_failure_count INTEGER NOT NULL DEFAULT 0,
            circuit_last_failure_at TIMESTAMPTZ,
            circuit_half_open_at TIMESTAMPTZ,
            created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
            updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
            UNIQUE(tenant_id, name)
        )
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS webhook_events (
            id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
            tenant_id UUID NOT NULL,
            endpoint_id UUID NOT NULL,
            source_event_id TEXT NOT NULL,
            idempotency_strategy TEXT NOT NULL,
            status TEXT NOT NULL,
            failure_count INTEGER NOT NULL DEFAULT 0,
            last_attempt_at TIMESTAMPTZ,
            next_retry_at TIMESTAMPTZ,
            headers JSONB NOT NULL,
            body BYTEA NOT NULL,
            content_type TEXT NOT NULL,
            payload_size INTEGER NOT NULL,
            signature_valid BOOLEAN,
            signature_error TEXT,
            received_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
            delivered_at TIMESTAMPTZ,
            failed_at TIMESTAMPTZ,
            tigerbeetle_id UUID,
            UNIQUE(tenant_id, endpoint_id, source_event_id),
            CHECK (payload_size > 0 AND payload_size <= 10485760)
        )
        "#,
    )
    .execute(pool)
    .await?;

    // Add payload_size column if it doesn't exist (migration for existing test
    // databases)
    sqlx::query(
        r#"
        DO $$
        BEGIN
            IF NOT EXISTS (
                SELECT 1 FROM information_schema.columns
                WHERE table_name = 'webhook_events' AND column_name = 'payload_size'
            ) THEN
                ALTER TABLE webhook_events ADD COLUMN payload_size INTEGER NOT NULL DEFAULT 1;
                ALTER TABLE webhook_events ADD CONSTRAINT webhook_events_payload_size_check
                    CHECK (payload_size > 0 AND payload_size <= 10485760);
            END IF;
        END $$;
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS delivery_attempts (
            id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
            event_id UUID NOT NULL REFERENCES webhook_events(id),
            attempt_number INTEGER NOT NULL,
            request_url TEXT NOT NULL,
            request_headers JSONB NOT NULL,
            response_status INTEGER,
            response_headers JSONB,
            response_body TEXT,
            attempted_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
            duration_ms INTEGER NOT NULL,
            error_type TEXT,
            error_message TEXT
        )
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        CREATE INDEX IF NOT EXISTS idx_webhook_events_status
        ON webhook_events(status, next_retry_at)
        WHERE status IN ('pending', 'delivering')
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query("CREATE INDEX IF NOT EXISTS idx_webhook_events_tenant ON webhook_events(tenant_id, received_at DESC)")
        .execute(pool)
        .await?;

    sqlx::query("CREATE INDEX IF NOT EXISTS idx_delivery_attempts_event ON delivery_attempts(event_id, attempt_number)")
        .execute(pool)
        .await?;

    Ok(())
}

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

/// Seeds test webhook events.
async fn seed_webhook_events(pool: &PgPool) -> Result<()> {
    let endpoint: (Uuid, Uuid) =
        sqlx::query_as("SELECT id, tenant_id FROM endpoints LIMIT 1").fetch_one(pool).await?;

    sqlx::query(
        r#"
        INSERT INTO webhook_events (
            tenant_id, endpoint_id, source_event_id, idempotency_strategy,
            status, headers, body, content_type, payload_size
        )
        VALUES
            ($1, $2, 'test-event-1', 'header', 'pending', '{}', 'test body 1', 'text/plain', 11),
            ($1, $2, 'test-event-2', 'header', 'delivered', '{}', 'test body 2', 'text/plain', 11)
        ON CONFLICT DO NOTHING
        "#,
    )
    .bind(endpoint.1)
    .bind(endpoint.0)
    .execute(pool)
    .await?;

    Ok(())
}

/// Database test assertions.
pub mod assertions {
    use uuid::Uuid;

    use super::PgPool;

    /// Asserts event exists with expected status.
    pub async fn assert_event_status(
        pool: &PgPool,
        event_id: &str,
        expected_status: &str,
    ) -> Result<(), String> {
        let event_uuid = Uuid::parse_str(event_id).map_err(|e| format!("Invalid UUID: {}", e))?;

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

    /// Asserts delivery attempt count for event.
    pub async fn assert_delivery_attempts(
        pool: &PgPool,
        event_id: &str,
        expected_count: i64,
    ) -> Result<(), String> {
        let event_uuid = Uuid::parse_str(event_id).map_err(|e| format!("Invalid UUID: {}", e))?;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_database_url_port_parsing() {
        // Test parsing port from DATABASE_URL
        let test_cases = vec![
            ("postgres://postgres:postgres@localhost:5432/kapsel_test", 5432),
            ("postgres://user:pass@127.0.0.1:5433/testdb", 5433),
            ("postgres://postgres:postgres@localhost:3000/db", 3000),
        ];

        for (url, expected_port) in test_cases {
            std::env::set_var("DATABASE_URL", url);

            let port = std::env::var("DATABASE_URL")
                .ok()
                .and_then(|url| {
                    url.split(':')
                        .nth(3)
                        .and_then(|port_str| port_str.split('/').next())
                        .and_then(|port_str| port_str.parse::<u16>().ok())
                })
                .unwrap_or(5432);

            assert_eq!(port, expected_port, "Failed to parse port from URL: {}", url);
        }

        // Test default port when DATABASE_URL is not set
        std::env::remove_var("DATABASE_URL");
        let port = std::env::var("DATABASE_URL")
            .ok()
            .and_then(|url| {
                url.split(':')
                    .nth(3)
                    .and_then(|port_str| port_str.split('/').next())
                    .and_then(|port_str| port_str.parse::<u16>().ok())
            })
            .unwrap_or(5432);
        assert_eq!(port, 5432, "Should default to 5432 when DATABASE_URL is not set");
    }

    #[tokio::test]
    async fn database_setup_succeeds() {
        let pool = setup_test_database().await.unwrap();

        let result = sqlx::query_scalar::<_, i32>("SELECT 1").fetch_one(&pool).await.unwrap();

        assert_eq!(result, 1);
    }

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
