//! Database testing utilities.
//!
//! Provides isolated test databases using testcontainers and transaction-based
//! test isolation for fast, parallel test execution.

use anyhow::{Context, Result};
use log::LevelFilter;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::{ConnectOptions, PgPool};
use std::str::FromStr;
use testcontainers::clients::Cli;

use testcontainers::Container;
use testcontainers_modules::postgres::Postgres;
use uuid::Uuid;

/// Test database container wrapper.
pub struct TestDatabase {
    // Keep container alive for test duration
    #[allow(dead_code)]
    container: Container<'static, Postgres>,
    pool: PgPool,
    // Stored for debugging/logging purposes
    #[allow(dead_code)]
    database_name: String,
}

impl TestDatabase {
    /// Spawns a new PostgreSQL container and returns a connection pool.
    pub async fn new() -> Result<Self> {
        // Box::leak is intentional for testcontainers lifetime management
        // The Cli client must live for 'static to keep containers running
        #[allow(clippy::disallowed_methods)]
        let docker = Box::leak(Box::new(Cli::default()));
        let database_name = format!("test_{}", Uuid::new_v4().simple());

        // Start PostgreSQL container
        let postgres = Postgres::default().with_db_name(&database_name);

        let container = docker.run(postgres);
        let port = container.get_host_port_ipv4(5432);

        // Build connection options without logging SQL queries in tests
        let connect_options = PgConnectOptions::from_str(&format!(
            "postgres://postgres:postgres@127.0.0.1:{}/{}",
            port, database_name
        ))?
        .log_statements(LevelFilter::Debug)
        .log_slow_statements(LevelFilter::Warn, std::time::Duration::from_secs(1));

        // Create connection pool
        let pool = PgPoolOptions::new()
            .max_connections(5)
            .connect_with(connect_options)
            .await
            .context("Failed to connect to test database")?;

        // Run migrations
        run_migrations(&pool).await?;

        Ok(Self { container, pool, database_name })
    }

    /// Returns the connection pool.
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Seeds test data into the database.
    pub async fn seed_test_data(&self) -> Result<()> {
        seed_endpoints(&self.pool).await?;
        seed_webhook_events(&self.pool).await?;
        Ok(())
    }
}

/// Sets up a test database and returns the connection pool.
///
/// Each test gets its own isolated database that's cleaned up automatically.
pub async fn setup_test_database() -> Result<PgPool> {
    let db = TestDatabase::new().await?;

    // Leak the container to keep it alive for the test duration
    // This is safe in tests - cleanup happens automatically via Docker lifecycle
    let pool = db.pool().clone();
    #[allow(clippy::disallowed_methods)]
    Box::leak(Box::new(db));

    Ok(pool)
}

/// Runs database migrations.
async fn run_migrations(pool: &PgPool) -> Result<()> {
    // For now, create tables directly. Will be replaced with sqlx migrate
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
            received_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
            delivered_at TIMESTAMPTZ,
            failed_at TIMESTAMPTZ,
            tigerbeetle_id UUID,
            UNIQUE(tenant_id, endpoint_id, source_event_id)
        )
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

    // Create indexes for performance
    sqlx::query(
        r#"
        CREATE INDEX IF NOT EXISTS idx_webhook_events_status
        ON webhook_events(status, next_retry_at)
        WHERE status IN ('pending', 'delivering')
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        CREATE INDEX IF NOT EXISTS idx_webhook_events_tenant
        ON webhook_events(tenant_id, received_at DESC)
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        CREATE INDEX IF NOT EXISTS idx_delivery_attempts_event
        ON delivery_attempts(event_id, attempt_number)
        "#,
    )
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
    // Fetch a test endpoint
    let endpoint: (Uuid, Uuid) =
        sqlx::query_as("SELECT id, tenant_id FROM endpoints LIMIT 1").fetch_one(pool).await?;

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

/// Database test assertions.
pub mod assertions {
    use sqlx::PgPool;
    use uuid::Uuid;

    /// Asserts that an event exists with the expected status.
    pub async fn assert_event_status(
        pool: &PgPool,
        event_id: Uuid,
        expected_status: &str,
    ) -> Result<(), String> {
        let status: Option<String> =
            sqlx::query_scalar("SELECT status FROM webhook_events WHERE id = $1")
                .bind(event_id)
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

    /// Asserts the number of delivery attempts for an event.
    pub async fn assert_delivery_attempts(
        pool: &PgPool,
        event_id: Uuid,
        expected_count: i64,
    ) -> Result<(), String> {
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM delivery_attempts WHERE event_id = $1")
                .bind(event_id)
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

    #[tokio::test]
    async fn database_setup_succeeds() {
        let pool = setup_test_database().await.unwrap();

        // Verify connection works
        let result: i32 = sqlx::query_scalar("SELECT 1").fetch_one(&pool).await.unwrap();

        assert_eq!(result, 1);
    }

    #[tokio::test]
    async fn migrations_create_tables() {
        let pool = setup_test_database().await.unwrap();

        // Verify tables exist
        let tables: Vec<String> = sqlx::query_scalar(
            "SELECT table_name FROM information_schema.tables
             WHERE table_schema = 'public'",
        )
        .fetch_all(&pool)
        .await
        .unwrap();

        assert!(tables.contains(&"webhook_events".to_string()));
        assert!(tables.contains(&"delivery_attempts".to_string()));
        assert!(tables.contains(&"endpoints".to_string()));
    }
}
