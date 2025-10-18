//! Webhook delivery engine with reliability guarantees.
//!
//! Implements async worker pools for webhook delivery with circuit breakers,
//! exponential backoff, and database-backed persistence. Uses PostgreSQL
//! SKIP LOCKED for lock-free work distribution across multiple workers.
//!
//! # Worker Pool Architecture
//!
//! ```text
//!                    ┌───────────────────────────────────────────┐
//!                    │                PostgreSQL                 │
//!                    │  ┌─────────────────────────────────────┐  │
//!                    │  │        webhook_events table         │  │
//!                    │  │  ┌───────────────────────────────┐  │  │
//!                    │  │  │    FOR UPDATE SKIP LOCKED     │  │  │
//!                    │  │  │   (Lock-free work claiming)   │  │  │
//!                    │  │  └───────────────────────────────┘  │  │
//!                    │  └─────────────────────────────────────┘  │
//!                    └───────────────────────────────────────────┘
//!                                          │
//!                                  Concurrent Claims
//!                                    (No Blocking)
//!                                          │
//!                        ┌─────────────────┼─────────────────┐
//!                        │                 │                 │
//!                        ▼                 ▼                 ▼
//!                 ┌──────────────┐ ┌──────────────┐ ┌──────────────┐
//!                 │   Worker 1   │ │   Worker 2   │ │   Worker N   │
//!                 │              │ │              │ │              │
//!                 │ ┌──────────┐ │ │ ┌──────────┐ │ │ ┌──────────┐ │
//!                 │ │ Circuit  │ │ │ │ Circuit  │ │ │ │ Circuit  │ │
//!                 │ │ Breaker  │ │ │ │ Breaker  │ │ │ │ Breaker  │ │
//!                 │ └──────────┘ │ │ └──────────┘ │ │ └──────────┘ │
//!                 └──────────────┘ └──────────────┘ └──────────────┘
//!                        │                 │                 │
//!                        ▼                 ▼                 ▼
//!                 ┌──────────────┐ ┌──────────────┐ ┌──────────────┐
//!                 │ Destination  │ │ Destination  │ │ Destination  │
//!                 │ Endpoint A   │ │ Endpoint B   │ │ Endpoint C   │
//!                 └──────────────┘ └──────────────┘ └──────────────┘
//! ```
//!
//! Key benefits:
//! - **Zero contention**: SKIP LOCKED prevents workers from blocking each other
//! - **Automatic distribution**: PostgreSQL handles work allocation fairly
//! - **Fault isolation**: Circuit breakers prevent cascade failures per
//!   endpoint

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod circuit;
pub mod client;
pub mod error;
pub mod retry;
pub mod worker;
pub mod worker_pool;

pub use client::{ClientConfig, DeliveryClient, DeliveryRequest, DeliveryResponse};
pub use error::{DeliveryError, Result};
pub use kapsel_core::{
    DeliveryEvent, DeliveryFailedEvent, DeliverySucceededEvent, EventHandler,
    MulticastEventHandler, NoOpEventHandler,
};
pub use worker::{DeliveryConfig, DeliveryEngine};
pub use worker_pool::WorkerPool;

/// Default number of concurrent delivery workers.
pub const DEFAULT_WORKER_COUNT: usize = 3;

/// Default batch size for claiming events from database.
pub const DEFAULT_BATCH_SIZE: usize = 10;

/// Default HTTP request timeout in seconds.
pub const DEFAULT_TIMEOUT_SECONDS: u64 = 30;
