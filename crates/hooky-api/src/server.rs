//! HTTP server setup.

use axum::{routing::post, Router};
use sqlx::PgPool;
use std::net::SocketAddr;

use crate::handlers;

/// Creates the Axum router with all routes.
pub fn create_router(db: PgPool) -> Router {
    Router::new()
        .route("/ingest/:endpoint_id", post(handlers::ingest_webhook))
        .with_state(db)
}

/// Starts the HTTP server.
pub async fn start_server(db: PgPool, addr: SocketAddr) -> Result<(), std::io::Error> {
    let app = create_router(db);

    let listener = tokio::net::TcpListener::bind(addr).await?;

    axum::serve(listener, app).await
}
