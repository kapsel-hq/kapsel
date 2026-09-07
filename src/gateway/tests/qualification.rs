    use std::{collections::BTreeMap, time::Instant};

    use http::{Request, Response};
    use kube::{Client, client::Body};
    use serde_json::json;
    use tower_test::mock;

    const WARMUPS: usize = 5;
    const SAMPLES: usize = 30;

    fn maximum_request() -> SetDeploymentImageRequest {
        SetDeploymentImageRequest {
            operation_id: "o".repeat(128),
            namespace: "n".repeat(63),
            deployment: format!(
                "{}.{}.{}.{}",
                "a".repeat(63),
                "b".repeat(63),
                "c".repeat(63),
                "d".repeat(61)
            ),
            container: "c".repeat(63),
            immutable_image_digest: format!("{}@sha256:{}", "i".repeat(440), "0".repeat(64)),
        }
    }

    fn maximum_authorization(request: &SetDeploymentImageRequest) -> ExactAuthorization {
        let mut authorization = authorization(request);
        authorization.authorization_id = "a".repeat(128);
        authorization
    }

    fn elapsed_microseconds(started: Instant) -> u64 {
        u64::try_from(started.elapsed().as_micros()).unwrap()
    }

    fn successful_response(request: &SetDeploymentImageRequest, resource_version: &str) -> Vec<u8> {
        serde_json::to_vec(&json!({
            "apiVersion": "apps/v1",
            "kind": "Deployment",
            "metadata": {
                "name": request.deployment,
                "namespace": request.namespace,
                "uid": "deployment-uid-1",
                "resourceVersion": resource_version,
                "generation": 2
            },
            "spec": {
                "template": {"spec": {"containers": [{
                    "name": request.container,
                    "image": request.immutable_image_digest
                }]}}
            }
        }))
        .unwrap()
    }

    async fn measure_concrete_adapter(
        measurements: &mut BTreeMap<&'static str, Vec<u64>>,
        request: &SetDeploymentImageRequest,
    ) {
        let (service, mut handle) = mock::pair::<Request<Body>, Response<Body>>();
        let mut adapter = KubernetesDeploymentImageAdapter::new(Client::new(service, "default"));
        let identify_response = successful_response(request, "resource-version-1");
        let identify_server = tokio::spawn(async move {
            let (_, send) = handle.next_request().await.unwrap();
            send.send_response(Response::new(Body::from(identify_response)));
            handle
        });
        let started = Instant::now();
        let target = adapter.identify(request).await.unwrap();
        measurements
            .entry("target_read")
            .or_default()
            .push(elapsed_microseconds(started));
        let mut handle = identify_server.await.unwrap();
        let patch_response = successful_response(request, "resource-version-2");
        let patch_server = tokio::spawn(async move {
            let (_, send) = handle.next_request().await.unwrap();
            send.send_response(Response::new(Body::from(patch_response)));
        });
        let started = Instant::now();
        let outcome = adapter.apply(request, &target).await.unwrap();
        measurements
            .entry("conditional_patch")
            .or_default()
            .push(elapsed_microseconds(started));
        patch_server.await.unwrap();
        assert!(outcome.accepted);
    }

    #[allow(clippy::print_stdout)]
    #[tokio::test]
    #[ignore = "private release-optimized beta qualification journal growth measurement"]
    async fn beta_qualification_journal_growth_measurement() {
        let path = database_path("qualification-growth");
        drop(Gateway::open_for_test(&path).unwrap());
        let empty_bytes = fs::metadata(&path).unwrap().len();
        let gateway = Gateway::open_for_test(&path).unwrap();
        for index in 0..10_000 {
            let mut request = maximum_request();
            request.operation_id = format!("operation-{index:0>118}");
            let mut authorization = maximum_authorization(&request);
            authorization.authorization_id = format!("authorization-{index:0>114}");
            gateway
                .submit_exact_for_test(&request, &authorization)
                .unwrap();
        }
        drop(gateway);
        let final_bytes = fs::metadata(&path).unwrap().len();
        let connection = Connection::open(&path).unwrap();
        let page_count = u64::try_from(
            connection
                .query_row("PRAGMA page_count", [], |row| row.get::<_, i64>(0))
                .unwrap(),
        )
        .unwrap();
        let page_size = u64::try_from(
            connection
                .query_row("PRAGMA page_size", [], |row| row.get::<_, i64>(0))
                .unwrap(),
        )
        .unwrap();
        drop(connection);
        for suffix in ["-journal", "-wal", "-shm"] {
            assert!(!PathBuf::from(format!("{}{suffix}", path.display())).exists());
        }
        let receipt_path = database_path("qualification-receipt-size");
        let receipt_directory = receipt_path.parent().unwrap().join("receipts");
        private_directory(&receipt_directory);
        let request = maximum_request();
        let authorization = maximum_authorization(&request);
        let mut gateway = Gateway::open_for_test(&receipt_path).unwrap();
        gateway
            .submit_exact_for_test(&request, &authorization)
            .unwrap();
        let mut adapter = failed_adapter(&receipt_path, &request);
        assert_eq!(
            gateway
                .run_once_with_adapter(&mut adapter, None)
                .await
                .unwrap(),
            Some(OperationState::ReceiverObserved)
        );
        assert_eq!(
            gateway
                .finalize_receipt_once(&ReceiptSettings {
                    signing_seed: &[13_u8; 32],
                    key_id: "qualification-receipt-key",
                })
                .unwrap(),
            Some(OperationState::Finalized)
        );
        let receipt_bytes = Gateway::read_loaded_receipt(
            gateway
                .journal
                .operation(&request.operation_id)
                .unwrap()
                .unwrap(),
        )
        .unwrap()
        .0
        .len();
        let (persisted_value_bytes_max, sqlite_value_or_row_bytes_max, rollback_bytes_max) =
            journal::qualification_storage_limits();
        println!(
            "BETA_QUALIFICATION_GROWTH={}",
            json!({
                "empty_bytes": empty_bytes,
                "final_bytes": final_bytes,
                "page_bytes": page_count * page_size,
                "average_growth_bytes": (final_bytes - empty_bytes) / 10_000,
                "operations": 10_000,
                "maximal_receipt_bytes": receipt_bytes,
                "persisted_value_bytes_max": persisted_value_bytes_max,
                "sqlite_value_or_row_bytes_max": sqlite_value_or_row_bytes_max,
                "rollback_journal_bytes_max": rollback_bytes_max
            })
        );
        drop(gateway);
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
        fs::remove_dir_all(receipt_path.parent().unwrap()).unwrap();
    }

    #[allow(
        clippy::print_stdout,
        clippy::too_many_lines,
        reason = "one frozen ignored test emits the fixed internal phase set as one sample document"
    )]
    #[tokio::test]
    #[ignore = "private release-optimized beta qualification phase measurement"]
    async fn beta_qualification_internal_phase_measurements() {
        let request = maximum_request();
        let authorization = maximum_authorization(&request);
        let mut measurements = BTreeMap::new();

        for sample in 0..WARMUPS + SAMPLES {
            let submit_path = database_path(&format!("qualification-submit-{sample}"));
            let gateway = Gateway::open_for_test(&submit_path).unwrap();
            let started = Instant::now();
            gateway
                .submit_exact_for_test(&request, &authorization)
                .unwrap();
            let elapsed = elapsed_microseconds(started);
            assert_eq!(
                gateway.get(&request.operation_id).unwrap(),
                Some(OperationState::Authorized)
            );
            drop(gateway);
            fs::remove_dir_all(submit_path.parent().unwrap()).unwrap();
            if sample >= WARMUPS {
                measurements
                    .entry("submit_authorized")
                    .or_insert_with(Vec::new)
                    .push(elapsed);
            }

            let reconcile_path = database_path(&format!("qualification-reconcile-{sample}"));
            let mut gateway = Gateway::open_for_test(&reconcile_path).unwrap();
            gateway
                .submit_exact_for_test(&request, &authorization)
                .unwrap();
            let mut first = failed_adapter(&reconcile_path, &request);
            assert!(matches!(
                gateway
                    .run_once_with_adapter(&mut first, Some(FaultPoint::ApplyStartedCommitted))
                    .await,
                Err(GatewayError::InjectedFault)
            ));
            assert_eq!(first.apply_calls, 0);
            drop(gateway);
            let started = Instant::now();
            let mut gateway = Gateway::open_for_test(&reconcile_path).unwrap();
            let mut recovery = failed_adapter(&reconcile_path, &request);
            assert_eq!(
                gateway
                    .run_once_with_adapter(&mut recovery, None)
                    .await
                    .unwrap(),
                Some(OperationState::ReceiverObserved)
            );
            let elapsed = elapsed_microseconds(started);
            assert_eq!(recovery.apply_calls, 0);
            drop(gateway);
            fs::remove_dir_all(reconcile_path.parent().unwrap()).unwrap();
            if sample >= WARMUPS {
                measurements
                    .entry("reconcile_apply_started")
                    .or_insert_with(Vec::new)
                    .push(elapsed);
            }

            let receipt_path = database_path(&format!("qualification-receipt-{sample}"));
            let receipt_directory = receipt_path.parent().unwrap().join("receipts");
            private_directory(&receipt_directory);
            let mut gateway = Gateway::open_for_test(&receipt_path).unwrap();
            gateway
                .submit_exact_for_test(&request, &authorization)
                .unwrap();
            let mut adapter = failed_adapter(&receipt_path, &request);
            assert_eq!(
                gateway
                    .run_once_with_adapter(&mut adapter, None)
                    .await
                    .unwrap(),
                Some(OperationState::ReceiverObserved)
            );
            publication::validate_private_directory(&receipt_directory).unwrap();
            let started = Instant::now();
            assert_eq!(
                gateway
                    .finalize_receipt_once(&ReceiptSettings {
                        signing_seed: &[13_u8; 32],
                        key_id: "qualification-receipt-key",
                    })
                    .unwrap(),
                Some(OperationState::Finalized)
            );
            let elapsed = elapsed_microseconds(started);
            drop(gateway);
            fs::remove_dir_all(receipt_path.parent().unwrap()).unwrap();
            if sample >= WARMUPS {
                measurements
                    .entry("receipt_finalize")
                    .or_insert_with(Vec::new)
                    .push(elapsed);
            }

            let recovery_path = database_path(&format!("qualification-recovery-{sample}"));
            let mut gateway = Gateway::open_for_test(&recovery_path).unwrap();
            gateway
                .submit_exact_for_test(&request, &authorization)
                .unwrap();
            let mut first = failed_adapter(&recovery_path, &request);
            assert!(matches!(
                gateway
                    .run_once_with_adapter(&mut first, Some(FaultPoint::ApplyReturned))
                    .await,
                Err(GatewayError::InjectedFault)
            ));
            assert_eq!(first.apply_calls, 1);
            drop(gateway);
            let started = Instant::now();
            let mut gateway = Gateway::open_for_test(&recovery_path).unwrap();
            let mut recovery = failed_adapter(&recovery_path, &request);
            assert_eq!(
                gateway
                    .run_once_with_adapter(&mut recovery, None)
                    .await
                    .unwrap(),
                Some(OperationState::ReceiverObserved)
            );
            let elapsed = elapsed_microseconds(started);
            assert_eq!(recovery.apply_calls, 0);
            drop(gateway);
            fs::remove_dir_all(recovery_path.parent().unwrap()).unwrap();
            if sample >= WARMUPS {
                measurements
                    .entry("restart_recovery")
                    .or_insert_with(Vec::new)
                    .push(elapsed);
            }

            if sample >= WARMUPS {
                measure_concrete_adapter(&mut measurements, &request).await;
            }
        }
        assert!(measurements.values().all(|values| values.len() == SAMPLES));
        println!(
            "BETA_QUALIFICATION_MEASUREMENTS={}",
            serde_json::to_string(&measurements).unwrap()
        );
    }
