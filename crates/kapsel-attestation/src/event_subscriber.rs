//! Event subscriber for delivery-to-attestation integration.
//!
//! Bridges delivery system events with Merkle tree attestation by converting
//! successful delivery events into cryptographic audit trail leaves.

use std::sync::Arc;

use kapsel_core::{DeliveryEvent, DeliveryFailedEvent, DeliverySucceededEvent, EventHandler};
use tokio::sync::RwLock;
use tracing::{debug, error};

use crate::{AttestationError, LeafData, MerkleService, Result};

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
        let leaf_data = match Self::create_leaf_data_from_success(&event) {
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
        if let Err(e) = service.add_leaf(leaf_data) {
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
    fn handle_delivery_failure(event: &DeliveryFailedEvent) {
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
    fn create_leaf_data_from_success(event: &DeliverySucceededEvent) -> Result<LeafData> {
        LeafData::new(
            event.delivery_attempt_id,
            event.event_id.0,
            event.endpoint_url.clone(),
            event.payload_hash,
            i32::try_from(event.attempt_number).map_err(|_| AttestationError::InvalidTreeSize {
                tree_size: i64::from(event.attempt_number),
            })?,
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
                Self::handle_delivery_failure(&failure_event);
            },
            DeliveryEvent::AttemptStarted(_) => {
                // We don't create attestation leaves for delivery attempts,
                // only for completed deliveries (success or failure)
            },
        }
    }
}
