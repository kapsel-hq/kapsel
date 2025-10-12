//! Kapsel webhook reliability service.
//!
//! Main entry point for the Kapsel server. Initializes all subsystems
//! and coordinates graceful startup and shutdown.

use std::{net::SocketAddr, time::Duration};

use anyhow::{Context, Result};
use sqlx::postgres::PgPoolOptions;
use tracing::{error, info};

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing with structured logging
    init_tracing();

    info!("Starting Kapsel webhook reliability service");

    // Load configuration from environment
    let config = Config::from_env()?;
    info!(
        database_url = %config.database_url_masked(),
        server_addr = %config.server_addr,
        max_connections = config.database_max_connections,
        "Configuration loaded"
    );

    // Create database connection pool
    let db_pool = create_database_pool(&config).await?;
    info!("Database connection pool established");

    // Run database migrations
    run_migrations(&db_pool).await?;
    info!("Database migrations completed");

    // Start HTTP server
    let server_handle = tokio::spawn({
        let db_pool = db_pool.clone();
        let addr = config.server_addr;
        async move {
            if let Err(e) = kapsel_api::start_server(db_pool, addr).await {
                error!(error = %e, "Server failed");
            }
        }
    });

    info!(addr = %config.server_addr, "Kapsel is ready to receive webhooks");

    // Wait for shutdown signal
    shutdown_signal().await;
    info!("Shutdown signal received, starting graceful shutdown");

    // Give in-flight requests time to complete
    tokio::select! {
        _ = tokio::time::sleep(Duration::from_secs(30)) => {
            info!("Shutdown grace period expired");
        }
        _ = server_handle => {
            info!("Server stopped");
        }
    }

    // Close database connections
    db_pool.close().await;
    info!("Database connections closed");

    info!("Kapsel shutdown complete");
    Ok(())
}

/// Initializes tracing with environment-based configuration.
fn init_tracing() {
    use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

    let filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new("info,kapsel=debug,tower_http=debug"))
        .expect("Invalid RUST_LOG environment variable");

    let fmt_layer = fmt::layer()
        .with_target(true)
        .with_thread_ids(true)
        .with_thread_names(true)
        .with_file(true)
        .with_line_number(true);

    tracing_subscriber::registry().with(filter).with(fmt_layer).init();
}

/// Creates the database connection pool with retry logic.
async fn create_database_pool(config: &Config) -> Result<sqlx::PgPool> {
    let mut retries = 0;
    const MAX_RETRIES: u32 = 5;
    const RETRY_DELAY: Duration = Duration::from_secs(2);

    loop {
        match PgPoolOptions::new()
            .max_connections(config.database_max_connections)
            .min_connections(2)
            .acquire_timeout(Duration::from_secs(10))
            .idle_timeout(Duration::from_secs(600))
            .max_lifetime(Duration::from_secs(1800))
            .connect(&config.database_url)
            .await
        {
            Ok(pool) => {
                // Verify connection works
                sqlx::query("SELECT 1")
                    .fetch_one(&pool)
                    .await
                    .context("Failed to verify database connection")?;

                return Ok(pool);
            },
            Err(_e) if retries < MAX_RETRIES => {
                retries += 1;
                info!(
                    attempt = retries,
                    max_retries = MAX_RETRIES,
                    "Database connection failed, retrying..."
                );
                tokio::time::sleep(RETRY_DELAY).await;
            },
            Err(e) => {
                return Err(e).context("Failed to create database connection pool after retries");
            },
        }
    }
}

/// Runs database migrations.
async fn run_migrations(pool: &sqlx::PgPool) -> Result<()> {
    // TODO: Use sqlx::migrate! macro once migrations are set up
    // For now, ensure tables exist

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
            UNIQUE(endpoint_id, source_event_id)
        )
        "#,
    )
    .execute(pool)
    .await
    .context("Failed to create webhook_events table")?;

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
    .await
    .context("Failed to create endpoints table")?;

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
    .await
    .context("Failed to create delivery_attempts table")?;

    // Create indexes
    sqlx::query(
        r#"
        CREATE INDEX IF NOT EXISTS idx_webhook_events_status
        ON webhook_events(status, next_retry_at)
        WHERE status IN ('pending', 'delivering')
        "#,
    )
    .execute(pool)
    .await
    .context("Failed to create webhook_events status index")?;

    sqlx::query(
        r#"
        CREATE INDEX IF NOT EXISTS idx_webhook_events_tenant
        ON webhook_events(tenant_id, received_at DESC)
        "#,
    )
    .execute(pool)
    .await
    .context("Failed to create webhook_events tenant index")?;

    sqlx::query(
        r#"
        CREATE INDEX IF NOT EXISTS idx_delivery_attempts_event
        ON delivery_attempts(event_id, attempt_number)
        "#,
    )
    .execute(pool)
    .await
    .context("Failed to create delivery_attempts index")?;

    Ok(())
}

/// Waits for shutdown signal (CTRL+C or SIGTERM).
async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c().await.expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {
            info!("Received CTRL+C signal");
        },
        _ = terminate => {
            info!("Received SIGTERM signal");
        },
    }
}

/// Service configuration.
struct Config {
    /// PostgreSQL connection string
    database_url: String,
    /// Maximum database connections
    database_max_connections: u32,
    /// Server bind address
    server_addr: SocketAddr,
}

impl Config {
    /// Loads configuration from environment variables.
    fn from_env() -> Result<Self> {
        let database_url =
            std::env::var("DATABASE_URL").context("DATABASE_URL environment variable not set")?;

        let database_max_connections = std::env::var("DATABASE_MAX_CONNECTIONS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(10);

        let server_addr = std::env::var("SERVER_ADDR")
            .unwrap_or_else(|_| "127.0.0.1:8080".to_string())
            .parse()
            .context("Invalid SERVER_ADDR format")?;

        Ok(Self { database_url, database_max_connections, server_addr })
    }

    /// Returns database URL with password masked for logging.
    fn database_url_masked(&self) -> String {
        if let Some(at_pos) = self.database_url.find('@') {
            if let Some(password_start) = self.database_url[..at_pos].rfind(':') {
                if let Some(user_start) = self.database_url[..password_start].rfind('/') {
                    return format!(
                        "{}//{}:***@{}",
                        &self.database_url[..user_start],
                        &self.database_url[user_start + 2..password_start],
                        &self.database_url[at_pos + 1..]
                    );
                }
            }
        }
        // Fallback: just return postgresql://***
        "postgresql://***".to_string()
    }
}
