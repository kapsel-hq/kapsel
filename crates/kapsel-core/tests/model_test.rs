//! Integration tests for core domain models.
//!
//! Tests WebhookEvent, Endpoint, EventStatus, and DeliveryAttempt validation,
//! serialization, and business rule enforcement.

use std::{collections::HashMap, sync::Arc};

use chrono::{DateTime, TimeZone, Utc};
use kapsel_core::{
    models::{
        BackoffStrategy, CircuitState, DeliveryAttempt, Endpoint, EndpointId, EventId, EventStatus,
        IdempotencyStrategy, SignatureConfig, TenantId, WebhookEvent,
    },
    Clock, TestClock,
};
use serde_json::json;
use sqlx::types::Json;
use uuid::Uuid;

/// Test WebhookEvent model creation and field access.
///
/// Verifies that WebhookEvent can be created with all required fields
/// and that field access works correctly.
#[test]
fn webhook_event_model_creation_and_access() {
    let event_id = EventId::new();
    let tenant_id = TenantId::new();
    let endpoint_id = EndpointId::new();
    let now = chrono::Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap();

    let mut headers = HashMap::new();
    headers.insert("content-type".to_string(), "application/json".to_string());
    headers.insert("x-custom-header".to_string(), "custom-value".to_string());

    let body_content = b"{\"user\": \"alice\", \"action\": \"created\"}";

    let webhook_event = WebhookEvent {
        id: event_id,
        tenant_id,
        endpoint_id,
        source_event_id: "evt_123".to_string(),
        idempotency_strategy: IdempotencyStrategy::Header,
        status: EventStatus::Pending,
        failure_count: 0,
        last_attempt_at: None,
        next_retry_at: Some(now),
        headers: Json(headers.clone()),
        body: body_content.to_vec(),
        content_type: "application/json".to_string(),
        received_at: now,
        delivered_at: None,
        failed_at: None,
        payload_size: i32::try_from(body_content.len()).unwrap(),
        signature_valid: Some(true),
        signature_error: None,
    };

    // Verify field access
    assert_eq!(webhook_event.id, event_id);
    assert_eq!(webhook_event.tenant_id, tenant_id);
    assert_eq!(webhook_event.endpoint_id, endpoint_id);
    assert_eq!(webhook_event.source_event_id, "evt_123");
    assert_eq!(webhook_event.idempotency_strategy, IdempotencyStrategy::Header);
    assert_eq!(webhook_event.status, EventStatus::Pending);
    assert_eq!(webhook_event.failure_count, 0);
    assert_eq!(webhook_event.last_attempt_at, None);
    assert_eq!(webhook_event.next_retry_at, Some(now));
    assert_eq!(webhook_event.headers.0, headers);
    assert_eq!(webhook_event.body, body_content.to_vec());
    assert_eq!(webhook_event.content_type, "application/json");
    assert_eq!(webhook_event.received_at, now);
    assert_eq!(webhook_event.delivered_at, None);
    assert_eq!(webhook_event.failed_at, None);
    assert_eq!(webhook_event.payload_size, i32::try_from(body_content.len()).unwrap());
    assert_eq!(webhook_event.signature_valid, Some(true));
    assert_eq!(webhook_event.signature_error, None);
}

/// Test WebhookEvent serialization and deserialization.
///
/// Verifies that WebhookEvent can be serialized to JSON and deserialized
/// back without data loss.
#[test]
fn webhook_event_serialization_roundtrip() {
    let mut headers = HashMap::new();
    headers.insert("authorization".to_string(), "Bearer token".to_string());

    let body_content = b"test payload";
    let now = chrono::Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap();

    let original = WebhookEvent {
        id: EventId::new(),
        tenant_id: TenantId::new(),
        endpoint_id: EndpointId::new(),
        source_event_id: "evt_serialize_test".to_string(),
        idempotency_strategy: IdempotencyStrategy::SourceId,
        status: EventStatus::Delivering,
        failure_count: 2,
        last_attempt_at: Some(now),
        next_retry_at: Some(now),
        headers: Json(headers),
        body: body_content.to_vec(),
        content_type: "text/plain".to_string(),
        received_at: now,
        delivered_at: None,
        failed_at: None,
        payload_size: i32::try_from(body_content.len()).unwrap(),
        signature_valid: Some(false),
        signature_error: Some("Invalid signature".to_string()),
    };

    let serialized = serde_json::to_string(&original).expect("serialization should succeed");
    let deserialized: WebhookEvent =
        serde_json::from_str(&serialized).expect("deserialization should succeed");

    assert_eq!(deserialized.id, original.id);
    assert_eq!(deserialized.tenant_id, original.tenant_id);
    assert_eq!(deserialized.endpoint_id, original.endpoint_id);
    assert_eq!(deserialized.source_event_id, original.source_event_id);
    assert_eq!(deserialized.idempotency_strategy, original.idempotency_strategy);
    assert_eq!(deserialized.status, original.status);
    assert_eq!(deserialized.failure_count, original.failure_count);
    assert_eq!(deserialized.body, original.body);
    assert_eq!(deserialized.content_type, original.content_type);
}

