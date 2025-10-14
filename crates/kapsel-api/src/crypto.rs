//! Cryptographic utilities for webhook signature validation.
//!
//! This module provides HMAC-SHA256 signature generation and validation
//! for webhook authenticity verification. Supports multiple signature
//! formats used by popular webhook providers.

use std::fmt;

use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// Result of signature validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationResult {
    /// Whether the signature is valid.
    pub is_valid: bool,
    /// Error message if validation failed.
    pub error_message: Option<String>,
}

impl ValidationResult {
    /// Creates a successful validation result.
    pub fn valid() -> Self {
        Self { is_valid: true, error_message: None }
    }

    /// Creates a failed validation result with error message.
    pub fn invalid(message: impl Into<String>) -> Self {
        Self { is_valid: false, error_message: Some(message.into()) }
    }
}

/// Signature validation errors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignatureError {
    /// Missing signature header.
    MissingSignature,
    /// Invalid signature format.
    InvalidFormat(String),
    /// Signature verification failed.
    VerificationFailed,
    /// Invalid secret key.
    InvalidSecret,
}

impl fmt::Display for SignatureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingSignature => write!(f, "signature header missing"),
            Self::InvalidFormat(format) => write!(f, "invalid signature format: {format}"),
            Self::VerificationFailed => write!(f, "signature verification failed"),
            Self::InvalidSecret => write!(f, "invalid secret key"),
        }
    }
}

impl std::error::Error for SignatureError {}

/// Validates webhook signature using HMAC-SHA256.
///
/// Supports multiple signature formats:
/// - GitHub: "sha256=&lt;hex&gt;"
/// - Stripe: "v1=&lt;hex&gt;"
/// - Raw: "&lt;hex&gt;"
///
/// Compares the provided signature against the expected HMAC-SHA256 of the
/// payload using the given secret. Returns validation result with error
/// details if verification fails.
///
/// # Example
///
/// ```
/// use kapsel_api::crypto::validate_signature;
///
/// let payload = b"webhook payload";
/// let signature = "sha256=abc123...";
/// let secret = "my_secret_key";
///
/// let result = validate_signature(payload, signature, secret);
/// assert!(result.is_valid);
/// ```
pub fn validate_signature(payload: &[u8], signature: &str, secret: &str) -> ValidationResult {
    if signature.is_empty() {
        return ValidationResult::invalid("signature header is empty");
    }

    if secret.is_empty() {
        return ValidationResult::invalid("secret key is empty");
    }

    // Parse signature format
    let hex_signature = match parse_signature_format(signature) {
        Ok(hex) => hex,
        Err(err) => return ValidationResult::invalid(err.to_string()),
    };

    // Generate expected signature
    let expected_signature = match generate_hmac_hex(payload, secret) {
        Ok(sig) => sig,
        Err(err) => return ValidationResult::invalid(err.to_string()),
    };

    // Timing-safe comparison
    if timing_safe_eq(&hex_signature, &expected_signature) {
        ValidationResult::valid()
    } else {
        ValidationResult::invalid("signature mismatch")
    }
}

/// Generates HMAC-SHA256 signature as hex string.
///
/// Creates an HMAC-SHA256 hash of the provided payload using the secret key
/// and returns it as a lowercase hexadecimal string. This is the expected
/// signature format for webhook validation.
///
/// # Errors
///
/// Returns `SignatureError::InvalidSecret` if the secret key is invalid.
pub fn generate_hmac_hex(payload: &[u8], secret: &str) -> Result<String, SignatureError> {
    let mut mac =
        HmacSha256::new_from_slice(secret.as_bytes()).map_err(|_| SignatureError::InvalidSecret)?;

    mac.update(payload);
    let result = mac.finalize();
    Ok(hex::encode(result.into_bytes()))
}

/// Parses signature from various formats to raw hex.
///
/// Supported formats:
/// - "sha256=<hex>" (GitHub style)
/// - "v1=<hex>" (Stripe style)
/// - "<hex>" (raw hex)
fn parse_signature_format(signature: &str) -> Result<String, SignatureError> {
    // GitHub format: "sha256=<hex>"
    if let Some(hex) = signature.strip_prefix("sha256=") {
        return Ok(hex.to_string());
    }

    // Stripe format: "v1=<hex>"
    if let Some(hex) = signature.strip_prefix("v1=") {
        return Ok(hex.to_string());
    }

    // Check if it's valid hex (raw format)
    if signature.chars().all(|c| c.is_ascii_hexdigit()) && signature.len() == 64 {
        return Ok(signature.to_string());
    }

    Err(SignatureError::InvalidFormat(format!(
        "expected 'sha256=<hex>', 'v1=<hex>', or raw hex, got: {signature}",
    )))
}

/// Timing-safe string comparison to prevent timing attacks.
///
/// Uses constant-time comparison to avoid leaking information
/// about the expected signature through timing analysis.
fn timing_safe_eq(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }

    let a_bytes = a.as_bytes();
    let b_bytes = b.as_bytes();

    let mut result = 0u8;
    for (a_byte, b_byte) in a_bytes.iter().zip(b_bytes.iter()) {
        result |= a_byte ^ b_byte;
    }

    result == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_signature_success() {
        let payload = b"test payload";
        let secret = "test_secret";

        // Generate expected signature
        let expected = generate_hmac_hex(payload, secret).unwrap();
        let signature = format!("sha256={expected}");

        let result = validate_signature(payload, &signature, secret);
        assert!(result.is_valid);
        assert!(result.error_message.is_none());
    }

    #[test]
    fn validate_signature_invalid() {
        let payload = b"test payload";
        let signature = "sha256=invalid_signature_here_definitely_wrong_length_and_content";
        let secret = "test_secret";

        let result = validate_signature(payload, signature, secret);
        assert!(!result.is_valid);
        assert!(result.error_message.is_some());
    }

    #[test]
    fn validate_signature_missing() {
        let payload = b"test payload";
        let signature = "";
        let secret = "test_secret";

        let result = validate_signature(payload, signature, secret);
        assert!(!result.is_valid);
        assert_eq!(result.error_message.unwrap(), "signature header is empty");
    }

    #[test]
    fn parse_signature_format_github() {
        let signature = "sha256=abc123";
        let result = parse_signature_format(signature).unwrap();
        assert_eq!(result, "abc123");
    }

    #[test]
    fn parse_signature_format_stripe() {
        let signature = "v1=def456";
        let result = parse_signature_format(signature).unwrap();
        assert_eq!(result, "def456");
    }

    #[test]
    fn parse_signature_format_raw_hex() {
        let signature = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let result = parse_signature_format(signature).unwrap();
        assert_eq!(result, signature);
    }

    #[test]
    fn parse_signature_format_invalid() {
        let signature = "invalid_format";
        let result = parse_signature_format(signature);
        assert!(result.is_err());
    }

    #[test]
    fn timing_safe_eq_same() {
        assert!(timing_safe_eq("hello", "hello"));
    }

    #[test]
    fn timing_safe_eq_different() {
        assert!(!timing_safe_eq("hello", "world"));
    }

    #[test]
    fn timing_safe_eq_different_length() {
        assert!(!timing_safe_eq("hello", "hello_world"));
    }

    #[test]
    fn generate_hmac_hex_consistent() {
        let payload = b"test payload";
        let secret = "secret";

        let sig1 = generate_hmac_hex(payload, secret).unwrap();
        let sig2 = generate_hmac_hex(payload, secret).unwrap();

        assert_eq!(sig1, sig2);
        assert_eq!(sig1.len(), 64); // SHA256 hex is 64 chars
    }
}
