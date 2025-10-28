//! Core domain models and strongly-typed identifiers.
//!
//! Defines webhook events, endpoints, delivery attempts, and newtype ID
//! wrappers for compile-time type safety. Includes database serialization
//! traits and state transition logic for the webhook delivery pipeline.

use std::{collections::HashMap, fmt};

use bytes::Bytes;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

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

/// Tenant represents an isolated customer or organization.
///
/// All resources (endpoints, events, etc.) are scoped to a tenant,
/// ensuring complete data isolation between customers.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Tenant {
    /// Unique identifier for this tenant.
    pub id: TenantId,

    /// Human-readable name for the tenant.
    pub name: String,

    /// Subscription tier (e.g., "free", "pro", "enterprise").
    pub tier: String,

    /// Maximum events allowed per month.
    pub max_events_per_month: i32,

    /// Maximum endpoints allowed.
    pub max_endpoints: i32,

    /// Events processed this month.
    pub events_this_month: i32,

    /// When this tenant was created.
    pub created_at: DateTime<Utc>,

    /// When this tenant was last updated.
    pub updated_at: DateTime<Utc>,

    /// When this tenant was deleted (soft delete).
    pub deleted_at: Option<DateTime<Utc>>,

    /// Stripe customer ID for billing.
    pub stripe_customer_id: Option<String>,

    /// Stripe subscription ID.
    pub stripe_subscription_id: Option<String>,
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
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
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
    pub idempotency_strategy: IdempotencyStrategy,

    /// Current processing status.
    pub status: EventStatus,

    /// Number of failed delivery attempts.
    ///
    /// Incremented after each failure. Event fails permanently
    /// when this reaches the endpoint's `max_retries`.
    pub failure_count: i32,

    /// Timestamp of most recent delivery attempt.
    pub last_attempt_at: Option<DateTime<Utc>>,

    /// When to retry next (calculated using exponential backoff).
    pub next_retry_at: Option<DateTime<Utc>>,

    /// Original HTTP headers from ingestion.
    pub headers: sqlx::types::Json<HashMap<String, String>>,

    /// Raw webhook payload.
    ///
    /// Stored as `Vec<u8>` for database compatibility, converted to Bytes for
    /// zero-copy operations.
    pub body: Vec<u8>,

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
}

impl WebhookEvent {
    /// Headers as a regular HashMap for easy access.
    pub fn headers(&self) -> &HashMap<String, String> {
        &self.headers.0
    }

    /// Body as Bytes for zero-copy operations.
    pub fn body_bytes(&self) -> Bytes {
        Bytes::from(self.body.clone())
    }

    /// Raw body as a byte slice.
    pub fn body(&self) -> &[u8] {
        &self.body
    }

    /// Create a WebhookEvent with the given data.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: EventId,
        tenant_id: TenantId,
        endpoint_id: EndpointId,
        source_event_id: String,
        headers: HashMap<String, String>,
        body: Vec<u8>,
        content_type: String,
        received_at: DateTime<Utc>,
    ) -> Self {
        let payload_size = i32::try_from(body.len())
            .unwrap_or(i32::MAX) // Use i32::MAX on casting failure
            .max(1); // Ensure minimum size of 1

        Self {
            id,
            tenant_id,
            endpoint_id,
            source_event_id,
            idempotency_strategy: IdempotencyStrategy::Header,
            status: EventStatus::Received,
            failure_count: 0,
            last_attempt_at: None,
            next_retry_at: None,
            headers: sqlx::types::Json(headers),
            body,
            content_type,
            received_at,
            delivered_at: None,
            failed_at: None,
            payload_size,
            signature_valid: None,
            signature_error: None,
        }
    }
}

/// Webhook endpoint configuration.
///
/// Defines where and how to deliver webhooks. Each endpoint has its own
/// retry policy, timeout settings, and circuit breaker to prevent cascading
/// failures.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
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

    /// Signature configuration for webhook validation.
    ///
    /// Uses tagged union pattern to ensure signing secret and header
    /// are always configured together when signatures are enabled.
    pub signature_config: SignatureConfig,

    /// Maximum delivery attempts before marking as failed.
    ///
    /// Includes the initial attempt. Zero means unlimited retries.
    pub max_retries: i32,

    /// HTTP request timeout in seconds.
    ///
    /// Prevents slow endpoints from blocking workers.
    pub timeout_seconds: i32,

    /// Retry backoff strategy: exponential, linear, or fixed.
    pub retry_strategy: BackoffStrategy,

    /// Circuit breaker current state.
    pub circuit_state: CircuitState,

    /// Consecutive failures in current window.
    ///
    /// Reset to zero on successful delivery.
    pub circuit_failure_count: i32,

    /// Consecutive successes in half-open state.
    ///
    /// Used to determine when to transition from half-open back to closed.
    /// Typically requires 3 consecutive successes.
    pub circuit_success_count: i32,

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
/// HTTP methods supported for webhook delivery.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, Default,
)]
#[serde(rename_all = "UPPERCASE")]
pub enum HttpMethod {
    /// HTTP GET method.
    Get,
    /// HTTP POST method (default).
    #[default]
    Post,
    /// HTTP PUT method.
    Put,
    /// HTTP PATCH method.
    Patch,
}

