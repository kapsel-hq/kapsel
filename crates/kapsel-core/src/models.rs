//! Domain models for Kapsel webhook reliability service.
//!
//! This module defines the core domain entities used throughout the system.
//! All models follow Domain-Driven Design principles with clear boundaries
//! and explicit state transitions.
//!
//! # Key Concepts
//!
//! - **Event**: An incoming webhook that needs reliable delivery
//! - **Endpoint**: A configured destination for webhook delivery
//! - **Delivery Attempt**: A record of each delivery attempt with full audit
//!   trail
//!
//! # Type Safety
//!
//! We use newtype wrappers for IDs to prevent mixing different identifier types
//! at compile time. This catches bugs early and makes the code
//! self-documenting.

use std::{collections::HashMap, fmt};

use bytes::Bytes;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// Type aliases for common sqlx types to reduce verbosity
type PgDb = sqlx::Postgres;
type PgRow = sqlx::postgres::PgRow;
type PgValueRef<'r> = sqlx::postgres::PgValueRef<'r>;
type PgTypeInfo = sqlx::postgres::PgTypeInfo;
type PgArgumentBuffer = sqlx::postgres::PgArgumentBuffer;
type EncodeResult =
    Result<sqlx::encode::IsNull, Box<dyn std::error::Error + Send + Sync + 'static>>;
type BoxDynError = sqlx::error::BoxDynError;

/// Strongly-typed event identifier.
///
/// Wraps a UUID to prevent mixing with other ID types. Events are immutable
/// once created, and this ID follows them through their entire lifecycle.
///
/// # Example
///
/// ```
/// use kapsel_core::models::EventId;
/// let event_id = EventId::new();
/// println!("Processing event: {}", event_id);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EventId(pub Uuid);

impl EventId {
    /// Creates a new random event ID.
    ///
    /// Uses UUID v4 for globally unique identifiers without coordination.
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for EventId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for EventId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<Uuid> for EventId {
    fn from(uuid: Uuid) -> Self {
        Self(uuid)
    }
}

impl sqlx::Type<PgDb> for EventId {
    fn type_info() -> PgTypeInfo {
        <Uuid as sqlx::Type<PgDb>>::type_info()
    }
}

impl<'r> sqlx::Decode<'r, PgDb> for EventId {
    fn decode(value: PgValueRef<'r>) -> Result<Self, BoxDynError> {
        let uuid = <Uuid as sqlx::Decode<PgDb>>::decode(value)?;
        Ok(Self(uuid))
    }
}

impl sqlx::Encode<'_, PgDb> for EventId {
    fn encode_by_ref(&self, buf: &mut PgArgumentBuffer) -> EncodeResult {
        <Uuid as sqlx::Encode<PgDb>>::encode_by_ref(&self.0, buf)
    }
}

/// Strongly-typed tenant identifier.
///
/// Provides multi-tenancy isolation. All operations are scoped to a tenant,
/// ensuring complete data isolation between customers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TenantId(pub Uuid);

impl TenantId {
    /// Creates a new random tenant ID.
    ///
    /// Used during tenant provisioning. Once assigned, a tenant ID is
    /// immutable.
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for TenantId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for TenantId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<Uuid> for TenantId {
    fn from(uuid: Uuid) -> Self {
        Self(uuid)
    }
}

impl sqlx::Type<PgDb> for TenantId {
    fn type_info() -> PgTypeInfo {
        <Uuid as sqlx::Type<PgDb>>::type_info()
    }
}

impl<'r> sqlx::Decode<'r, PgDb> for TenantId {
    fn decode(value: PgValueRef<'r>) -> Result<Self, BoxDynError> {
        let uuid = <Uuid as sqlx::Decode<PgDb>>::decode(value)?;
        Ok(Self(uuid))
    }
}

impl sqlx::Encode<'_, PgDb> for TenantId {
    fn encode_by_ref(&self, buf: &mut PgArgumentBuffer) -> EncodeResult {
        <Uuid as sqlx::Encode<PgDb>>::encode_by_ref(&self.0, buf)
    }
}

/// Strongly-typed endpoint identifier.
///
/// Each endpoint represents a unique webhook destination URL with its own
/// delivery configuration, retry policy, and circuit breaker state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EndpointId(pub Uuid);