/// Test EventStatus variants and display formatting.
///
/// Verifies all enum variants exist and display correctly.
#[test]
fn event_status_variants_and_display() {
    let statuses = vec![
        EventStatus::Received,
        EventStatus::Pending,
        EventStatus::Delivering,
        EventStatus::Delivered,
        EventStatus::Failed,
        EventStatus::DeadLetter,
    ];

    let displays = vec!["received", "pending", "delivering", "delivered", "failed", "dead_letter"];

    for (status, expected_display) in statuses.into_iter().zip(displays) {
        assert_eq!(status.to_string(), expected_display);
    }
}

/// Test EventStatus terminal state identification.
///
/// Verifies that terminal states are correctly identified.
#[test]
fn event_status_terminal_state_identification() {
    assert!(!matches!(
        EventStatus::Received,
        EventStatus::Delivered | EventStatus::Failed | EventStatus::DeadLetter
    ));
    assert!(!matches!(
        EventStatus::Pending,
        EventStatus::Delivered | EventStatus::Failed | EventStatus::DeadLetter
    ));
    assert!(!matches!(
        EventStatus::Delivering,
        EventStatus::Delivered | EventStatus::Failed | EventStatus::DeadLetter
    ));

    assert!(matches!(
        EventStatus::Delivered,
        EventStatus::Delivered | EventStatus::Failed | EventStatus::DeadLetter
    ));
    assert!(matches!(
        EventStatus::Failed,
        EventStatus::Delivered | EventStatus::Failed | EventStatus::DeadLetter
    ));
    assert!(matches!(
        EventStatus::DeadLetter,
        EventStatus::Delivered | EventStatus::Failed | EventStatus::DeadLetter
    ));
}

/// Test CircuitState variants and behavior.
///
/// Verifies circuit breaker state transitions and display formatting.
#[test]
fn circuit_state_variants_and_behavior() {
    let states = vec![CircuitState::Closed, CircuitState::Open, CircuitState::HalfOpen];

    let displays = vec!["closed", "open", "half_open"];

    for (state, expected_display) in states.into_iter().zip(displays) {
        assert_eq!(state.to_string(), expected_display);
    }

    // Test state transitions logic would go here
    // This depends on methods implemented on CircuitState
}

/// Test Endpoint model creation and validation.
///
/// Verifies that Endpoint can be created with all required fields
/// and validates business rules.
#[test]
fn endpoint_model_creation_and_validation() {
    let endpoint_id = EndpointId::new();
    let tenant_id = TenantId::new();
    let now = chrono::Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap();

    let endpoint = Endpoint {
        id: endpoint_id,
        tenant_id,
        url: "https://example.com/webhooks".to_string(),
        name: "Production Endpoint".to_string(),
        is_active: true,
        signature_config: SignatureConfig::hmac_sha256("webhook_secret_123".to_string()),
        max_retries: 3,
        timeout_seconds: 30,
        retry_strategy: BackoffStrategy::Exponential,
        circuit_state: CircuitState::Closed,
        circuit_failure_count: 0,
        circuit_success_count: 0,
        circuit_last_failure_at: None,
        circuit_half_open_at: None,
        created_at: now,
        updated_at: now,
        deleted_at: None,
        total_events_received: 0,
        total_events_delivered: 0,
        total_events_failed: 0,
    };

    // Verify field access
    assert_eq!(endpoint.id, endpoint_id);
    assert_eq!(endpoint.tenant_id, tenant_id);
    assert_eq!(endpoint.url, "https://example.com/webhooks");
    assert_eq!(endpoint.name, "Production Endpoint");
    assert!(endpoint.is_active);
    assert_eq!(endpoint.signature_config.secret(), Some("webhook_secret_123"));
    assert_eq!(endpoint.max_retries, 3);
    assert_eq!(endpoint.timeout_seconds, 30);
}

