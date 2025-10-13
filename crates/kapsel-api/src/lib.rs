//! Kapsel HTTP API.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod crypto;
pub mod handlers;
pub mod server;

pub use server::{create_router, start_server};
