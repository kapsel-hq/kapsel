//! Cryptographic attestation using Merkle trees and Ed25519 signatures.
//!
//! Implements RFC 6962 compliant Merkle tree construction for webhook delivery
//! audit trails. Provides batch leaf processing, signed tree heads, and
//! cryptographic proof generation for tamper-evident delivery records.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod error;
pub mod event_subscriber;
pub mod leaf;
pub mod merkle;
pub mod signing;

pub use error::{AttestationError, Result};
pub use event_subscriber::AttestationEventSubscriber;
pub use leaf::LeafData;
pub use merkle::{MerkleService, SignedTreeHead};
pub use signing::SigningService;
