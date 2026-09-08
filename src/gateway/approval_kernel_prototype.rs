//! Rejected, disposable policy experiment. Not part of any production execution path.
//!
//! The driver authenticates authorization and correlates fresh durable acknowledgements. A claim
//! is not storage evidence. Only the driver's *fresh successful conditional commit* may report Won.
//! Neither a snapshot nor a repeated read can supply that fact. See the experiment report.

use super::{
    ApplyOutcome, AuthorizedRequest, GatewayError, OperationResult, ReceiverObservation,
    TargetReadError, TargetRejection, ValidatedRequest, ValidatedTargetIdentity,
};

mod tests;

#[derive(Clone, Debug, Eq, PartialEq)]
struct Frozen {
    request: ValidatedRequest,
    target: ValidatedTargetIdentity,
}

impl Frozen {
    fn approved(authorized: &AuthorizedRequest) -> Result<Self, GatewayError> {
        let approved = authorized
            .authorization()
            .authorization
            .approved_target
            .as_ref()
            .ok_or(GatewayError::AuthorizationMismatch)?;
        Ok(Self {
            request: authorized.request().clone(),
            target: ValidatedTargetIdentity::try_from(super::TargetIdentity {
                deployment_uid: approved.uid.clone(),
                resource_version: approved.resource_version.clone(),
            })?,
        })
    }

    fn unknown_outcome(&self) -> ApplyOutcome {
        ApplyOutcome {
            accepted: false,
            requested_generation: None,
            deployment_uid: Some(self.target.deployment_uid().to_owned()),
            resource_version: Some(self.target.resource_version().to_owned()),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Claim {
    // Driver-owned unique invocation incarnation, never caller input or a persisted permission.
    invocation: u64,
    frozen: Frozen,
}

// Intentionally neither Clone nor Copy. No constructor outside this private policy owner.
#[derive(Debug)]
struct SendPermission(Claim);

impl SendPermission {
    fn consume(self) -> Frozen {
        self.0.frozen
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CommitOutcome {
    Won,
    Lost,
    Ambiguous,
}

#[derive(Debug)]
enum Recovery {
    Authorized,
    Attempted(ApplyOutcome),
}

#[derive(Debug)]
enum Event {
    Target(Result<ValidatedTargetIdentity, TargetReadError>),
    CommitAck(Claim, CommitOutcome),
    Dispatch,
    Response(Claim, Result<ApplyOutcome, ()>),
    Receiver(Result<ReceiverObservation, ()>),
    Tick,
    Cancel,
    Loss,
}

#[derive(Debug)]
enum Output {
    None,
    Commit(Claim),
    Reject {
        reason: TargetRejection,
        observed: Option<ValidatedTargetIdentity>,
    },
    Send(SendPermission),
    Observed(OperationResult),
}

#[derive(Debug)]
enum Phase {
    Reading,
    Committing,
    Ready(SendPermission),
    Observing,
    Rejected(TargetRejection),
    CancelledBeforeClaim,
    Finished(OperationResult),
    Inactive,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Mutation {
    None,
    RemintOnDuplicateAck,
    ResendOnRecovery,
}

#[derive(Debug)]
struct Kernel {
    claim: Claim,
    phase: Phase,
    outcome: ApplyOutcome,
    // A local observation budget only. Never grant expiry or a new authorization condition.
    observation_deadline: u64,
    response_seen: bool,
    dispatched: bool,
    mutation: Mutation,
}

impl Kernel {
    fn new(
        frozen: Frozen,
        invocation: u64,
        recovery: Recovery,
        observation_deadline: u64,
        mutation: Mutation,
    ) -> Self {
        let claim = Claim { invocation, frozen };
        let (phase, outcome) = match recovery {
            Recovery::Authorized => (Phase::Reading, claim.frozen.unknown_outcome()),
            Recovery::Attempted(outcome) => {
                let phase = if mutation == Mutation::ResendOnRecovery {
                    Phase::Ready(SendPermission(claim.clone()))
                } else {
                    Phase::Observing
                };
                (phase, outcome)
            },
        };
        Self {
            claim,
            phase,
            outcome,
            observation_deadline,
            response_seen: false,
            dispatched: false,
            mutation,
        }
    }

    fn step(&mut self, event: Event, now: u64) -> Output {
        match event {
            Event::Loss => self.phase = Phase::Inactive,
            Event::Cancel => match self.phase {
                Phase::Reading => self.phase = Phase::CancelledBeforeClaim,
                Phase::Committing | Phase::Ready(_) | Phase::Observing => {
                    // This is an invocation-local conclusion, not a durable receipt decision.
                    self.phase = Phase::Finished(OperationResult::Unknown);
                },
                _ => {},
            },
            Event::Target(read) if matches!(self.phase, Phase::Reading) => match read {
                Ok(target) if target == self.claim.frozen.target => {
                    self.phase = Phase::Committing;
                    return Output::Commit(self.claim.clone());
                },
                Ok(target) => return self.reject(TargetRejection::StaleApproval, Some(target)),
                Err(TargetReadError::Permanent(reason)) => return self.reject(reason, None),
                Err(TargetReadError::Transient) => {},
            },
            Event::CommitAck(claim, result) if claim == self.claim => {
                let pending = matches!(self.phase, Phase::Committing);
                let remint = self.mutation == Mutation::RemintOnDuplicateAck
                    && matches!(self.phase, Phase::Observing);
                if pending || remint {
                    self.phase = match result {
                        CommitOutcome::Won => Phase::Ready(SendPermission(claim)),
                        CommitOutcome::Lost | CommitOutcome::Ambiguous => Phase::Observing,
                    };
                }
            },
            Event::Dispatch if matches!(self.phase, Phase::Ready(_)) => {
                if let Phase::Ready(permission) =
                    std::mem::replace(&mut self.phase, Phase::Observing)
                {
                    self.dispatched = true;
                    return Output::Send(permission);
                }
            },
            Event::Response(claim, response)
                if claim == self.claim
                    && matches!(self.phase, Phase::Observing)
                    && self.dispatched
                    && !self.response_seen
                    && now < self.observation_deadline =>
            {
                self.response_seen = true;
                if let Ok(outcome) = response {
                    if outcome.validate().is_ok()
                        && outcome.deployment_uid.as_deref()
                            == Some(self.claim.frozen.target.deployment_uid())
                        && outcome.resource_version.is_some()
                    {
                        self.outcome = outcome;
                    }
                }
            },
            Event::Receiver(observation) if matches!(self.phase, Phase::Observing) => {
                let result = if now >= self.observation_deadline {
                    OperationResult::Unknown
                } else {
                    observation
                        .ok()
                        .filter(|value| value.validate().is_ok())
                        .map_or(OperationResult::Unknown, |value| {
                            value.classify(&self.claim.frozen.request, &self.outcome)
                        })
                };
                self.phase = Phase::Finished(result);
                return Output::Observed(result);
            },
            Event::Tick
                if matches!(self.phase, Phase::Observing) && now >= self.observation_deadline =>
            {
                self.phase = Phase::Finished(OperationResult::Unknown);
                return Output::Observed(OperationResult::Unknown);
            },
            _ => {},
        }
        Output::None
    }

    fn reject(
        &mut self,
        reason: TargetRejection,
        observed: Option<ValidatedTargetIdentity>,
    ) -> Output {
        self.phase = Phase::Rejected(reason);
        Output::Reject { reason, observed }
    }
}
