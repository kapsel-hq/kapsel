//! Webhook ingestion handler.

use axum::{extract::Path, http::StatusCode, Json};
use chrono::Utc;
use hooky_core::EventId;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use std::collections::HashMap;
use uuid::Uuid;

/// Request body for webhook ingestion (any JSON).
#[derive(Debug, Deserialize)]
pub struct IngestRequest {
    #[serde(flatten)]
    pub payload: serde_json::Value,
}

/// Response from webhook ingestion.
#[derive(Debug, Serialize)]
pub struct IngestResponse {
    pub event_id: String,
    pub status: String,
}

/// Handles POST /ingest/:endpoint_id
pub async fn ingest_webhook(
    Path(endpoint_id): Path<Uuid>,
    axum::extract::State(db): axum::extract::State<PgPool>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> Result<Json<IngestResponse>, StatusCode> {
    // Extract tenant_id from endpoint
    let endpoint: (Uuid, Uuid) = sqlx::query_as(
        "SELECT id, tenant_id FROM endpoints WHERE id = $1"
    )
    .bind(endpoint_id)
    .fetch_one(&db)
    .await
    .map_err(|_| StatusCode::NOT_FOUND)?;

    let (_, tenant_id) = endpoint;

    // Generate event ID
    let event_id = EventId::new();

    // Extract idempotency key from headers
    let source_event_id = headers
        .get("X-Idempotency-Key")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    // Convert headers to HashMap
    let mut headers_map = HashMap::new();
    for (name, value) in headers.iter() {
        if let Ok(value_str) = value.to_str() {
            headers_map.insert(name.as_str().to_string(), value_str.to_string());
        }
    }

    let headers_json = serde_json::to_value(&headers_map)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // Insert into database
    sqlx::query(
        r#"
        INSERT INTO webhook_events (
            id, tenant_id, endpoint_id, source_event_id,
            idempotency_strategy, status, headers, body, content_type, received_at
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
        "#,
    )
    .bind(event_id.0)
    .bind(tenant_id)
    .bind(endpoint_id)
    .bind(source_event_id)
    .bind("header")
    .bind("received")
    .bind(headers_json)
    .bind(body.as_ref())
    .bind("application/json")
    .bind(Utc::now())
    .execute(&db)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(IngestResponse {
        event_id: event_id.to_string(),
        status: "received".to_string(),
    }))
}
