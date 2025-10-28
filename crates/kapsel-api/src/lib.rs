//! HTTP API server and request handling.
//!
//! Provides REST endpoints for webhook ingestion, health checks, and service
//! management. Includes request validation, authentication middleware, and
//! structured error responses with proper HTTP status codes.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::sync::Arc;

use kapsel_core::{storage::Storage, Clock};

/// Application state containing storage and clock for API handlers.
#[derive(Clone)]
pub struct AppState {
    /// Database storage layer
    pub storage: Arc<Storage>,
    /// Clock abstraction for time operations
    pub clock: Arc<dyn Clock>,
}

impl AppState {
    /// Creates new application state with the given storage and clock.
    pub fn new(storage: Arc<Storage>, clock: Arc<dyn Clock>) -> Self {
        Self { storage, clock }
    }
}

pub mod config;
pub mod crypto;
pub mod handlers;
pub mod middleware;
pub mod server;

pub use config::Config;
pub use server::{create_router, create_test_router, start_server};