impl fmt::Display for HttpMethod {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Get => write!(f, "GET"),
            Self::Post => write!(f, "POST"),
            Self::Put => write!(f, "PUT"),
            Self::Patch => write!(f, "PATCH"),
        }
    }
}

impl sqlx::Type<PgDb> for HttpMethod {
    fn type_info() -> PgTypeInfo {
        <str as sqlx::Type<PgDb>>::type_info()
    }
}

impl<'r> sqlx::Decode<'r, PgDb> for HttpMethod {
    fn decode(value: PgValueRef<'r>) -> Result<Self, BoxDynError> {
        let s = <&str as sqlx::Decode<PgDb>>::decode(value)?;
        match s {
            "GET" => Ok(Self::Get),
            "POST" => Ok(Self::Post),
            "PUT" => Ok(Self::Put),
            "PATCH" => Ok(Self::Patch),
            _ => Err(format!("invalid http method: {s}").into()),
        }
    }
}

impl sqlx::Encode<'_, PgDb> for HttpMethod {
    fn encode_by_ref(&self, buf: &mut PgArgumentBuffer) -> EncodeResult {
        <String as sqlx::Encode<PgDb>>::encode_by_ref(&self.to_string(), buf)
    }
}

/// Webhook signature configuration using tagged union pattern.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, Default)]
#[serde(tag = "type")]
pub enum SignatureConfig {
    /// No signature validation.
    #[serde(rename = "none")]
    #[default]
    None,
    /// HMAC-SHA256 signature with custom header.
    #[serde(rename = "hmac_sha256")]
    HmacSha256 {
        /// Secret key for HMAC generation.
        secret: String,
        /// Header name for signature (defaults to X-Webhook-Signature).
        header: String,
    },
}

impl SignatureConfig {
    /// Create HMAC SHA256 signature config with default header.
    pub fn hmac_sha256(secret: String) -> Self {
        Self::HmacSha256 { secret, header: "X-Webhook-Signature".to_string() }
    }

    /// Create HMAC SHA256 signature config with custom header.
    pub fn hmac_sha256_with_header(secret: String, header: String) -> Self {
        Self::HmacSha256 { secret, header }
    }

    /// Check if signature validation is enabled.
    pub fn is_enabled(&self) -> bool {
        !matches!(self, Self::None)
    }

    /// Get the signing secret if available.
    pub fn secret(&self) -> Option<&str> {
        match self {
            Self::None => None,
            Self::HmacSha256 { secret, .. } => Some(secret),
        }
    }

    /// Get the signature header name if available.
    pub fn header(&self) -> Option<&str> {
        match self {
            Self::None => None,
            Self::HmacSha256 { header, .. } => Some(header),
        }
    }
}

impl sqlx::Type<PgDb> for SignatureConfig {
    fn type_info() -> PgTypeInfo {
        <str as sqlx::Type<PgDb>>::type_info()
    }
}

impl<'r> sqlx::Decode<'r, PgDb> for SignatureConfig {
    fn decode(value: PgValueRef<'r>) -> Result<Self, BoxDynError> {
        let s = <&str as sqlx::Decode<PgDb>>::decode(value)?;
        if s == "none" {
            return Ok(Self::None);
        }

        // Parse "hmac_sha256:header:secret" format
        if let Some(rest) = s.strip_prefix("hmac_sha256:") {
            if let Some((header, secret)) = rest.split_once(':') {
                return Ok(Self::HmacSha256 {
                    secret: secret.to_string(),
                    header: header.to_string(),
                });
            }
        }

        Err(format!("invalid signature config: {s}").into())
    }
}

impl sqlx::Encode<'_, PgDb> for SignatureConfig {
    fn encode_by_ref(&self, buf: &mut PgArgumentBuffer) -> EncodeResult {
        let s = match self {
            Self::None => "none".to_string(),
            Self::HmacSha256 { secret, header } => {
                format!("hmac_sha256:{header}:{secret}")
            },
        };
        <String as sqlx::Encode<PgDb>>::encode_by_ref(&s, buf)
    }
}

