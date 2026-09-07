    #[test]
    fn requested_recovery_rechecks_exact_authorization_before_advancing() {
        let path = database_path("requested-recovery");
        let request = request();
        let authorization = authorization(&request);
        {
            let gateway = Gateway::open_for_test(&path).unwrap();
            assert!(matches!(
                gateway.submit_exact_with_fault_for_test(
                    &request,
                    &authorization,
                    Some(FaultPoint::RequestedCommitted)
                ),
                Err(GatewayError::InjectedFault)
            ));
            assert_eq!(
                gateway.get(&request.operation_id).unwrap(),
                Some(OperationState::Requested)
            );
            assert!(matches!(
                gateway.journal.operation(&request.operation_id).unwrap(),
                Some(journal::LoadedOperation::Requested(_))
            ));
        }
        let gateway = Gateway::open_for_test(&path).unwrap();
        let mut mismatch = authorization.clone();
        mismatch.container = "other".into();
        assert!(matches!(
            gateway.submit_exact_for_test(&request, &mismatch),
            Err(GatewayError::AuthorizationMismatch)
        ));
        assert_eq!(
            gateway.get(&request.operation_id).unwrap(),
            Some(OperationState::Requested)
        );
        assert_eq!(
            gateway
                .submit_exact_for_test(&request, &authorization)
                .unwrap(),
            SubmissionResult::Created
        );
        assert_eq!(
            gateway.get(&request.operation_id).unwrap(),
            Some(OperationState::Authorized)
        );
        drop(gateway);
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[tokio::test]
    async fn authorized_commit_reopens_and_begins_exactly_one_apply() {
        let path = database_path("authorized-recovery");
        let request = request();
        {
            let gateway = Gateway::open_for_test(&path).unwrap();
            assert!(matches!(
                gateway.submit_exact_with_fault_for_test(
                    &request,
                    &authorization(&request),
                    Some(FaultPoint::AuthorizedCommitted)
                ),
                Err(GatewayError::InjectedFault)
            ));
            assert_eq!(
                gateway.get(&request.operation_id).unwrap(),
                Some(OperationState::Authorized)
            );
        }
        let mut gateway = Gateway::open_for_test(&path).unwrap();
        let mut adapter = failed_adapter(&path, &request);
        assert_eq!(
            gateway
                .run_once_with_adapter(&mut adapter, None)
                .await
                .unwrap(),
            Some(OperationState::ReceiverObserved)
        );
        assert_eq!(adapter.identify_calls, 1);
        assert_eq!(adapter.apply_calls, 1);
        assert_eq!(adapter.observe_calls, 1);
        drop(gateway);
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[tokio::test]
    async fn production_writers_reload_as_their_exact_next_phase() {
        let path = database_path("writer-decoder-symmetry");
        let request = request();
        let mut gateway = Gateway::open_for_test(&path).unwrap();
        gateway
            .submit_exact_for_test(&request, &authorization(&request))
            .unwrap();
        assert!(matches!(
            gateway.journal.operation(&request.operation_id).unwrap(),
            Some(journal::LoadedOperation::Authorized(_))
        ));

        let mut adapter = failed_adapter(&path, &request);
        assert!(matches!(
            gateway
                .run_operation_once_with_adapter_and_fault(
                    &request.operation_id,
                    &mut adapter,
                    Some(FaultPoint::ApplyStartedCommitted),
                )
                .await,
            Err(GatewayError::InjectedFault)
        ));
        assert!(matches!(
            gateway.journal.operation(&request.operation_id).unwrap(),
            Some(journal::LoadedOperation::ApplyStarted(_))
        ));
        assert_eq!(adapter.apply_calls, 0);

        assert_eq!(
            gateway
                .run_operation_once_with_adapter(&request.operation_id, &mut adapter)
                .await
                .unwrap(),
            Some(OperationState::ReceiverObserved)
        );
        assert!(matches!(
            gateway.journal.operation(&request.operation_id).unwrap(),
            Some(journal::LoadedOperation::ReceiverObserved(_))
        ));
        assert_eq!(adapter.apply_calls, 0);

        let settings = ReceiptSettings {
            signing_seed: &[51_u8; 32],
            key_id: "symmetry-receipt-key",
        };
        assert!(matches!(
            gateway.finalize_operation_receipt_once_with_fault(
                &request.operation_id,
                &settings,
                Some(FaultPoint::BeforeReceiptCommit),
            ),
            Err(GatewayError::InjectedFault)
        ));
        assert!(matches!(
            gateway.journal.operation(&request.operation_id).unwrap(),
            Some(journal::LoadedOperation::ReceiverObserved(_))
        ));

        assert!(matches!(
            gateway.finalize_operation_receipt_once_with_fault(
                &request.operation_id,
                &settings,
                Some(FaultPoint::FinalizedCommitted),
            ),
            Err(GatewayError::InjectedFault)
        ));
        assert!(matches!(
            gateway.journal.operation(&request.operation_id).unwrap(),
            Some(journal::LoadedOperation::Finalized(_))
        ));

        drop(gateway);
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[tokio::test]
    async fn apply_outcome_writer_reloads_as_apply_started_with_complete_response_facts() {
        let path = database_path("apply-outcome-writer-decoder-symmetry");
        let request = request();
        let mut gateway = Gateway::open_for_test(&path).unwrap();
        gateway
            .submit_exact_for_test(&request, &authorization(&request))
            .unwrap();
        let mut adapter = failed_adapter(&path, &request);

        assert!(matches!(
            gateway
                .run_operation_once_with_adapter_and_fault(
                    &request.operation_id,
                    &mut adapter,
                    Some(FaultPoint::ApplyOutcomeCommitted),
                )
                .await,
            Err(GatewayError::InjectedFault)
        ));
        assert!(matches!(
            gateway.journal.operation(&request.operation_id).unwrap(),
            Some(journal::LoadedOperation::ApplyStarted(_))
        ));
        assert_eq!(adapter.apply_calls, 1);

        drop(gateway);
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[tokio::test]
    async fn permanent_target_rejection_is_terminal_and_does_not_block_later_operations() {
        let path = database_path("permanent-target-rejection");
        let mut rejected = request();
        rejected.operation_id = "op-a".into();
        let mut later = request();
        later.operation_id = "op-b".into();
        {
            let gateway = Gateway::open_for_test(&path).unwrap();
            gateway
                .submit_exact_for_test(&rejected, &authorization(&rejected))
                .unwrap();
            gateway
                .submit_exact_for_test(&later, &authorization(&later))
                .unwrap();
        }
        let mut adapter = TargetRoutingAdapter::permanent(
            &rejected.operation_id,
            TargetRejection::ContainerNotFound,
        );
        {
            let mut gateway = Gateway::open_for_test(&path).unwrap();
            assert!(matches!(
                gateway
                    .run_once_with_adapter(&mut adapter, Some(FaultPoint::TargetRejectedCommitted),)
                    .await,
                Err(GatewayError::InjectedFault)
            ));
            assert_eq!(
                gateway.get(&rejected.operation_id).unwrap(),
                Some(OperationState::NotAttempted)
            );
            assert_eq!(
                gateway.target_rejection(&rejected.operation_id).unwrap(),
                Some(TargetRejection::ContainerNotFound)
            );
            assert!(matches!(
                gateway.journal.operation(&rejected.operation_id).unwrap(),
                Some(journal::LoadedOperation::NotAttempted(_))
            ));
            assert_eq!(gateway.result(&rejected.operation_id).unwrap(), None);
            assert_eq!(
                gateway.receipt_reference(&rejected.operation_id).unwrap(),
                None
            );
        }
        let mut gateway = Gateway::open_for_test(&path).unwrap();
        assert_eq!(
            gateway
                .run_once_with_adapter(&mut adapter, None)
                .await
                .unwrap(),
            Some(OperationState::ReceiverObserved)
        );
        assert_eq!(adapter.identify_order, ["op-a", "op-b"]);
        assert_eq!(adapter.apply_order, ["op-b"]);
        assert_eq!(adapter.observe_order, ["op-b"]);
        assert_eq!(
            gateway.result(&later.operation_id).unwrap(),
            Some(OperationResult::Failed)
        );
        drop(gateway);
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[tokio::test]
    async fn transient_target_error_defers_fairly_without_head_of_line_blocking() {
        let path = database_path("transient-target-deferral");
        let mut deferred = request();
        deferred.operation_id = "op-a".into();
        let mut later = request();
        later.operation_id = "op-b".into();
        let mut gateway = Gateway::open_for_test(&path).unwrap();
        gateway
            .submit_exact_for_test(&deferred, &authorization(&deferred))
            .unwrap();
        gateway
            .submit_exact_for_test(&later, &authorization(&later))
            .unwrap();
        let mut adapter = TargetRoutingAdapter::transient_once(&deferred.operation_id);

        assert!(matches!(
            gateway.run_once_with_adapter(&mut adapter, None).await,
            Err(GatewayError::KubernetesTargetObservation)
        ));
        assert_eq!(
            gateway.get(&deferred.operation_id).unwrap(),
            Some(OperationState::Authorized)
        );
        assert_eq!(
            gateway
                .run_once_with_adapter(&mut adapter, None)
                .await
                .unwrap(),
            Some(OperationState::ReceiverObserved)
        );
        assert_eq!(
            gateway
                .run_once_with_adapter(&mut adapter, None)
                .await
                .unwrap(),
            Some(OperationState::ReceiverObserved)
        );
        assert_eq!(adapter.identify_order, ["op-a", "op-b", "op-a"]);
        assert_eq!(adapter.apply_order, ["op-b", "op-a"]);
        assert_eq!(adapter.observe_order, ["op-b", "op-a"]);
        drop(gateway);
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[tokio::test]
    async fn targeted_application_reconciliation_does_not_advance_another_operation() {
        let path = database_path("targeted-application-operation");
        let mut first = request();
        first.operation_id = "op-a".into();
        let mut configured = request();
        configured.operation_id = "op-b".into();
        let mut gateway = Gateway::open_for_test(&path).unwrap();
        gateway
            .submit_exact_for_test(&first, &authorization(&first))
            .unwrap();
        gateway
            .submit_exact_for_test(&configured, &authorization(&configured))
            .unwrap();
        let mut adapter = TargetRoutingAdapter::transient_once("never-transient");

        assert_eq!(
            gateway
                .run_operation_once_with_adapter(&configured.operation_id, &mut adapter)
                .await
                .unwrap(),
            Some(OperationState::ReceiverObserved)
        );
        assert_eq!(adapter.identify_order, ["op-b"]);
        assert_eq!(adapter.apply_order, ["op-b"]);
        assert_eq!(adapter.observe_order, ["op-b"]);
        assert_eq!(
            gateway.get(&first.operation_id).unwrap(),
            Some(OperationState::Authorized)
        );
        drop(gateway);
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[tokio::test]
    async fn targeted_application_finalization_does_not_sign_another_operation() {
        let path = database_path("targeted-application-finalization");
        let mut first = request();
        first.operation_id = "op-a".into();
        let mut configured = request();
        configured.operation_id = "op-b".into();
        let mut gateway = Gateway::open_for_test(&path).unwrap();
        gateway
            .submit_exact_for_test(&first, &authorization(&first))
            .unwrap();
        gateway
            .submit_exact_for_test(&configured, &authorization(&configured))
            .unwrap();
        gateway
            .run_operation_once_with_adapter(
                &first.operation_id,
                &mut failed_adapter(&path, &first),
            )
            .await
            .unwrap();
        gateway
            .run_operation_once_with_adapter(
                &configured.operation_id,
                &mut failed_adapter(&path, &configured),
            )
            .await
            .unwrap();

        let receipt_settings = ReceiptSettings {
            signing_seed: &[51_u8; 32],
            key_id: "targeted-receipt-key",
        };
        assert!(matches!(
            gateway.finalize_operation_receipt_once_with_fault(
                &configured.operation_id,
                &receipt_settings,
                Some(FaultPoint::BeforeReceiptCommit),
            ),
            Err(GatewayError::InjectedFault)
        ));
        assert_eq!(
            gateway.get(&first.operation_id).unwrap(),
            Some(OperationState::ReceiverObserved)
        );
        assert_eq!(
            gateway.get(&configured.operation_id).unwrap(),
            Some(OperationState::ReceiverObserved)
        );
        assert_eq!(
            gateway
                .finalize_operation_receipt_once(&configured.operation_id, &receipt_settings)
                .unwrap(),
            Some(OperationState::Finalized)
        );
        assert_eq!(
            gateway.get(&first.operation_id).unwrap(),
            Some(OperationState::ReceiverObserved)
        );
        assert_eq!(
            gateway.get(&configured.operation_id).unwrap(),
            Some(OperationState::Finalized)
        );
        drop(gateway);
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[tokio::test]
    async fn target_read_crash_stays_authorized_and_repeats_only_the_safe_get() {
        let path = database_path("target-read-recovery");
        let request = request();
        let mut adapter = failed_adapter(&path, &request);
        {
            let mut gateway = Gateway::open_for_test(&path).unwrap();
            gateway
                .submit_exact_for_test(&request, &authorization(&request))
                .unwrap();
            assert!(matches!(
                gateway
                    .run_operation_once_with_adapter_and_fault(
                        &request.operation_id,
                        &mut adapter,
                        Some(FaultPoint::TargetObserved),
                    )
                    .await,
                Err(GatewayError::InjectedFault)
            ));
            assert_eq!(
                gateway.get(&request.operation_id).unwrap(),
                Some(OperationState::Authorized)
            );
            assert_eq!(adapter.identify_calls, 1);
            assert_eq!(adapter.apply_calls, 0);
        }
        let mut gateway = Gateway::open_for_test(&path).unwrap();
        assert_eq!(
            gateway
                .run_once_with_adapter(&mut adapter, None)
                .await
                .unwrap(),
            Some(OperationState::ReceiverObserved)
        );
        assert_eq!(adapter.identify_calls, 2);
        assert_eq!(adapter.apply_calls, 1);
        drop(gateway);
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
