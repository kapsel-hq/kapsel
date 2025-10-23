//! HTTP API server and request handling.
//!
//! Provides REST endpoints for webhook ingestion, health checks, and service
//! management. Includes request validation, authentication middleware, and
//! structured error responses with proper HTTP status codes.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod config;
pub mod crypto;
pub mod handlers;
pub mod middleware;
pub mod server;

pub use config::Config;
pub use server::{create_router, create_test_router, start_server};