/// Strategy for determining webhook idempotency.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdempotencyStrategy {
    /// Use X-Idempotency-Key header for deduplication.
    Header,
    /// Extract event ID from webhook payload using JSONPath.
    SourceId,
    /// Use SHA256 hash of payload content.
    ContentHash,
}

impl sqlx::Type<PgDb> for IdempotencyStrategy {
    fn type_info() -> PgTypeInfo {
        <str as sqlx::Type<PgDb>>::type_info()
    }
}

impl<'r> sqlx::Decode<'r, PgDb> for IdempotencyStrategy {
    fn decode(value: PgValueRef<'r>) -> Result<Self, BoxDynError> {
        let s = <&str as sqlx::Decode<PgDb>>::decode(value)?;
        match s {
            "header" => Ok(Self::Header),
            "source_id" => Ok(Self::SourceId),
            "content_hash" => Ok(Self::ContentHash),
            _ => Err(format!("invalid idempotency strategy: {s}").into()),
        }
    }
}

impl sqlx::Encode<'_, PgDb> for IdempotencyStrategy {
    fn encode_by_ref(&self, buf: &mut PgArgumentBuffer) -> EncodeResult {
        let s = match self {
            Self::Header => "header",
            Self::SourceId => "source_id",
            Self::ContentHash => "content_hash",
        };
        <&str as sqlx::Encode<PgDb>>::encode_by_ref(&s, buf)
    }
}

impl fmt::Display for IdempotencyStrategy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Header => write!(f, "header"),
            Self::SourceId => write!(f, "source_id"),
            Self::ContentHash => write!(f, "content_hash"),
        }
    }
}

/// Retry backoff strategy for failed webhook deliveries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackoffStrategy {
    /// Fixed delay between retries.
    Fixed,
    /// Exponential backoff: delay doubles each attempt.
    Exponential,
    /// Linear backoff: delay increases by base amount each attempt.
    Linear,
}

impl fmt::Display for BackoffStrategy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Fixed => write!(f, "fixed"),
            Self::Exponential => write!(f, "exponential"),
            Self::Linear => write!(f, "linear"),
        }
    }
}

impl sqlx::Type<PgDb> for BackoffStrategy {
    fn type_info() -> PgTypeInfo {
        <str as sqlx::Type<PgDb>>::type_info()
    }
}

impl<'r> sqlx::Decode<'r, PgDb> for BackoffStrategy {
    fn decode(value: PgValueRef<'r>) -> Result<Self, BoxDynError> {
        let s = <&str as sqlx::Decode<PgDb>>::decode(value)?;
        match s {
            "fixed" => Ok(Self::Fixed),
            "exponential" => Ok(Self::Exponential),
            "linear" => Ok(Self::Linear),
            _ => Err(format!("invalid backoff strategy: {s}").into()),
        }
    }
}

impl sqlx::Encode<'_, PgDb> for BackoffStrategy {
    fn encode_by_ref(&self, buf: &mut PgArgumentBuffer) -> EncodeResult {
        <String as sqlx::Encode<PgDb>>::encode_by_ref(&self.to_string(), buf)
    }
}

/// Classification of delivery attempt errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryAttemptErrorType {
    /// Request timeout (network or HTTP).
    Timeout,
    /// Connection could not be established.
    ConnectionRefused,
    /// DNS resolution failed for target host.
    DnsFailure,
    /// TLS/SSL handshake failed.
    TlsFailure,
    /// HTTP 4xx client error response.
    ClientError,
    /// HTTP 5xx server error response.
    ServerError,
    /// Rate limiting (HTTP 429 or similar).
    RateLimited,
    /// Malformed response or protocol error.
    ProtocolError,
    /// Network partition or unreachable.
    NetworkError,
}

impl fmt::Display for DeliveryAttemptErrorType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Timeout => write!(f, "timeout"),
            Self::ConnectionRefused => write!(f, "connection_refused"),
            Self::DnsFailure => write!(f, "dns_failure"),
            Self::TlsFailure => write!(f, "tls_failure"),
            Self::ClientError => write!(f, "client_error"),
            Self::ServerError => write!(f, "server_error"),
            Self::RateLimited => write!(f, "rate_limited"),
            Self::ProtocolError => write!(f, "protocol_error"),
            Self::NetworkError => write!(f, "network_error"),
        }
    }
}

impl sqlx::Type<PgDb> for DeliveryAttemptErrorType {
    fn type_info() -> PgTypeInfo {
        <str as sqlx::Type<PgDb>>::type_info()
    }
}

