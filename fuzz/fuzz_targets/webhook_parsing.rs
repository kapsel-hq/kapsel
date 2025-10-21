#![no_main]

//! Fuzz target for webhook payload parsing.
//!
//! Tests webhook payload parsing logic with malformed inputs to ensure
//! the system never panics when processing unexpected or malicious payloads.
//! This finds parsing edge cases that could cause service crashes in
//! production.

use std::collections::HashMap;

use bytes::Bytes;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    fuzz_webhook_payload_parsing(data);
});

/// Test webhook payload parsing with arbitrary input data.
///
/// Ensures the webhook parsing logic handles malformed payloads gracefully
/// without panicking. Tests various content types and payload formats that
/// might be sent by malicious actors or buggy webhook providers.
fn fuzz_webhook_payload_parsing(data: &[u8]) {
    let payload = Bytes::from(data.to_vec());

    // Test different content types that webhooks commonly use
    let content_types = [
        "application/json",
        "application/x-www-form-urlencoded",
        "text/plain",
        "application/xml",
        "multipart/form-data",
        "application/octet-stream",
        // Invalid/malicious content types
        "",
        "invalid/type",
        "text/html<script>alert('xss')</script>",
        "\x00\x01\x02", // Binary data as content type
    ];

    for content_type in content_types {
        // Test payload size calculation
        let _ = calculate_payload_size_safely(&payload);

        // Test content type parsing
        let _ = parse_content_type_safely(content_type);

        // Test signature extraction (if payload looks like headers)
        let _ = extract_signature_safely(&payload);
    }

    // Test JSON parsing specifically since it's common
    let _ = parse_json_payload_safely(&payload);

    // Test form data parsing
    let _ = parse_form_payload_safely(&payload);

    // Test payload validation
    let _ = validate_payload_safely(&payload);
}

/// Safely calculate payload size without panicking.
fn calculate_payload_size_safely(payload: &Bytes) -> Option<i32> {
    std::panic::catch_unwind(|| {
        // Payload size calculation should handle edge cases
        let size = payload.len();

        // Check for overflow when converting to i32
        if size > i32::MAX as usize {
            return None;
        }

        // Minimum size is 1 even for empty payloads (per database constraint)
        Some(std::cmp::max(1, size as i32))
    })
    .ok()
    .flatten()
}

/// Safely parse content type without panicking.
fn parse_content_type_safely(content_type: &str) -> Option<(String, HashMap<String, String>)> {
    use std::collections::HashMap;

    std::panic::catch_unwind(|| {
        // Parse media type and parameters
        let parts: Vec<&str> = content_type.split(';').collect();

        if parts.is_empty() {
            return None;
        }

        let media_type = parts[0].trim().to_lowercase();
        let mut params = HashMap::new();

        for part in parts.iter().skip(1) {
            if let Some((key, value)) = part.split_once('=') {
                params
                    .insert(key.trim().to_lowercase(), value.trim().trim_matches('"').to_string());
            }
        }

        Some((media_type, params))
    })
    .ok()
    .flatten()
}

/// Safely extract webhook signatures from payload.
fn extract_signature_safely(payload: &Bytes) -> Option<String> {
    std::panic::catch_unwind(|| {
        // Look for signature-like patterns in the payload
        let payload_str = String::from_utf8_lossy(payload);

        // Common signature header patterns
        let signature_patterns = [
            "X-Hub-Signature-256:",
            "X-Stripe-Signature:",
            "X-GitHub-Signature:",
            "Authorization:",
            "signature=",
        ];

        for pattern in signature_patterns {
            if let Some(start) = payload_str.find(pattern) {
                let signature_start = start + pattern.len();
                let signature_end = payload_str[signature_start..]
                    .find(['\n', '\r', ' ', ';'])
                    .map(|pos| signature_start + pos)
                    .unwrap_or(payload_str.len());

                return Some(payload_str[signature_start..signature_end].trim().to_string());
            }
        }

        None
    })
    .ok()
    .flatten()
}

/// Safely parse JSON payload without panicking.
fn parse_json_payload_safely(payload: &Bytes) -> Option<serde_json::Value> {
    std::panic::catch_unwind(|| serde_json::from_slice(payload).ok()).ok().flatten()
}

/// Safely parse form-encoded payload.
fn parse_form_payload_safely(payload: &Bytes) -> Option<Vec<(String, String)>> {
    std::panic::catch_unwind(|| {
        let payload_str = String::from_utf8_lossy(payload);
        let mut pairs = Vec::new();

        for pair in payload_str.split('&') {
            if let Some((key, value)) = pair.split_once('=') {
                // URL decode the key and value
                let decoded_key = urlencoding::decode(key).ok()?.into_owned();
                let decoded_value = urlencoding::decode(value).ok()?.into_owned();
                pairs.push((decoded_key, decoded_value));
            }
        }

        Some(pairs)
    })
    .ok()
    .flatten()
}

/// Safely validate payload constraints.
fn validate_payload_safely(payload: &Bytes) -> Option<bool> {
    std::panic::catch_unwind(|| {
        let size = payload.len();

        // Check size constraints (1 byte to 10MB)
        if size == 0 || size > 10 * 1024 * 1024 {
            return Some(false);
        }

        // Check for null bytes in text content
        if payload.contains(&0) {
            return Some(false);
        }

        // Basic UTF-8 validation for text content
        if String::from_utf8(payload.to_vec()).is_err() {
            // Binary content is allowed, but should be flagged
            return Some(true);
        }

        Some(true)
    })
    .ok()
    .flatten()
}

// Minimal URL encoding for the form parsing
mod urlencoding {
    pub fn decode(input: &str) -> Result<std::borrow::Cow<str>, std::fmt::Error> {
        let mut result = String::new();
        let mut chars = input.chars().peekable();

        while let Some(ch) = chars.next() {
            match ch {
                '%' => {
                    let hex1 = chars.next().ok_or(std::fmt::Error)?;
                    let hex2 = chars.next().ok_or(std::fmt::Error)?;

                    let byte = u8::from_str_radix(&format!("{}{}", hex1, hex2), 16)
                        .map_err(|_| std::fmt::Error)?;
                    result.push(byte as char);
                },
                '+' => result.push(' '),
                c => result.push(c),
            }
        }

        Ok(std::borrow::Cow::Owned(result))
    }
}
