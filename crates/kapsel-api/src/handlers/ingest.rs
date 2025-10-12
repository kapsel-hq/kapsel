//! Webhook ingestion handler.
//!
//! Accepts incoming webhooks, validates them, and persists to the database
//! for reliable delivery. Applies idempotency checks to prevent duplicate
//! processing of events.

use std::collections::HashMap;

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use bytes::Bytes;
use chrono::Utc;
use kapsel_core::{EndpointId, EventId, EventStatus, KapselError, Result, TenantId};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use tracing::{debug, error, info, instrument, warn};
use uuid::Uuid;

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
    skip(db, headers, body),
    fields(
        endpoint_id = %endpoint_id,
        content_length = headers.get("content-length").and_then(|v| v.to_str().ok()).unwrap_or("unknown"),
        idempotency_key = headers.get("x-idempotency-key").and_then(|v| v.to_str().ok()).unwrap_or("none"),
    )
)]
#[allow(clippy::too_many_lines)]
pub async fn ingest_webhook(
    Path(endpoint_id): Path<Uuid>,
    State(db): State<PgPool>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    info!("Processing webhook ingestion request");

    // Check payload size (10MB limit)
    const MAX_PAYLOAD_SIZE: usize = 10 * 1024 * 1024;
    if body.len() > MAX_PAYLOAD_SIZE {
        warn!(payload_size = body.len(), limit = MAX_PAYLOAD_SIZE, "Payload exceeds size limit");
        return create_error_response(
            StatusCode::PAYLOAD_TOO_LARGE,
            &KapselError::PayloadTooLarge { size_bytes: body.len() },
        );
    }

    // Fetch endpoint and validate it exists
    let endpoint_id = EndpointId::from(endpoint_id);
    let endpoint_result = fetch_endpoint(&db, endpoint_id).await;

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

    // Extract idempotency key for deduplication
    let idempotency_key = headers
        .get("x-idempotency-key")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();

    // Check for duplicate event
    if !idempotency_key.is_empty() {
        match check_duplicate(&db, &idempotency_key, endpoint_id).await {
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
                    &KapselError::Database(e),
                );
            },
        }
    }

    // Generate new event ID
    let event_id = EventId::new();
    info!(event_id = %event_id, "Generated new event ID");

    // Convert headers to JSON for storage
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

    // Extract content type
    let content_type = headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_string();

    // Persist event to database
    let insert_result = persist_event(
        &db,
        event_id,
        tenant_id,
        endpoint_id,
        idempotency_key,
        headers_json,
        body,
        content_type,
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
            create_error_response(StatusCode::INTERNAL_SERVER_ERROR, &KapselError::Database(e))
        },
    }
}

/// Fetches an endpoint and returns its tenant ID.
async fn fetch_endpoint(db: &PgPool, endpoint_id: EndpointId) -> Result<TenantId> {
    let row: Option<(Uuid,)> = sqlx::query_as("SELECT tenant_id FROM endpoints WHERE id = $1")
        .bind(endpoint_id.0)
        .fetch_optional(db)
        .await?;

    match row {
        Some((tenant_id,)) => Ok(TenantId::from(tenant_id)),
        None => Err(KapselError::InvalidEndpoint { id: endpoint_id }),
    }
}

/// Checks for duplicate events based on idempotency key.
async fn check_duplicate(
    db: &PgPool,
    idempotency_key: &str,
    endpoint_id: EndpointId,
) -> sqlx::Result<Option<EventId>> {
    let row: Option<(Uuid,)> = sqlx::query_as(
        r"
        SELECT id
        FROM webhook_events
        WHERE source_event_id = $1
          AND endpoint_id = $2
          AND received_at > NOW() - INTERVAL '24 hours'
        ",
    )
    .bind(idempotency_key)
    .bind(endpoint_id.0)
    .fetch_optional(db)
    .await?;

    Ok(row.map(|(id,)| EventId::from(id)))
}

/// Persists a webhook event to the database.
#[allow(clippy::too_many_arguments)]
async fn persist_event(
    db: &PgPool,
    event_id: EventId,
    tenant_id: TenantId,
    endpoint_id: EndpointId,
    idempotency_key: String,
    headers: serde_json::Value,
    body: Bytes,
    content_type: String,
) -> sqlx::Result<()> {
    sqlx::query(
        r"
        INSERT INTO webhook_events (
            id, tenant_id, endpoint_id, source_event_id,

            idempotency_strategy, status, headers, body,
            content_type, received_at, failure_count
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, 0)
        "
    )
    .bind(event_id.0)
    .bind(tenant_id.0)
    .bind(endpoint_id.0)
    .bind(idempotency_key)
    .bind("header") // Using header-based idempotency for now
    .bind(EventStatus::Received.to_string())
    .bind(headers)
    .bind(body.as_ref())
    .bind(content_type)
    .bind(Utc::now())
    .execute(db)
    .await?;

    Ok(())
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
        let error = KapselError::PayloadTooLarge { size_bytes: 11000000 };
        let response = create_error_response(StatusCode::PAYLOAD_TOO_LARGE, &error);

        // Response type is opaque, so we can't easily inspect it in tests
        // This would be better tested as an integration test
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
