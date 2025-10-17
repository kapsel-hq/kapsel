//! Health check handler for monitoring service status.
//!
//! Provides lightweight health monitoring endpoints for orchestration systems
//! like Kubernetes. Checks critical dependencies like database connectivity
//! and returns structured health information.

use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::PgPool;
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
#[instrument(name = "health_check", skip(db))]
pub async fn health_check(State(db): State<PgPool>) -> Response {
    debug!("Performing health check");

    let timestamp = Utc::now();
    let start_time = std::time::Instant::now();

    // Check database connectivity
    let db_health = check_database_health(&db).await;
    let db_duration = start_time.elapsed();

    // Determine overall status
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
async fn check_database_health(db: &PgPool) -> DatabaseHealth {
    let _start_time = std::time::Instant::now();

    match sqlx::query_scalar::<_, i32>("SELECT 1").fetch_one(db).await {
        Ok(1) => {
            debug!("Database health check passed");
            DatabaseHealth { status: ComponentStatus::Up, message: None }
        },
        Ok(_) => {
            error!("Database health check returned unexpected result");
            DatabaseHealth {
                status: ComponentStatus::Down,
                message: Some("Database query returned unexpected result".to_string()),
            }
        },
        Err(e) => {
            error!(error = %e, "Database health check failed");
            DatabaseHealth {
                status: ComponentStatus::Down,
                message: Some(format!("Database connection failed: {e}")),
            }
        },
    }
}

/// Internal structure for database health check results.
struct DatabaseHealth {
    status: ComponentStatus,
    message: Option<String>,
}

/// Readiness check endpoint for Kubernetes probes.
///
/// Similar to health check but focuses on whether the service is ready
/// to accept traffic. Currently identical to health check but could
/// include additional startup-specific checks in the future.
#[instrument(name = "readiness_check", skip(db))]
pub async fn readiness_check(State(db): State<PgPool>) -> Response {
    // For now, readiness is the same as health
    // In the future, this could include additional checks like:
    // - Configuration loaded
    // - External dependencies available
    // - Startup procedures completed
    health_check(State(db)).await
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
    use kapsel_testing::TestEnv;

    use super::*;

    #[tokio::test]
    async fn database_health_check_succeeds_with_valid_connection() {
        let env = TestEnv::new().await.expect("test env setup");

        let health = check_database_health(env.pool()).await;

        assert!(matches!(health.status, ComponentStatus::Up));
        assert!(health.message.is_none());
    }

    #[tokio::test]
    async fn health_check_returns_structured_response() {
        let env = TestEnv::new().await.expect("test env setup");

        let response = health_check(State(env.pool().clone())).await;

        // Should return OK status for healthy database
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn liveness_check_always_succeeds() {
        let response = liveness_check().await;

        assert_eq!(response.status(), StatusCode::OK);
    }
}
