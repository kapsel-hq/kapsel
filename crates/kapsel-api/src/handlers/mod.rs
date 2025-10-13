//! HTTP request handlers for the Kapsel API.
//!
//! This module contains all HTTP endpoint handlers following a consistent
//! pattern:
//! - Input validation with appropriate error codes
//! - Tracing for observability
//! - Database transaction management
//! - Standardized error responses
//!
//! # Handler Organization
//!
//! Handlers are grouped by functionality:
//! - `ingest` - Webhook ingestion endpoints
//! - Future: `delivery` - Delivery status and retry management
//! - Future: `endpoints` - Endpoint CRUD operations
//! - Future: `health` - Health check and readiness probes
//!
//! # Error Handling
//!
//! All handlers return standardized error responses with:
//! - Appropriate HTTP status codes
//! - Error codes from our taxonomy (E1001-E3004)
//! - Human-readable error messages
//! - Request tracing IDs for debugging
//!
//! # Security
//!
//! Handlers implement defense-in-depth:
//! - Input validation before processing
//! - Tenant isolation for all operations
//! - Rate limiting per tenant
//! - Payload size limits (10MB max)

pub mod ingest;

// Re-export handlers for convenient access
pub use ingest::ingest_webhook;
