use orna_foundation_v1::{InvocationFacts, InvocationFactsError, InvocationStatus};

fn facts(status: InvocationStatus) -> InvocationFacts {
    InvocationFacts {
        status,
        has_parent: false,
        has_owner_session: true,
        is_top_level_command: false,
        has_result: false,
        has_failure: false,
        has_trace: false,
        has_idempotency_key_hash: false,
    }
}

#[test]
fn accepts_exactly_one_owner_for_each_supported_lifecycle() {
    for status in [
        InvocationStatus::Queued,
        InvocationStatus::Running,
        InvocationStatus::Succeeded,
        InvocationStatus::Failed,
        InvocationStatus::Cancelled,
        InvocationStatus::Orphaned,
    ] {
        let mut candidate = facts(status);
        match status {
            InvocationStatus::Succeeded => candidate.has_result = true,
            InvocationStatus::Failed => candidate.has_failure = true,
            _ => {}
        }
        assert_eq!(candidate.validate(), Ok(()), "{status:?}");
    }
}

#[test]
fn rejects_zero_or_multiple_launch_owners() {
    let mut candidate = facts(InvocationStatus::Running);
    candidate.has_owner_session = false;
    assert_eq!(
        candidate.validate(),
        Err(InvocationFactsError::OwnerExclusivity)
    );

    candidate.has_parent = true;
    assert_eq!(candidate.validate(), Ok(()));

    candidate.has_owner_session = true;
    candidate.is_top_level_command = true;
    assert_eq!(
        candidate.validate(),
        Err(InvocationFactsError::OwnerExclusivity)
    );
}

#[test]
fn accepts_top_level_command_without_parent_or_session() {
    let mut candidate = facts(InvocationStatus::Succeeded);
    candidate.has_owner_session = false;
    candidate.is_top_level_command = true;
    candidate.has_result = true;
    assert_eq!(candidate.validate(), Ok(()));
}

#[test]
fn enforces_active_and_terminal_result_failure_invariants() {
    for status in [InvocationStatus::Queued, InvocationStatus::Running] {
        let mut candidate = facts(status);
        candidate.has_result = true;
        assert_eq!(
            candidate.validate(),
            Err(InvocationFactsError::ActiveHasTerminalEvidence),
            "{status:?} with a result"
        );

        candidate.has_result = false;
        candidate.has_failure = true;
        assert_eq!(
            candidate.validate(),
            Err(InvocationFactsError::ActiveHasTerminalEvidence),
            "{status:?} with a failure"
        );
    }

    let mut candidate = facts(InvocationStatus::Succeeded);
    assert_eq!(
        candidate.validate(),
        Err(InvocationFactsError::SucceededOutcome)
    );
    candidate.has_result = true;
    candidate.has_failure = true;
    assert_eq!(
        candidate.validate(),
        Err(InvocationFactsError::SucceededOutcome)
    );

    let mut candidate = facts(InvocationStatus::Failed);
    assert_eq!(
        candidate.validate(),
        Err(InvocationFactsError::FailedOutcome)
    );
    candidate.has_failure = true;
    candidate.has_result = true;
    assert_eq!(
        candidate.validate(),
        Err(InvocationFactsError::FailedOutcome)
    );
}

#[test]
fn cancellation_and_orphaning_cannot_expose_a_result() {
    for status in [InvocationStatus::Cancelled, InvocationStatus::Orphaned] {
        let mut candidate = facts(status);
        candidate.has_result = true;
        assert_eq!(
            candidate.validate(),
            Err(InvocationFactsError::CancelledOrOrphanedOutcome),
            "{status:?}"
        );

        candidate.has_result = false;
        candidate.has_failure = true;
        assert_eq!(candidate.validate(), Ok(()), "{status:?}");
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
