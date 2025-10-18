//! Error types and result handling for attestation operations.
//!
//! Defines comprehensive error taxonomy for cryptographic signing, Merkle tree
//! operations, key management, and database persistence with detailed context
//! for debugging attestation failures.

/// Errors that can occur during attestation operations.
///
/// Covers cryptographic signing, key management, Merkle tree operations,
/// and database persistence with detailed error context for debugging.
#[derive(Debug, thiserror::Error)]
pub enum AttestationError {
    /// Invalid Ed25519 signature format or verification failed.
    #[error("signature verification failed")]
    InvalidSignature,

    /// Invalid Ed25519 key format or size.
    #[error("invalid key format: {message}")]
    InvalidKeyFormat {
        /// Detailed error message explaining the format issue.
        message: String,
    },

    /// Database operation failed.
    #[error("database error: {source}")]
    Database {
        /// Underlying database error.
        #[from]
        source: sqlx::Error,
    },

    /// JSON serialization or deserialization failed.
    #[error("serialization error: {source}")]
    Serialization {
        /// Underlying serialization error.
        #[from]
        source: serde_json::Error,
    },

    /// No active attestation key found in database.
    #[error("no active attestation key found")]
    NoActiveKey,

    /// Multiple active keys found when only one should exist.
    #[error("found {count} active keys, expected exactly 1")]
    MultipleActiveKeys {
        /// Number of active keys found.
        count: usize,
    },

    /// Merkle tree root not found for the specified tree size.
    #[error("missing merkle root for tree size {tree_size}")]
    MissingRoot {
        /// Tree size that was requested.
        tree_size: u64,
    },

    /// Invalid tree size (negative or inconsistent with existing tree).
    #[error("invalid tree size: {tree_size}")]
    InvalidTreeSize {
        /// The invalid tree size value.
        tree_size: i64,
    },

    /// Leaf already exists in tree (duplicate delivery attempt).
    #[error("leaf already exists for delivery attempt {delivery_attempt_id}")]
    DuplicateLeaf {
        /// The delivery attempt ID that already has a leaf.
        delivery_attempt_id: uuid::Uuid,
    },

    /// Batch processing failed due to concurrent modification.
    #[error("batch commit failed: {reason}")]
    BatchCommitFailed {
        /// Reason for batch commit failure.
        reason: String,
    },

    /// Key rotation failed due to invalid state.
    #[error("key rotation failed: {reason}")]
    KeyRotationFailed {
        /// Reason for key rotation failure.
        reason: String,
    },

    /// Proof generation failed for the specified leaf or tree range.
    #[error("proof generation failed: {reason}")]
    ProofGenerationFailed {
        /// Reason for proof generation failure.
        reason: String,
    },

    /// Invalid proof format or verification failed.
    #[error("proof verification failed: {reason}")]
    ProofVerificationFailed {
        /// Reason for proof verification failure.
        reason: String,
    },

    /// Timestamp is invalid (negative, in future, or inconsistent).
    #[error("invalid timestamp: {timestamp_ms}")]
    InvalidTimestamp {
        /// The invalid timestamp value in milliseconds.
        timestamp_ms: i64,
    },
}

impl AttestationError {
    /// Create an invalid key format error with a custom message.
    pub fn invalid_key_format(message: impl Into<String>) -> Self {
        Self::InvalidKeyFormat { message: message.into() }
    }

    /// Create a batch commit failed error with a custom reason.
    pub fn batch_commit_failed(reason: impl Into<String>) -> Self {
        Self::BatchCommitFailed { reason: reason.into() }
    }

    /// Create a key rotation failed error with a custom reason.
    pub fn key_rotation_failed(reason: impl Into<String>) -> Self {
        Self::KeyRotationFailed { reason: reason.into() }
    }

    /// Create a proof generation failed error with a custom reason.
    pub fn proof_generation_failed(reason: impl Into<String>) -> Self {
        Self::ProofGenerationFailed { reason: reason.into() }
    }

