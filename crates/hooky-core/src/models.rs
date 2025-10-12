//! Domain models for Hooky webhook reliability service.

use bytes::Bytes;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap as HashMap;
use std::fmt;
use uuid::Uuid;

/// Newtype for event identifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EventId(pub Uuid);

impl EventId {
    /// Creates a new random event ID.
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

/// Newtype for tenant identifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TenantId(pub Uuid);

impl TenantId {
    /// Creates a new random tenant ID.
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

/// Newtype for endpoint identifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EndpointId(pub Uuid);

impl EndpointId {
    /// Creates a new random endpoint ID.
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

/// Event status with type states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EventStatus {
    /// Webhook received and persisted, not yet queued for delivery.
    Received,
    /// Queued for delivery.
    Pending,
    /// Currently being delivered.
    Delivering,
    /// Successfully delivered.
    Delivered,
    /// Permanently failed after all retries exhausted.
    Failed,
}

impl fmt::Display for EventStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Received => write!(f, "received"),
            Self::Pending => write!(f, "pending"),
            Self::Delivering => write!(f, "delivering"),
            Self::Delivered => write!(f, "delivered"),
            Self::Failed => write!(f, "failed"),
        }
    }
}

/// Webhook event domain model.
#[derive(Debug, Clone)]
pub struct WebhookEvent {
    pub id: EventId,
    pub tenant_id: TenantId,
    pub endpoint_id: EndpointId,
    pub source_event_id: String,
    pub idempotency_strategy: String,
    pub status: EventStatus,
    pub failure_count: u32,
    pub last_attempt_at: Option<DateTime<Utc>>,
    pub next_retry_at: Option<DateTime<Utc>>,
    pub headers: HashMap<String, String>,
    pub body: Bytes,
    pub content_type: String,
    pub received_at: DateTime<Utc>,
    pub delivered_at: Option<DateTime<Utc>>,
    pub failed_at: Option<DateTime<Utc>>,
}

/// Endpoint configuration.
#[derive(Debug, Clone)]
pub struct Endpoint {
    pub id: EndpointId,
    pub tenant_id: TenantId,
    pub url: String,
    pub name: String,
    pub signing_secret: Option<String>,
    pub signature_header: Option<String>,
    pub max_retries: u32,
    pub timeout_seconds: u32,
    pub circuit_state: CircuitState,
    pub circuit_failure_count: u32,
    pub circuit_last_failure_at: Option<DateTime<Utc>>,
    pub circuit_half_open_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Circuit breaker state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CircuitState {
    Closed,
    Open,
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

/// Delivery attempt record.
#[derive(Debug, Clone)]
pub struct DeliveryAttempt {
    pub id: Uuid,
    pub event_id: EventId,
    pub attempt_number: u32,
    pub request_url: String,
    pub request_headers: HashMap<String, String>,
    pub response_status: Option<u16>,
    pub response_headers: Option<HashMap<String, String>>,
    pub response_body: Option<String>,
    pub attempted_at: DateTime<Utc>,
    pub duration_ms: u32,
    pub error_type: Option<String>,
    pub error_message: Option<String>,
}
