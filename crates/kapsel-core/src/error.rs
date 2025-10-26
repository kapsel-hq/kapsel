//! Error types and result handling for webhook operations.
//!
//! Defines structured error taxonomy with codes for client disambiguation
//! and proper HTTP status mapping. Covers validation, processing, and
//! infrastructure failures across the webhook reliability pipeline.

use thiserror::Error;

use crate::EndpointId;

/// Result type alias using `CoreError`.
pub type Result<T> = std::result::Result<T, CoreError>;

/// Core error type for internal operations.
#[derive(Debug, Error)]
pub enum CoreError {
    /// Database operation failed.
    #[error("Database error: {0}")]
    Database(String),

    /// Entity not found.
    #[error("Not found: {0}")]
    NotFound(String),

    /// Constraint violation.
    #[error("Constraint violation: {0}")]
    ConstraintViolation(String),

    /// Invalid input.
    #[error("Invalid input: {0}")]
    InvalidInput(String),
}

impl From<sqlx::Error> for CoreError {
    fn from(err: sqlx::Error) -> Self {
        match err {
            sqlx::Error::RowNotFound => Self::NotFound("requested entity not found".to_string()),
            sqlx::Error::Database(db_err) if db_err.is_unique_violation() => {
                Self::ConstraintViolation(format!("unique constraint violation: {}", db_err))
            },
            sqlx::Error::Database(db_err) if db_err.is_foreign_key_violation() => {
                Self::ConstraintViolation(format!("foreign key constraint violation: {}", db_err))
            },
            sqlx::Error::Database(db_err) if db_err.is_check_violation() => {
                Self::ConstraintViolation(format!("check constraint violation: {}", db_err))
            },
            _ => Self::Database(err.to_string()),
        }
    }
}

/// Kapsel error types with codes matching specification.
#[derive(Debug, Error)]
pub enum KapselError {
    // Application Errors (E1001-E1005)
    /// HMAC signature validation failed (E1001).
    #[error("[E1001] Invalid signature: HMAC validation failed")]
    InvalidSignature,

    /// Payload exceeds 10MB limit (E1002).
    #[error("[E1002] Payload too large: size {size_bytes} bytes exceeds 10MB limit")]
    PayloadTooLarge {
        /// Size of the payload in bytes
        size_bytes: usize,
    },

    /// Endpoint not found (E1003).
    #[error("[E1003] Invalid endpoint: endpoint {id} not found")]
    InvalidEndpoint {
        /// The endpoint ID that was not found
        id: EndpointId,
    },

    /// Tenant quota exceeded (E1004).
    #[error("[E1004] Rate limited: tenant quota exceeded")]
    RateLimited,

    /// Duplicate event detected by idempotency check (E1005).
    #[error("[E1005] Duplicate event: {source_event_id} already processed")]
    DuplicateEvent {
        /// The source event ID that was already processed
        source_event_id: String,
    },

    // Delivery Errors (E2001-E2005)
    /// Target endpoint unavailable (E2001).
    #[error("[E2001] Connection refused: target endpoint unavailable")]
    ConnectionRefused,

    /// Request timeout exceeded (E2002).
    #[error("[E2002] Connection timeout: exceeded {timeout_ms}ms")]
    ConnectionTimeout {
        /// Timeout duration that was exceeded in milliseconds
        timeout_ms: u64,
    },

    /// HTTP 4xx client error (E2003).
    #[error("[E2003] HTTP client error: {status} response from endpoint")]
    HttpClientError {
        /// HTTP status code returned by the endpoint
        status: u16,
    },

    /// HTTP 5xx server error (E2004).
    #[error("[E2004] HTTP server error: {status} response from endpoint")]
    HttpServerError {
        /// HTTP status code returned by the endpoint
        status: u16,
    },

    /// Circuit breaker is open (E2005).
    #[error("[E2005] Circuit open: endpoint {id} circuit breaker triggered")]
    CircuitOpen {
        /// The endpoint ID whose circuit breaker is open
        id: EndpointId,
    },

    // System Errors (E3001-E3004)
    /// PostgreSQL connection failed (E3001).
    #[error("[E3001] Database unavailable: PostgreSQL connection failed")]
    DatabaseUnavailable,

    /// Bounded channel at capacity (E3003).
    #[error("[E3003] Queue full: channel at capacity")]
    QueueFull,

    /// No available workers (E3004).
    #[error("[E3004] Worker pool exhausted: no available workers")]
    WorkerPoolExhausted,

    /// Generic database error.
    #[error("Database error: {0}")]
    Database(#[from] sqlx::Error),

    /// Generic HTTP error.
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    /// Generic error for wrapping other errors.
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl KapselError {
    /// Returns the error code (E1001-E3004).
    pub const fn code(&self) -> &'static str {
        match self {
            Self::InvalidSignature => "E1001",
            Self::PayloadTooLarge { .. } => "E1002",
            Self::InvalidEndpoint { .. } => "E1003",
            Self::RateLimited => "E1004",
            Self::DuplicateEvent { .. } => "E1005",
            Self::ConnectionRefused => "E2001",
            Self::ConnectionTimeout { .. } => "E2002",
            Self::HttpClientError { .. } => "E2003",
            Self::HttpServerError { .. } => "E2004",
            Self::CircuitOpen { .. } => "E2005",
            Self::DatabaseUnavailable => "E3001",

            Self::QueueFull => "E3003",
            Self::WorkerPoolExhausted => "E3004",
            Self::Database(_) | Self::Http(_) | Self::Other(_) => "E9999",
        }
    }

    /// Returns whether this error should trigger a retry.
    pub const fn is_retryable(&self) -> bool {
        matches!(
            self,
            Self::RateLimited
                | Self::ConnectionRefused
                | Self::ConnectionTimeout { .. }
                | Self::HttpServerError { .. }
                | Self::CircuitOpen { .. }
                | Self::DatabaseUnavailable
                | Self::QueueFull
                | Self::WorkerPoolExhausted
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_codes_match_specification() {
        assert_eq!(KapselError::InvalidSignature.code(), "E1001");
        assert_eq!(KapselError::PayloadTooLarge { size_bytes: 0 }.code(), "E1002");
        assert_eq!(KapselError::RateLimited.code(), "E1004");
        assert_eq!(KapselError::ConnectionRefused.code(), "E2001");
        assert_eq!(KapselError::DatabaseUnavailable.code(), "E3001");
    }

    #[test]
    fn retryable_errors_identified() {
        assert!(!KapselError::InvalidSignature.is_retryable());
        assert!(!KapselError::PayloadTooLarge { size_bytes: 0 }.is_retryable());
        assert!(KapselError::RateLimited.is_retryable());
        assert!(KapselError::ConnectionRefused.is_retryable());
        assert!(KapselError::HttpServerError { status: 500 }.is_retryable());
        assert!(!KapselError::HttpClientError { status: 400 }.is_retryable());
    }
}
