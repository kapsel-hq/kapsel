//! Error types for webhook delivery operations.
//!
//! Defines all error conditions that can occur during webhook delivery,
//! including network failures, HTTP errors, circuit breaker states, and
//! database operations. Errors include context for debugging and proper
//! categorization for retry decisions.

use std::fmt;

use thiserror::Error;

/// Result type alias for delivery operations.
pub type Result<T> = std::result::Result<T, DeliveryError>;

/// Comprehensive error types for webhook delivery operations.
#[derive(Debug, Clone, Error)]
pub enum DeliveryError {
    /// Network-level connectivity failure.
    #[error("network connection failed: {message}")]
    NetworkError {
        /// Error message describing the network failure
        message: String,
    },

    /// HTTP request timeout exceeded.
    #[error("request timeout after {timeout_seconds}s")]
    Timeout {
        /// Number of seconds before the request timed out
        timeout_seconds: u64,
    },

    /// HTTP response indicated client error (4xx).
    #[error("client error: HTTP {status_code}")]
    ClientError {
        /// HTTP status code (4xx)
        status_code: u16,
        /// Response body content
        body: String,
    },

    /// HTTP response indicated server error (5xx).
    #[error("server error: HTTP {status_code}")]
    ServerError {
        /// HTTP status code (5xx)
        status_code: u16,
        /// Response body content
        body: String,
    },

    /// Rate limit exceeded with retry guidance.
    #[error("rate limited: retry after {retry_after_seconds}s")]
    RateLimited {
        /// Seconds to wait before retrying
        retry_after_seconds: u64,
    },

    /// Circuit breaker is open, delivery blocked.
    #[error("circuit breaker open for endpoint {endpoint_id}")]
    CircuitOpen {
        /// Identifier of the endpoint with open circuit
        endpoint_id: String,
    },

    /// All retry attempts exhausted.
    #[error("delivery failed after {attempts} attempts")]
    RetriesExhausted {
        /// Number of retry attempts made
        attempts: u32,
    },

    /// Database operation failed during delivery.
    #[error("database error: {message}")]
    DatabaseError {
        /// Database error message
        message: String,
    },

    /// Invalid endpoint configuration.
    #[error("invalid endpoint configuration: {message}")]
    ConfigurationError {
        /// Configuration error message
        message: String,
    },

    /// Worker shutdown requested.
    #[error("worker shutdown requested")]
    ShutdownRequested,

    /// Unexpected internal error.
    #[error("internal delivery error: {message}")]
    InternalError {
        /// Internal error message
        message: String,
    },
}

impl DeliveryError {
    /// Creates a network error from a message.
    pub fn network(message: impl Into<String>) -> Self {
        Self::NetworkError { message: message.into() }
    }

    /// Creates a timeout error.
    pub fn timeout(timeout_seconds: u64) -> Self {
        Self::Timeout { timeout_seconds }
    }

    /// Creates a client error from HTTP response.
    pub fn client_error(status_code: u16, body: impl Into<String>) -> Self {
        Self::ClientError { status_code, body: body.into() }
    }

    /// Creates a server error from HTTP response.
    pub fn server_error(status_code: u16, body: impl Into<String>) -> Self {
        Self::ServerError { status_code, body: body.into() }
    }

    /// Creates a rate limit error with retry guidance.
    pub fn rate_limited(retry_after_seconds: u64) -> Self {
        Self::RateLimited { retry_after_seconds }
    }

    /// Creates a circuit open error.
    pub fn circuit_open(endpoint_id: impl Into<String>) -> Self {
        Self::CircuitOpen { endpoint_id: endpoint_id.into() }
    }

    /// Creates a retries exhausted error.
    pub fn retries_exhausted(attempts: u32) -> Self {
        Self::RetriesExhausted { attempts }
    }

    /// Creates a database error.
    pub fn database(message: impl Into<String>) -> Self {
        Self::DatabaseError { message: message.into() }
    }

    /// Creates a configuration error.
    pub fn configuration(message: impl Into<String>) -> Self {
        Self::ConfigurationError { message: message.into() }
    }

    /// Creates an internal error.
    pub fn internal(message: impl Into<String>) -> Self {
        Self::InternalError { message: message.into() }
    }

