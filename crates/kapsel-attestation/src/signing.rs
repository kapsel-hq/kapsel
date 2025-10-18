//! Ed25519 cryptographic signing and verification.
//!
//! Provides deterministic signing of Merkle tree heads and signature
//! verification for tamper-evident audit trails.

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use rand::rngs::OsRng;

use crate::error::AttestationError;

/// Ed25519 signing service for attestation operations.
///
/// Provides deterministic signing of Merkle tree heads and leaf data with
/// cryptographic verification capabilities. Uses Ed25519 for fast signature
/// generation and verification.
#[derive(Debug)]
pub struct SigningService {
    signing_key: SigningKey,
    verifying_key: VerifyingKey,
    key_id: uuid::Uuid,
}

impl SigningService {
    /// Create an ephemeral signing service with a fresh Ed25519 keypair.
    ///
    /// Generates a new random keypair suitable for testing and development.
    /// For production use, keys should be loaded from secure storage.
    pub fn ephemeral() -> Self {
        let signing_key = SigningKey::generate(&mut OsRng);
        let verifying_key = signing_key.verifying_key();
        let key_id = Self::compute_key_id(&verifying_key);

        Self { signing_key, verifying_key, key_id }
    }

    /// Create signing service from existing key bytes.
    ///
    /// # Arguments
    /// * `private_key_bytes` - Ed25519 private key (32 bytes)
    ///
    /// # Errors
    /// Returns `AttestationError::InvalidKeyFormat` if key bytes are invalid.
    pub fn try_from_bytes(private_key_bytes: &[u8; 32]) -> Result<Self, AttestationError> {
        let signing_key = SigningKey::from_bytes(private_key_bytes);
        let verifying_key = signing_key.verifying_key();
        let key_id = Self::compute_key_id(&verifying_key);

        Ok(Self { signing_key, verifying_key, key_id })
    }

    /// Sign a Merkle tree head following RFC 6962 format.
    ///
    /// Creates a deterministic Ed25519 signature over the concatenated tree
    /// state: tree_size || root_hash || timestamp_ms
    ///
    /// # Arguments
    /// * `root_hash` - Merkle tree root hash (32 bytes)
    /// * `tree_size` - Number of leaves in the tree
    /// * `timestamp_ms` - Unix timestamp in milliseconds
    ///
    /// # Returns
    /// Ed25519 signature bytes (64 bytes)
    pub fn sign_tree_head(
        &self,
        root_hash: &[u8; 32],
        tree_size: i64,
        timestamp_ms: i64,
    ) -> Result<Vec<u8>, AttestationError> {
        let message = self.encode_tree_head(root_hash, tree_size, timestamp_ms);
        let signature = self.signing_key.sign(&message);
        Ok(signature.to_bytes().to_vec())
    }

    /// Verify a tree head signature.
    ///
    /// # Arguments
    /// * `root_hash` - Expected Merkle tree root hash (32 bytes)
    /// * `tree_size` - Expected tree size
    /// * `timestamp_ms` - Expected timestamp
    /// * `signature_bytes` - Signature to verify (64 bytes)
    ///
    /// # Returns
    /// `true` if signature is valid, `false` otherwise
    ///
    /// # Errors
    /// Returns `AttestationError::InvalidSignature` if signature format is
    /// invalid.
    pub fn verify_tree_head(
        &self,
        root_hash: &[u8; 32],
        tree_size: i64,
        timestamp_ms: i64,
        signature_bytes: &[u8],
    ) -> Result<bool, AttestationError> {
        let signature = Signature::from_slice(signature_bytes)
            .map_err(|_| AttestationError::InvalidSignature)?;

        let message = self.encode_tree_head(root_hash, tree_size, timestamp_ms);

        match self.verifying_key.verify(&message, &signature) {
            Ok(()) => Ok(true),
            Err(_) => Ok(false),
        }
    }

    /// Returns the public key as bytes.
    ///
    /// Returns the Ed25519 public key in canonical 32-byte format.
    pub fn public_key_as_bytes(&self) -> Vec<u8> {
        self.verifying_key.to_bytes().to_vec()
    }

    /// Returns the key identifier.
    ///
    /// Returns a deterministic identifier derived from the public key.
    pub fn key_id(&self) -> uuid::Uuid {
        self.key_id
    }

    /// Update the key identifier to match database-generated ID.
    ///
    /// This method allows setting the key ID to the UUID returned by the
    /// database when inserting into the attestation_keys table, since the
    /// database generates its own primary key.
    ///
    /// # Arguments
    /// * `key_id` - Database-generated UUID for this key
    pub fn with_key_id(mut self, key_id: uuid::Uuid) -> Self {
        self.key_id = key_id;
        self
    }

    /// Sign leaf data for Merkle tree inclusion.
    ///
    /// Creates a signature over delivery attempt data for audit trail purposes.
    ///
    /// # Arguments
    /// * `leaf_hash` - RFC 6962 leaf hash (32 bytes)
    /// * `delivery_attempt_id` - Unique delivery attempt identifier
    /// * `event_id` - Source webhook event identifier
    pub fn sign_leaf(
        &self,
        leaf_hash: &[u8; 32],
        delivery_attempt_id: uuid::Uuid,
        event_id: uuid::Uuid,
    ) -> Vec<u8> {
        let message = self.encode_leaf_payload(leaf_hash, delivery_attempt_id, event_id);
        let signature = self.signing_key.sign(&message);
        signature.to_bytes().to_vec()
    }

