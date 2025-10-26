//! Health check handlers for service monitoring.
//!
//! Provides liveness, readiness, and health endpoints with database
//! connectivity checks for orchestration systems like Kubernetes.

use std::sync::Arc;

use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use chrono::{DateTime, Utc};
use kapsel_core::storage::Storage;
use serde::Serialize;
use tracing::{debug, error, instrument};

/// Health check response structure.
#[derive(Debug, Serialize)]
pub struct HealthResponse {
    /// Overall service health status
    pub status: HealthStatus,
    /// Timestamp when health check was performed
    pub timestamp: DateTime<Utc>,
    /// Individual component health checks
    pub checks: HealthChecks,
    /// Service version information
    pub version: String,
}

/// Overall health status enumeration.
#[derive(Debug, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum HealthStatus {
    /// All systems operational
    Healthy,
    /// Some non-critical issues detected
    Degraded,
    /// Critical systems failing
    Unhealthy,
}

/// Individual component health check results.
#[derive(Debug, Serialize)]
pub struct HealthChecks {
    /// Database connectivity and basic query test
    pub database: ComponentHealth,
}

/// Health status for individual components.
#[derive(Debug, Serialize)]
pub struct ComponentHealth {
    /// Component status
    pub status: ComponentStatus,
    /// Optional error message if unhealthy
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// Response time in milliseconds
    pub response_time_ms: u64,
}

/// Component-level health status.
#[derive(Debug, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ComponentStatus {
    /// Component is healthy
    Up,
    /// Component is experiencing issues
    Down,
}

/// Primary health check endpoint.
///
/// Performs lightweight checks of critical system components including
/// database connectivity. Returns structured JSON with overall health
/// status and individual component details.
///
/// This endpoint is designed to be called frequently by orchestration
/// systems and load balancers, so it avoids expensive operations.
#[instrument(name = "health_check", skip(storage))]
pub async fn health_check(State(storage): State<Arc<Storage>>) -> Response {
    debug!("Performing health check");

    let timestamp = Utc::now();
    let start_time = std::time::Instant::now();

    let db_health = check_database_health(&storage).await;
    let db_duration = start_time.elapsed();

    let overall_status = match db_health.status {
        ComponentStatus::Up => HealthStatus::Healthy,
        ComponentStatus::Down => HealthStatus::Unhealthy,
    };

    let response = HealthResponse {
        status: overall_status,
        timestamp,
        checks: HealthChecks {
            database: ComponentHealth {
                status: db_health.status,
                message: db_health.message,
                response_time_ms: u64::try_from(db_duration.as_millis()).unwrap_or(u64::MAX),
            },
        },
        version: env!("CARGO_PKG_VERSION").to_string(),
    };

    let status_code = match response.status {
        HealthStatus::Healthy | HealthStatus::Degraded => StatusCode::OK,
        HealthStatus::Unhealthy => StatusCode::SERVICE_UNAVAILABLE,
    };

    debug!(
        status = ?response.status,
        db_status = ?response.checks.database.status,
        "Health check completed"
    );

    (status_code, Json(response)).into_response()
}

/// Checks database health by performing a simple connectivity test.
///
/// Executes a lightweight query to verify the database connection
/// is working properly. Does not perform expensive operations.
pub async fn check_database_health(storage: &Storage) -> DatabaseHealth {
    let _start_time = std::time::Instant::now();

    match storage.health_check().await {
        Ok(()) => {
            debug!("Database health check passed");
            DatabaseHealth { status: ComponentStatus::Up, message: None }
        },
        Err(e) => {
            error!("Database health check failed: {}", e);
            DatabaseHealth {
                status: ComponentStatus::Down,
                message: Some(format!("Database connection failed: {e}")),
            }
        },
    }
}

/// Internal structure for database health check results.
pub struct DatabaseHealth {
    /// Current status of the database component
    pub status: ComponentStatus,
    /// Optional status message with additional details
    pub message: Option<String>,
}

/// Readiness check endpoint for Kubernetes probes.
///
/// Similar to health check but focuses on whether the service is ready
/// to accept traffic.
///
/// TODO: Currently identical to health check but could
/// include additional startup-specific checks in the future.
#[instrument(name = "readiness_check", skip(storage))]
pub async fn readiness_check(State(storage): State<Arc<Storage>>) -> Response {
    health_check(State(storage)).await
}

/// Liveness check endpoint for Kubernetes probes.
///
/// Returns a simple response indicating the service process is alive.
/// This is a minimal check that doesn't test external dependencies,
/// focusing only on whether the HTTP server is responding.
#[instrument(name = "liveness_check")]
pub async fn liveness_check() -> Response {
    debug!("Performing liveness check");

    let response = serde_json::json!({
        "status": "alive",
        "timestamp": Utc::now(),
        "service": "kapsel-api"
    });

    (StatusCode::OK, Json(response)).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn liveness_check_always_succeeds() {
        let response = liveness_check().await;

        assert_eq!(response.status(), StatusCode::OK);
    }
}