impl EndpointId {
    /// Creates a new random endpoint ID.
    ///
    /// Generated when a tenant registers a new webhook endpoint.
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for EndpointId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for EndpointId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<Uuid> for EndpointId {
    fn from(uuid: Uuid) -> Self {
        Self(uuid)
    }
}

impl sqlx::Type<PgDb> for EndpointId {
    fn type_info() -> PgTypeInfo {
        <Uuid as sqlx::Type<PgDb>>::type_info()
    }
}

impl<'r> sqlx::Decode<'r, PgDb> for EndpointId {
    fn decode(value: PgValueRef<'r>) -> Result<Self, BoxDynError> {
        let uuid = <Uuid as sqlx::Decode<PgDb>>::decode(value)?;
        Ok(Self(uuid))
    }
}

impl sqlx::Encode<'_, PgDb> for EndpointId {
    fn encode_by_ref(&self, buf: &mut PgArgumentBuffer) -> EncodeResult {
        <Uuid as sqlx::Encode<PgDb>>::encode_by_ref(&self.0, buf)
    }
}

/// Event lifecycle status.
///
/// Events progress through these states during processing. State transitions
/// are strictly controlled to maintain consistency:
///
/// ```text
/// Received -> Pending -> Delivering -> Delivered
///                    |              -> Failed
///                    └-> Failed (after max retries)
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventStatus {
    /// Initial state after ingestion.
    ///
    /// Event has been persisted but not yet queued for delivery.
    Received,

    /// Queued and waiting for a worker.
    ///
    /// Event is in the delivery queue but no worker has claimed it yet.
    Pending,

    /// Worker actively delivering.
    ///
    /// A worker has claimed this event and is attempting delivery.
    /// This state prevents duplicate deliveries.
    Delivering,

    /// Successfully delivered to endpoint.
    ///
    /// Terminal success state. Event will not be retried.
    Delivered,

    /// Permanently failed.
    ///
    /// Terminal failure state after all retries exhausted or
    /// non-retryable error encountered.
    Failed,

    /// Event moved to dead letter queue after permanent failure.
    ///
    /// Terminal failure state for events that cannot be delivered even after
    /// all retries exhausted. Requires manual intervention or reprocessing.
    /// Used for debugging and compliance audit trail.
    DeadLetter,
}

impl fmt::Display for EventStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Received => write!(f, "received"),
            Self::Pending => write!(f, "pending"),
            Self::Delivering => write!(f, "delivering"),
            Self::Delivered => write!(f, "delivered"),
            Self::Failed => write!(f, "failed"),
            Self::DeadLetter => write!(f, "dead_letter"),
        }
    }
}

impl sqlx::Type<PgDb> for EventStatus {
    fn type_info() -> PgTypeInfo {
        <&str as sqlx::Type<PgDb>>::type_info()
    }
}

impl<'r> sqlx::Decode<'r, PgDb> for EventStatus {
    fn decode(value: PgValueRef<'r>) -> Result<Self, BoxDynError> {
        let s = <&str as sqlx::Decode<PgDb>>::decode(value)?;
        match s {
            "received" => Ok(Self::Received),
            "pending" => Ok(Self::Pending),
            "delivering" => Ok(Self::Delivering),
            "delivered" => Ok(Self::Delivered),
            "failed" => Ok(Self::Failed),
            "dead_letter" => Ok(Self::DeadLetter),
            _ => Err(format!("invalid event status: {s}").into()),
        }
    }
}

/// Core webhook event entity.
///
/// Represents an incoming webhook that needs reliable delivery to an endpoint.
/// Tracks the complete lifecycle from ingestion to final delivery/failure.
///
/// # Idempotency
///
/// Events are deduplicated using `source_event_id` within a 24-hour window.
/// This prevents duplicate processing when source systems retry.
#[derive(Debug, Clone)]
pub struct WebhookEvent {
    /// Unique identifier for this event.
    pub id: EventId,

    /// Tenant that owns this event.
    pub tenant_id: TenantId,

    /// Target endpoint for delivery.
    pub endpoint_id: EndpointId,

    /// External ID for idempotency checking.
    ///
    /// Usually from X-Idempotency-Key header. Used to detect duplicates.
    pub source_event_id: String,

    /// Strategy for idempotency checks.
    ///
    /// Currently supports "header" based deduplication.
    pub idempotency_strategy: String,

    /// Current processing status.
    pub status: EventStatus,

    /// Number of failed delivery attempts.
    ///
    /// Incremented after each failure. Event fails permanently
    /// when this reaches the endpoint's `max_retries`.
    pub failure_count: u32,

    /// Timestamp of most recent delivery attempt.
    pub last_attempt_at: Option<DateTime<Utc>>,

    /// When to retry next (calculated using exponential backoff).
    pub next_retry_at: Option<DateTime<Utc>>,

    /// Original HTTP headers from ingestion.
    pub headers: HashMap<String, String>,

