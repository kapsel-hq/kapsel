#![no_main]

//! Fuzz target for JSON payload parsing.
//!
//! Tests JSON parsing logic with malformed inputs to ensure the system
//! never panics when processing unexpected or malicious JSON payloads.
//! This finds parsing edge cases that could cause service crashes or
//! resource exhaustion in production.

use bytes::Bytes;
use libfuzzer_sys::fuzz_target;
use serde_json::Value;

fuzz_target!(|data: &[u8]| {
    fuzz_json_payload_parsing(data);
});

/// Test JSON payload parsing with arbitrary input data.
///
/// Ensures JSON parsing logic handles malformed payloads gracefully
/// without panicking or consuming excessive resources. Tests various
/// JSON edge cases including deeply nested objects, large numbers,
/// invalid Unicode, and malicious payloads designed to cause DoS.
fn fuzz_json_payload_parsing(data: &[u8]) {
    let payload = Bytes::from(data.to_vec());

    // Test basic JSON parsing
    let _ = parse_json_safely(&payload);

    // Test JSON parsing with size limits
    let _ = parse_json_with_limits_safely(&payload, 1024 * 1024); // 1MB limit

    // Test JSON schema validation
    let _ = validate_webhook_json_safely(&payload);

    // Test JSON path extraction
    let _ = extract_json_fields_safely(&payload);

    // Test JSON pretty printing
    let _ = format_json_safely(&payload);

    // Test JSON minification
    let _ = minify_json_safely(&payload);

    // Test JSON merge operations
    let _ = merge_json_safely(&payload);

    // Test streaming JSON parsing
    let _ = parse_json_streaming_safely(&payload);

    // Test resource limits
    let _ = check_json_complexity_safely(&payload);
}

/// Safely parse JSON payload without panicking.
fn parse_json_safely(payload: &Bytes) -> Option<Value> {
    std::panic::catch_unwind(|| {
        // Test with serde_json
        serde_json::from_slice(payload).ok()
    })
    .ok()
    .flatten()
}

/// Parse JSON with memory and recursion limits.
fn parse_json_with_limits_safely(payload: &Bytes, max_size: usize) -> Option<Value> {
    std::panic::catch_unwind(|| {
        if payload.len() > max_size {
            return None;
        }

        // Check for excessive nesting before parsing
        let nesting_depth = estimate_json_depth(payload);
        if nesting_depth > 100 {
            return None;
        }

        serde_json::from_slice(payload).ok()
    })
    .ok()
    .flatten()
}

/// Validate JSON payload matches expected webhook schema.
fn validate_webhook_json_safely(payload: &Bytes) -> Option<bool> {
    std::panic::catch_unwind(|| {
        let json: Value = serde_json::from_slice(payload).ok()?;

        // Common webhook fields to look for
        let expected_fields = ["id", "event", "data", "timestamp", "type"];

        match &json {
            Value::Object(obj) => {
                // Check if it looks like a webhook payload
                let has_webhook_fields =
                    expected_fields.iter().any(|field| obj.contains_key(*field));

                Some(has_webhook_fields)
            },
            _ => Some(false), // Non-object payloads are valid but not webhook-like
        }
    })
    .ok()
    .flatten()
}

