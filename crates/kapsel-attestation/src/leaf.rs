//! Merkle tree leaf structures and RFC 6962 compliant hashing.
//!
//! Provides deterministic leaf data representation and hash computation
//! for webhook delivery attempts in cryptographic audit trails.

use sha2::{Digest, Sha256};

use crate::error::{AttestationError, Result};

/// RFC 6962 leaf hash prefix for Merkle tree leaves.
///
/// This single byte prefix (0x00) distinguishes leaf hashes from internal
/// node hashes (0x01) in the Merkle tree structure.
const RFC6962_LEAF_PREFIX: u8 = 0x00;

/// Merkle tree leaf representing a webhook delivery attempt.
///
/// Contains all relevant data about a delivery attempt in a deterministic
/// format suitable for cryptographic attestation. The leaf hash is computed
/// following RFC 6962 standards for Certificate Transparency.
///
/// # Design Philosophy
///
/// Following Kapsel's principles of simplicity and correctness:
/// - Immutable after construction to prevent tampering
/// - Deterministic hashing for consistent proofs
/// - Comprehensive data capture for audit purposes
/// - Zero-allocation hash computation where possible
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeafData {
    /// Unique identifier for the delivery attempt.
    pub delivery_attempt_id: uuid::Uuid,

    /// Source webhook event identifier.
    pub event_id: uuid::Uuid,

    /// Target endpoint URL for delivery.
    pub endpoint_url: String,

    /// SHA256 hash of the webhook payload.
    pub payload_hash: [u8; 32],

    /// Delivery attempt number (1-based).
    pub attempt_number: i32,

    /// HTTP response status code (None if network error).
    pub response_status: Option<u16>,

    /// Timestamp when delivery was attempted.
    pub attempted_at: chrono::DateTime<chrono::Utc>,
}

