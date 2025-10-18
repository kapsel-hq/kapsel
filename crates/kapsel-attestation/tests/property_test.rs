//! Property-based tests for attestation component invariants.
//!
//! Uses randomly generated inputs to verify cryptographic invariants
//! always hold regardless of input data or system state.

use kapsel_attestation::{LeafData, SigningService};
use proptest::{prelude::*, test_runner::Config as ProptestConfig};

/// Creates property test configuration based on environment.
///
/// Uses environment variables:
/// - `PROPTEST_CASES`: Number of test cases (default: 20 for dev, 100 for CI)
/// - `CI`: If set to "true", uses CI configuration
fn proptest_config() -> ProptestConfig {
    let is_ci = std::env::var("CI").unwrap_or_default() == "true";
    let default_cases = if is_ci { 100 } else { 20 };

    let cases =
        std::env::var("PROPTEST_CASES").ok().and_then(|s| s.parse().ok()).unwrap_or(default_cases);

    ProptestConfig::with_cases(cases)
}

proptest! {
    #![proptest_config(proptest_config())]

    /// Verifies that any valid tree head can be signed and verified correctly.
    #[test]
    fn attestation_signatures_are_always_valid_for_correct_data(
        root_hash in prop::array::uniform32(any::<u8>()),
        tree_size in 1i64..1_000_000i64,
        timestamp_ms in 1_000_000_000_000i64..9_999_999_999_999i64,
    ) {
        let service = SigningService::ephemeral();

        let signature = service.sign_tree_head(&root_hash, tree_size, timestamp_ms)
            .expect("signing should succeed");
        let is_valid = service.verify_tree_head(&root_hash, tree_size, timestamp_ms, &signature)
            .expect("verification should succeed");

        prop_assert!(is_valid, "Valid signatures must always verify");
    }

    /// Verifies that tampering with signed data always causes verification to fail.
    #[test]
    fn attestation_tampered_signatures_always_fail(
        root_hash in prop::array::uniform32(any::<u8>()),
        tree_size in 1i64..1_000_000i64,
        timestamp_ms in 1_000_000_000_000i64..9_999_999_999_999i64,
    ) {
        let service = SigningService::ephemeral();
        let signature = service.sign_tree_head(&root_hash, tree_size, timestamp_ms)
            .expect("signing should succeed");

        // Tamper with tree size
        let is_valid = service.verify_tree_head(&root_hash, tree_size.wrapping_add(1), timestamp_ms, &signature)
            .expect("verification should succeed");

        prop_assert!(!is_valid, "Tampered data must fail verification");
    }

    /// Verifies that leaf data hashing is deterministic and consistent.
    #[test]
    fn leaf_hashing_is_deterministic(
        delivery_attempt_id in prop::strategy::Strategy::prop_map(any::<[u8; 16]>(), uuid::Uuid::from_bytes),
        event_id in prop::strategy::Strategy::prop_map(any::<[u8; 16]>(), uuid::Uuid::from_bytes),
        endpoint_url in "https://[a-zA-Z0-9.-]+\\.[a-zA-Z]{2,}/[a-zA-Z0-9/_-]*",
        payload_hash in prop::array::uniform32(any::<u8>()),
        attempt_number in 1i32..1000i32,
        response_status in prop::option::of(200u16..600u16),
        timestamp_seconds in 1_000_000_000i64..2_000_000_000i64,
    ) {
        let timestamp = chrono::DateTime::from_timestamp(timestamp_seconds, 0)
            .unwrap()
            .with_timezone(&chrono::Utc);

        let leaf1 = LeafData::new(
            delivery_attempt_id,
            event_id,
            endpoint_url.clone(),
            payload_hash,
            attempt_number,
            response_status,
            timestamp,
        )?;

        let leaf2 = LeafData::new(
            delivery_attempt_id,
            event_id,
            endpoint_url,
            payload_hash,
            attempt_number,
            response_status,
            timestamp,
        )?;

        let hash1 = leaf1.compute_hash();
        let hash2 = leaf2.compute_hash();

        prop_assert_eq!(hash1, hash2, "Identical leaf data must produce identical hashes");
        prop_assert_eq!(hash1.len(), 32, "SHA256 hash must be exactly 32 bytes");
    }
}
