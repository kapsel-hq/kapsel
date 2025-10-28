//! Webhook ingestion handler with validation and persistence.
//!
//! Accepts incoming webhooks, validates signatures and payload constraints,
//! and persists to database with idempotency protection.

use std::{collections::HashMap, sync::Arc};

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use bytes::Bytes;
use kapsel_core::{
    error::CoreError, storage::Storage, Clock, EndpointId, EventId, EventStatus, KapselError,
    Result, SignatureConfig, TenantId, WebhookEvent,
};
use serde::{Deserialize, Serialize};
use tracing::{debug, error, info, instrument, warn};
use uuid::Uuid;

use crate::{
    crypto::{validate_signature, ValidationResult},
    AppState,
};

/// Ingestion service that encapsulates clock dependency for webhook processing.
pub struct IngestService {
    clock: Arc<dyn Clock>,
}

impl IngestService {
    /// Creates a new ingestion service with the given clock.
    pub fn new(clock: Arc<dyn Clock>) -> Self {
        Self { clock }
    }

    /// Persists a webhook event to the database.
    #[allow(clippy::too_many_arguments)]
    async fn persist_event(
        &self,
        storage: &Storage,
        event_id: EventId,
        tenant_id: TenantId,
        endpoint_id: EndpointId,
        idempotency_key: String,
        headers: serde_json::Value,
        body: Bytes,
        content_type: String,
        signature_valid: Option<bool>,
        signature_error: Option<String>,
    ) -> Result<()> {
        let received_at = chrono::DateTime::<chrono::Utc>::from(self.clock.now_system());

        let event = WebhookEvent::new(
            event_id,
            tenant_id,
            EndpointId(endpoint_id.0),
            idempotency_key.clone(),
            serde_json::from_value(headers).unwrap_or_default(),
            body.to_vec(),
            content_type,
            received_at,
        );

        // Create the full event struct with additional fields
        let full_event = WebhookEvent { signature_valid, signature_error, ..event };

        storage.webhook_events.create(&full_event).await?;
        info!(
            event_id = ?event_id,
            tenant_id = ?tenant_id,
            endpoint_id = ?endpoint_id,
            payload_size = body.len(),
            "webhook event persisted successfully"
        );

        Ok(())
    }
}

/// Request body for webhook ingestion.
///
/// Accepts any valid JSON payload up to 10MB.
#[derive(Debug, Deserialize)]
pub struct IngestRequest {
    /// The webhook payload as arbitrary JSON
    #[serde(flatten)]
    pub payload: serde_json::Value,
}

/// Response from successful webhook ingestion.
#[derive(Debug, Serialize)]
pub struct IngestResponse {
    /// Unique identifier for the ingested event
    pub event_id: String,
    /// Current processing status of the event
    pub status: String,
}

/// Error response with code and message.
#[derive(Debug, Serialize)]
pub struct ErrorResponse {
    /// Error details including code and message
    pub error: ErrorDetail,
}

/// Detailed error information.
#[derive(Debug, Serialize)]
pub struct ErrorDetail {
    /// Error code from our taxonomy (E1001-E3004)
    pub code: String,
    /// Human-readable error description
    pub message: String,
}

/// Ingests a webhook for reliable delivery.
///
/// Validates the endpoint exists, checks payload size limits,
/// applies idempotency logic, and persists the event for processing.
///
/// # Errors
///
/// Returns appropriate HTTP status codes:
/// - 404: Endpoint not found
/// - 409: Duplicate event (idempotency check)
/// - 413: Payload too large (>10MB)
/// - 500: Database or internal errors
#[instrument(
    name = "ingest_webhook",
    skip(app_state, headers, body),
    fields(
        endpoint_id = %endpoint_id,
        content_length = headers.get("content-length").and_then(|v| v.to_str().ok()).unwrap_or("unknown"),
        idempotency_key = headers.get("x-idempotency-key").and_then(|v| v.to_str().ok()).unwrap_or("none"),
    )
)]
#[allow(clippy::too_many_lines)]
pub async fn ingest_webhook(
    State(app_state): State<AppState>,
    Path(endpoint_id): Path<Uuid>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    info!("Processing webhook ingestion request");

    const MAX_PAYLOAD_SIZE: usize = 10 * 1024 * 1024;
    if body.len() > MAX_PAYLOAD_SIZE {
        warn!(payload_size = body.len(), limit = MAX_PAYLOAD_SIZE, "Payload exceeds size limit");
        return create_error_response(
            StatusCode::PAYLOAD_TOO_LARGE,
            &KapselError::PayloadTooLarge { size_bytes: body.len() },
        );
    }

    let endpoint_id = EndpointId::from(endpoint_id);
    let endpoint_result = fetch_endpoint(&app_state.storage, endpoint_id).await;

    let tenant_id = match endpoint_result {
        Ok(tenant_id) => tenant_id,
        Err(e) => {
            warn!(error = %e, "Endpoint not found");
            return create_error_response(StatusCode::NOT_FOUND, &KapselError::InvalidEndpoint {
                id: endpoint_id,
            });
        },
    };

    debug!(tenant_id = %tenant_id, "Endpoint validated");

    let idempotency_key = headers
        .get("x-idempotency-key")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();

    if !idempotency_key.is_empty() {
        match check_duplicate(&app_state.storage, &idempotency_key, endpoint_id).await {
            Ok(Some(existing_id)) => {
                info!(
                    existing_event_id = %existing_id,
                    "Duplicate event detected, returning existing"
                );
                return (
                    StatusCode::OK,
                    Json(IngestResponse {
                        event_id: existing_id.to_string(),
                        status: EventStatus::Received.to_string(),
                    }),
                )
                    .into_response();
            },
            Ok(None) => {
                debug!("No duplicate found, proceeding with ingestion");
            },
            Err(e) => {
                error!(error = %e, "Failed to check for duplicates");
                return create_error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    &KapselError::Other(anyhow::anyhow!("Failed to check for duplicates: {e}")),
                );
            },
        }
    }

    let (signature_valid, signature_error) =
        match validate_webhook_signature(&app_state.storage, endpoint_id, &headers, &body).await {
            Ok(validation_result) => {
                (Some(validation_result.is_valid), validation_result.error_message)
            },
            Err(e) => {
                error!(error = %e, "Failed to validate signature");
                return create_error_response(
                    StatusCode::BAD_REQUEST,
                    &KapselError::Other(anyhow::anyhow!("Signature validation failed: {e}")),
                );
            },
        };

    if signature_valid == Some(false) {
        warn!("Webhook signature validation failed");
        return create_error_response(
            StatusCode::BAD_REQUEST,
            &KapselError::Other(anyhow::anyhow!("Invalid webhook signature")),
        );
    }

    let event_id = EventId::new();
    info!(event_id = %event_id, "Generated new event ID");

    let headers_map = extract_headers(&headers);
    let headers_json = match serde_json::to_value(&headers_map) {
        Ok(json) => json,
        Err(e) => {
            error!(error = %e, "Failed to serialize headers");
            return create_error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                &KapselError::Other(anyhow::anyhow!("Failed to serialize headers: {e}")),
            );
        },
    };

    let content_type = headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_string();

    let ingest_service = IngestService::new(app_state.clock.clone());
    let insert_result = ingest_service
        .persist_event(
            &app_state.storage,
            event_id,
            tenant_id,
            endpoint_id,
            idempotency_key,
            headers_json,
            body.clone(),
            content_type,
            signature_valid,
            signature_error,
        )
        .await;

    match insert_result {
        Ok(()) => {
            info!(event_id = %event_id, "Webhook successfully ingested");
            (
                StatusCode::OK,
                Json(IngestResponse {
                    event_id: event_id.to_string(),
                    status: EventStatus::Received.to_string(),
                }),
            )
                .into_response()
        },
        Err(e) => {
            error!(error = %e, "Failed to persist webhook event");
            create_error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                &KapselError::Other(anyhow::anyhow!("Failed to persist webhook event: {e}")),
            )
        },
    }
}

