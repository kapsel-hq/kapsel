//! HTTP request handlers for API endpoints.
//!
//! Provides webhook ingestion and health check handlers with input validation,
//! tenant isolation, and standardized error responses. Handlers follow
//! consistent patterns for tracing, database transactions, and error codes.

pub mod health;
pub mod ingest;

pub use health::{health_check, liveness_check, readiness_check};
pub use ingest::ingest_webhook;
