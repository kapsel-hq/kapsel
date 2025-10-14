//! Webhook delivery engine with reliability guarantees.
//!
//! This crate implements the core delivery system that processes webhook events
//! from the database and delivers them to configured endpoints with exponential
//! backoff, circuit breakers, and comprehensive retry logic.
//!
//! # Architecture
//!
//! The delivery engine uses a worker pool model where multiple async tasks
//! claim events from PostgreSQL using `FOR UPDATE SKIP LOCKED` for lock-free
//! work distribution. Each worker handles the complete delivery lifecycle:
//!
//! 1. **Claim Events** - Worker claims pending events from database
//! 2. **Circuit Check** - Verify endpoint circuit breaker state
//! 3. **HTTP Delivery** - Send webhook with timeout and retries
//! 4. **Status Update** - Record delivery result and schedule retries
//!
//! # Key Features
//!
//! - **Lock-free Distribution** - PostgreSQL SKIP LOCKED prevents worker
//!   contention
//! - **Circuit Breakers** - Per-endpoint failure protection prevents cascades
//! - **Exponential Backoff** - Configurable retry delays with jitter
//! - **Graceful Shutdown** - Workers complete in-flight deliveries before exit
//!
//! # Example
//!
//! ```no_run
//! use kapsel_delivery::{DeliveryConfig, DeliveryEngine, DeliveryError};
//! use sqlx::PgPool;
//!
//! # async fn example(pool: PgPool) -> std::result::Result<(), DeliveryError> {
//! let config = DeliveryConfig::default();
//! let mut engine = DeliveryEngine::new(pool, config)?;
//!
//! // Start the delivery workers
//! engine.start().await?;
//! # Ok(())
//! # }
//! ```

pub mod circuit;
pub mod client;
pub mod engine;
pub mod error;
pub mod retry;
mod worker;

// Re-export main public API
pub use engine::{DeliveryConfig, DeliveryEngine};
pub use error::{DeliveryError, Result};

/// Default number of concurrent delivery workers.
pub const DEFAULT_WORKER_COUNT: usize = 3;

/// Default batch size for claiming events from database.
pub const DEFAULT_BATCH_SIZE: usize = 10;

/// Default HTTP request timeout in seconds.
pub const DEFAULT_TIMEOUT_SECONDS: u64 = 30;
