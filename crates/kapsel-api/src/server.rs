//! HTTP server configuration and request routing.
//!
//! Provides Axum server setup with middleware stack, graceful shutdown,
//! and connection pooling integration for webhook ingestion endpoints.
//! Requests flow through middleware in order:
//! 1. Request ID generation
//! 2. Request/response logging
//! 3. Timeout enforcement (30s default)
//! 4. CORS handling (if configured)
//! 5. Authentication (future)
//! 6. Rate limiting (future)
//! 7. Handler execution
//!
//! # Graceful Shutdown
//!
//! The server handles SIGTERM gracefully:
//! - Stops accepting new connections
//! - Waits for in-flight requests (30s max)
//! - Closes database connections
//! - Returns appropriate exit code

use std::{net::SocketAddr, sync::Arc, time::Duration};

use axum::{
    extract::Request,
    middleware::{self, Next},
    response::Response,
    routing::{get, post},
    Router,
};
use kapsel_core::{storage::Storage, Clock};
use sqlx::PgPool;
use tower_http::{timeout::TimeoutLayer, trace::TraceLayer};
use tracing::{info, warn};
use uuid::Uuid;

use crate::{config::Config, handlers, middleware::auth::auth_middleware, AppState};

/// Creates the Axum router with all routes and middleware.
///
/// Sets up:
/// - All API endpoints
/// - Request tracing and logging
/// - Timeout handling (configurable)
/// - Shared application state
///
/// # Example
///
/// ```no_run
/// use kapsel_api::{server::create_router, Config};
/// use sqlx::PgPool;
///
/// async fn start(db: PgPool, config: &Config) {
///     let app = create_router(db, config);
///     // Serve the app...
/// }
/// ```
pub fn create_router(storage: Arc<Storage>, clock: Arc<dyn Clock>, config: &Config) -> Router {
    let app_state = AppState::new(storage.clone(), clock);

    let health_routes = Router::new()
        .route("/health", get(handlers::health_check))
        .route("/ready", get(handlers::readiness_check))
        .route("/live", get(handlers::liveness_check))
        .with_state(app_state.clone());

    let api_routes = Router::new()
        .route("/ingest/{endpoint_id}", post(handlers::ingest_webhook))
        .layer(middleware::from_fn_with_state(storage.clone(), auth_middleware))
        .with_state(app_state);

    Router::new()
        .merge(health_routes)
        .merge(api_routes)
        .layer(TimeoutLayer::new(Duration::from_secs(config.request_timeout)))
        .layer(TraceLayer::new_for_http())
        .layer(middleware::from_fn(inject_request_id))
        .with_state(storage)
}

/// Middleware to inject request ID into all responses.
///
/// Adds X-Request-Id header for tracing requests across services.
async fn inject_request_id(req: Request, next: Next) -> Response {
    let request_id = Uuid::new_v4().to_string();

    let mut req = req;
    req.extensions_mut().insert(request_id.clone());

    let mut response = next.run(req).await;

    if let Ok(header_value) = request_id.parse() {
        response.headers_mut().insert("X-Request-Id", header_value);
    }

    response
}

/// Starts the HTTP server with graceful shutdown support.
///
/// Binds to the specified address and serves requests until shutdown
/// signal received. Handles graceful shutdown with timeout.
///
/// # Errors
///
/// Returns `std::io::Error` if:
/// - Port is already in use
/// - Network interface unavailable
/// - TLS configuration invalid (future)
///
/// # Example
///
/// ```no_run
/// use std::net::SocketAddr;
///
/// use kapsel_api::{server::start_server, Config};
/// use sqlx::PgPool;
///
/// #[tokio::main]
/// async fn main() -> Result<(), Box<dyn std::error::Error>> {
///     let db = PgPool::connect("postgresql://...").await?;
///     let config = Config::load()?;
///     let addr = "127.0.0.1:8080".parse()?;
///
///     start_server(db, &config, addr).await?;
///     Ok(())
/// }
/// ```
/// Starts the HTTP server on the specified address.
pub async fn start_server(
    db: PgPool,
    clock: Arc<dyn Clock>,
    config: &Config,
    addr: SocketAddr,
) -> Result<(), std::io::Error> {
    let storage = Arc::new(Storage::new(db, &clock));
    let app = create_router(storage, clock, config);

    info!("Starting HTTP server on {}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    let actual_addr = listener.local_addr()?;

    info!("HTTP server listening on {}", actual_addr);

    axum::serve(listener, app).with_graceful_shutdown(shutdown_signal()).await?;

    info!("HTTP server stopped gracefully");
    Ok(())
}

/// Waits for shutdown signal (CTRL+C or SIGTERM).
///
/// Enables graceful shutdown on:
/// - CTRL+C (SIGINT) - Development
/// - SIGTERM - Kubernetes/Docker
async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(e) = tokio::signal::ctrl_c().await {
            tracing::error!("Failed to install Ctrl+C handler: {}", e);
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            },
            Err(e) => {
                tracing::error!("Failed to install SIGTERM handler: {}", e);
            },
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {
            info!("Received CTRL+C, starting graceful shutdown");
        },
        () = terminate => {
            info!("Received SIGTERM, starting graceful shutdown");
        },
    }

    warn!("Waiting for in-flight requests to complete");
}

/// Test helper to create router with default config.
pub fn create_test_router(pool: PgPool, clock: Arc<dyn Clock>) -> Router {
    let storage = Arc::new(Storage::new(pool, &clock));
    let config = Config::default();
    create_router(storage, clock, &config)
}
