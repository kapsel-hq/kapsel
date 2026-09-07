    #[tokio::test]
    async fn receipt_statement_retains_exact_available_condition_reason() {
        let path = database_path("receipt-available-reason");
        let request = request();
        let mut gateway = Gateway::open_for_test(&path).unwrap();
        gateway
            .submit_exact_for_test(&request, &authorization(&request))
            .unwrap();
        let mut adapter = failed_adapter(&path, &request);
        adapter.observation.updated_replicas = Some(1);
        adapter.observation.available_replicas = Some(1);
        adapter.observation.unavailable_replicas = Some(0);
        adapter.observation.rollout_condition_type = Some("Available".into());
        adapter.observation.rollout_condition_status = Some("True".into());
        adapter.observation.rollout_condition_reason = Some("DifferentObservedReason".into());
        gateway
            .run_once_with_adapter(&mut adapter, None)
            .await
            .unwrap();

        let statement = gateway
            .journal
            .receipt_statement(&request.operation_id)
            .unwrap()
            .unwrap();
        assert_eq!(statement.result(), OperationResult::Succeeded);
        assert_eq!(
            statement.rollout_condition_reason(),
            Some("DifferentObservedReason")
        );
        drop(gateway);
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[tokio::test]
    async fn receipt_inspection_reports_frozen_failed_receiver_facts() {
        let path = database_path("receipt-first-tracer");
        let request = request();
        let mut gateway = Gateway::open_for_test(&path).unwrap();
        gateway
            .submit_exact_for_test(&request, &authorization(&request))
            .unwrap();
        let mut adapter = failed_adapter(&path, &request);
        assert_eq!(
            gateway
                .run_once_with_adapter(&mut adapter, None)
                .await
                .unwrap(),
            Some(OperationState::ReceiverObserved)
        );

        let statement = gateway
            .journal
            .receipt_statement(&request.operation_id)
            .unwrap()
            .unwrap();
        assert_eq!(statement.operation_id, request.operation_id);
        assert_eq!(statement.authorization_id, "auth-001");
        assert_eq!(
            statement.authorization_signer_key_id(),
            "effect-gateway-authorization-test-key"
        );
        assert_eq!(statement.authorization_grant_digest().len(), 64);
        assert_eq!(statement.write_strategy(), WRITE_STRATEGY);
        assert_eq!(statement.target_uid(), "deployment-uid-1");
        assert_eq!(statement.target_resource_version(), "resource-version-0");
        assert_eq!(statement.receiver_uid(), Some("deployment-uid-1"));
        assert_eq!(
            statement.observed_image(),
            Some(request.immutable_image_digest.as_str())
        );
        assert_eq!(statement.observed_operation_marker(), Some("op-001"));
        assert_eq!(statement.current_generation(), Some(2));
        assert_eq!(statement.requested_generation(), Some(2));
        assert_eq!(statement.observed_generation(), Some(2));
        assert_eq!(statement.desired_replicas(), Some(1));
        assert_eq!(statement.updated_replicas(), Some(0));
        assert_eq!(statement.available_replicas(), Some(0));
        assert_eq!(statement.unavailable_replicas(), Some(1));
        assert_eq!(statement.result, OperationResult::Failed);
        assert_eq!(
            statement.rollout_condition_reason.as_deref(),
            Some("ProgressDeadlineExceeded")
        );

        let seed = [7_u8; 32];
        let signing_key = ed25519_dalek::SigningKey::from_bytes(&seed);
        let trust = ReceiptTrust {
            key_id: "effect-gateway-test-key".into(),
            public_key: signing_key.verifying_key().to_bytes(),
            accepted_purpose: "kapsel.kap0038.kubernetes-effect-receipt.v2".into(),
            not_before_unix_s: 100,
            not_after_unix_s: 200,
        }
        .encode()
        .unwrap();
        let receipt = sign_statement(&statement, &seed, "effect-gateway-test-key").unwrap();
        let report = inspect_receipt(&receipt, &trust, 150, InspectionLimits::default());

        assert_eq!(report.status(), InspectionStatus::Inspected);
        assert_eq!(report.statement(), Some(&statement));
        assert_eq!(report.non_claims(), Some(statement.non_claims()));
        drop(gateway);
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn hostile_receipt_inputs_fail_closed_without_verified_vocabulary() {
        let statement = ReceiptStatement {
            approved_target: None,
            operation_id: "op-001".into(),
            authorization_id: "auth-001".into(),
            authorization_signer_key_id: "effect-gateway-authorization-test-key".into(),
            authorization_grant_digest: "0".repeat(64),
            namespace: "demo".into(),
            deployment: "agent-api".into(),
            container: "api".into(),
            immutable_image_digest: request().immutable_image_digest,
            write_strategy: WRITE_STRATEGY.into(),
            target_uid: "deployment-uid-1".into(),
            target_resource_version: "resource-version-0".into(),
            receiver_uid: Some("deployment-uid-1".into()),
            observed_image: Some(request().immutable_image_digest),
            observed_operation_marker: Some("op-001".into()),
            current_generation: Some(2),
            requested_generation: Some(2),
            observed_generation: Some(2),
            observed_resource_version: Some("resource-version-2".into()),
            desired_replicas: Some(1),
            updated_replicas: Some(0),
            available_replicas: Some(0),
            unavailable_replicas: Some(1),
            rollout_condition_type: Some("Progressing".into()),
            rollout_condition_status: Some("False".into()),
            rollout_condition_reason: Some("ProgressDeadlineExceeded".into()),
            result: OperationResult::Failed,
        };
        let seed = [8_u8; 32];
        let signing_key = ed25519_dalek::SigningKey::from_bytes(&seed);
        let trust = ReceiptTrust {
            key_id: "effect-gateway-test-key".into(),
            public_key: signing_key.verifying_key().to_bytes(),
            accepted_purpose: "kapsel.kap0038.kubernetes-effect-receipt.v2".into(),
            not_before_unix_s: 100,
            not_after_unix_s: 200,
        }
        .encode()
        .unwrap();
        let receipt = sign_statement(&statement, &seed, "effect-gateway-test-key").unwrap();

        let mut malformed = receipt.clone();
        malformed[0] = b'X';
        assert_eq!(
            inspect_receipt(&malformed, &trust, 150, InspectionLimits::default()).status(),
            InspectionStatus::StructureRejected
        );

        let mut bad_signature = receipt.clone();
        let last = bad_signature.last_mut().unwrap();
        *last ^= 1;
        assert_eq!(
            inspect_receipt(&bad_signature, &trust, 150, InspectionLimits::default()).status(),
            InspectionStatus::SignatureRejected
        );

        assert_eq!(
            inspect_receipt(&receipt, &trust, 250, InspectionLimits::default()).status(),
            InspectionStatus::UntrustedSigner
        );
        assert!(
            !format!(
                "{:?}{:?}{:?}{:?}",
                InspectionStatus::StructureRejected,
                InspectionStatus::SignatureRejected,
                InspectionStatus::UntrustedSigner,
                InspectionStatus::Inspected
            )
            .contains("Verified")
        );
    }

    #[tokio::test]
    async fn finalizer_contender_changes_no_durable_or_public_fact() {
        let path = database_path("receipt-finalizer-lock");
        let output_directory = path.parent().unwrap().join("receipts");
        private_directory(&output_directory);
        let output_directory = fs::canonicalize(output_directory).unwrap();
        let seed = [21_u8; 32];
        let request = request();
        let mut first = Gateway::open_for_test(&path).unwrap();
        first
            .submit_exact_for_test(&request, &authorization(&request))
            .unwrap();
        first
            .run_once_with_adapter(&mut failed_adapter(&path, &request), None)
            .await
            .unwrap();
        let worker_lock = first.journal.try_lock_worker().unwrap().unwrap();
        let contender = Gateway::open_for_test(&path).unwrap();

        assert_eq!(
            contender
                .finalize_receipt_once(&ReceiptSettings {
                    signing_seed: &seed,
                    key_id: "effect-gateway-test-key",
                })
                .unwrap(),
            None
        );
        assert_eq!(
            contender.get(&request.operation_id).unwrap(),
            Some(OperationState::ReceiverObserved)
        );
        assert_eq!(fs::read_dir(&output_directory).unwrap().count(), 0);

        drop(worker_lock);
        assert_eq!(
            contender
                .finalize_receipt_once(&ReceiptSettings {
                    signing_seed: &seed,
                    key_id: "effect-gateway-test-key",
                })
                .unwrap(),
            Some(OperationState::Finalized)
        );
        drop(contender);
        drop(first);
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines, reason = "one trace compares stored and retrieved evidence")]
    async fn receipt_commit_freezes_bytes_and_signer_under_acknowledgement_loss() {
        for fault in [
            FaultPoint::BeforeReceiptCommit,
            FaultPoint::FinalizedCommitted,
            FaultPoint::ReceiptCommitAcknowledgementLost,
        ] {
            let path = database_path(&format!("receipt-commit-{fault:?}"));
            let request = request();
            let mut gateway = Gateway::open_for_test(&path).unwrap();
            gateway
                .submit_exact_for_test(&request, &authorization(&request))
                .unwrap();
            let mut adapter = failed_adapter(&path, &request);
            gateway
                .run_once_with_adapter(&mut adapter, None)
                .await
                .unwrap();
            assert_eq!(adapter.apply_calls, 1);
            let statement = gateway
                .journal
                .receipt_statement(&request.operation_id)
                .unwrap()
                .unwrap();
            // Bad signing identity simulates a signer failure before the final transaction.
            assert!(
                gateway
                    .finalize_receipt_once(&ReceiptSettings {
                        signing_seed: &[61; 32],
                        key_id: "invalid key",
                    })
                    .is_err()
            );
            assert_eq!(
                gateway.get(&request.operation_id).unwrap(),
                Some(OperationState::ReceiverObserved)
            );
            assert_eq!(
                gateway
                    .journal
                    .receipt_statement(&request.operation_id)
                    .unwrap()
                    .unwrap(),
                statement
            );
            assert!(matches!(
                gateway.finalize_receipt_once_with_fault(
                    &ReceiptSettings {
                        signing_seed: &[61; 32],
                        key_id: "original-key",
                    },
                    Some(fault)
                ),
                Err(GatewayError::InjectedFault)
            ));
            // The returned error models acknowledgement loss, not an actual SQLite I/O failure.
            let committed = fault != FaultPoint::BeforeReceiptCommit;
            let before: (String, Option<Vec<u8>>, Option<String>, Option<String>) = gateway
                .journal
                .connection
                .query_row(
                    "SELECT state, receipt_bytes, receipt_digest, receipt_key_id
                    FROM kubernetes_image_operations WHERE operation_id = ?1",
                    [&request.operation_id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )
                .unwrap();
            assert_eq!(
                before.0,
                if committed {
                    "finalized"
                } else {
                    "receiver_observed"
                }
            );
            assert_eq!(before.1.is_some(), committed);
            assert_eq!(before.2.is_some(), committed);
            assert_eq!(before.3.is_some(), committed);
            drop(gateway);
            let mut reopened = Gateway::open_for_test(&path).unwrap();
            assert_eq!(
                reopened
                    .run_once_with_adapter(&mut adapter, None)
                    .await
                    .unwrap(),
                None
            );
            let result = reopened
                .finalize_receipt_once(&ReceiptSettings {
                    signing_seed: &[62; 32],
                    key_id: "rotated-key",
                })
                .unwrap();
            assert_eq!(
                result,
                if committed {
                    None
                } else {
                    Some(OperationState::Finalized)
                }
            );
            let (bytes, digest) = Gateway::read_loaded_receipt(
                reopened
                    .journal
                    .operation(&request.operation_id)
                    .unwrap()
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(publication::receipt_digest_hex(&bytes), digest);
            let (key, signed_statement) =
                super::super::receipt::decode_frozen_receipt(&bytes).unwrap();
            assert_eq!(
                key,
                if committed {
                    "original-key"
                } else {
                    "rotated-key"
                }
            );
            assert_eq!(signed_statement, statement);
            if committed {
                assert_eq!(Some(bytes.clone()), before.1);
            }
            for _ in 0..2 {
                assert_eq!(
                    Gateway::read_loaded_receipt(
                        reopened
                            .journal
                            .operation(&request.operation_id)
                            .unwrap()
                            .unwrap()
                    )
                    .unwrap(),
                    (bytes.clone(), digest.clone())
                );
            }
            assert_eq!(adapter.apply_calls, 1);
            assert_eq!(adapter.observe_calls, 1);
            drop(reopened);
            fs::remove_dir_all(path.parent().unwrap()).unwrap();
        }
    }

    #[tokio::test]
    async fn process_exit_before_and_after_receipt_commit_preserves_frozen_observation() {
        for scenario in ["before_receipt_commit", "receipt"] {
            let path = database_path(scenario);
            let request = request();
            let mut gateway = Gateway::open_for_test(&path).unwrap();
            gateway
                .submit_exact_for_test(&request, &authorization(&request))
                .unwrap();
            let mut adapter = failed_adapter(&path, &request);
            gateway
                .run_once_with_adapter(&mut adapter, None)
                .await
                .unwrap();
            let statement = gateway
                .journal
                .receipt_statement(&request.operation_id)
                .unwrap();
            drop(gateway);
            let ready = path.parent().unwrap().join("ready");
            let mut child = spawn_process_child(scenario, &path, &ready, None, None);
            wait_for_child_seam(&mut child, &ready);
            kill_child(&mut child);
            let gateway = Gateway::open_for_test(&path).unwrap();
            let old = gateway
                .journal
                .operation(&request.operation_id)
                .unwrap()
                .unwrap();
            let old_bytes = if scenario == "receipt" {
                Some(Gateway::read_loaded_receipt(old).unwrap().0)
            } else {
                assert_eq!(old.state(), OperationState::ReceiverObserved);
                None
            };
            gateway
                .finalize_receipt_once(&ReceiptSettings {
                    signing_seed: &[63; 32],
                    key_id: "after-restart",
                })
                .unwrap();
            let (bytes, _) = Gateway::read_loaded_receipt(
                gateway
                    .journal
                    .operation(&request.operation_id)
                    .unwrap()
                    .unwrap(),
            )
            .unwrap();
            if let Some(old) = old_bytes {
                assert_eq!(bytes, old);
            }
            assert_eq!(
                gateway
                    .journal
                    .receipt_statement(&request.operation_id)
                    .unwrap(),
                statement
            );
            assert_eq!(adapter.apply_calls, 1);
            assert_eq!(adapter.observe_calls, 1);
            drop(gateway);
            fs::remove_dir_all(path.parent().unwrap()).unwrap();
        }
    }
