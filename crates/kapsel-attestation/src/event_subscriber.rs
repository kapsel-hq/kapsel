//! Event subscriber for delivery-to-attestation integration.
//!
//! Bridges delivery system events with Merkle tree attestation by converting
//! successful delivery events into cryptographic audit trail leaves.

use std::sync::Arc;

use kapsel_core::{DeliveryEvent, DeliveryFailedEvent, DeliverySucceededEvent, EventHandler};
use tokio::sync::RwLock;
use tracing::{debug, error};

use crate::{LeafData, MerkleService, Result};

/// Attestation event subscriber that converts delivery events into merkle
/// leaves.
///
/// This service implements `EventHandler` to receive delivery events from
/// the delivery system and creates attestation leaves for successful
/// deliveries. Failed deliveries are logged but do not generate attestation
/// leaves.
#[derive(Debug)]
pub struct AttestationEventSubscriber {
    merkle_service: Arc<RwLock<MerkleService>>,
}

impl AttestationEventSubscriber {
    /// Creates a new attestation event subscriber.
    pub fn new(merkle_service: Arc<RwLock<MerkleService>>) -> Self {
        Self { merkle_service }
    }

    /// Handles a successful delivery event by creating an attestation leaf.
    async fn handle_delivery_success(&self, event: DeliverySucceededEvent) {
        let leaf_data = match self.create_leaf_data_from_success(&event) {
            Ok(leaf) => leaf,
            Err(e) => {
                error!(
                    event_id = %event.event_id,
                    delivery_attempt_id = %event.delivery_attempt_id,
                    error = %e,
                    "failed to create attestation leaf data from delivery success"
                );
                return;
            },
        };

        let mut service = self.merkle_service.write().await;
        if let Err(e) = service.add_leaf(leaf_data).await {
            error!(
                event_id = %event.event_id,
                delivery_attempt_id = %event.delivery_attempt_id,
                error = %e,
                "failed to add attestation leaf for successful delivery"
            );
        } else {
            debug!(
                event_id = %event.event_id,
                delivery_attempt_id = %event.delivery_attempt_id,
                attempt_number = event.attempt_number,
                endpoint_url = %event.endpoint_url,
                "attestation leaf added for successful delivery"
            );
        }
    }

    /// Handles a failed delivery event by logging it.
    ///
    /// Failed deliveries do not generate attestation leaves since we only
    /// attest to successful deliveries. However, we log the failure for
    /// operational visibility.
    async fn handle_delivery_failure(&self, event: DeliveryFailedEvent) {
        debug!(
            event_id = %event.event_id,
            delivery_attempt_id = %event.delivery_attempt_id,
            attempt_number = event.attempt_number,
            endpoint_url = %event.endpoint_url,
            error = %event.error_message,
            is_retryable = event.is_retryable,
            "delivery failure recorded (no attestation leaf created)"
        );
    }

    /// Creates attestation leaf data from a successful delivery event.
    fn create_leaf_data_from_success(&self, event: &DeliverySucceededEvent) -> Result<LeafData> {
        LeafData::new(
            event.delivery_attempt_id,
            event.event_id.0,
            event.endpoint_url.clone(),
            event.payload_hash,
            event.attempt_number as i32,
            Some(event.response_status),
            event.delivered_at,
        )
    }
}

#[async_trait::async_trait]
impl EventHandler for AttestationEventSubscriber {
    async fn handle_event(&self, event: DeliveryEvent) {
        match event {
            DeliveryEvent::Succeeded(success_event) => {
                self.handle_delivery_success(success_event).await;
            },
            DeliveryEvent::Failed(failure_event) => {
                self.handle_delivery_failure(failure_event).await;
            },
            DeliveryEvent::AttemptStarted(_) => {
                // We don't create attestation leaves for delivery attempts,
                // only for completed deliveries (success or failure)
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use chrono::Utc;
    use kapsel_core::{
        models::{EventId, TenantId},
        DeliveryEvent, DeliveryFailedEvent, DeliverySucceededEvent,
    };
    use kapsel_testing::TestEnv;
    use tokio::sync::RwLock;
    use uuid::Uuid;

    use super::*;
    use crate::{MerkleService, SigningService};

    #[tokio::test]
    async fn successful_delivery_creates_attestation_leaf() {
        let env = TestEnv::new().await.expect("test environment setup failed");
        let signing_service = SigningService::ephemeral();
        let merkle_service =
            Arc::new(RwLock::new(MerkleService::new(env.pool().clone(), signing_service)));
        let subscriber = AttestationEventSubscriber::new(merkle_service.clone());

        let success_event = create_test_delivery_success_event();
        subscriber.handle_event(DeliveryEvent::Succeeded(success_event)).await;

        // Verify attestation leaf was created
        let service = merkle_service.read().await;
        assert_eq!(service.pending_count().await.expect("failed to get pending count"), 1);
    }

    #[tokio::test]
    async fn failed_delivery_does_not_create_attestation_leaf() {
        let env = TestEnv::new().await.expect("test environment setup failed");
        let signing_service = SigningService::ephemeral();
        let merkle_service =
            Arc::new(RwLock::new(MerkleService::new(env.pool().clone(), signing_service)));
        let subscriber = AttestationEventSubscriber::new(merkle_service.clone());

        let failure_event = create_test_delivery_failure_event();
        subscriber.handle_event(DeliveryEvent::Failed(failure_event)).await;

        // Verify no attestation leaf was created for failure
        let service = merkle_service.read().await;
        assert_eq!(service.pending_count().await.expect("failed to get pending count"), 0);
    }

    #[tokio::test]
    async fn handles_multiple_success_events() {
        let env = TestEnv::new().await.expect("test environment setup failed");
        let signing_service = SigningService::ephemeral();
        let merkle_service =
            Arc::new(RwLock::new(MerkleService::new(env.pool().clone(), signing_service)));
        let subscriber = AttestationEventSubscriber::new(merkle_service.clone());

        // Handle multiple success events
        for _ in 0..3 {
            let success_event = create_test_delivery_success_event();
            subscriber.handle_event(DeliveryEvent::Succeeded(success_event)).await;
        }

        // Verify all attestation leaves were created
        let service = merkle_service.read().await;
        assert_eq!(service.pending_count().await.expect("failed to get pending count"), 3);
    }

    fn create_test_delivery_success_event() -> DeliverySucceededEvent {
        DeliverySucceededEvent {
            delivery_attempt_id: Uuid::new_v4(),
            event_id: EventId::new(),
            tenant_id: TenantId::new(),
            endpoint_url: "https://example.com/webhook".to_string(),
            response_status: 200,
            attempt_number: 1,
            delivered_at: Utc::now(),
            payload_hash: [1u8; 32], // Use different hash to avoid duplicates
            payload_size: 1024,
        }
    }

    fn create_test_delivery_failure_event() -> DeliveryFailedEvent {
        DeliveryFailedEvent {
            delivery_attempt_id: Uuid::new_v4(),
            event_id: EventId::new(),
            tenant_id: TenantId::new(),
            endpoint_url: "https://example.com/webhook".to_string(),
            response_status: Some(500),
            attempt_number: 1,
            failed_at: Utc::now(),
            error_message: "Internal server error".to_string(),
            is_retryable: true,
        }
    }
}