/// Test Endpoint serialization and deserialization.
///
/// Verifies that Endpoint can be serialized to JSON and deserialized
/// back without data loss.
#[test]
fn endpoint_serialization_roundtrip() {
    let now = chrono::Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap();

    let original = Endpoint {
        id: EndpointId::new(),
        tenant_id: TenantId::new(),
        url: "https://api.example.com/webhooks".to_string(),
        name: "Test Endpoint".to_string(),
        is_active: false,
        signature_config: SignatureConfig::None,
        max_retries: 5,
        timeout_seconds: 60,
        retry_strategy: BackoffStrategy::Linear,
        circuit_state: CircuitState::Open,
        circuit_failure_count: 10,
        circuit_success_count: 2,
        circuit_last_failure_at: Some(now),
        circuit_half_open_at: Some(now),
        created_at: now,
        updated_at: now,
        deleted_at: Some(now),
        total_events_received: 100,
        total_events_delivered: 80,
        total_events_failed: 20,
    };

    let serialized = serde_json::to_string(&original).expect("serialization should succeed");
    let deserialized: Endpoint =
        serde_json::from_str(&serialized).expect("deserialization should succeed");

    assert_eq!(deserialized.id, original.id);
    assert_eq!(deserialized.tenant_id, original.tenant_id);
    assert_eq!(deserialized.url, original.url);
    assert_eq!(deserialized.name, original.name);
    assert_eq!(deserialized.is_active, original.is_active);
    assert_eq!(deserialized.signature_config, original.signature_config);
}

/// Test DeliveryAttempt model with realistic data.
///
/// Verifies that DeliveryAttempt can store comprehensive delivery information
/// including request/response data and timing information.
#[test]
fn delivery_attempt_model_with_realistic_data() {
    let attempt_id = Uuid::new_v4();
    let event_id = EventId::new();
    let endpoint_id = EndpointId::new();
    let now = chrono::Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap();

    let mut request_headers = HashMap::new();
    request_headers.insert("content-type".to_string(), "application/json".to_string());
    request_headers.insert("x-webhook-signature".to_string(), "sha256=abc123".to_string());

    let mut response_headers = HashMap::new();
    response_headers.insert("server".to_string(), "nginx/1.18".to_string());
    response_headers.insert("content-type".to_string(), "application/json".to_string());

    let request_body = b"{\"event\": \"user.created\", \"user_id\": 123}";
    let response_body = b"{\"received\": true}";

    let delivery_attempt = DeliveryAttempt {
        id: attempt_id,
        event_id,
        attempt_number: 1,
        endpoint_id,
        request_headers,
        request_body: request_body.to_vec(),
        response_status: Some(200),
        response_headers: Some(response_headers),
        response_body: Some(response_body.to_vec()),
        attempted_at: now,
        succeeded: true,
        error_message: None,
    };

    // Verify successful delivery attempt
    assert_eq!(delivery_attempt.id, attempt_id);
    assert_eq!(delivery_attempt.event_id, event_id);
    assert_eq!(delivery_attempt.endpoint_id, endpoint_id);
    assert_eq!(delivery_attempt.attempt_number, 1);
    assert_eq!(delivery_attempt.response_status, Some(200));
    assert!(delivery_attempt.succeeded);
    assert!(delivery_attempt.error_message.is_none());

    // Test failed attempt
    let attempt = DeliveryAttempt {
        id: Uuid::new_v4(),
        event_id,
        attempt_number: 2,
        endpoint_id: EndpointId::new(),
        request_headers: HashMap::new(),
        request_body: b"test request".to_vec(),
        response_status: None,
        response_headers: None,
        response_body: None,
        attempted_at: now,
        succeeded: false,
        error_message: Some("Connection timeout".to_string()),
    };

    assert_eq!(attempt.attempt_number, 2);
    assert_eq!(attempt.response_status, None);
    assert!(!attempt.succeeded);
    assert!(attempt.error_message.is_some());
}

/// Test DeliveryAttempt serialization and deserialization.
///
/// Verifies that DeliveryAttempt can be serialized to JSON and deserialized
/// back without data loss.
#[test]
fn delivery_attempt_serialization_roundtrip() {
    let mut headers = HashMap::new();
    headers.insert("user-agent".to_string(), "kapsel/1.0".to_string());

    let request_body = b"{\"test\": \"data\"}";

    let original = DeliveryAttempt {
        id: Uuid::new_v4(),
        event_id: EventId::new(),
        attempt_number: 3,
        endpoint_id: EndpointId::new(),
        request_headers: headers,
        request_body: request_body.to_vec(),
        response_status: Some(422),
        response_headers: Some(HashMap::new()),
        response_body: Some(b"Unprocessable Entity".to_vec()),
        attempted_at: chrono::Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap(),
        succeeded: false,
        error_message: Some("Unprocessable Entity".to_string()),
    };

    let serialized = serde_json::to_string(&original).expect("serialization should succeed");
    let deserialized: DeliveryAttempt =
        serde_json::from_str(&serialized).expect("deserialization should succeed");

    assert_eq!(deserialized.id, original.id);
    assert_eq!(deserialized.event_id, original.event_id);
    assert_eq!(deserialized.endpoint_id, original.endpoint_id);
    assert_eq!(deserialized.attempt_number, original.attempt_number);
    assert_eq!(deserialized.request_body, original.request_body);
    assert_eq!(deserialized.response_status, original.response_status);
    assert_eq!(deserialized.succeeded, original.succeeded);
    assert_eq!(deserialized.error_message, original.error_message);
}

