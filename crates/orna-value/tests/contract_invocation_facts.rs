use orna_value::{
    InvocationEvidence, InvocationFacts, InvocationFactsError, InvocationOwner, InvocationStatus,
};

fn facts(status: InvocationStatus) -> InvocationFacts {
    InvocationFacts {
        status,
        owner: InvocationOwner::OwnerSession,
        evidence: InvocationEvidence::None,
        has_trace: false,
        has_idempotency_key_hash: false,
    }
}

fn evidence_for(status: InvocationStatus) -> InvocationEvidence {
    match status {
        InvocationStatus::Succeeded => InvocationEvidence::Result,
        InvocationStatus::Failed => InvocationEvidence::Failure,
        InvocationStatus::Queued
        | InvocationStatus::Running
        | InvocationStatus::Cancelled
        | InvocationStatus::Orphaned => InvocationEvidence::None,
    }
}

#[test]
fn accepts_each_launch_owner_for_each_supported_lifecycle() {
    for owner in [
        InvocationOwner::Parent,
        InvocationOwner::OwnerSession,
        InvocationOwner::TopLevelCommand,
    ] {
        for status in [
            InvocationStatus::Queued,
            InvocationStatus::Running,
            InvocationStatus::Succeeded,
            InvocationStatus::Failed,
            InvocationStatus::Cancelled,
            InvocationStatus::Orphaned,
        ] {
            let candidate = InvocationFacts {
                owner,
                evidence: evidence_for(status),
                ..facts(status)
            };
            assert_eq!(candidate.validate(), Ok(()), "{owner:?} {status:?}");
        }
    }
}

#[test]
fn enforces_active_and_terminal_result_failure_invariants() {
    for status in [InvocationStatus::Queued, InvocationStatus::Running] {
        for evidence in [InvocationEvidence::Result, InvocationEvidence::Failure] {
            let candidate = InvocationFacts {
                evidence,
                ..facts(status)
            };
            assert_eq!(
                candidate.validate(),
                Err(InvocationFactsError::ActiveHasTerminalEvidence),
                "{status:?} with {evidence:?}"
            );
        }
    }

    for evidence in [InvocationEvidence::None, InvocationEvidence::Failure] {
        let candidate = InvocationFacts {
            evidence,
            ..facts(InvocationStatus::Succeeded)
        };
        assert_eq!(
            candidate.validate(),
            Err(InvocationFactsError::SucceededOutcome),
            "succeeded with {evidence:?}"
        );
    }

    for evidence in [InvocationEvidence::None, InvocationEvidence::Result] {
        let candidate = InvocationFacts {
            evidence,
            ..facts(InvocationStatus::Failed)
        };
        assert_eq!(
            candidate.validate(),
            Err(InvocationFactsError::FailedOutcome),
            "failed with {evidence:?}"
        );
    }
}

#[test]
fn cancellation_and_orphaning_cannot_expose_a_result() {
    for status in [InvocationStatus::Cancelled, InvocationStatus::Orphaned] {
        let candidate = InvocationFacts {
            evidence: InvocationEvidence::Result,
            ..facts(status)
        };
        assert_eq!(
            candidate.validate(),
            Err(InvocationFactsError::CancelledOrOrphanedOutcome),
            "{status:?}"
        );

        for evidence in [InvocationEvidence::None, InvocationEvidence::Failure] {
            let candidate = InvocationFacts {
                evidence,
                ..facts(status)
            };
            assert_eq!(candidate.validate(), Ok(()), "{status:?} {evidence:?}");
        }
    }
}

#[test]
fn unsupported_trace_and_idempotency_links_fail_closed() {
    let mut candidate = facts(InvocationStatus::Running);
    candidate.has_trace = true;
    assert_eq!(
        candidate.validate(),
        Err(InvocationFactsError::UnsupportedLinkPresent)
    );

    candidate.has_trace = false;
    candidate.has_idempotency_key_hash = true;
    assert_eq!(
        candidate.validate(),
        Err(InvocationFactsError::UnsupportedLinkPresent)
    );
}