    /// Encode tree head payload for signing.
    ///
    /// Creates the canonical byte representation following RFC 6962:
    /// tree_size (8 bytes, big-endian) || root_hash (32 bytes) || timestamp_ms
    /// (8 bytes, big-endian)
    fn encode_tree_head(&self, root_hash: &[u8; 32], tree_size: i64, timestamp_ms: i64) -> Vec<u8> {
        let mut message = Vec::with_capacity(8 + 32 + 8);
        message.extend_from_slice(&tree_size.to_be_bytes());
        message.extend_from_slice(root_hash);
        message.extend_from_slice(&timestamp_ms.to_be_bytes());
        message
    }

    /// Encode leaf payload for signing.
    ///
    /// Creates byte representation for leaf signatures:
    /// leaf_hash (32 bytes) || delivery_attempt_id (16 bytes) || event_id (16
    /// bytes)
    fn encode_leaf_payload(
        &self,
        leaf_hash: &[u8; 32],
        delivery_attempt_id: uuid::Uuid,
        event_id: uuid::Uuid,
    ) -> Vec<u8> {
        let mut message = Vec::with_capacity(32 + 16 + 16);
        message.extend_from_slice(leaf_hash);
        message.extend_from_slice(delivery_attempt_id.as_bytes());
        message.extend_from_slice(event_id.as_bytes());
        message
    }

    /// Compute deterministic key identifier from public key.
    ///
    /// Uses SHA256 hash of public key bytes to create a stable UUID identifier.
    fn compute_key_id(verifying_key: &VerifyingKey) -> uuid::Uuid {
        use sha2::{Digest, Sha256};
        let hash = Sha256::digest(verifying_key.to_bytes());
        // Use first 16 bytes of hash to create UUID
        let mut uuid_bytes = [0u8; 16];
        uuid_bytes.copy_from_slice(&hash[..16]);
        uuid::Uuid::from_bytes(uuid_bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_valid_ed25519_keypair() {
        let service = SigningService::ephemeral();
        let bytes = service.public_key_as_bytes();

        assert_eq!(bytes.len(), 32, "Ed25519 public key is 32 bytes");
    }

    #[test]
    fn signs_tree_head_deterministically() {
        let service = SigningService::ephemeral();
        let root_hash = [0u8; 32];
        let tree_size = 100i64;
        let timestamp_ms = 1234567890000i64;

        let sig1 = service.sign_tree_head(&root_hash, tree_size, timestamp_ms).unwrap();
        let sig2 = service.sign_tree_head(&root_hash, tree_size, timestamp_ms).unwrap();

        assert_eq!(sig1, sig2, "Signatures must be deterministic");
        assert_eq!(sig1.len(), 64, "Ed25519 signature is 64 bytes");
    }

    #[test]
    fn signature_verifies_with_public_key() {
        let service = SigningService::ephemeral();
        let root_hash = [1u8; 32];
        let tree_size = 42i64;
        let timestamp_ms = 9876543210000i64;

        let signature = service.sign_tree_head(&root_hash, tree_size, timestamp_ms).unwrap();
        let is_valid =
            service.verify_tree_head(&root_hash, tree_size, timestamp_ms, &signature).unwrap();

        assert!(is_valid, "Signature must verify");
    }

    #[test]
    fn signature_fails_with_tampered_data() {
        let service = SigningService::ephemeral();
        let root_hash = [2u8; 32];
        let tree_size = 100i64;
        let timestamp_ms = 1111111111111i64;

        let signature = service.sign_tree_head(&root_hash, tree_size, timestamp_ms).unwrap();

        // Tamper with root hash
        let tampered_root = [3u8; 32];
        let is_valid =
            service.verify_tree_head(&tampered_root, tree_size, timestamp_ms, &signature).unwrap();

        assert!(!is_valid, "Tampered data must fail verification");
    }

    #[test]
    fn key_id_is_deterministic() {
        let key_bytes = [1u8; 32];
        let service1 = SigningService::try_from_bytes(&key_bytes).unwrap();
        let service2 = SigningService::try_from_bytes(&key_bytes).unwrap();

        assert_eq!(service1.key_id(), service2.key_id(), "Key ID must be deterministic");
        assert!(!service1.key_id().is_nil(), "Key ID must not be nil");
    }

    #[test]
    fn leaf_signing_produces_valid_signature() {
        let service = SigningService::ephemeral();
        let leaf_hash = [0x42u8; 32];
        let delivery_attempt_id = uuid::Uuid::new_v4();
        let event_id = uuid::Uuid::new_v4();

        let signature = service.sign_leaf(&leaf_hash, delivery_attempt_id, event_id);

        assert_eq!(signature.len(), 64, "Leaf signature must be 64 bytes");
    }
}
