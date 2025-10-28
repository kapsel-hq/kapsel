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
use kapsel_core::{storage::Storage, Clock};
use serde::Serialize;
use tracing::{debug, error, instrument};

use crate::AppState;

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
/// Health service that encapsulates clock dependency for testable health
/// checks.
pub struct HealthService {
    clock: Arc<dyn Clock>,
}

impl HealthService {
    /// Creates a new health service with the given clock.
    pub fn new(clock: Arc<dyn Clock>) -> Self {
        Self { clock }
    }

    /// Performs comprehensive service health checks.
    pub async fn health_check(&self, storage: &Storage) -> HealthResponse {
        debug!("Performing health check");

        let timestamp = DateTime::<Utc>::from(self.clock.now_system());
        let start_time = self.clock.now();

        let db_health = self.check_database_health(storage).await;
        let db_duration = start_time.elapsed();

        let overall_status = match db_health.status {
            ComponentStatus::Up => HealthStatus::Healthy,
            ComponentStatus::Down => HealthStatus::Unhealthy,
        };

        HealthResponse {
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
        }
    }

    /// Checks database connectivity and health.
    ///
    /// Executes a lightweight query to verify the database connection
    /// is working properly. Does not perform expensive operations.
    async fn check_database_health(&self, storage: &Storage) -> DatabaseHealth {
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
}

/// Health check endpoint handler.
///
/// This endpoint is designed to be called frequently by orchestration
/// systems and load balancers, so it avoids expensive operations.
#[instrument(name = "health_check", skip(app_state))]
pub async fn health_check(State(app_state): State<AppState>) -> Response {
    let health_service = HealthService::new(app_state.clock.clone());
    let response = health_service.health_check(&app_state.storage).await;

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
#[instrument(name = "readiness_check", skip(app_state))]
pub async fn readiness_check(State(app_state): State<AppState>) -> Response {
    health_check(State(app_state)).await
}

/// Liveness check endpoint for Kubernetes probes.
///
/// Returns a simple response indicating the service process is alive.
/// This is a minimal check that doesn't test external dependencies,
/// focusing only on whether the HTTP server is responding.
#[instrument(name = "liveness_check", skip(app_state))]
pub async fn liveness_check(State(app_state): State<AppState>) -> Response {
    debug!("Performing liveness check");

    let response = serde_json::json!({
        "status": "alive",
        "timestamp": chrono::DateTime::<chrono::Utc>::from(app_state.clock.now_system()),
        "service": "kapsel-api"
    });

    (StatusCode::OK, Json(response)).into_response()
}