    /// Raw webhook payload.
    ///
    /// Using Bytes for zero-copy efficiency.
    pub body: Bytes,

    /// MIME type of the payload.
    pub content_type: String,

    /// When the webhook was first received.
    pub received_at: DateTime<Utc>,

    /// When successfully delivered (terminal state).
    pub delivered_at: Option<DateTime<Utc>>,

    /// When permanently failed (terminal state).
    pub failed_at: Option<DateTime<Utc>>,

    /// Size of the payload in bytes.
    ///
    /// Must be between 1 and 10MB (10485760 bytes) to satisfy database
    /// CHECK constraint. Even empty payloads are stored as size 1.
    pub payload_size: i32,

    /// Result of signature validation (if signing_secret configured).
    ///
    /// None if validation not attempted, Some(true) if valid,
    /// Some(false) if invalid.
    pub signature_valid: Option<bool>,

    /// Error message from signature validation failure.
    ///
    /// Only populated when signature_valid is Some(false).
    pub signature_error: Option<String>,

    /// Reference to immutable audit log entry in TigerBeetle.
    ///
    /// Populated after event is written to audit log for compliance.
    pub tigerbeetle_id: Option<Uuid>,
}

impl<'r> sqlx::FromRow<'r, PgRow> for WebhookEvent {
    fn from_row(row: &'r PgRow) -> Result<Self, sqlx::Error> {
        use std::collections::HashMap;

        use sqlx::Row;

        // Handle headers JSONB -> HashMap conversion
        let headers_value: serde_json::Value = row.try_get("headers")?;
        let headers: HashMap<String, String> =
            serde_json::from_value(headers_value).map_err(|e| sqlx::Error::ColumnDecode {
                index: "headers".to_string(),
                source: e.into(),
            })?;

        // Handle failure_count i32 -> u32 conversion
        let failure_count_i32: i32 = row.try_get("failure_count")?;
        let failure_count = u32::try_from(failure_count_i32).map_err(|e| {
            sqlx::Error::ColumnDecode { index: "failure_count".to_string(), source: e.into() }
        })?;

        // Handle body BYTEA -> Bytes conversion
        let body_vec: Vec<u8> = row.try_get("body")?;
        let body = Bytes::from(body_vec);

        Ok(Self {
            id: row.try_get("id")?,
            tenant_id: row.try_get("tenant_id")?,
            endpoint_id: row.try_get("endpoint_id")?,
            source_event_id: row.try_get("source_event_id")?,
            idempotency_strategy: row.try_get("idempotency_strategy")?,
            status: row.try_get("status")?,
            failure_count,
            last_attempt_at: row.try_get("last_attempt_at")?,
            next_retry_at: row.try_get("next_retry_at")?,
            headers,
            body,
            content_type: row.try_get("content_type")?,
            received_at: row.try_get("received_at")?,
            delivered_at: row.try_get("delivered_at")?,
            failed_at: row.try_get("failed_at")?,
            payload_size: row.try_get("payload_size")?,
            signature_valid: row.try_get("signature_valid")?,
            signature_error: row.try_get("signature_error")?,
            tigerbeetle_id: row.try_get("tigerbeetle_id")?,
        })
    }
}

/// Webhook endpoint configuration.
///
/// Defines where and how to deliver webhooks. Each endpoint has its own
/// retry policy, timeout settings, and circuit breaker to prevent cascading
/// failures.
#[derive(Debug, Clone)]
pub struct Endpoint {
    /// Unique identifier for this endpoint.
    pub id: EndpointId,

    /// Tenant that owns this endpoint.
    pub tenant_id: TenantId,

    /// Target URL for webhook delivery.
    ///
    /// Must be HTTPS in production environments.
    pub url: String,

    /// Human-readable endpoint name.
    pub name: String,

    /// Whether this endpoint is active and should receive webhooks.
    ///
    /// Inactive endpoints are skipped during delivery. Used for soft-disable
    /// without deleting endpoint configuration.
    pub is_active: bool,

    /// Secret for HMAC signature generation.
    ///
    /// When present, webhooks include an HMAC-SHA256 signature.
    pub signing_secret: Option<String>,

    /// HTTP header name for the signature.
    ///
    /// Defaults to "X-Webhook-Signature" if signing is enabled.
    pub signature_header: Option<String>,

    /// Maximum delivery attempts before marking as failed.
    ///
    /// Includes the initial attempt. Zero means unlimited retries.
    pub max_retries: u32,

    /// HTTP request timeout in seconds.
    ///
    /// Prevents slow endpoints from blocking workers.
    pub timeout_seconds: u32,