    /// Determines if this error represents a temporary failure that should be
    /// retried.
    ///
    /// Returns `true` for network errors, timeouts, server errors (5xx), and
    /// rate limits. Returns `false` for client errors (4xx), circuit breaker
    /// states, configuration issues, and exhausted retries.
    pub fn is_retryable(&self) -> bool {
        match self {
            // Retryable errors - temporary network/server issues
            Self::NetworkError { .. }
            | Self::Timeout { .. }
            | Self::ServerError { .. }
            | Self::RateLimited { .. }
            | Self::DatabaseError { .. } => true,

            // Non-retryable errors - client issues or circuit protection
            Self::ClientError { .. }
            | Self::CircuitOpen { .. }
            | Self::RetriesExhausted { .. }
            | Self::ConfigurationError { .. }
            | Self::ShutdownRequested
            | Self::InternalError { .. } => false,
        }
    }

    /// Returns the suggested retry delay in seconds for retryable errors.
    ///
    /// Uses the Retry-After header value for rate limits, or None to indicate
    /// standard exponential backoff should be used.
    pub fn retry_after_seconds(&self) -> Option<u64> {
        match self {
            Self::RateLimited { retry_after_seconds } => Some(*retry_after_seconds),
            _ => None,
        }
    }
}

/// Category of delivery error for metrics and monitoring.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCategory {
    /// Network connectivity issues.
    Network,
    /// HTTP client errors (4xx).
    Client,
    /// HTTP server errors (5xx).
    Server,
    /// Rate limiting.
    RateLimit,
    /// Circuit breaker protection.
    Circuit,
    /// Database operations.
    Database,
    /// Configuration problems.
    Configuration,
    /// Internal system errors.
    Internal,
}

impl From<&DeliveryError> for ErrorCategory {
    fn from(error: &DeliveryError) -> Self {
        match error {
            DeliveryError::NetworkError { .. } | DeliveryError::Timeout { .. } => Self::Network,
            DeliveryError::ClientError { .. } | DeliveryError::RetriesExhausted { .. } => {
                Self::Client
            },
            DeliveryError::ServerError { .. } => Self::Server,
            DeliveryError::RateLimited { .. } => Self::RateLimit,
            DeliveryError::CircuitOpen { .. } => Self::Circuit,
            DeliveryError::DatabaseError { .. } => Self::Database,
            DeliveryError::ConfigurationError { .. } => Self::Configuration,
            DeliveryError::ShutdownRequested | DeliveryError::InternalError { .. } => {
                Self::Internal
            },
        }
    }
}

impl fmt::Display for ErrorCategory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Network => write!(f, "network"),
            Self::Client => write!(f, "client"),
            Self::Server => write!(f, "server"),
            Self::RateLimit => write!(f, "rate_limit"),
            Self::Circuit => write!(f, "circuit"),
            Self::Database => write!(f, "database"),
            Self::Configuration => write!(f, "configuration"),
            Self::Internal => write!(f, "internal"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retryable_errors_identified_correctly() {
        // Retryable errors
        assert!(DeliveryError::network("connection refused").is_retryable());
        assert!(DeliveryError::timeout(30).is_retryable());
        assert!(DeliveryError::server_error(500, "internal server error").is_retryable());
        assert!(DeliveryError::rate_limited(60).is_retryable());
        assert!(DeliveryError::database("connection lost").is_retryable());

        // Non-retryable errors
        assert!(!DeliveryError::client_error(404, "not found").is_retryable());
        assert!(!DeliveryError::circuit_open("endpoint-123").is_retryable());
        assert!(!DeliveryError::retries_exhausted(5).is_retryable());
        assert!(!DeliveryError::configuration("invalid URL").is_retryable());
        assert!(!DeliveryError::ShutdownRequested.is_retryable());
    }

    #[test]
    fn rate_limit_retry_after_extracted() {
        let error = DeliveryError::rate_limited(120);
        assert_eq!(error.retry_after_seconds(), Some(120));

        let timeout_error = DeliveryError::timeout(30);
        assert_eq!(timeout_error.retry_after_seconds(), None);
    }

    #[test]
    fn error_categories_mapped_correctly() {
        assert_eq!(ErrorCategory::from(&DeliveryError::network("test")), ErrorCategory::Network);
        assert_eq!(
            ErrorCategory::from(&DeliveryError::client_error(400, "bad request")),
            ErrorCategory::Client
        );
        assert_eq!(
            ErrorCategory::from(&DeliveryError::server_error(500, "error")),
            ErrorCategory::Server
        );
        assert_eq!(ErrorCategory::from(&DeliveryError::rate_limited(60)), ErrorCategory::RateLimit);
    }

    #[test]
    fn error_display_format() {
        let error = DeliveryError::timeout(30);
        assert_eq!(error.to_string(), "request timeout after 30s");

        let circuit_error = DeliveryError::circuit_open("endpoint-123");
        assert_eq!(circuit_error.to_string(), "circuit breaker open for endpoint endpoint-123");
    }
}
