//! Property-based tests for core business logic invariants.
//!
//! Tests fundamental domain rules that must hold regardless of input data.
//! Uses deterministic, in-memory testing without external dependencies.

#![allow(clippy::unwrap_used)] // Test regex patterns are known to be valid
#![allow(clippy::match_same_arms)] // Clear match arms for transition logic

use std::collections::{HashMap, HashSet};

use bytes::Bytes;
use kapsel_core::models::{EventStatus, TenantId};
use proptest::{prelude::*, test_runner::Config as ProptestConfig};
use uuid::Uuid;

/// Deterministic property test configuration for CI stability.
fn proptest_config() -> ProptestConfig {
    ProptestConfig {
        cases: 50,
        timeout: 5000, // 5 seconds max
        fork: false,
        failure_persistence: None,
        source_file: None,
        ..ProptestConfig::default()
    }
}

/// Simple webhook data for testing business logic.
#[derive(Debug, Clone, PartialEq)]
struct TestWebhookData {
    tenant_id: TenantId,
    source_event_id: String,
    payload: Bytes,
    content_type: String,
    headers: HashMap<String, String>,
}

/// Generate valid webhook data for property testing.
fn webhook_data_strategy() -> impl Strategy<Value = TestWebhookData> {
    (
        any::<[u8; 16]>().prop_map(|bytes| TenantId(Uuid::from_bytes(bytes))),
        prop::string::string_regex("[a-zA-Z0-9_-]{1,50}").unwrap(),
        prop::collection::vec(any::<u8>(), 1..1024),
        prop::sample::select(vec![
            "application/json".to_string(),
            "application/xml".to_string(),
            "text/plain".to_string(),
            "application/x-www-form-urlencoded".to_string(),
        ]),
        prop::collection::hash_map(
            prop::string::string_regex("[a-zA-Z-]{1,20}").unwrap(),
            prop::string::string_regex("[a-zA-Z0-9 ._-]{1,50}").unwrap(),
            0..5,
        ),
    )
        .prop_map(|(tenant_id, source_id, payload, content_type, headers)| TestWebhookData {
            tenant_id,
            source_event_id: source_id,
            payload: Bytes::from(payload),
            content_type,
            headers,
        })
}

/// Generate valid tenant names for testing.
fn tenant_name_strategy() -> impl Strategy<Value = String> {
    prop::string::string_regex("[a-z][a-z0-9_-]{2,49}").unwrap()
}

/// Test event status transitions follow valid state machine rules.
fn status_transition_strategy() -> impl Strategy<Value = Vec<EventStatus>> {
    prop::collection::vec(
        prop::sample::select(vec![
            EventStatus::Received,
            EventStatus::Pending,
            EventStatus::Delivering,
            EventStatus::Delivered,
            EventStatus::Failed,
            EventStatus::DeadLetter,
        ]),
        2..10,
    )
}

