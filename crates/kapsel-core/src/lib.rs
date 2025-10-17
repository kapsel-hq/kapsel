//! Kapsel core domain models and types.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod error;
pub mod events;
pub mod models;

// Re-export commonly used types
pub use error::{KapselError, Result};
pub use events::{
    DeliveryEvent, DeliveryFailedEvent, DeliverySucceededEvent, EventHandler,
    MulticastEventHandler, NoOpEventHandler,
};
pub use models::{
    CircuitState, DeliveryAttempt, Endpoint, EndpointId, EventId, EventStatus, TenantId,
    WebhookEvent,
};
