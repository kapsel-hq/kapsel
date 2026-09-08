#![allow(
    clippy::panic,
    reason = "invalid prototype evidence must fail the test"
)]

use std::collections::{HashSet, VecDeque};

use super::*;
use crate::gateway::{
    authorization::VerifiedAuthorization, ApprovedTarget, ExactAuthorization,
    SetDeploymentImageRequest, TargetIdentity,
};

mod disk;

fn fixture() -> (AuthorizedRequest, Frozen) {
    let request = SetDeploymentImageRequest {
        operation_id: "prototype-op".into(),
        namespace: "demo".into(),
        deployment: "api".into(),
        container: "api".into(),
        immutable_image_digest: format!("example/api@sha256:{}", "a".repeat(64)),
    };
    let authorization = ExactAuthorization {
        authorization_id: "prototype-approval".into(),
        operation_id: request.operation_id.clone(),
        namespace: request.namespace.clone(),
        deployment: request.deployment.clone(),
        container: request.container.clone(),
        immutable_image_digest: request.immutable_image_digest.clone(),
        approved_target: Some(ApprovedTarget {
            uid: "original-uid".into(),
            resource_version: "opaque:rv/00".into(),
        }),
    };
    // Pure fixtures assume signature verification. Disk fixtures use the existing signed submit.
    let authorized = AuthorizedRequest::bind(
        ValidatedRequest::try_from(&request).unwrap(),
        VerifiedAuthorization {
            authorization,
            signer_key_id: "fixture".into(),
            grant_digest: "a".repeat(64),
        },
    )
    .unwrap();
    let frozen = Frozen::approved(&authorized).unwrap();
    (authorized, frozen)
}

// Independent receiver: count every request/admission, even if stale preconditions prevent another
// stored change. The oracle never asks the kernel whether a request was sent or should succeed.
#[derive(Debug, Default)]
struct Receiver {
    requests: usize,
    changes: usize,
}

impl Receiver {
    fn receive(&mut self, permission: SendPermission) {
        let payload = permission.consume();
        assert_eq!(payload, fixture().1);
        self.requests += 1;
        if self.changes == 0 {
            self.changes += 1;
        }
    }

    fn response(&self) -> ApplyOutcome {
        ApplyOutcome {
            accepted: self.requests > 0,
            requested_generation: Some(2),
            deployment_uid: Some("original-uid".into()),
            resource_version: Some("after-patch".into()),
        }
    }