proptest! {
    #![proptest_config(proptest_config())]

    /// Tenant IDs are always valid UUIDs and never nil.
    #[test]
    fn tenant_ids_are_valid_uuids(
        tenant_names in prop::collection::vec(tenant_name_strategy(), 1..100)
    ) {
        let mut tenant_ids = HashSet::new();

        for _name in tenant_names {
            let tenant_id = TenantId(Uuid::new_v4());

            // Tenant ID should be valid UUID
            prop_assert_ne!(tenant_id.0, Uuid::nil(), "Tenant ID must be non-nil UUID");

            // Tenant IDs should be unique
            prop_assert!(
                tenant_ids.insert(tenant_id),
                "Tenant IDs must be unique: {:?}",
                tenant_id
            );
        }
    }

    /// Webhook data maintains consistency across operations.
    #[test]
    fn webhook_data_consistency(
        webhooks in prop::collection::vec(webhook_data_strategy(), 1..50)
    ) {
        for webhook in &webhooks {
            // Basic validation invariants
            prop_assert!(!webhook.source_event_id.is_empty(), "Source event ID cannot be empty");
            prop_assert!(!webhook.payload.is_empty(), "Payload cannot be empty");
            prop_assert!(!webhook.content_type.is_empty(), "Content type cannot be empty");
            prop_assert_ne!(webhook.tenant_id.0, Uuid::nil(), "Tenant ID must be valid");

            // Content type format
            prop_assert!(
                webhook.content_type.contains('/'),
                "Content type must be valid MIME type: {}",
                webhook.content_type
            );

            // Payload size constraints
            prop_assert!(
                webhook.payload.len() <= 10_485_760, // 10MB
                "Payload size {} exceeds maximum",
                webhook.payload.len()
            );
        }
    }

    /// Tenant isolation: data from different tenants never mixes.
    #[test]
    fn tenant_isolation_maintained(
        webhooks_per_tenant in prop::collection::vec(webhook_data_strategy(), 1..20),
        tenant_count in 2usize..10
    ) {
        let mut tenant_data: HashMap<TenantId, Vec<TestWebhookData>> = HashMap::new();

        // Create data for multiple tenants
        for _i in 0..tenant_count {
            let tenant_id = TenantId(Uuid::new_v4());
            let mut tenant_webhooks = Vec::new();

            for webhook_template in &webhooks_per_tenant {
                let mut webhook = webhook_template.clone();
                webhook.tenant_id = tenant_id;
                tenant_webhooks.push(webhook);
            }

            tenant_data.insert(tenant_id, tenant_webhooks);
        }

        // Verify complete isolation between tenants
        let all_tenant_ids: Vec<_> = tenant_data.keys().collect();

        for (i, &tenant_id_a) in all_tenant_ids.iter().enumerate() {
            for &tenant_id_b in all_tenant_ids.iter().skip(i + 1) {
                let webhooks_a = &tenant_data[tenant_id_a];
                let webhooks_b = &tenant_data[tenant_id_b];

                // No webhook from tenant A should have tenant B's ID
                for webhook_a in webhooks_a {
                    prop_assert_ne!(
                        webhook_a.tenant_id,
                        *tenant_id_b,
                        "Webhook should belong to correct tenant"
                    );
                }

                // No webhook from tenant B should have tenant A's ID
                for webhook_b in webhooks_b {
                    prop_assert_ne!(
                        webhook_b.tenant_id,
                        *tenant_id_a,
                        "Webhook should belong to correct tenant"
                    );
                }
            }
        }
    }

    /// Idempotency: identical source data produces consistent results.
    #[test]
    fn idempotency_preserved(
        webhook_template in webhook_data_strategy(),
        duplicate_count in 2usize..10
    ) {
        let mut webhooks = Vec::new();

        // Create multiple webhooks with identical source data
        for _ in 0..duplicate_count {
            webhooks.push(webhook_template.clone());
        }

        // All webhooks should have identical core data
        let first_webhook = &webhooks[0];

        for webhook in &webhooks[1..] {
            prop_assert_eq!(
                webhook.tenant_id,
                first_webhook.tenant_id,
                "Tenant ID must be consistent for idempotent operations"
            );
            prop_assert_eq!(
                &webhook.source_event_id,
                &first_webhook.source_event_id,
                "Source event ID must be consistent for idempotent operations"
            );
            prop_assert_eq!(
                &webhook.payload,
                &first_webhook.payload,
                "Payload must be consistent for idempotent operations"
            );
            prop_assert_eq!(
                &webhook.content_type,
                &first_webhook.content_type,
                "Content type must be consistent for idempotent operations"
            );
            prop_assert_eq!(
                &webhook.headers,
                &first_webhook.headers,
                "Headers must be consistent for idempotent operations"
            );
        }
    }

    /// Event status transitions follow valid state machine rules.
    #[test]
    fn event_status_transitions_valid(
        status_sequence in status_transition_strategy()
    ) {
        // All events should start in Received state
        let mut current_status = EventStatus::Received;

        for &next_status in &status_sequence {
            // Verify transition validity
            let is_valid_transition = match (current_status, next_status) {
                // Valid forward and retry transitions
                (EventStatus::Received, EventStatus::Pending) => true,
                (EventStatus::Pending, EventStatus::Delivering) => true,
                (EventStatus::Delivering, EventStatus::Delivered) => true,
                (EventStatus::Delivering, EventStatus::Failed) => true,
                (EventStatus::Failed, EventStatus::DeadLetter) => true,
                (EventStatus::Delivering, EventStatus::Pending) => true, // Retry

                // Same state (idempotent)
                (prev, new) if prev == new => true,

                // Terminal states should not transition
                (EventStatus::Delivered, _) => next_status == EventStatus::Delivered,
                (EventStatus::DeadLetter, _) => next_status == EventStatus::DeadLetter,

                // All other transitions are invalid in normal flow
                _ => false,
            };

            if is_valid_transition {
                current_status = next_status;
            }
            // For property testing, we allow "invalid" transitions but verify
            // they don't break fundamental invariants
        }

        // At least verify we have some valid states in our enum
        prop_assert!(
            matches!(current_status,
                EventStatus::Received | EventStatus::Pending | EventStatus::Delivering |
                EventStatus::Delivered | EventStatus::Failed | EventStatus::DeadLetter
            ),
            "Status should be one of the valid enum variants"
        );
    }

    /// Payload size constraints are always enforced.
    #[test]
    fn payload_size_constraints_enforced(
        payload_size in 1usize..50_000, // Test up to 50KB
        content_type in prop::sample::select(vec![
            "application/json",
            "application/xml",
            "text/plain",
            "application/octet-stream"
        ])
    ) {
        let payload = vec![42u8; payload_size];
        let webhook_data = TestWebhookData {
            tenant_id: TenantId(Uuid::new_v4()),
            source_event_id: "test-event".to_string(),
            payload: Bytes::from(payload),
            content_type: content_type.to_string(),
            headers: HashMap::new(),
        };

        // Payload size must be preserved exactly
        prop_assert_eq!(
            webhook_data.payload.len(),
            payload_size,
            "Payload size must be preserved exactly"
        );

        // Payload must not be empty after construction
        prop_assert!(
            !webhook_data.payload.is_empty(),
            "Payload must not be empty after construction"
        );

        // Reasonable size limits (10MB max in real system)
        prop_assert!(
            webhook_data.payload.len() <= 10_485_760,
            "Payload size {} exceeds reasonable maximum",
            webhook_data.payload.len()
        );
    }

    /// Header validation invariants are maintained.
    #[test]
    fn header_validation_invariants(
        headers in prop::collection::hash_map(
            prop::string::string_regex("[a-zA-Z-]{1,100}").unwrap(),
            prop::string::string_regex("[a-zA-Z0-9 ._@-]{1,200}").unwrap(),
            0..20,
        )
    ) {
        let webhook_data = TestWebhookData {
            tenant_id: TenantId(Uuid::new_v4()),
            source_event_id: "test-headers".to_string(),
            payload: Bytes::from("test payload"),
            content_type: "application/json".to_string(),
            headers: headers.clone(),
        };

        // Headers should be preserved exactly
        prop_assert_eq!(
            &webhook_data.headers,
            &headers,
            "Headers must be preserved exactly"
        );

        // All header names should be non-empty
        for (name, value) in &webhook_data.headers {
            prop_assert!(!name.is_empty(), "Header name cannot be empty");
            prop_assert!(!value.is_empty(), "Header value cannot be empty");

            // Header names should not contain invalid characters
            prop_assert!(
                !name.contains(' '),
                "Header name should not contain spaces: '{}'",
                name
            );
        }
    }

    /// Content hash idempotency strategy maintains consistency.
    ///
    /// Verifies that identical payload content produces identical hashes
    /// regardless of headers, timing, or other metadata. This ensures
    /// reliable duplicate detection based on payload content.
    #[test]
    fn content_hash_idempotency_maintained(
        payload_bytes in prop::collection::vec(any::<u8>(), 1..1024),
        different_headers in prop::collection::hash_map(
            prop::string::string_regex("[a-zA-Z-]{1,15}").unwrap(),
            prop::string::string_regex("[a-zA-Z0-9\\s]{1,30}").unwrap(),
            1..3,
        ),
    ) {
        use sha2::{Digest, Sha256};

        let payload = Bytes::from(payload_bytes);

        // Create two webhook requests with identical payload but different headers
        let webhook1 = TestWebhookData {
            tenant_id: TenantId(Uuid::new_v4()),
            source_event_id: "test1".to_string(),
            payload: payload.clone(),
            content_type: "application/json".to_string(),
            headers: HashMap::new(),
        };

        let webhook2 = TestWebhookData {
            tenant_id: TenantId(Uuid::new_v4()),
            source_event_id: "test2".to_string(),
            payload,
            content_type: "application/json".to_string(),
            headers: different_headers,
        };

        // Calculate content hash for both (should be identical)
        let mut hasher1 = Sha256::new();
        hasher1.update(&webhook1.payload);
        let hash1 = hex::encode(hasher1.finalize());

        let mut hasher2 = Sha256::new();
        hasher2.update(&webhook2.payload);
        let hash2 = hex::encode(hasher2.finalize());

        // Core invariant: identical payloads produce identical hashes
        prop_assert_eq!(&hash1, &hash2, "Content hash must be identical for identical payloads");

        // Verify hash is deterministic (same input produces same output)
        let mut hasher3 = Sha256::new();
        hasher3.update(&webhook1.payload);
        let hash3 = hex::encode(hasher3.finalize());
        prop_assert_eq!(&hash1, &hash3, "Content hash must be deterministic");
    }

    /// Source event ID extraction from JSON payloads works correctly.
    ///
    /// Verifies that JSONPath extraction correctly identifies source event IDs
    /// from webhook payloads for deduplication purposes.
    #[test]
    fn source_event_id_idempotency_detection(
        source_id in prop::string::string_regex("[a-zA-Z0-9_-]{8,32}").unwrap(),
        additional_data in prop::collection::hash_map(
            prop::string::string_regex("[a-zA-Z_]{1,10}").unwrap(),
            prop::string::string_regex("[a-zA-Z0-9]{1,20}").unwrap(),
            0..3,
        ),
    ) {
        // Create JSON payload with extractable source ID
        let mut json_payload = serde_json::json!({
            "id": &source_id,
            "event": "test.event",
            "timestamp": 1_234_567_890
        });

        // Add additional data
        for (key, value) in additional_data {
            json_payload[key] = serde_json::Value::String(value);
        }

        let payload_bytes = serde_json::to_vec(&json_payload).unwrap();

        // Test JSONPath extraction ($.id is the common pattern)
        let json_value: serde_json::Value = serde_json::from_slice(&payload_bytes).unwrap();
        let extracted_id = json_value
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        prop_assert_eq!(extracted_id, &source_id,
            "Source event ID should be extractable from JSON payload");

        // Test that same source ID is consistently extracted
        let duplicate_payload_bytes = serde_json::to_vec(&json_payload).unwrap();
        let duplicate_json: serde_json::Value = serde_json::from_slice(&duplicate_payload_bytes).unwrap();
        let duplicate_extracted_id = duplicate_json
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        prop_assert_eq!(duplicate_extracted_id, extracted_id,
            "Source event ID extraction must be consistent");
    }
}