    /// Retry backoff strategy: exponential, linear, or fixed.
    pub retry_strategy: String,

    /// Circuit breaker current state.
    pub circuit_state: CircuitState,

    /// Consecutive failures in current window.
    ///
    /// Reset to zero on successful delivery.
    pub circuit_failure_count: u32,

    /// Consecutive successes in half-open state.
    ///
    /// Used to determine when to transition from half-open back to closed.
    /// Typically requires 3 consecutive successes.
    pub circuit_success_count: u32,

    /// When the last failure occurred.
    ///
    /// Used to implement sliding windows for circuit breaker.
    pub circuit_last_failure_at: Option<DateTime<Utc>>,

    /// When to transition from Open to `HalfOpen`.
    ///
    /// Allows periodic retry attempts after circuit opens.
    pub circuit_half_open_at: Option<DateTime<Utc>>,

    /// When this endpoint was created.
    pub created_at: DateTime<Utc>,

    /// When configuration was last modified.
    pub updated_at: DateTime<Utc>,

    /// Soft delete timestamp.
    ///
    /// When present, endpoint is logically deleted but retained for audit.
    pub deleted_at: Option<DateTime<Utc>>,

    // Performance statistics (denormalized for dashboard queries)
    /// Total number of webhook events received for this endpoint.
    pub total_events_received: i64,

    /// Total number successfully delivered (status = delivered).
    pub total_events_delivered: i64,

    /// Total number permanently failed (status = failed or dead_letter).
    pub total_events_failed: i64,
}

/// Circuit breaker state machine.
///
/// Prevents cascading failures by temporarily stopping requests to failing
/// endpoints. State transitions:
///
/// ```text
/// Closed -> Open (after threshold failures)
/// Open -> HalfOpen (after cooldown period)
/// HalfOpen -> Closed (on success)
/// HalfOpen -> Open (on failure)
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CircuitState {
    /// Normal operation, requests allowed.
    Closed,

    /// Endpoint is failing, requests blocked.
    ///
    /// No delivery attempts while in this state.
    Open,

    /// Testing if endpoint recovered.
    ///
    /// Allows limited requests to test endpoint health.
    HalfOpen,
}

impl fmt::Display for CircuitState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Closed => write!(f, "closed"),
            Self::Open => write!(f, "open"),
            Self::HalfOpen => write!(f, "half_open"),
        }
    }
}

/// Complete audit record of a delivery attempt.
///
/// Captures full request/response details for debugging and compliance.
/// Immutable once created - we never modify audit records.
#[derive(Debug, Clone)]
pub struct DeliveryAttempt {
    /// Unique identifier for this attempt.
    pub id: Uuid,

    /// Event being delivered.
    pub event_id: EventId,

    /// Sequential attempt number for this event.
    ///
    /// Starts at 1 for the first attempt.
    pub attempt_number: u32,

    /// Actual URL used (after any redirects).
    pub request_url: String,

    /// HTTP headers sent with the request.
    pub request_headers: HashMap<String, String>,

    /// HTTP method used for delivery request.
    ///
    /// Defaults to POST but endpoints may configure other methods.
    pub request_method: String,

    /// HTTP status code received.
    ///
    /// None if request failed before receiving response.
    pub response_status: Option<u16>,

    /// Response headers received.
    pub response_headers: Option<HashMap<String, String>>,

    /// Response body (truncated if too large).
    ///
    /// Useful for debugging integration issues.
    pub response_body: Option<String>,

    /// When this attempt was made.
    pub attempted_at: DateTime<Utc>,

    /// Total request duration in milliseconds.
    ///
    /// Includes connection time, TLS handshake, and response streaming.
    pub duration_ms: u32,

    /// Classification of any error that occurred.
    ///
    /// Examples: "timeout", `"connection_refused"`, `"dns_failure"`
    pub error_type: Option<String>,

    /// Human-readable error description.
    pub error_message: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_status_dead_letter_variant_exists() {
        // Test that DeadLetter variant exists and formats correctly
        let status = EventStatus::DeadLetter;
        assert_eq!(status.to_string(), "dead_letter");
    }

    #[test]
    fn event_status_display_format() {
        // Test all EventStatus variants format correctly for database storage
        assert_eq!(EventStatus::Received.to_string(), "received");
        assert_eq!(EventStatus::Pending.to_string(), "pending");
        assert_eq!(EventStatus::Delivering.to_string(), "delivering");
        assert_eq!(EventStatus::Delivered.to_string(), "delivered");
        assert_eq!(EventStatus::Failed.to_string(), "failed");
        assert_eq!(EventStatus::DeadLetter.to_string(), "dead_letter");
    }
}
