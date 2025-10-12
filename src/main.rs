//! Hooky webhook reliability service.

use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,hooky=debug")),
        )
        .init();

    tracing::info!("Starting Hooky webhook reliability service");

    // TODO: Initialize configuration
    // TODO: Setup database connection pool
    // TODO: Initialize TigerBeetle client
    // TODO: Start HTTP server
    // TODO: Start background workers

    tracing::info!("Hooky is ready to receive webhooks");

    // Keep the service running
    tokio::signal::ctrl_c().await?;
    tracing::info!("Shutting down gracefully...");

    Ok(())
}