/// Fetches an endpoint and returns its tenant ID.
async fn fetch_endpoint(storage: &Storage, endpoint_id: EndpointId) -> Result<TenantId> {
    let endpoint = storage
        .endpoints
        .find_by_id(endpoint_id)
        .await?
        .ok_or_else(|| CoreError::NotFound(format!("endpoint {} not found", endpoint_id.0)))?;

    Ok(endpoint.tenant_id)
}

/// Checks for duplicate events based on idempotency key.
async fn check_duplicate(
    storage: &Storage,
    idempotency_key: &str,
    endpoint_id: EndpointId,
) -> Result<Option<EventId>> {
    let duplicate_event =
        storage.webhook_events.find_duplicate(endpoint_id, idempotency_key).await?;

    Ok(duplicate_event.map(|event| event.id))
}

/// Extracts headers into a HashMap for storage.
fn extract_headers(headers: &HeaderMap) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for (name, value) in headers {
        if let Ok(value_str) = value.to_str() {
            map.insert(name.as_str().to_string(), value_str.to_string());
        }
    }
    map
}

/// Validates webhook signature against endpoint configuration.
///
/// Returns validation result or error if endpoint configuration is invalid.
///
/// # Errors
///
/// Returns error if endpoint lookup fails or signature verification fails.
async fn validate_webhook_signature(
    storage: &Storage,
    endpoint_id: EndpointId,
    headers: &HeaderMap,
    body: &[u8],
) -> Result<ValidationResult> {
    let endpoint = storage
        .endpoints
        .find_by_id(endpoint_id)
        .await?
        .ok_or_else(|| CoreError::NotFound(format!("endpoint {} not found", endpoint_id.0)))?;

    let (signing_secret, signature_header) = match endpoint.signature_config {
        SignatureConfig::None => (None, None),
        SignatureConfig::HmacSha256 { secret, header } => (Some(secret), Some(header)),
    };

    let Some(signing_secret) = signing_secret else { return Ok(ValidationResult::valid()) };

    let header_name = signature_header.unwrap_or_else(|| "X-Webhook-Signature".to_string());

    let signature = headers.get(&header_name).and_then(|v| v.to_str().ok()).unwrap_or("");

    if signature.is_empty() {
        return Ok(ValidationResult::invalid("signature required but missing"));
    }

    Ok(validate_signature(body, signature, &signing_secret))
}

/// Creates a standardized error response.
fn create_error_response(status: StatusCode, error: &KapselError) -> Response {
    let error_response = ErrorResponse {
        error: ErrorDetail { code: error.code().to_string(), message: error.to_string() },
    };

    (status, Json(error_response)).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_response_includes_code() {
        let error = KapselError::PayloadTooLarge { size_bytes: 11_000_000 };
        let response = create_error_response(StatusCode::PAYLOAD_TOO_LARGE, &error);

        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[test]
    fn headers_extraction_preserves_all_values() {
        let mut headers = HeaderMap::new();
        headers.insert("content-type", "application/json".parse().unwrap());
        headers.insert("x-custom-header", "test-value".parse().unwrap());

        let extracted = extract_headers(&headers);

        assert_eq!(extracted.get("content-type").unwrap(), "application/json");
        assert_eq!(extracted.get("x-custom-header").unwrap(), "test-value");
    }
}