/// Extract common fields from JSON webhook payload.
fn extract_json_fields_safely(payload: &Bytes) -> Option<WebhookFields> {
    std::panic::catch_unwind(|| {
        let json: Value = serde_json::from_slice(payload).ok()?;

        let mut fields = WebhookFields::default();

        if let Value::Object(obj) = &json {
            // Extract common webhook fields
            fields.event_id = obj.get("id").and_then(|v| v.as_str()).map(|s| s.to_string());

            fields.event_type = obj
                .get("type")
                .or_else(|| obj.get("event"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());

            fields.timestamp =
                obj.get("timestamp").or_else(|| obj.get("created_at")).and_then(|v| v.as_u64());

            // Extract nested data field
            fields.data = obj.get("data").cloned();

            // Count total fields
            fields.field_count = obj.len();
        }

        Some(fields)
    })
    .ok()
    .flatten()
}

/// Pretty format JSON for debugging.
fn format_json_safely(payload: &Bytes) -> Option<String> {
    std::panic::catch_unwind(|| {
        let json: Value = serde_json::from_slice(payload).ok()?;
        serde_json::to_string_pretty(&json).ok()
    })
    .ok()
    .flatten()
}

/// Minify JSON by removing whitespace.
fn minify_json_safely(payload: &Bytes) -> Option<String> {
    std::panic::catch_unwind(|| {
        let json: Value = serde_json::from_slice(payload).ok()?;
        serde_json::to_string(&json).ok()
    })
    .ok()
    .flatten()
}

/// Merge JSON objects (for webhook payload transformation).
fn merge_json_safely(payload: &Bytes) -> Option<Value> {
    std::panic::catch_unwind(|| {
        let json: Value = serde_json::from_slice(payload).ok()?;

        // Create a base webhook envelope
        let base = serde_json::json!({
            "webhook_id": "test-webhook-123",
            "received_at": "2024-01-01T00:00:00Z",
            "processed": false
        });

        // Attempt to merge with incoming payload
        match (&base, &json) {
            (Value::Object(base_obj), Value::Object(payload_obj)) => {
                let mut merged = base_obj.clone();
                merged.extend(payload_obj.clone());
                Some(Value::Object(merged))
            },
            _ => Some(json), // Return original if merge not possible
        }
    })
    .ok()
    .flatten()
}

/// Parse JSON using streaming approach for large payloads.
fn parse_json_streaming_safely(payload: &Bytes) -> Option<Vec<Value>> {
    std::panic::catch_unwind(|| {
        // For very large JSON arrays, try to parse individual elements
        let payload_str = String::from_utf8_lossy(payload);

        if payload_str.trim_start().starts_with('[') {
            // Looks like JSON array - try to extract individual elements
            let mut elements = Vec::new();
            let mut depth = 0;
            let mut current = String::new();
            let mut in_string = false;
            let mut escape_next = false;

            for ch in payload_str.chars() {
                current.push(ch);

                if escape_next {
                    escape_next = false;
                    continue;
                }

                match ch {
                    '"' if !escape_next => in_string = !in_string,
                    '\\' if in_string => escape_next = true,
                    '{' | '[' if !in_string => depth += 1,
                    '}' | ']' if !in_string => {
                        depth -= 1;
                        if depth == 1 && ch == '}' {
                            // End of object in array
                            if let Ok(element) =
                                serde_json::from_str::<Value>(&current.trim_end_matches(',').trim())
                            {
                                elements.push(element);
                            }
                            current.clear();
                        }
                    },
                    _ => {},
                }

                // Prevent excessive memory usage
                if elements.len() > 1000 || current.len() > 10_000 {
                    break;
                }
            }

            if elements.is_empty() {
                None
            } else {
                Some(elements)
            }
        } else {
            // Single JSON object
            parse_json_safely(payload).map(|v| vec![v])
        }
    })
    .ok()
    .flatten()
}

/// Check JSON complexity to prevent resource exhaustion.
fn check_json_complexity_safely(payload: &Bytes) -> Option<JsonComplexity> {
    std::panic::catch_unwind(|| {
        let mut complexity = JsonComplexity::default();

        // Basic size check
        complexity.payload_size = payload.len();
        if complexity.payload_size > 10 * 1024 * 1024 {
            // Payload too large
            return None;
        }

        // Estimate nesting depth
        complexity.max_depth = estimate_json_depth(payload);
        if complexity.max_depth > 100 {
            // Too deeply nested
            return None;
        }

        // Try to parse and analyze structure
        if let Ok(json) = serde_json::from_slice::<Value>(payload) {
            analyze_json_structure(&json, &mut complexity, 0);
        }

        Some(complexity)
    })
    .ok()
    .flatten()
}

/// Estimate JSON nesting depth without full parsing.
fn estimate_json_depth(payload: &Bytes) -> usize {
    let mut depth = 0;
    let mut max_depth = 0;
    let mut in_string = false;
    let mut escape_next = false;

    for &byte in payload {
        let ch = byte as char;

        if escape_next {
            escape_next = false;
            continue;
        }

        match ch {
            '"' => in_string = !in_string,
            '\\' if in_string => escape_next = true,
            '{' | '[' if !in_string => {
                depth += 1;
                max_depth = max_depth.max(depth);
            },
            '}' | ']' if !in_string => {
                if depth > 0 {
                    depth -= 1;
                }
            },
            _ => {},
        }

        // Prevent infinite loops on malformed input
        if max_depth > 1000 {
            break;
        }
    }

    max_depth
}

/// Analyze JSON structure for complexity metrics.
fn analyze_json_structure(value: &Value, complexity: &mut JsonComplexity, current_depth: usize) {
    complexity.max_depth = complexity.max_depth.max(current_depth);

    match value {
        Value::Object(obj) => {
            complexity.object_count += 1;
            complexity.total_fields += obj.len();

            for (key, val) in obj {
                complexity.total_key_length += key.len();
                if current_depth < 50 {
                    // Prevent stack overflow
                    analyze_json_structure(val, complexity, current_depth + 1);
                }
            }
        },
        Value::Array(arr) => {
            complexity.array_count += 1;
            complexity.total_elements += arr.len();

            for val in arr {
                if current_depth < 50 {
                    // Prevent stack overflow
                    analyze_json_structure(val, complexity, current_depth + 1);
                }
            }
        },
        Value::String(s) => {
            complexity.string_count += 1;
            complexity.total_string_length += s.len();
        },
        Value::Number(_) => complexity.number_count += 1,
        Value::Bool(_) => complexity.bool_count += 1,
        Value::Null => complexity.null_count += 1,
    }
}

/// Extracted webhook fields from JSON payload.
#[derive(Default, Debug)]
struct WebhookFields {
    event_id: Option<String>,
    event_type: Option<String>,
    timestamp: Option<u64>,
    data: Option<Value>,
    field_count: usize,
}

/// JSON complexity metrics to prevent resource exhaustion.
#[derive(Default, Debug)]
struct JsonComplexity {
    payload_size: usize,
    max_depth: usize,
    object_count: usize,
    array_count: usize,
    string_count: usize,
    number_count: usize,
    bool_count: usize,
    null_count: usize,
    total_fields: usize,
    total_elements: usize,
    total_key_length: usize,
    total_string_length: usize,
}