impl LeafData {
    /// Create new leaf data for a delivery attempt.
    ///
    /// # Errors
    ///
    /// Returns `AttestationError::InvalidTreeSize` if attempt_number is not
    /// positive.
    pub fn new(
        delivery_attempt_id: uuid::Uuid,
        event_id: uuid::Uuid,
        endpoint_url: String,
        payload_hash: [u8; 32],
        attempt_number: i32,
        response_status: Option<u16>,
        attempted_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<Self> {
        if attempt_number <= 0 {
            return Err(AttestationError::InvalidTreeSize { tree_size: i64::from(attempt_number) });
        }

        Ok(Self {
            delivery_attempt_id,
            event_id,
            endpoint_url,
            payload_hash,
            attempt_number,
            response_status,
            attempted_at,
        })
    }

    /// Compute RFC 6962 compliant leaf hash.
    ///
    /// Creates a deterministic SHA256 hash following Certificate Transparency
    /// standards. The hash includes all leaf data in a canonical format to
    /// ensure consistent verification across different implementations.
    ///
    /// # Format
    /// ```text
    /// leaf_hash = SHA256(0x00 || delivery_attempt_id || event_id ||
    ///                    endpoint_url_len || endpoint_url ||
    ///                    payload_hash || attempt_number ||
    ///                    response_status || attempted_at_ms)
    /// ```
    pub fn compute_hash(&self) -> [u8; 32] {
        let mut hasher = Sha256::new();

        // RFC 6962 leaf prefix
        hasher.update([RFC6962_LEAF_PREFIX]);

        // Delivery attempt UUID (16 bytes)
        hasher.update(self.delivery_attempt_id.as_bytes());

        // Event UUID (16 bytes)
        hasher.update(self.event_id.as_bytes());

        // Endpoint URL with length prefix
        let url_bytes = self.endpoint_url.as_bytes();
        hasher.update(u32::try_from(url_bytes.len()).unwrap_or(u32::MAX).to_be_bytes());
        hasher.update(url_bytes);

        // Payload hash (32 bytes)
        hasher.update(self.payload_hash);

        // Attempt number (4 bytes)
        hasher.update(self.attempt_number.to_be_bytes());

        // Response status (2 bytes for status, 1 byte for presence flag)
        if let Some(status) = self.response_status {
            hasher.update([1u8]); // Present flag
            hasher.update(status.to_be_bytes());
        } else {
            hasher.update([0u8]); // Absent flag
            hasher.update([0u8, 0u8]); // Padding for alignment
        }

        // Timestamp as milliseconds since Unix epoch (8 bytes)
        hasher.update(self.attempted_at.timestamp_millis().to_be_bytes());

        hasher.finalize().into()
    }

    /// Returns the endpoint URL.
    pub fn endpoint_url(&self) -> &str {
        &self.endpoint_url
    }

    /// Returns the attempt timestamp.
    pub fn attempted_at(&self) -> chrono::DateTime<chrono::Utc> {
        self.attempted_at
    }

    /// Check if the delivery attempt was successful.
    ///
    /// Returns `true` if response status indicates success (2xx range).
    pub fn is_successful(&self) -> bool {
        self.response_status.is_some_and(|status| (200..300).contains(&status))
    }

    /// Check if the delivery attempt had a network error.
    ///
    /// Returns `true` if no response status was recorded, indicating
    /// a network-level failure.
    pub fn has_network_error(&self) -> bool {
        self.response_status.is_none()
    }

    /// Returns a display representation for debugging.
    ///
    /// Produces a human-readable string suitable for logging and debugging
    /// that includes key identifiers without sensitive data.
    pub fn display_summary(&self) -> String {
        format!(
            "LeafData {{ attempt: {}, event: {}, status: {:?}, url: {} }}",
            self.delivery_attempt_id,
            self.event_id,
            self.response_status,
            self.endpoint_url.chars().take(50).collect::<String>()
        )
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    fn create_test_leaf() -> LeafData {
        LeafData::new(
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            "https://example.com/webhook".to_string(),
            [0x42u8; 32], // Test payload hash
            1,
            Some(200),
            chrono::Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn leaf_hash_is_deterministic() {
        let attempt_id = uuid::Uuid::new_v4();
        let event_id = uuid::Uuid::new_v4();
        let timestamp =
            chrono::DateTime::from_timestamp(1_234_567_890, 0).unwrap().with_timezone(&chrono::Utc);

        let leaf1 = LeafData::new(
            attempt_id,
            event_id,
            "https://webhook.example.com".to_string(),
            [0xAAu8; 32],
            1,
            Some(200),
            timestamp,
        )
        .unwrap();

        let leaf2 = LeafData::new(
            attempt_id,
            event_id,
            "https://webhook.example.com".to_string(),
            [0xAAu8; 32],
            1,
            Some(200),
            timestamp,
        )
        .unwrap();

        let hash1 = leaf1.compute_hash();
        let hash2 = leaf2.compute_hash();

        assert_eq!(hash1, hash2, "Identical leaf data must produce identical hashes");
    }

    #[test]
    fn uses_rfc6962_leaf_prefix() {
        let leaf = create_test_leaf();
        let hash = leaf.compute_hash();

        // Verify hash is computed (not all zeros)
        assert_ne!(hash, [0u8; 32], "Hash should not be all zeros");

        // Create a manual hash with the same data to verify prefix usage
        let mut hasher = Sha256::new();
        hasher.update([RFC6962_LEAF_PREFIX]);
        hasher.update(leaf.delivery_attempt_id.as_bytes());
        hasher.update(leaf.event_id.as_bytes());

        let url_bytes = leaf.endpoint_url.as_bytes();
        hasher.update(u32::try_from(url_bytes.len()).unwrap_or(0).to_be_bytes());
        hasher.update(url_bytes);

        hasher.update(leaf.payload_hash);
        hasher.update(leaf.attempt_number.to_be_bytes());
        hasher.update([1u8]); // Status present
        hasher.update(leaf.response_status.unwrap().to_be_bytes());
        hasher.update(leaf.attempted_at.timestamp_millis().to_be_bytes());

        let expected_hash: [u8; 32] = hasher.finalize().into();
        assert_eq!(hash, expected_hash, "Hash should match manual RFC 6962 computation");
    }

    #[test]
    fn hash_always_32_bytes() {
        let leaf = create_test_leaf();
        let hash = leaf.compute_hash();

        assert_eq!(hash.len(), 32, "SHA256 hash must be exactly 32 bytes");

        // Test with different data sizes
        let long_url_leaf = LeafData::new(
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            "https://very-long-endpoint-url-that-exceeds-normal-length.example.com/webhook/path"
                .to_string(),
            [0xFFu8; 32],
            999,
            None, // Network error case
            chrono::Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap(),
        )
        .unwrap();

        let long_hash = long_url_leaf.compute_hash();
        assert_eq!(long_hash.len(), 32, "Hash length must be consistent regardless of data size");
    }

    #[test]
    fn different_data_produces_different_hashes() {
        let base_leaf = create_test_leaf();
        let base_hash = base_leaf.compute_hash();

        // Different attempt number
        let mut different_leaf = base_leaf.clone();
        different_leaf.attempt_number = 2;
        assert_ne!(
            base_hash,
            different_leaf.compute_hash(),
            "Different attempt numbers should produce different hashes"
        );

        // Different endpoint URL
        let mut different_leaf = base_leaf.clone();
        different_leaf.endpoint_url = "https://different.example.com".to_string();
        assert_ne!(
            base_hash,
            different_leaf.compute_hash(),
            "Different URLs should produce different hashes"
        );

        // Different response status
        let mut different_leaf = base_leaf;
        different_leaf.response_status = Some(500);
        assert_ne!(
            base_hash,
            different_leaf.compute_hash(),
            "Different status codes should produce different hashes"
        );
    }

    #[test]
    fn network_error_vs_status_code_hashes_differ() {
        let attempt_id = uuid::Uuid::new_v4();
        let event_id = uuid::Uuid::new_v4();
        let timestamp = chrono::Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap();

        let success_leaf = LeafData::new(
            attempt_id,
            event_id,
            "https://test.example.com".to_string(),
            [0x11u8; 32],
            1,
            Some(200),
            timestamp,
        )
        .unwrap();

        let network_error_leaf = LeafData::new(
            attempt_id,
            event_id,
            "https://test.example.com".to_string(),
            [0x11u8; 32],
            1,
            None, // Network error
            timestamp,
        )
        .unwrap();

        assert_ne!(
            success_leaf.compute_hash(),
            network_error_leaf.compute_hash(),
            "Network error and success status should produce different hashes"
        );
    }

    #[test]
    fn invalid_attempt_number_rejected() {
        let result = LeafData::new(
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            "https://example.com".to_string(),
            [0u8; 32],
            0, // Invalid: not positive
            Some(200),
            chrono::Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap(),
        );

        assert!(result.is_err(), "Zero attempt number should be rejected");

        let result = LeafData::new(
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            "https://example.com".to_string(),
            [0u8; 32],
            -1, // Invalid: negative
            Some(200),
            chrono::Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap(),
        );

        assert!(result.is_err(), "Negative attempt number should be rejected");
    }

    #[test]
    fn status_classification_methods() {
        let successful_leaf = LeafData::new(
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            "https://example.com".to_string(),
            [0u8; 32],
            1,
            Some(201), // Created
            chrono::Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap(),
        )
        .unwrap();

        assert!(successful_leaf.is_successful(), "2xx status should be successful");
        assert!(!successful_leaf.has_network_error(), "2xx status should not be network error");

        let error_leaf = LeafData::new(
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            "https://example.com".to_string(),
            [0u8; 32],
            1,
            Some(500), // Server error
            chrono::Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap(),
        )
        .unwrap();

        assert!(!error_leaf.is_successful(), "5xx status should not be successful");
        assert!(!error_leaf.has_network_error(), "5xx status should not be network error");

        let network_leaf = LeafData::new(
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            "https://example.com".to_string(),
            [0u8; 32],
            1,
            None, // Network failure
            chrono::Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap(),
        )
        .unwrap();

        assert!(!network_leaf.is_successful(), "Network error should not be successful");
        assert!(network_leaf.has_network_error(), "None status should be network error");
    }

    #[test]
    fn display_summary_truncates_long_urls() {
        let long_url = "https://".to_string() + &"a".repeat(100) + ".example.com/webhook";
        let leaf = LeafData::new(
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            long_url,
            [0u8; 32],
            1,
            Some(200),
            chrono::Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap(),
        )
        .unwrap();

        let summary = leaf.display_summary();

        // Should contain key info but truncate URL
        assert!(summary.contains("attempt:"), "Summary should contain attempt ID");
        assert!(summary.contains("event:"), "Summary should contain event ID");
        assert!(summary.contains("status: Some(200)"), "Summary should contain status");
        assert!(summary.len() < 200, "Summary should be reasonably short");
    }
}