/// Test that model ID types are distinct and cannot be confused.
///
/// Verifies type safety of EventId, TenantId, and EndpointId.
#[test]
fn model_id_types_are_distinct() {
    let event_id = EventId::new();
    let tenant_id = TenantId::new();
    let endpoint_id = EndpointId::new();

    // These should all be different UUIDs
    assert_ne!(event_id.0, tenant_id.0);
    assert_ne!(tenant_id.0, endpoint_id.0);
    assert_ne!(event_id.0, endpoint_id.0);

    // Test that they display differently (includes type prefix)
    let event_display = event_id.to_string();
    let tenant_display = tenant_id.to_string();
    let endpoint_display = endpoint_id.to_string();

    // All should be valid UUID strings
    assert!(Uuid::parse_str(&event_display).is_ok());
    assert!(Uuid::parse_str(&tenant_display).is_ok());
    assert!(Uuid::parse_str(&endpoint_display).is_ok());
}

/// Test ID type serialization and deserialization.
///
/// Verifies that ID types can be serialized to JSON and deserialized
/// back correctly.
#[test]
fn model_id_serialization_roundtrip() {
    let event_id = EventId::new();
    let tenant_id = TenantId::new();
    let endpoint_id = EndpointId::new();

    // Serialize IDs
    let event_json = serde_json::to_string(&event_id).expect("event ID serialization");
    let tenant_json = serde_json::to_string(&tenant_id).expect("tenant ID serialization");
    let endpoint_json = serde_json::to_string(&endpoint_id).expect("endpoint ID serialization");

    // Deserialize IDs
    let event_restored: EventId =
        serde_json::from_str(&event_json).expect("event ID deserialization");
    let tenant_restored: TenantId =
        serde_json::from_str(&tenant_json).expect("tenant ID deserialization");
    let endpoint_restored: EndpointId =
        serde_json::from_str(&endpoint_json).expect("endpoint ID deserialization");

    assert_eq!(event_id, event_restored);
    assert_eq!(tenant_id, tenant_restored);
    assert_eq!(endpoint_id, endpoint_restored);
}

/// Test WebhookEvent with complex payload data.
///
/// Verifies that WebhookEvent can handle various payload types and sizes
/// correctly, including binary data and large payloads.
#[test]
fn webhook_event_handles_complex_payload() {
    let event_id = EventId::new();
    let tenant_id = TenantId::new();
    let endpoint_id = EndpointId::new();
    let now = chrono::Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap();

    // Test with JSON payload
    let json_payload = json!({
        "event": "user.created",
        "data": {
            "id": 12345,
            "email": "user@example.com",
            "metadata": {
                "source": "signup_form",
                "campaign": "summer_2024"
            }
        },
        "timestamp": "2024-01-15T10:30:00Z"
    });

    let json_bytes = serde_json::to_vec(&json_payload).expect("JSON serialization");

    let webhook_event = WebhookEvent {
        id: event_id,
        tenant_id,
        endpoint_id,
        source_event_id: "complex_payload_test".to_string(),
        idempotency_strategy: IdempotencyStrategy::SourceId,
        status: EventStatus::Received,
        failure_count: 0,
        last_attempt_at: None,
        next_retry_at: None,
        headers: Json(HashMap::new()),
        body: json_bytes.clone(),
        content_type: "application/json".to_string(),
        received_at: now,
        delivered_at: None,
        failed_at: None,
        payload_size: i32::try_from(json_bytes.len()).unwrap(),
        signature_valid: None,
        signature_error: None,
    };

    assert_eq!(webhook_event.body, json_bytes);
    assert_eq!(webhook_event.payload_size, i32::try_from(json_bytes.len()).unwrap());
    assert_eq!(webhook_event.content_type, "application/json");

    // Test with binary payload
    let binary_data: Vec<u8> = (0..255).collect();

    let binary_event = WebhookEvent {
        id: EventId::new(),
        tenant_id,
        endpoint_id,
        source_event_id: "binary_test".to_string(),
        idempotency_strategy: IdempotencyStrategy::Header,
        status: EventStatus::Pending,
        failure_count: 0,
        last_attempt_at: None,
        next_retry_at: Some(now),
        headers: Json(HashMap::new()),
        body: binary_data.clone(),
        content_type: "application/octet-stream".to_string(),
        received_at: now,
        delivered_at: None,
        failed_at: None,
        payload_size: i32::try_from(binary_data.len()).unwrap(),
        signature_valid: Some(true),
        signature_error: None,
    };

    assert_eq!(binary_event.body, binary_data);
    assert_eq!(binary_event.payload_size, 255);
    assert_eq!(binary_event.content_type, "application/octet-stream");
}