impl<'r> sqlx::Decode<'r, PgDb> for DeliveryAttemptErrorType {
    fn decode(value: PgValueRef<'r>) -> Result<Self, BoxDynError> {
        let s = <&str as sqlx::Decode<PgDb>>::decode(value)?;
        match s {
            "timeout" => Ok(Self::Timeout),
            "connection_refused" => Ok(Self::ConnectionRefused),
            "dns_failure" => Ok(Self::DnsFailure),
            "tls_failure" => Ok(Self::TlsFailure),
            "client_error" => Ok(Self::ClientError),
            "server_error" => Ok(Self::ServerError),
            "rate_limited" => Ok(Self::RateLimited),
            "protocol_error" => Ok(Self::ProtocolError),
            "network_error" => Ok(Self::NetworkError),
            _ => Err(format!("invalid delivery error type: {s}").into()),
        }
    }
}

impl sqlx::Encode<'_, PgDb> for DeliveryAttemptErrorType {
    fn encode_by_ref(&self, buf: &mut PgArgumentBuffer) -> EncodeResult {
        <String as sqlx::Encode<PgDb>>::encode_by_ref(&self.to_string(), buf)
    }
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

impl sqlx::Type<PgDb> for CircuitState {
    fn type_info() -> PgTypeInfo {
        <str as sqlx::Type<PgDb>>::type_info()
    }
}

impl<'r> sqlx::Decode<'r, PgDb> for CircuitState {
    fn decode(value: PgValueRef<'r>) -> Result<Self, BoxDynError> {
        let s = <&str as sqlx::Decode<PgDb>>::decode(value)?;
        match s {
            "closed" => Ok(Self::Closed),
            "open" => Ok(Self::Open),
            "half_open" => Ok(Self::HalfOpen),
            _ => Err(format!("invalid circuit state: {s}").into()),
        }
    }
}

impl sqlx::Encode<'_, PgDb> for CircuitState {
    fn encode_by_ref(&self, buf: &mut PgArgumentBuffer) -> EncodeResult {
        <String as sqlx::Encode<PgDb>>::encode_by_ref(&self.to_string(), buf)
    }
}

/// Complete audit record of a delivery attempt.
///
/// Captures full request/response details for debugging and compliance.
/// Immutable once created - we never modify audit records.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeliveryAttempt {
    /// Unique identifier for this attempt.
    pub id: Uuid,

    /// Event being delivered.
    pub event_id: EventId,

    /// Sequential attempt number for this event.
    ///
    /// Starts at 1 for the first attempt.
    pub attempt_number: u32,

    /// Endpoint this attempt was made to.
    pub endpoint_id: EndpointId,

    /// HTTP headers sent with the request.
    pub request_headers: HashMap<String, String>,

    /// Raw request body sent to the endpoint.
    pub request_body: Vec<u8>,

    /// HTTP status code received.
    ///
    /// None if request failed before receiving response.
    pub response_status: Option<i32>,

    /// Response headers received.
    pub response_headers: Option<HashMap<String, String>>,

    /// Response body (truncated if too large).
    ///
    /// Useful for debugging integration issues.
    pub response_body: Option<Vec<u8>>,

    /// When this attempt was made.
    pub attempted_at: DateTime<Utc>,

    /// Whether the delivery was successful.
    ///
    /// True for 2xx HTTP status codes, false otherwise.
    pub succeeded: bool,

    /// Human-readable error description.
    pub error_message: Option<String>,
}

impl<'r> sqlx::FromRow<'r, PgRow> for DeliveryAttempt {
    fn from_row(row: &'r PgRow) -> Result<Self, sqlx::Error> {
        use sqlx::Row;

        let request_headers: sqlx::types::Json<HashMap<String, String>> =
            row.try_get("request_headers")?;
        let response_headers: Option<sqlx::types::Json<HashMap<String, String>>> =
            row.try_get("response_headers")?;

        Ok(Self {
            id: row.try_get("id")?,
            event_id: row.try_get("event_id")?,
            attempt_number: {
                let val: i32 = row.try_get("attempt_number")?;
                val.try_into()
                    .map_err(|_| sqlx::Error::Decode("attempt_number cannot be negative".into()))?
            },
            endpoint_id: row.try_get("endpoint_id")?,
            request_headers: request_headers.0,
            request_body: row.try_get("request_body")?,
            response_status: row.try_get("response_status")?,
            response_headers: response_headers.map(|h| h.0),
            response_body: row.try_get("response_body")?,
            attempted_at: row.try_get("attempted_at")?,
            succeeded: row.try_get("succeeded")?,
            error_message: row.try_get("error_message")?,
        })
    }
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
