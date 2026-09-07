    #[test]
    #[ignore = "invoked only as a subprocess by process-kill recovery tests"]
    fn process_kill_child() {
        let scenario = std::env::var("KAPSEL_PROCESS_CHILD_SCENARIO").unwrap();
        let database = PathBuf::from(std::env::var_os("KAPSEL_PROCESS_DATABASE").unwrap());
        let ready = PathBuf::from(std::env::var_os("KAPSEL_PROCESS_READY").unwrap());
        if scenario == "mutation" {
            let patch_count =
                PathBuf::from(std::env::var_os("KAPSEL_PROCESS_PATCH_COUNT").unwrap());
            let mut gateway = Gateway::open_for_test(&database).unwrap();
            let mut adapter = ProcessMutationAdapter {
                ready_path: ready,
                patch_count_path: patch_count,
            };
            tokio::runtime::Builder::new_current_thread()
                .enable_time()
                .build()
                .unwrap()
                .block_on(gateway.run_operation_once_with_adapter_and_fault(
                    "op-001",
                    &mut adapter,
                    None,
                ))
                .unwrap();
            unreachable!("the parent must kill the child while apply is pending");
        }
        assert!(matches!(
            scenario.as_str(),
            "receipt" | "before_receipt_commit"
        ));
        let gateway = Gateway::open_for_test(&database).unwrap();
        assert!(matches!(
            gateway.finalize_operation_receipt_once_with_fault(
                "op-001",
                &ReceiptSettings {
                    signing_seed: &[31_u8; 32],
                    key_id: "process-receipt-key",
                },
                Some(if scenario == "receipt" {
                    FaultPoint::FinalizedCommitted
                } else {
                    FaultPoint::BeforeReceiptCommit
                }),
            ),
            Err(GatewayError::InjectedFault)
        ));
        fs::write(ready, b"receipt-commit-seam").unwrap();
        loop {
            std::thread::park();
        }
    }

    #[tokio::test]
    async fn process_kill_after_provider_side_effect_recovers_without_second_mutation() {
        let path = database_path("process-kill-mutation");
        let request = request();
        {
            let gateway = Gateway::open_for_test(&path).unwrap();
            gateway
                .submit_exact_for_test(&request, &authorization(&request))
                .unwrap();
        }
        let ready = path.parent().unwrap().join("mutation-ready");
        let patch_count = path.parent().unwrap().join("patch-count");
        let mut child = spawn_process_child("mutation", &path, &ready, Some(&patch_count), None);
        wait_for_child_seam(&mut child, &ready);
        assert_eq!(fs::read_to_string(&patch_count).unwrap(), "1");
        kill_child(&mut child);

        let mut gateway = Gateway::open_for_test(&path).unwrap();
        assert_eq!(
            gateway.get(&request.operation_id).unwrap(),
            Some(OperationState::ApplyStarted)
        );
        let mut recovery = failed_adapter(&path, &request);
        assert_eq!(
            gateway
                .run_once_with_adapter(&mut recovery, None)
                .await
                .unwrap(),
            Some(OperationState::ReceiverObserved)
        );
        assert_eq!(recovery.identify_calls, 0);
        assert_eq!(recovery.apply_calls, 0);
        assert_eq!(recovery.observe_calls, 1);
        assert_eq!(fs::read_to_string(&patch_count).unwrap(), "1");
        assert_eq!(
            gateway.result(&request.operation_id).unwrap(),
            Some(OperationResult::Failed)
        );
        drop(gateway);
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[tokio::test]
    async fn worker_lock_prevents_overlapping_provider_activity() {
        let path = database_path("worker-lock");
        let request = request();
        let first_gateway = Gateway::open_for_test(&path).unwrap();
        first_gateway
            .submit_exact_for_test(&request, &authorization(&request))
            .unwrap();
        let worker_lock = first_gateway.journal.try_lock_worker().unwrap().unwrap();
        let mut second_gateway = Gateway::open_for_test(&path).unwrap();
        let mut adapter = failed_adapter(&path, &request);

        assert_eq!(
            second_gateway
                .run_once_with_adapter(&mut adapter, None)
                .await
                .unwrap(),
            None
        );
        assert_eq!(adapter.identify_calls, 0);
        assert_eq!(adapter.apply_calls, 0);
        assert_eq!(adapter.observe_calls, 0);

        drop(worker_lock);
        assert_eq!(
            second_gateway
                .run_once_with_adapter(&mut adapter, None)
                .await
                .unwrap(),
            Some(OperationState::ReceiverObserved)
        );
        drop(second_gateway);
        drop(first_gateway);
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[tokio::test]
    async fn restart_after_apply_observes_without_a_blind_second_apply() {
        let path = database_path("apply-recovery");
        let request = request();
        let mut adapter = failed_adapter(&path, &request);
        {
            let mut gateway = Gateway::open_for_test(&path).unwrap();
            gateway
                .submit_exact_for_test(&request, &authorization(&request))
                .unwrap();
            assert!(matches!(
                gateway
                    .run_once_with_adapter(&mut adapter, Some(FaultPoint::ApplyReturned))
                    .await,
                Err(GatewayError::InjectedFault)
            ));
            assert_eq!(
                gateway.get(&request.operation_id).unwrap(),
                Some(OperationState::ApplyStarted)
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
        assert!(adapter.apply_started_seen);
        assert_eq!(adapter.identify_calls, 1);
        assert_eq!(adapter.apply_calls, 1);
        assert_eq!(adapter.observe_calls, 1);
        assert_eq!(
            gateway.result(&request.operation_id).unwrap(),
            Some(OperationResult::Failed)
        );
        let statement = gateway
            .journal
            .receipt_statement(&request.operation_id)
            .unwrap()
            .unwrap();
        assert_eq!(statement.requested_generation(), Some(2));
        assert_eq!(statement.receiver_uid(), Some("deployment-uid-1"));
        assert_eq!(
            statement.rollout_condition_reason(),
            Some("ProgressDeadlineExceeded")
        );
        drop(gateway);
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[tokio::test]
    async fn recovery_receipt_does_not_reuse_target_uid_when_receiver_uid_is_missing() {
        let path = database_path("receiver-uid-missing");
        let request = request();
        let mut adapter = failed_adapter(&path, &request);
        adapter.observation = ReceiverObservation::unknown();
        {
            let mut gateway = Gateway::open_for_test(&path).unwrap();
            gateway
                .submit_exact_for_test(&request, &authorization(&request))
                .unwrap();
            assert!(matches!(
                gateway
                    .run_once_with_adapter(&mut adapter, Some(FaultPoint::ApplyReturned))
                    .await,
                Err(GatewayError::InjectedFault)
            ));
        }
        let mut gateway = Gateway::open_for_test(&path).unwrap();
        gateway
            .run_once_with_adapter(&mut adapter, None)
            .await
            .unwrap();
        let statement = gateway
            .journal
            .receipt_statement(&request.operation_id)
            .unwrap()
            .unwrap();
        assert_eq!(statement.receiver_uid(), None);
        assert_eq!(statement.requested_generation(), None);
        assert_eq!(statement.result(), OperationResult::Unknown);
        drop(gateway);
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[tokio::test]
    async fn every_apply_window_recovers_without_a_second_mutation() {
        let cases = [
            (FaultPoint::TargetObserved, 1, OperationResult::Failed),
            (
                FaultPoint::ApplyStartedCommitted,
                0,
                OperationResult::Failed,
            ),
            (FaultPoint::ApplyReturned, 1, OperationResult::Failed),
            (
                FaultPoint::ApplyOutcomeCommitted,
                1,
                OperationResult::Failed,
            ),
            (FaultPoint::ReceiverRead, 1, OperationResult::Failed),
            (
                FaultPoint::ReceiverObservedCommitted,
                1,
                OperationResult::Failed,
            ),
        ];
        for (index, (fault, expected_apply_calls, expected_result)) in cases.into_iter().enumerate()
        {
            let path = database_path(&format!("fault-window-{index}"));
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
                            Some(fault),
                        )
                        .await,
                    Err(GatewayError::InjectedFault)
                ));
            }
            let mut gateway = Gateway::open_for_test(&path).unwrap();
            let state = gateway.get(&request.operation_id).unwrap().unwrap();
            if matches!(
                state,
                OperationState::Authorized | OperationState::ApplyStarted
            ) {
                assert_eq!(
                    gateway
                        .run_operation_once_with_adapter(&request.operation_id, &mut adapter)
                        .await
                        .unwrap(),
                    Some(OperationState::ReceiverObserved)
                );
            } else {
                assert_eq!(state, OperationState::ReceiverObserved);
                assert_eq!(
                    gateway
                        .run_operation_once_with_adapter(&request.operation_id, &mut adapter)
                        .await
                        .unwrap(),
                    None
                );
            }
            assert_eq!(adapter.apply_calls, expected_apply_calls);
            assert_eq!(
                gateway.result(&request.operation_id).unwrap(),
                Some(expected_result)
            );
            drop(gateway);
            fs::remove_dir_all(path.parent().unwrap()).unwrap();
        }
    }
