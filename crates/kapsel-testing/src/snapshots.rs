//! Snapshot methods for TestEnv - used for deterministic regression testing

use anyhow::{Context, Result};
use sqlx::Row;

use crate::{TestEnv, WebhookEventData};

impl TestEnv {
    /// Create a deterministic snapshot of the events table for regression
    /// testing.
    ///
    /// Orders results deterministically and formats as readable string for
    /// insta snapshots. Redacts dynamic fields like timestamps and UUIDs
    /// for stable snapshots.
    pub async fn snapshot_events_table(&self) -> Result<String> {
        let events = sqlx::query_as::<_, WebhookEventData>(
            "SELECT
                id, tenant_id, endpoint_id, source_event_id, idempotency_strategy,
                status, failure_count, last_attempt_at, next_retry_at,
                headers, body, content_type, payload_size,
                signature_valid, signature_error,
                received_at, delivered_at, failed_at
             FROM webhook_events
             ORDER BY received_at ASC, id ASC",
        )
        .fetch_all(self.pool())
        .await
        .context("failed to fetch events for snapshot")?;

        let mut output = String::new();
        output.push_str("Events Table Snapshot\n");
        output.push_str("====================\n\n");

        for (i, event) in events.iter().enumerate() {
            use std::fmt::Write;
            let _ = writeln!(output, "Event {}:", i + 1);
            let _ = writeln!(output, "  source_event_id: {}", event.source_event_id);
            let _ = writeln!(output, "  status: {}", event.status);
            let _ = writeln!(output, "  failure_count: {}", event.failure_count);
            let _ = writeln!(output, "  attempt_count: {}", event.attempt_count());
            let _ = writeln!(output, "  payload_size: {}", event.payload_size);
            let _ = writeln!(output, "  content_type: {}", event.content_type);

            // Redact dynamic fields for stable snapshots
            output.push_str("  id: [UUID]\n");
            output.push_str("  tenant_id: [UUID]\n");
            output.push_str("  endpoint_id: [UUID]\n");
            output.push_str("  received_at: [TIMESTAMP]\n");

            if event.delivered_at.is_some() {
                output.push_str("  delivered_at: [TIMESTAMP]\n");
            }
            if event.last_attempt_at.is_some() {
                output.push_str("  last_attempt_at: [TIMESTAMP]\n");
            }
            if event.next_retry_at.is_some() {
                output.push_str("  next_retry_at: [TIMESTAMP]\n");
            }

            output.push('\n');
        }

        if events.is_empty() {
            output.push_str("No events found.\n");
        }

        Ok(output)
    }

    /// Create a snapshot of delivery attempts for debugging and regression
    /// testing.
    pub async fn snapshot_delivery_attempts(&self) -> Result<String> {
        #[derive(sqlx::FromRow)]
        struct DeliveryAttempt {
            attempt_number: i32,
            response_status: Option<i32>,
            succeeded: bool,
            request_body: Option<Vec<u8>>,
        }

        let attempts = sqlx::query_as::<_, DeliveryAttempt>(
            "SELECT attempt_number, response_status, succeeded, request_body
             FROM delivery_attempts
             ORDER BY attempt_number ASC",
        )
        .fetch_all(self.pool())
        .await
        .context("failed to fetch delivery attempts for snapshot")?;

        let mut output = String::new();
        output.push_str("Delivery Attempts Snapshot\n");
        output.push_str("=========================\n\n");

        for (i, attempt) in attempts.iter().enumerate() {
            use std::fmt::Write;
            let _ = writeln!(output, "Attempt {}:", i + 1);
            let _ = writeln!(output, "  attempt_number: {}", attempt.attempt_number);
            let _ = writeln!(output, "  response_status: {:?}", attempt.response_status);
            let _ = writeln!(output, "  succeeded: {}", attempt.succeeded);
            let _ = writeln!(
                output,
                "  request_body_size: {}",
                attempt.request_body.as_ref().map_or(0, Vec::len)
            );
            output.push('\n');
        }

        if attempts.is_empty() {
            output.push_str("No delivery attempts found.\n");
        }

        Ok(output)
    }

    /// Snapshot the database schema for detecting schema evolution issues.
    ///
    /// This captures table structure, indexes, and constraints to catch
    /// unintended schema changes in pull requests.
    pub async fn snapshot_database_schema(&self) -> Result<String> {
        use std::fmt::Write;

        // Query information_schema for deterministic schema representation
        let tables = sqlx::query(
            "SELECT table_name, column_name, data_type, is_nullable, column_default
             FROM information_schema.columns
             WHERE table_schema = 'public'
             ORDER BY table_name, ordinal_position",
        )
        .fetch_all(self.pool())
        .await
        .context("failed to fetch schema information")?;

        let indexes = sqlx::query(
            "SELECT schemaname, tablename, indexname, indexdef
             FROM pg_indexes
             WHERE schemaname = 'public'
             ORDER BY tablename, indexname",
        )
        .fetch_all(self.pool())
        .await
        .context("failed to fetch index information")?;

        let mut output = String::new();
        output.push_str("Database Schema Snapshot\n");
        output.push_str("=======================\n\n");

        // Group columns by table
        let mut current_table = String::new();
        for row in tables {
            let table_name: String = row.get("table_name");
            let column_name: String = row.get("column_name");
            let data_type: String = row.get("data_type");
            let is_nullable: String = row.get("is_nullable");
            let column_default: Option<String> = row.get("column_default");

            if current_table != table_name {
                if !current_table.is_empty() {
                    output.push('\n');
                }
                current_table.clone_from(&table_name);
                let _ = writeln!(output, "Table: {current_table}");
                let _ = writeln!(output, "{}", "-".repeat(current_table.len() + 7));
            }

            let _ = writeln!(
                output,
                "  {}: {} {} {}",
                column_name,
                data_type,
                if is_nullable == "YES" { "NULL" } else { "NOT NULL" },
                column_default.as_deref().unwrap_or("")
            );
        }

        output.push_str("\nIndexes:\n");
        output.push_str("========\n");
        for index in indexes {
            let tablename: String = index.get("tablename");
            let indexname: String = index.get("indexname");
            let indexdef: String = index.get("indexdef");
            let _ = writeln!(output, "{tablename}.{indexname}: {indexdef}");
        }

        Ok(output)
    }

    /// Generic table snapshot method for any table.
    ///
    /// Useful for snapshotting lookup tables, configuration, etc.
    pub async fn snapshot_table(&self, table_name: &str, order_by: &str) -> Result<String> {
        use std::fmt::Write;

        let query = format!("SELECT * FROM {table_name} ORDER BY {order_by}");

        let rows = sqlx::query(&query)
            .fetch_all(self.pool())
            .await
            .context(format!("failed to snapshot table {table_name}"))?;

        let mut output = String::new();
        let _ = writeln!(output, "Table Snapshot: {table_name}");
        let _ = writeln!(output, "{}\n", "=".repeat(table_name.len() + 16));

        if rows.is_empty() {
            output.push_str("No rows found.\n");
        } else {
            let _ = writeln!(output, "Row count: {}", rows.len());
            output.push_str("(Use specific snapshot methods for detailed row inspection)\n");
        }

        Ok(output)
    }
}