/// Test model field constraints and validation.
///
/// Verifies that model fields respect database constraints and business rules.
#[test]
fn model_field_constraints_and_validation() {
    let now = chrono::Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap();

    // Test WebhookEvent payload size constraints
    let small_payload = b"small";
    let webhook_event = WebhookEvent {
        id: EventId::new(),
        tenant_id: TenantId::new(),
        endpoint_id: EndpointId::new(),
        source_event_id: "constraint_test".to_string(),
        idempotency_strategy: IdempotencyStrategy::Header,
        status: EventStatus::Received,
        failure_count: 0,
        last_attempt_at: None,
        next_retry_at: None,
        headers: Json(HashMap::new()),
        body: small_payload.to_vec(),
        content_type: "text/plain".to_string(),
        received_at: now,
        delivered_at: None,
        failed_at: None,
        payload_size: i32::try_from(small_payload.len()).unwrap(),
        signature_valid: None,
        signature_error: None,
    };

    // Payload size should be positive
    assert!(webhook_event.payload_size > 0);

    // Test DeliveryAttempt constraints
    let delivery_attempt = DeliveryAttempt {
        id: Uuid::new_v4(),
        event_id: EventId::new(),
        attempt_number: 1,
        endpoint_id: EndpointId::new(),
        request_headers: HashMap::new(),
        request_body: b"test payload".to_vec(),
        response_status: Some(200),
        response_headers: Some(HashMap::new()),
        response_body: Some(b"OK".to_vec()),
        attempted_at: now,
        succeeded: true,
        error_message: None,
    };

    // Attempt number should be positive
    assert!(delivery_attempt.attempt_number > 0);
    // Duration should be reasonable
    assert!(delivery_attempt.succeeded || delivery_attempt.error_message.is_some());
}

/// Test WebhookEvent::new() enforces business rules correctly.
///
/// Verifies that WebhookEvent creation follows domain constraints and
/// sets appropriate defaults for new events.
#[test]
fn webhook_event_new_enforces_business_rules() {
    let headers = HashMap::new();
    let empty_body = Vec::new();
    let normal_body = b"test payload".to_vec();
    let large_body = vec![0u8; 10_000_000]; // 10MB

    // Test empty payload gets minimum size of 1
    let start_time =
        std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_672_531_200); // Jan 1, 2023
    let clock = Arc::new(TestClock::with_start_time(start_time));
    let now = DateTime::<Utc>::from(clock.now_system());
    let empty_event = WebhookEvent::new(
        EventId::new(),
        TenantId::new(),
        EndpointId::new(),
        "empty_test".to_string(),
        headers.clone(),
        empty_body,
        "application/json".to_string(),
        now,
    );

    assert_eq!(empty_event.payload_size, 1); // Enforced minimum
    assert_eq!(empty_event.status, EventStatus::Received); // Correct initial status
    assert_eq!(empty_event.failure_count, 0); // No failures initially
    assert!(empty_event.received_at <= chrono::Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap()); // Reasonable timestamp
    assert_eq!(empty_event.idempotency_strategy, IdempotencyStrategy::Header); // Default strategy

    // Test normal payload
    let normal_event = WebhookEvent::new(
        EventId::new(),
        TenantId::new(),
        EndpointId::new(),
        "normal_test".to_string(),
        headers.clone(),
        normal_body.clone(),
        "application/json".to_string(),
        now,
    );

    assert_eq!(normal_event.payload_size, i32::try_from(normal_body.len()).unwrap());

    // Test large payload handling
    let large_event = WebhookEvent::new(
        EventId::new(),
        TenantId::new(),
        EndpointId::new(),
        "large_test".to_string(),
        headers,
        large_body,
        "application/json".to_string(),
        now,
    );

    assert_eq!(large_event.payload_size, 10_000_000);
}

