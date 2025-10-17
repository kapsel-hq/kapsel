//! Merkle tree attestation and cryptographic signing.
//!
//! Provides RFC 6962 compliant Merkle tree construction and Ed25519 signing
//! for webhook delivery attestation.

#![warn(missing_docs)]

pub mod error;
pub mod leaf;
pub mod merkle;
pub mod signing;

pub use error::{AttestationError, Result};
pub use leaf::LeafData;
pub use merkle::{MerkleService, SignedTreeHead};
pub use signing::SigningService;
