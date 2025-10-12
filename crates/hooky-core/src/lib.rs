//! Hooky core domain models and types.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod error;
pub mod models;

// Re-export commonly used types
pub use error::{HookyError, Result};
pub use models::{
    CircuitState, DeliveryAttempt, Endpoint, EndpointId, EventId, EventStatus, TenantId,
    WebhookEvent,
};
