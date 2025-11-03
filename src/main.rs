//! Kapsel webhook reliability service.
//!
//! Main entry point for the Kapsel server. Initializes all subsystems
//! and coordinates graceful startup and shutdown.

use std::{sync::Arc, time::Duration};

use anyhow::{Context, Result};
use kapsel_api::Config;
use kapsel_core::{time::RealClock, Clock};
use kapsel_delivery::DeliveryEngine;
use sqlx::postgres::PgPoolOptions;
use tracing::{error, info};

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();

    info!("Starting Kapsel webhook reliability service");

    let config = Config::load()?;
    info!(
        database_url = %config.database_url_masked(),
        server_addr = %format!("{}:{}", config.host, config.port),
        max_connections = config.database_max_connections,
        worker_count = config.worker_pool_size,
        attestation_batch_size = config.attestation_batch_size,
        "Configuration loaded from defaults → kapsel.toml → environment variables"
    );

    let db_pool = create_database_pool(&config).await?;
    info!("Database connection pool established");

    run_migrations(&db_pool).await?;
    info!("Database migrations completed");

    let clock: Arc<dyn Clock> = Arc::new(RealClock::new());

    let mut delivery_engine =
        DeliveryEngine::new(&db_pool, config.to_delivery_config(), clock.clone())?;

    delivery_engine.start().await.context("Failed to start delivery engine")?;
    info!(worker_count = config.worker_pool_size, "Delivery engine started");

    let server_handle = tokio::spawn({
        let db_pool = db_pool.clone();
        let config = config.clone();
        let addr = config.parse_server_addr().context("Invalid server address")?;
        let clock = clock.clone();
        async move {
            if let Err(e) = kapsel_api::start_server(db_pool, clock, &config, addr).await {
                error!(error = %e, "Server failed");
            }
        }
    });

    info!(addr = %format!("{}:{}", config.host, config.port), "Kapsel is ready to receive webhooks");

    shutdown_signal().await?;
    info!("Shutdown signal received, starting graceful shutdown");

    info!("Shutting down delivery engine");
    delivery_engine.shutdown().await.context("Delivery engine shutdown failed")?;

    tokio::select! {
        _ = tokio::time::sleep(Duration::from_secs(config.delivery_timeout_seconds)) => {
            info!("Shutdown grace period expired");
        }
        _ = server_handle => {
            info!("Server stopped");
        }
    }

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
            .min_connections(config.database_min_connections)
            .acquire_timeout(Duration::from_secs(config.database_connection_timeout))
            .idle_timeout(Duration::from_secs(config.database_idle_timeout))
            .max_lifetime(Duration::from_secs(config.database_max_lifetime))
            .connect(&config.database_url)
            .await
        {
            Ok(pool) => {
                // Pool creation can succeed even if database is unreachable.
                // Force actual connection attempt to detect issues early.
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
    sqlx::migrate!("./migrations").run(pool).await.context("Failed to run database migrations")?;

    info!("Database migrations completed successfully");
    Ok(())
}

/// Waits for shutdown signal (CTRL+C or SIGTERM).
async fn shutdown_signal() -> Result<()> {
    let ctrl_c =
        async { tokio::signal::ctrl_c().await.context("Failed to install Ctrl+C handler") };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .context("Failed to install SIGTERM handler")?
            .recv()
            .await;
        Ok::<_, anyhow::Error>(())
    };

    #[cfg(not(unix))]
    let terminate = async { Ok::<_, anyhow::Error>(()) };

    tokio::select! {
        result = ctrl_c => {
            result?;
            info!("Received CTRL+C signal");
        },
        result = terminate => {
            result?;
            info!("Received SIGTERM signal");
        },
    }

    Ok(())
}