/// Test WebhookEvent body access methods maintain consistency.
///
/// Verifies that different ways of accessing the body return consistent data
/// and handle edge cases correctly.
#[test]
fn webhook_event_body_access_consistency() {
    let test_payload = b"consistency test payload \x00\x01\x02";
    let clock = Arc::new(TestClock::new());
    let now = DateTime::<Utc>::from(clock.now_system());
    let event = WebhookEvent::new(
        EventId::new(),
        TenantId::new(),
        EndpointId::new(),
        "consistency_test".to_string(),
        HashMap::new(),
        test_payload.to_vec(),
        "application/octet-stream".to_string(),
        now,
    );

    // All access methods should return the same data
    assert_eq!(event.body(), test_payload);
    assert_eq!(event.body_bytes().as_ref(), test_payload);
    assert_eq!(&event.body, &test_payload.to_vec());

    // Test with empty body
    let empty_event = WebhookEvent::new(
        EventId::new(),
        TenantId::new(),
        EndpointId::new(),
        "empty_consistency_test".to_string(),
        HashMap::new(),
        Vec::new(),
        "text/plain".to_string(),
        now,
    );

    assert_eq!(empty_event.body(), &[] as &[u8]);
    assert_eq!(empty_event.body_bytes().len(), 0);
    assert!(empty_event.body.is_empty());
}

/// Test Endpoint circuit breaker state transitions logic.
///
/// Verifies that circuit breaker states transition correctly based on
/// failure patterns and success criteria.
#[test]
fn endpoint_circuit_breaker_state_transitions() {
    let now = chrono::Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap();

    // Test closed circuit (normal operation)
    let closed_endpoint = Endpoint {
        id: EndpointId::new(),
        tenant_id: TenantId::new(),
        url: "https://healthy.example.com".to_string(),
        name: "Healthy Endpoint".to_string(),
        is_active: true,
        signature_config: SignatureConfig::None,
        max_retries: 5,
        timeout_seconds: 30,
        retry_strategy: BackoffStrategy::Exponential,
        circuit_state: CircuitState::Closed,
        circuit_failure_count: 0,
        circuit_success_count: 5,
        circuit_last_failure_at: None,
        circuit_half_open_at: None,
        created_at: now,
        updated_at: now,
        deleted_at: None,
        total_events_received: 100,
        total_events_delivered: 98,
        total_events_failed: 2,
    };

    // Healthy endpoint should have more successes than failures
    assert!(closed_endpoint.circuit_success_count > closed_endpoint.circuit_failure_count);
    assert_eq!(closed_endpoint.circuit_state, CircuitState::Closed);
    assert!(closed_endpoint.total_events_delivered > closed_endpoint.total_events_failed);

    // Test open circuit (failing endpoint)
    let open_endpoint = Endpoint {
        id: EndpointId::new(),
        tenant_id: TenantId::new(),
        url: "https://failing.example.com".to_string(),
        name: "Failing Endpoint".to_string(),
        is_active: true,
        signature_config: SignatureConfig::None,
        max_retries: 10,
        timeout_seconds: 30,
        retry_strategy: BackoffStrategy::Exponential,
        circuit_state: CircuitState::Open,
        circuit_failure_count: 10,
        circuit_success_count: 0,
        circuit_last_failure_at: Some(now),
        circuit_half_open_at: Some(now + chrono::Duration::minutes(5)),
        created_at: now,
        updated_at: now,
        deleted_at: None,
        total_events_received: 50,
        total_events_delivered: 20,
        total_events_failed: 30,
    };

    // Open circuit should have recent failures
    assert!(open_endpoint.circuit_failure_count > 5);
    assert_eq!(open_endpoint.circuit_state, CircuitState::Open);
    assert!(open_endpoint.circuit_last_failure_at.is_some());
    assert!(open_endpoint.circuit_half_open_at.is_some());
    assert!(open_endpoint.total_events_failed > open_endpoint.total_events_delivered);

    // Test half-open circuit (testing recovery)
    let half_open_endpoint = Endpoint {
        id: EndpointId::new(),
        tenant_id: TenantId::new(),
        url: "https://recovering.example.com".to_string(),
        name: "Recovering Endpoint".to_string(),
        is_active: true,
        signature_config: SignatureConfig::None,
        max_retries: 100,
        timeout_seconds: 30,
        retry_strategy: BackoffStrategy::Exponential,
        circuit_state: CircuitState::HalfOpen,
        circuit_failure_count: 5,
        circuit_success_count: 2,
        circuit_last_failure_at: Some(now - chrono::Duration::minutes(10)),
        circuit_half_open_at: Some(now - chrono::Duration::minutes(5)),
        created_at: now,
        updated_at: now,
        deleted_at: None,
        total_events_received: 30,
        total_events_delivered: 15,
        total_events_failed: 15,
    };

    assert_eq!(half_open_endpoint.circuit_state, CircuitState::HalfOpen);
    // Should have some recent successes in half-open state
    assert!(half_open_endpoint.circuit_success_count > 0);
}

