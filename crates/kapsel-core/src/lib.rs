//! Core domain models and event types.
//!
//! Provides strongly-typed domain primitives, event definitions, and error
//! handling for the webhook reliability system. All other crates depend on
//! these foundational types for type safety and consistency.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod error;
pub mod events;
pub mod models;
pub mod time;

pub use error::{KapselError, Result};
pub use events::{
    DeliveryEvent, DeliveryFailedEvent, DeliverySucceededEvent, EventHandler,
    MulticastEventHandler, NoOpEventHandler,
};
pub use models::{
    BackoffStrategy, CircuitState, DeliveryAttempt, DeliveryAttemptErrorType, Endpoint, EndpointId,
    EventId, EventStatus, HttpMethod, IdempotencyStrategy, SignatureConfig, TenantId, WebhookEvent,
};
pub use time::Clock;
