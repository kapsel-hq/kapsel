// Evidence for the adopted fresh-commit-to-dispatch boundary, using real journal decisions.

fn observed_target() -> ValidatedTargetIdentity {
    ValidatedTargetIdentity::try_from(TargetIdentity {
        deployment_uid: "deployment-uid-1".into(),
        resource_version: "resource-version-0".into(),
    })
    .unwrap()
}

fn load_authorized(gateway: &Gateway) -> journal::AuthorizedOperation {
    let Some(journal::LoadedOperation::Authorized(operation)) =
        gateway.journal.operation("op-001").unwrap()
    else {
        panic!("fixture must still be authorized");
    };
    operation
}

#[test]
fn competing_fresh_transitions_issue_one_bound_permission() {
    use std::sync::{Arc, Barrier};

    let path = database_path("dispatch-claimants");
    let request = request();
    let gateway = Gateway::open_for_test(&path).unwrap();
    gateway
        .submit_exact_for_test(&request, &authorization(&request))
        .unwrap();
    drop(gateway);
    let barrier = Arc::new(Barrier::new(2));
    // Array::map starts both threads before either is joined at the shared barrier.
    let claimants = [0, 1].map(|_| {
        let path = path.clone();
        let barrier = barrier.clone();
        std::thread::spawn(move || {
            let gateway = Gateway::open_for_test(path).unwrap();
            let operation = load_authorized(&gateway);
            barrier.wait();
            match gateway
                .journal
                .begin_attempt(&operation, observed_target(), None)
            {
                Ok(Some(permission)) => Some(permission.into_payload()),
                Err(GatewayError::InvalidTransition) => None,
                result => panic!("unexpected claim result: {}", result.is_ok()),
            }
        })
    });
    let winners: Vec<_> = claimants
        .into_iter()
        .filter_map(|claimant| claimant.join().unwrap())
        .collect();
    assert_eq!(winners.len(), 1);
    assert_eq!(winners[0].0, request);
    assert_eq!(winners[0].1, observed_target().to_adapter_target());
    let gateway = Gateway::open_for_test(&path).unwrap();
    assert!(matches!(
        gateway.journal.operation("op-001").unwrap(),
        Some(journal::LoadedOperation::ApplyStarted(_))
    ));
    drop(gateway);
    fs::remove_dir_all(path.parent().unwrap()).unwrap();
}

#[test]
fn a_snapshot_from_another_journal_cannot_authorize_a_different_frozen_action() {
    let first_path = database_path("dispatch-first-action");
    let second_path = database_path("dispatch-second-action");
    let first = Gateway::open_for_test(&first_path).unwrap();
    let second = Gateway::open_for_test(&second_path).unwrap();
    let request = request();
    first
        .submit_exact_for_test(&request, &authorization(&request))
        .unwrap();
    let mut different = request;
    different.container = "different".into();
    second
        .submit_exact_for_test(&different, &authorization(&different))
        .unwrap();
    let misplaced = load_authorized(&first);
    assert!(matches!(
        second
            .journal
            .begin_attempt(&misplaced, observed_target(), None),
        Err(GatewayError::InvalidTransition)
    ));
    assert_eq!(
        second.get("op-001").unwrap(),
        Some(OperationState::Authorized)
    );
    let permission = second
        .journal
        .begin_attempt(&load_authorized(&second), observed_target(), None)
        .unwrap()
        .unwrap();
    assert_eq!(permission.into_payload().0, different);
    drop(first);
    drop(second);
    fs::remove_dir_all(first_path.parent().unwrap()).unwrap();
    fs::remove_dir_all(second_path.parent().unwrap()).unwrap();
}

