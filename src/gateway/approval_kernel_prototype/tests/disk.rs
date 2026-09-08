//! Tiny scratch driver. Two independently opened Journal connections race the existing guarded
//! transition. No storage implementation is added, and production never invokes this driver.

use std::{
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::Path,
    sync::{Arc, Barrier},
};

use super::*;
use crate::gateway::{
    journal::{Journal, LoadedOperation},
    Gateway,
};

#[derive(Clone, Copy, Debug)]
enum Cut {
    Healthy,
    AckLoss,
    BeforeSend,
    AfterSend,
    CancelPending,
}

fn run_claimant(path: &Path, barrier: &Barrier, invocation: u64, cut: Cut) -> bool {
    let journal = Journal::open(path).unwrap();
    let Some(LoadedOperation::Authorized(authorized)) = journal.operation("prototype-op").unwrap()
    else {
        panic!("both contenders must load before CAS")
    };
    let (_, frozen) = fixture();
    assert_eq!(authorized.request(), &frozen.request);
    assert_eq!(
        authorized.approved_target().unwrap().uid,
        frozen.target.deployment_uid()
    );
    assert_eq!(
        authorized.approved_target().unwrap().resource_version,
        frozen.target.resource_version()
    );
    let mut kernel = Kernel::new(
        frozen.clone(),
        invocation,
        Recovery::Authorized,
        10,
        Mutation::None,
    );
    let Output::Commit(claim) = kernel.step(Event::Target(Ok(frozen.target)), 0) else {
        panic!("expected conditional attempt proposal")
    };
    if matches!(cut, Cut::CancelPending) {
        kernel.step(Event::Cancel, 0);
    }
    barrier.wait();
    // Deliberately bypass the worker lock here to stress the real SQLite conditional transition.
    // Production normally holds that lock throughout I/O. The frozen values come from the kernel.
    let committed = journal.mark_apply_started(&authorized, &claim.frozen.target);
    let outcome = match committed {
        Ok(()) => CommitOutcome::Won,
        Err(GatewayError::InvalidTransition) => CommitOutcome::Lost,
        Err(error) => panic!("unexpected scratch storage failure: {error}"),
    };
    let won = outcome == CommitOutcome::Won;
    if matches!(cut, Cut::AckLoss) {
        kernel.step(Event::CommitAck(claim.clone(), CommitOutcome::Ambiguous), 0);
    } else {
        kernel.step(Event::CommitAck(claim.clone(), outcome), 0);
    }
    if matches!(cut, Cut::BeforeSend) {
        kernel.step(Event::Loss, 0);
    }
    if let Output::Send(permission) = kernel.step(Event::Dispatch, 0) {
        record_receiver_request(path, permission);
    }
    if matches!(cut, Cut::AfterSend) {
        kernel.step(Event::Loss, 0);
    }
    // Duplicate completion cannot turn a consumed, cancelled, ambiguous, or lost permit live again.
    kernel.step(Event::CommitAck(claim, outcome), 0);
    assert!(matches!(kernel.step(Event::Dispatch, 0), Output::None));
    won
}

fn record_receiver_request(path: &Path, permission: SendPermission) {
    let payload = permission.consume();
    assert_eq!(payload, fixture().1);
    // Independent evidence records *every* crossing, not a boolean or kernel-issued count.
    let mut log = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path.with_extension("receiver-log"))
        .unwrap();
    writeln!(
        log,
        "{} {} {}",
        payload.request.operation_id(),
        payload.target.deployment_uid(),
        payload.target.resource_version()
    )
    .unwrap();
    log.sync_all().unwrap();
}

#[test]
fn real_journal_cas_contention_loss_reopen_and_independent_receiver_evidence() {
    for (cut, expected_requests) in [
        (Cut::Healthy, 1),
        (Cut::AckLoss, 0),
        (Cut::BeforeSend, 0),
        (Cut::AfterSend, 1),
        (Cut::CancelPending, 0),
    ] {
        let root = std::env::temp_dir().join(format!(
            "kapsel-approval-prototype-{}-{cut:?}",
            std::process::id()
        ));
        // Refuse collisions instead of deleting preexisting evidence.
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let path = root.join("journal.sqlite3");
        let (authorized, frozen) = fixture();
        {
            let gateway = Gateway::open_for_test(&path).unwrap();
            gateway
                .submit_exact_for_test(
                    &frozen.request.to_adapter_request(),
                    &authorized.authorization().authorization,
                )
                .unwrap();
            let second = Journal::open(&path).unwrap();
            let lock = gateway.journal.try_lock_worker().unwrap().unwrap();
            assert!(second.try_lock_worker().unwrap().is_none());
            drop(lock);
            assert!(second.try_lock_worker().unwrap().is_some());
        }
        let barrier = Arc::new(Barrier::new(2));
        let winners = std::thread::scope(|scope| {
            let first = scope.spawn(|| run_claimant(&path, &barrier, 0, cut));
            let second = scope.spawn(|| run_claimant(&path, &barrier, 1, cut));
            usize::from(first.join().unwrap()) + usize::from(second.join().unwrap())
        });
        assert_eq!(winners, 1, "{cut:?}");
        let evidence =
            fs::read_to_string(path.with_extension("receiver-log")).unwrap_or_else(|error| {
                assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
                String::new()
            });
        let requests = evidence.lines().count();
        assert_eq!(requests, expected_requests, "{cut:?}");
        let journal = Journal::open(&path).unwrap();
        let Some(LoadedOperation::ApplyStarted(started)) =
            journal.operation("prototype-op").unwrap()
        else {
            panic!("durable attempted marker must survive connection loss")
        };
        assert_eq!(started.request(), &frozen.request);
        let stored = started.classification_outcome();
        assert_eq!(
            stored.deployment_uid.as_deref(),
            Some(frozen.target.deployment_uid())
        );
        assert_eq!(
            stored.resource_version.as_deref(),
            Some(frozen.target.resource_version())
        );
        let mut recovered = Kernel::new(frozen, 2, Recovery::Attempted(stored), 10, Mutation::None);
        assert!(matches!(recovered.step(Event::Dispatch, 0), Output::None));
        let receiver = Receiver {
            requests,
            changes: usize::from(requests > 0),
        };
        let expected = if requests == 0 {
            OperationResult::Unknown
        } else {
            OperationResult::Succeeded
        };
        assert!(
            matches!(recovered.step(Event::Receiver(Ok(receiver.observation())), 0),
            Output::Observed(actual) if actual == expected)
        );
        // Observation cannot append independent receiver request evidence.
        assert_eq!(
            fs::read_to_string(path.with_extension("receiver-log"))
                .ok()
                .unwrap_or_default(),
            evidence
        );
        drop(journal);
        fs::remove_dir_all(root).unwrap();
    }
}