    /// Create a proof verification failed error with a custom reason.
    pub fn proof_verification_failed(reason: impl Into<String>) -> Self {
        Self::ProofVerificationFailed { reason: reason.into() }
    }

    /// Check if this error indicates a retryable operation.
    ///
    /// Database connection issues and concurrent modification errors are
    /// typically retryable, while cryptographic and validation errors are not.
    pub fn is_retryable(&self) -> bool {
        match self {
            // Retryable database issues
            Self::Database { source } => {
                matches!(
                    source,
                    sqlx::Error::PoolTimedOut
                        | sqlx::Error::Io(_)
                        | sqlx::Error::Protocol(_)
                        | sqlx::Error::Tls(_)
                )
            },

            // Retryable batch conflicts
            Self::BatchCommitFailed { .. } => true,

            // Non-retryable errors
            Self::InvalidSignature
            | Self::InvalidKeyFormat { .. }
            | Self::Serialization { .. }
            | Self::NoActiveKey
            | Self::MultipleActiveKeys { .. }
            | Self::MissingRoot { .. }
            | Self::InvalidTreeSize { .. }
            | Self::DuplicateLeaf { .. }
            | Self::KeyRotationFailed { .. }
            | Self::ProofGenerationFailed { .. }
            | Self::ProofVerificationFailed { .. }
            | Self::InvalidTimestamp { .. } => false,
        }
    }

    /// Check if this error indicates a client-side issue.
    ///
    /// Client errors are typically caused by invalid input data or
    /// incorrect API usage, as opposed to server-side failures.
    pub fn is_client_error(&self) -> bool {
        match self {
            // Client errors - invalid input or usage
            Self::InvalidSignature
            | Self::InvalidKeyFormat { .. }
            | Self::InvalidTreeSize { .. }
            | Self::DuplicateLeaf { .. }
            | Self::InvalidTimestamp { .. }
            | Self::ProofVerificationFailed { .. } => true,

            // Server errors - internal system issues
            Self::Database { .. }
            | Self::Serialization { .. }
            | Self::NoActiveKey
            | Self::MultipleActiveKeys { .. }
            | Self::MissingRoot { .. }
            | Self::BatchCommitFailed { .. }
            | Self::KeyRotationFailed { .. }
            | Self::ProofGenerationFailed { .. } => false,
        }
    }
}

/// Result type alias for attestation operations.
pub type Result<T> = std::result::Result<T, AttestationError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_retryability_classification() {
        // Retryable errors
        assert!(AttestationError::batch_commit_failed("concurrent update").is_retryable());

        // Non-retryable errors
        assert!(!AttestationError::InvalidSignature.is_retryable());
        assert!(!AttestationError::invalid_key_format("wrong size").is_retryable());
        assert!(!AttestationError::NoActiveKey.is_retryable());
    }

    #[test]
    fn error_client_classification() {
        // Client errors
        assert!(AttestationError::InvalidSignature.is_client_error());
        assert!(AttestationError::invalid_key_format("invalid bytes").is_client_error());

        // Server errors
        assert!(!AttestationError::NoActiveKey.is_client_error());
        assert!(!AttestationError::batch_commit_failed("deadlock").is_client_error());
    }

    #[test]
    fn error_message_formatting() {
        let err = AttestationError::MissingRoot { tree_size: 1000 };
        assert_eq!(err.to_string(), "missing merkle root for tree size 1000");

        let err = AttestationError::MultipleActiveKeys { count: 3 };
        assert_eq!(err.to_string(), "found 3 active keys, expected exactly 1");
    }

    #[test]
    fn constructor_methods() {
        let err = AttestationError::invalid_key_format("wrong length");
        match err {
            AttestationError::InvalidKeyFormat { message } => {
                assert_eq!(message, "wrong length");
            },
            _ => panic!("Expected InvalidKeyFormat variant"),
        }

        let err = AttestationError::proof_verification_failed("hash mismatch");
        match err {
            AttestationError::ProofVerificationFailed { reason } => {
                assert_eq!(reason, "hash mismatch");
            },
            _ => panic!("Expected ProofVerificationFailed variant"),
        }
    }
}