#[tokio::test]
async fn lost_acknowledgement_and_dropped_permission_strand_unsent_actions() {
    for fault in [None, Some(FaultPoint::AttemptCommitAcknowledgementLost)] {
        let path = database_path(&format!("dispatch-unsent-{fault:?}"));
        let request = request();
        let gateway = Gateway::open_for_test(&path).unwrap();
        gateway
            .submit_exact_for_test(&request, &authorization(&request))
            .unwrap();
        let authorized = load_authorized(&gateway);
        let result = gateway
            .journal
            .begin_attempt(&authorized, observed_target(), fault);
        if fault.is_some() {
            assert!(matches!(result, Err(GatewayError::InjectedFault)));
        } else {
            // Cancellation after acknowledged commitment drops an unused live permission.
            drop(result.unwrap().unwrap());
        }
        // A previously loaded Authorized snapshot is not another chance to send.
        assert!(matches!(
            gateway
                .journal
                .begin_attempt(&authorized, observed_target(), None),
            Err(GatewayError::InvalidTransition)
        ));
        assert_eq!(
            gateway.get("op-001").unwrap(),
            Some(OperationState::ApplyStarted)
        );
        assert_eq!(gateway.result("op-001").unwrap(), None);
        assert_eq!(gateway.target_rejection("op-001").unwrap(), None);
        drop(gateway);
        let mut gateway = Gateway::open_for_test(&path).unwrap();
        let mut adapter = failed_adapter(&path, &request);
        // No receiver mutation happened. Do not use the old fixture's independent failed rollout.
        adapter.observation = ReceiverObservation::unknown();
        gateway
            .run_once_with_adapter(&mut adapter, None)
            .await
            .unwrap();
        assert_eq!(
            (
                adapter.identify_calls,
                adapter.apply_calls,
                adapter.observe_calls
            ),
            (0, 0, 1)
        );
        assert_eq!(
            gateway.result("op-001").unwrap(),
            Some(OperationResult::Unknown)
        );
        gateway
            .finalize_receipt_once(&ReceiptSettings {
                signing_seed: &[13; 32],
                key_id: "dispatch-receipt",
            })
            .unwrap();
        let original =
            Gateway::read_loaded_receipt(gateway.loaded_for_test("op-001").unwrap().unwrap())
                .unwrap();
        drop(gateway);
        let mut gateway = Gateway::open_for_test(&path).unwrap();
        assert_eq!(
            gateway
                .run_once_with_adapter(&mut adapter, None)
                .await
                .unwrap(),
            None
        );
        assert_eq!(
            Gateway::read_loaded_receipt(gateway.loaded_for_test("op-001").unwrap().unwrap())
                .unwrap(),
            original
        );
        assert_eq!(adapter.apply_calls, 0);
        drop(gateway);
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
}

// Poll the real sequential driver to an async boundary, then drop its continuation. This is not
// another event machine: target/attempt/recovery decisions remain in Gateway and Journal.
#[tokio::test]
async fn cancellation_before_and_after_dispatch_preserves_durable_meaning() {
    struct PausingAdapter {
        inner: FakeAdapter,
        before_attempt: bool,
    }
    impl DeploymentImageAdapter for PausingAdapter {
        async fn identify(
            &mut self,
            request: &SetDeploymentImageRequest,
        ) -> Result<TargetIdentity, TargetReadError> {
            if self.before_attempt {
                std::future::pending().await
            } else {
                self.inner.identify(request).await
            }
        }
        async fn apply(&mut self, permission: DispatchPermission) -> Result<ApplyOutcome, ()> {
            self.inner.apply(permission).await?;
            std::future::pending().await
        }
        async fn observe(
            &mut self,
            request: &SetDeploymentImageRequest,
            outcome: &ApplyOutcome,
        ) -> Result<ReceiverObservation, ()> {
            self.inner.observe(request, outcome).await
        }
    }
    for before_attempt in [true, false] {
        let path = database_path(&format!("dispatch-cancel-{before_attempt}"));
        let request = request();
        let mut gateway = Gateway::open_for_test(&path).unwrap();
        gateway
            .submit_exact_for_test(&request, &authorization(&request))
            .unwrap();
        let mut adapter = PausingAdapter {
            inner: failed_adapter(&path, &request),
            before_attempt,
        };
        let mut execution = Box::pin(gateway.run_once_with_adapter(&mut adapter, None));
        assert!(matches!(
            std::future::poll_fn(|cx| std::task::Poll::Ready(execution.as_mut().poll(cx))).await,
            std::task::Poll::Pending
        ));
        drop(execution);
        assert_eq!(
            gateway.get("op-001").unwrap(),
            Some(if before_attempt {
                OperationState::Authorized
            } else {
                OperationState::ApplyStarted
            })
        );
        assert_eq!(gateway.result("op-001").unwrap(), None);
        drop(gateway);
        let mut gateway = Gateway::open_for_test(&path).unwrap();
        gateway
            .run_once_with_adapter(&mut adapter.inner, None)
            .await
            .unwrap();
        assert_eq!(adapter.inner.apply_calls, 1);
        assert_eq!(
            gateway.result("op-001").unwrap(),
            Some(OperationResult::Failed)
        );
        drop(gateway);
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
}