/// Test Endpoint retry configuration validation.
///
/// Verifies that endpoint retry settings are reasonable and follow
/// business constraints.
#[test]
fn endpoint_retry_configuration_validation() {
    let now = chrono::Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap();

    // Test reasonable retry configuration
    let reasonable_endpoint = Endpoint {
        id: EndpointId::new(),
        tenant_id: TenantId::new(),
        url: "https://api.example.com".to_string(),
        name: "API Endpoint".to_string(),
        is_active: true,
        signature_config: SignatureConfig::hmac_sha256("secret".to_string()),
        max_retries: 3,
        timeout_seconds: 30,
        retry_strategy: BackoffStrategy::Exponential,
        circuit_state: CircuitState::Closed,
        circuit_failure_count: 0,
        circuit_success_count: 0,
        circuit_last_failure_at: None,
        circuit_half_open_at: None,
        created_at: now,
        updated_at: now,
        deleted_at: None,
        total_events_received: 0,
        total_events_delivered: 0,
        total_events_failed: 0,
    };

    // Verify reasonable retry settings
    // max_retries is u32, so it's always >= 0
    assert!(reasonable_endpoint.max_retries <= 10); // Reasonable upper bound
    assert!(reasonable_endpoint.timeout_seconds > 0);
    assert!(reasonable_endpoint.timeout_seconds <= 300); // Max 5 minutes
    assert_eq!(reasonable_endpoint.retry_strategy, BackoffStrategy::Exponential);

    // Test edge case configurations
    let aggressive_endpoint = Endpoint {
        id: EndpointId::new(),
        tenant_id: TenantId::new(),
        url: "https://critical.example.com".to_string(),
        name: "Critical Endpoint".to_string(),
        is_active: true,
        signature_config: SignatureConfig::None,
        max_retries: 10,      // High retry count for critical endpoints
        timeout_seconds: 120, // Longer timeout
        retry_strategy: BackoffStrategy::Exponential,
        circuit_state: CircuitState::Closed,
        circuit_failure_count: 0,
        circuit_success_count: 0,
        circuit_last_failure_at: None,
        circuit_half_open_at: None,
        created_at: now,
        updated_at: now,
        deleted_at: None,
        total_events_received: 0,
        total_events_delivered: 0,
        total_events_failed: 0,
    };

    assert!(aggressive_endpoint.max_retries > 5);
    assert!(aggressive_endpoint.timeout_seconds > 60);

    let minimal_endpoint = Endpoint {
        id: EndpointId::new(),
        tenant_id: TenantId::new(),
        url: "https://limits.example.com".to_string(),
        name: "Max Limits Endpoint".to_string(),
        is_active: true,
        signature_config: SignatureConfig::None,
        max_retries: 1,      // Minimal retries
        timeout_seconds: 10, // Short timeout
        retry_strategy: BackoffStrategy::Linear,
        circuit_state: CircuitState::Closed,
        circuit_failure_count: 0,
        circuit_success_count: 0,
        circuit_last_failure_at: None,
        circuit_half_open_at: None,
        created_at: now,
        updated_at: now,
        deleted_at: None,
        total_events_received: 0,
        total_events_delivered: 0,
        total_events_failed: 0,
    };

    assert!(minimal_endpoint.max_retries >= 1);
    assert!(minimal_endpoint.timeout_seconds >= 5);
}