    fn observation(&self) -> ReceiverObservation {
        if self.changes == 0 {
            return ReceiverObservation::unknown();
        }
        ReceiverObservation {
            deployment_uid: Some("original-uid".into()),
            resource_version: Some("after-patch".into()),
            current_generation: Some(2),
            observed_generation: Some(2),
            image: Some(fixture().1.request.immutable_image_digest().into()),
            operation_marker: Some("prototype-op".into()),
            desired_replicas: Some(1),
            updated_replicas: Some(1),
            available_replicas: Some(1),
            unavailable_replicas: Some(0),
            rollout_condition_type: Some("Available".into()),
            rollout_condition_status: Some("True".into()),
            rollout_condition_reason: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum Action {
    Read(usize),
    Cas(usize),
    Ack(usize),
    AckLost(usize),
    Send(usize),
    Respond(usize),
    Observe(usize),
    Cancel(usize),
    Crash(usize),
    Recover(usize),
    Deadline,
}

#[derive(Debug)]
struct World {
    kernels: [Option<Kernel>; 2],
    proposals: [Option<Claim>; 2],
    acknowledgements: [Option<(Claim, CommitOutcome)>; 2],
    // This model CAS is independent of the kernel's phase and permission bookkeeping.
    durable: Option<Claim>,
    receiver: Receiver,
    crashed: [bool; 2],
    recovered_attempted: [bool; 2],
    recovered_sends: usize,
    now: u64,
}

impl World {
    fn new(mutation: Mutation) -> Self {
        Self {
            kernels: std::array::from_fn(|i| {
                Some(Kernel::new(
                    fixture().1,
                    i as u64,
                    Recovery::Authorized,
                    10,
                    mutation,
                ))
            }),
            proposals: [None, None],
            acknowledgements: [None, None],
            durable: None,
            receiver: Receiver::default(),
            crashed: [false; 2],
            recovered_attempted: [false; 2],
            recovered_sends: 0,
            now: 0,
        }
    }

    fn deliver(&mut self, i: usize, event: Event) {
        let Some(kernel) = &mut self.kernels[i] else {
            return;
        };
        match kernel.step(event, self.now) {
            Output::Commit(claim) => self.proposals[i] = Some(claim),
            Output::Send(permission) => {
                if self.recovered_attempted[i] {
                    self.recovered_sends += 1;
                }
                self.receiver.receive(permission);
            },
            Output::None | Output::Reject { .. } | Output::Observed(_) => {},
        }
    }

    fn apply(&mut self, action: Action, mutation: Mutation) {
        match action {
            Action::Read(i) => self.deliver(i, Event::Target(Ok(fixture().1.target))),
            Action::Cas(i) => {
                if let Some(claim) = self.proposals[i].take() {
                    let outcome = if self.durable.is_none() {
                        self.durable = Some(claim.clone());
                        CommitOutcome::Won
                    } else {
                        CommitOutcome::Lost
                    };
                    self.acknowledgements[i] = Some((claim, outcome));
                }
            },
            Action::Ack(i) => {
                if let Some((claim, outcome)) = self.acknowledgements[i].clone() {
                    self.deliver(i, Event::CommitAck(claim, outcome));
                }
            },
            Action::AckLost(i) => {
                if let Some((claim, _)) = self.acknowledgements[i].take() {
                    self.deliver(i, Event::CommitAck(claim, CommitOutcome::Ambiguous));
                }
            },
            Action::Send(i) => self.deliver(i, Event::Dispatch),
            Action::Respond(i) => {
                if let Some(kernel) = &self.kernels[i] {
                    let claim = kernel.claim.clone();
                    self.deliver(i, Event::Response(claim, Ok(self.receiver.response())));
                }
            },
            Action::Observe(i) => {
                self.deliver(i, Event::Receiver(Ok(self.receiver.observation())));
            },
            Action::Cancel(i) => self.deliver(i, Event::Cancel),
            Action::Crash(i) => {
                self.deliver(i, Event::Loss);
                self.kernels[i] = None;
                self.crashed[i] = true;
            },
            Action::Recover(i) => {
                let frozen = fixture().1;
                self.recovered_attempted[i] = self.durable.is_some();
                let recovery = self.durable.as_ref().map_or(Recovery::Authorized, |_| {
                    Recovery::Attempted(frozen.unknown_outcome())
                });
                self.kernels[i] = Some(Kernel::new(frozen, i as u64 + 2, recovery, 10, mutation));
            },
            Action::Deadline => {
                self.now = 10;
                for i in 0..2 {
                    self.deliver(i, Event::Tick);
                }
            },
        }
    }

    fn violation(&self) -> Option<&'static str> {
        if self.receiver.requests > 1 {
            return Some("duplicate receiver request/admission");
        }
        if self.recovered_sends > 0 {
            return Some("recovered attempted state resent");
        }
        if self.receiver.requests > 0 && self.durable.is_none() {
            return Some("send before durable attempt");
        }
        for kernel in self.kernels.iter().flatten() {
            if let Phase::Finished(result) = kernel.phase {
                if result != OperationResult::Unknown && self.receiver.changes == 0 {
                    return Some("unsent or acceptance promoted to receiver result");
                }
            }
        }
        None
    }

    fn actions(&self) -> Vec<Action> {
        let mut actions = Vec::new();
        for i in 0..2 {
            if self.proposals[i].is_some() {
                actions.push(Action::Cas(i));
            }
            if self.acknowledgements[i].is_some() {
                actions.extend([Action::Ack(i), Action::AckLost(i)]);
            }
            if let Some(kernel) = &self.kernels[i] {
                match kernel.phase {
                    Phase::Reading => actions.push(Action::Read(i)),
                    Phase::Ready(_) => actions.push(Action::Send(i)),
                    Phase::Observing => {
                        actions.push(Action::Observe(i));
                        if !kernel.response_seen && kernel.dispatched {
                            actions.push(Action::Respond(i));
                        }
                    },
                    _ => {},
                }
                if matches!(
                    kernel.phase,
                    Phase::Reading | Phase::Committing | Phase::Ready(_) | Phase::Observing
                ) {
                    actions.push(Action::Cancel(i));
                    if !self.crashed[i] {
                        actions.push(Action::Crash(i));
                    }
                }
            } else {
                actions.push(Action::Recover(i));
            }
        }
        if self.now == 0 {
            actions.push(Action::Deadline);
        }
        actions
    }
}

fn replay(trace: &[Action], mutation: Mutation) -> World {
    let mut world = World::new(mutation);
    for action in trace {
        world.apply(*action, mutation);
    }
    world
}

// Replay prefixes from scratch. Only immutable events/facts are copied, never kernels or permits.
fn explore(mutation: Mutation, depth: usize) -> (Option<Vec<Action>>, usize) {
    const STATE_LIMIT: usize = 100_000;
    let mut queue = VecDeque::from([Vec::new()]);
    let mut seen = HashSet::new();
    while let Some(trace) = queue.pop_front() {
        let world = replay(&trace, mutation);
        if world.violation().is_some() {
            return (Some(trace), seen.len());
        }
        // Exact deterministic state key, including pending I/O and independent storage/receiver.
        let key = format!("{world:?}");
        if !seen.insert(key) {
            continue;
        }
        assert!(seen.len() < STATE_LIMIT, "raise neither bound silently");
        if trace.len() == depth {
            continue;
        }
        for action in world.actions() {
            let mut next = trace.clone();
            next.push(action);
            queue.push_back(next);
        }
    }
    (None, seen.len())
}

#[test]
fn bounded_two_invocation_search_and_mutant_counterexamples() {
    let (counterexample, states) = explore(Mutation::None, 7);
    assert!(counterexample.is_none(), "{counterexample:?}");
    assert!(states > 1000, "search unexpectedly vacuous: {states}");
    for mutation in [Mutation::RemintOnDuplicateAck, Mutation::ResendOnRecovery] {
        let (counterexample, _) = explore(mutation, 6);
        let trace = counterexample.expect("mutant must be detected");
        assert!(trace.len() <= 6);
        assert!(replay(&trace, mutation).violation().is_some());
        eprintln!("{mutation:?}: {trace:?}");
    }
    eprintln!("correct kernel: {states} distinct states through depth 7");
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one bounded table retains complete failure traces"
)]
fn retained_short_failure_and_progress_traces() {
    use Action::*;
    let cases: &[(&str, &[Action], usize, OperationResult)] = &[
        (
            "healthy competing claimants",
            &[
                Read(0),
                Read(1),
                Cas(0),
                Cas(1),
                Ack(1),
                Ack(0),
                Send(0),
                Respond(0),
                Observe(0),
            ],
            1,
            OperationResult::Succeeded,
        ),
        (
            "duplicate ack",
            &[
                Read(0),
                Cas(0),
                Ack(0),
                Send(0),
                Ack(0),
                Send(0),
                Observe(0),
            ],
            1,
            OperationResult::Succeeded,
        ),
        (
            "ack loss",
            &[Read(0), Cas(0), AckLost(0), Send(0), Observe(0)],
            0,
            OperationResult::Unknown,
        ),
        (
            "crash before dispatch",
            &[
                Read(0),
                Cas(0),
                Ack(0),
                Crash(0),
                Recover(0),
                Ack(0),
                Send(0),
                Observe(0),
            ],
            0,
            OperationResult::Unknown,
        ),
        (
            "crash after dispatch",
            &[
                Read(0),
                Cas(0),
                Ack(0),
                Send(0),
                Crash(0),
                Recover(0),
                Ack(0),
                Send(0),
                Observe(0),
            ],
            1,
            OperationResult::Succeeded,
        ),
        (
            "cancel pending commit, late ack",
            &[Read(0), Cancel(0), Cas(0), Ack(0), Send(0)],
            0,
            OperationResult::Unknown,
        ),
        (
            "cancel ready permission",
            &[Read(0), Cas(0), Ack(0), Cancel(0), Ack(0), Send(0)],
            0,
            OperationResult::Unknown,
        ),
        (
            "cancel after dispatch",
            &[
                Read(0),
                Cas(0),
                Ack(0),
                Send(0),
                Cancel(0),
                Respond(0),
                Observe(0),
            ],
            1,
            OperationResult::Unknown,
        ),
        (
            "late receiver and response",
            &[
                Read(0),
                Cas(0),
                Ack(0),
                Send(0),
                Deadline,
                Respond(0),
                Observe(0),
            ],
            1,
            OperationResult::Unknown,
        ),
    ];
    for (name, trace, requests, result) in cases {
        let world = replay(trace, Mutation::None);
        assert_eq!(world.violation(), None, "{name}");
        assert_eq!(world.receiver.requests, *requests, "{name}");
        assert!(
            matches!(world.kernels[0].as_ref().unwrap().phase,
            Phase::Finished(actual) if actual == *result),
            "{name}"
        );
    }
}

#[test]
fn exact_approval_ack_correlation_and_local_deadlines() {
    let (_, frozen) = fixture();
    for target in [
        TargetIdentity {
            deployment_uid: "replacement".into(),
            resource_version: frozen.target.resource_version().into(),
        },
        TargetIdentity {
            deployment_uid: frozen.target.deployment_uid().into(),
            resource_version: "opaque:rv/0".into(),
        },
    ] {
        let mut kernel = Kernel::new(frozen.clone(), 1, Recovery::Authorized, 10, Mutation::None);
        assert!(matches!(
            kernel.step(
                Event::Target(Ok(ValidatedTargetIdentity::try_from(target).unwrap())),
                0
            ),
            Output::Reject {
                reason: TargetRejection::StaleApproval,
                observed: Some(_)
            }
        ));
        assert!(matches!(
            kernel.phase,
            Phase::Rejected(TargetRejection::StaleApproval)
        ));
    }
    let mut kernel = Kernel::new(frozen.clone(), 1, Recovery::Authorized, 10, Mutation::None);
    kernel.step(Event::Target(Err(TargetReadError::Transient)), 0);
    assert!(matches!(kernel.phase, Phase::Reading));
    // This deadline is NOT grant expiry. Reads and initial sends remain authorized afterward.
    kernel.step(Event::Tick, 100);
    let Output::Commit(claim) = kernel.step(Event::Target(Ok(frozen.target)), 100) else {
        panic!("expected claim")
    };
    let mut mismatches = vec![claim.clone(); 3];
    mismatches[0].invocation += 1;
    mismatches[1].frozen.request.operation_id.0 = "other-operation".into();
    mismatches[2].frozen.target = ValidatedTargetIdentity::try_from(TargetIdentity {
        deployment_uid: "other-uid".into(),
        resource_version: "other-rv".into(),
    })
    .unwrap();
    for mismatch in mismatches {
        kernel.step(Event::CommitAck(mismatch, CommitOutcome::Won), 100);
        assert!(matches!(kernel.step(Event::Dispatch, 100), Output::None));
    }
    kernel.step(Event::CommitAck(claim.clone(), CommitOutcome::Won), 100);
    assert!(matches!(kernel.step(Event::Dispatch, 100), Output::Send(_)));
    kernel.step(
        Event::Response(claim, Ok(Receiver::default().response())),
        100,
    );
    assert!(!kernel.outcome.accepted);
    assert!(matches!(
        kernel.step(Event::Receiver(Err(())), 100),
        Output::Observed(OperationResult::Unknown)
    ));
}

#[test]
fn every_bounded_healthy_interleaving_makes_progress() {
    use Action::*;
    // Exhaust all 252 order-preserving interleavings of two healthy five-step invocations.
    fn visit(trace: &mut Vec<Action>, positions: [usize; 2], completed: &mut usize) {
        if positions == [5, 5] {
            let world = replay(trace, Mutation::None);
            assert_eq!(world.violation(), None, "{trace:?}");
            assert_eq!(world.receiver.requests, 1, "{trace:?}");
            let winner = usize::try_from(world.durable.as_ref().unwrap().invocation).unwrap();
            assert!(
                matches!(
                    world.kernels[winner].as_ref().unwrap().phase,
                    Phase::Finished(OperationResult::Succeeded)
                ),
                "{trace:?}"
            );
            *completed += 1;
            return;
        }
        for i in 0..2 {
            let sequence = [Read(i), Cas(i), Ack(i), Send(i), Observe(i)];
            if positions[i] < sequence.len() {
                trace.push(sequence[positions[i]]);
                let mut next = positions;
                next[i] += 1;
                visit(trace, next, completed);
                trace.pop();
            }
        }
    }
    let mut completed = 0;
    visit(&mut Vec::new(), [0, 0], &mut completed);
    assert_eq!(completed, 252);
}

#[test]
fn acceptance_transport_loss_and_failed_rollout_are_distinct() {
    use Action::*;
    for response in [
        Ok(Receiver {
            requests: 1,
            changes: 1,
        }
        .response()),
        Err(()),
    ] {
        let mut world = replay(&[Read(0), Cas(0), Ack(0), Send(0)], Mutation::None);
        let claim = world.kernels[0].as_ref().unwrap().claim.clone();
        world.deliver(0, Event::Response(claim, response));
        assert!(matches!(
            world.kernels[0].as_ref().unwrap().phase,
            Phase::Observing
        ));
        let mut failed = world.receiver.observation();
        failed.rollout_condition_type = Some("Progressing".into());
        failed.rollout_condition_status = Some("False".into());
        failed.rollout_condition_reason = Some("ProgressDeadlineExceeded".into());
        world.deliver(0, Event::Receiver(Ok(failed)));
        assert!(matches!(
            world.kernels[0].as_ref().unwrap().phase,
            Phase::Finished(OperationResult::Failed)
        ));
        world.apply(Observe(0), Mutation::None);
        assert!(
            matches!(
                world.kernels[0].as_ref().unwrap().phase,
                Phase::Finished(OperationResult::Failed)
            ),
            "late health must not improve frozen result"
        );
    }
}