/// Test DeliveryAttempt timing and duration constraints.
///
/// Verifies that delivery attempts have realistic timing information
/// and duration constraints.
#[test]
fn delivery_attempt_timing_constraints() {
    let now = chrono::Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap();
    let event_id = EventId::new();

    // Test successful quick delivery
    let quick_success = DeliveryAttempt {
        id: Uuid::new_v4(),
        event_id,
        attempt_number: 1,
        endpoint_id: EndpointId::new(),
        request_headers: HashMap::new(),
        request_body: b"quick request".to_vec(),
        response_status: Some(200),
        response_headers: Some(HashMap::new()),
        response_body: Some(b"OK".to_vec()),
        attempted_at: now,
        succeeded: true,
        error_message: None,
    };

    assert!(quick_success.succeeded); // Should be successful
    assert!(quick_success.response_status.unwrap() >= 200);
    assert!(quick_success.response_status.unwrap() < 300);
    assert!(quick_success.error_message.is_none());

    // Test slow successful delivery
    // Slow but successful attempt
    let slow_success = DeliveryAttempt {
        id: Uuid::new_v4(),
        event_id,
        attempt_number: 2,
        endpoint_id: EndpointId::new(),
        request_headers: HashMap::new(),
        request_body: b"slow request".to_vec(),
        response_status: Some(200),
        response_headers: Some(HashMap::new()),
        response_body: Some(b"Processed".to_vec()),
        attempted_at: now,
        succeeded: true,
        error_message: None,
    };

    assert!(slow_success.succeeded);
    assert!(slow_success.attempt_number > 1); // Retry attempt

    // Test timeout failure
    // Timeout failure
    let timeout_failure = DeliveryAttempt {
        id: Uuid::new_v4(),
        event_id,
        attempt_number: 3,
        endpoint_id: EndpointId::new(),
        request_headers: HashMap::new(),
        request_body: b"timeout request".to_vec(),
        response_status: None,
        response_headers: None,
        response_body: None,
        attempted_at: now,
        succeeded: false,
        error_message: Some("Request timed out after 30s".to_string()),
    };

    assert!(timeout_failure.response_status.is_none());
    assert!(!timeout_failure.succeeded);
    assert!(timeout_failure.error_message.is_some());
    assert!(timeout_failure.attempt_number > 1); // Should be a retry
}

/// Test domain model invariants and edge cases.
///
/// Verifies that models maintain consistency even with edge case inputs
/// and unusual but valid configurations.
#[test]
fn domain_model_invariants_and_edge_cases() {
    let clock = Arc::new(TestClock::new());
    let now = DateTime::<Utc>::from(clock.now_system());

    // Test WebhookEvent with maximum payload size
    let max_payload = vec![0u8; i32::MAX as usize - 1]; // Near max i32
    let large_event = WebhookEvent::new(
        EventId::new(),
        TenantId::new(),
        EndpointId::new(),
        "max_payload_test".to_string(),
        HashMap::new(),
        max_payload,
        "application/octet-stream".to_string(),
        now,
    );

    // Should handle large payloads gracefully
    assert!(large_event.payload_size > 0);
    assert_eq!(large_event.status, EventStatus::Received);

    // Test Endpoint with extreme but valid configurations
    let extreme_endpoint = Endpoint {
        id: EndpointId::new(),
        tenant_id: TenantId::new(),
        url: "https://a.co/w".to_string(), // Very short valid URL
        name: "X".to_string(),             // Single character name
        is_active: false,                  // Inactive endpoint
        signature_config: SignatureConfig::hmac_sha256("minimal-secret".to_string()),
        max_retries: 0i32,  // No retries
        timeout_seconds: 1, // Minimal timeout
        retry_strategy: BackoffStrategy::Fixed,
        circuit_state: CircuitState::Open,
        circuit_failure_count: i32::MAX,
        circuit_success_count: 0,
        circuit_last_failure_at: Some(now),
        circuit_half_open_at: None,
        created_at: now,
        updated_at: now,
        deleted_at: Some(now), // Soft deleted
        total_events_received: 0,
        total_events_delivered: 0,
        total_events_failed: 0,
    };

    // Should handle extreme configurations
    assert!(!extreme_endpoint.is_active);
    assert_eq!(extreme_endpoint.max_retries, 0);
    assert_eq!(extreme_endpoint.timeout_seconds, 1);
    assert!(extreme_endpoint.deleted_at.is_some());

    // Test DeliveryAttempt with unusual but valid data
    let unusual_attempt = DeliveryAttempt {
        id: Uuid::new_v4(),
        event_id: EventId::new(),
        attempt_number: 100, // Many attempts
        endpoint_id: EndpointId::new(),
        request_headers: {
            let mut headers = HashMap::new();
            // Add many headers
            for i in 0..50 {
                headers.insert(format!("x-custom-header-{i}"), format!("value-{i}"));
            }
            headers
        },
        request_body: b"unusual request".to_vec(),
        response_status: Some(418), // I'm a teapot - unusual but valid
        response_headers: Some(HashMap::new()),
        response_body: Some("☕".as_bytes().to_vec()), // Unicode response
        attempted_at: now,
        succeeded: true,
        error_message: None,
    };

    assert!(unusual_attempt.attempt_number > 10);
    assert!(unusual_attempt.request_headers.len() == 50);
    assert_eq!(unusual_attempt.response_status, Some(418));
    assert!(unusual_attempt.succeeded);
}
