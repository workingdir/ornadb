use std::time::{Duration, SystemTime};

use orna_core::{
    CatalogueRevisionId, FunctionId, InspectEpochId, InvocationId, PrincipalId, SourceRevisionId,
    StateSlotId, TypeId,
    inspect::{
        InspectClassifier, InspectError, InspectOutcomeKind, InspectPrivilege,
        InspectResultSummary, InspectSnapshotEpoch, InspectSnapshotOptions, InspectSnapshotSummary,
        StateCellRow,
    },
    invocation::InvokeValue,
    security::{InspectDecision, InspectDenial, InspectEpochScope, authorise_inspect},
    state::UserStateKeyWithoutPrincipal,
    value::RuntimeValue,
};

const SESSION: PrincipalId = PrincipalId::from_bytes([0x11; 16]);
const FOREIGN: PrincipalId = PrincipalId::from_bytes([0x22; 16]);

#[test]
fn inspect_privileges_keep_scope_ladder_separate_from_classifiers() {
    assert_eq!(InspectPrivilege::OwnInvocation.ladder_rank(), Some(0));
    assert_eq!(InspectPrivilege::SessionInvocations.ladder_rank(), Some(1));
    assert_eq!(InspectPrivilege::AnyInvocation.ladder_rank(), Some(2));
    assert_eq!(
        InspectPrivilege::OwnInvocation.ladder_rank().unwrap() + 1,
        InspectPrivilege::SessionInvocations.ladder_rank().unwrap()
    );
    assert_eq!(
        InspectPrivilege::SessionInvocations.ladder_rank().unwrap() + 1,
        InspectPrivilege::AnyInvocation.ladder_rank().unwrap()
    );

    let classifiers = [
        (InspectPrivilege::Values, InspectClassifier::Values),
        (InspectPrivilege::Source, InspectClassifier::Source),
        (
            InspectPrivilege::SecurityDetails,
            InspectClassifier::SecurityDetails,
        ),
        (
            InspectPrivilege::RuntimeInternals,
            InspectClassifier::RuntimeInternals,
        ),
    ];
    for (privilege, classifier) in classifiers {
        assert!(privilege.is_classifier());
        assert!(!privilege.is_invocation_scope());
        assert_eq!(privilege.ladder_rank(), None);
        assert_eq!(privilege.classifier(), Some(classifier));
    }

    for privilege in [
        InspectPrivilege::OwnInvocation,
        InspectPrivilege::SessionInvocations,
        InspectPrivilege::AnyInvocation,
    ] {
        assert!(privilege.is_invocation_scope());
        assert!(!privilege.is_classifier());
        assert_eq!(privilege.classifier(), None);
    }
}

#[test]
fn inspect_authorization_requires_both_scope_and_requested_classifier() {
    assert_eq!(
        authorise_inspect(
            SESSION,
            InspectPrivilege::OwnInvocation,
            Some(SESSION),
            &[InspectPrivilege::Values],
        ),
        InspectDecision::Denied(InspectDenial::MissingPrivilege)
    );
    assert_eq!(
        authorise_inspect(
            SESSION,
            InspectPrivilege::Values,
            Some(SESSION),
            &[InspectPrivilege::OwnInvocation],
        ),
        InspectDecision::Denied(InspectDenial::MissingPrivilege)
    );
    assert_eq!(
        authorise_inspect(
            SESSION,
            InspectPrivilege::Values,
            Some(SESSION),
            &[InspectPrivilege::OwnInvocation, InspectPrivilege::Values],
        ),
        InspectDecision::Allowed {
            epoch_scope: InspectEpochScope::Own,
            requested: InspectPrivilege::Values,
        }
    );
    assert_eq!(
        authorise_inspect(
            SESSION,
            InspectPrivilege::Source,
            None,
            &[
                InspectPrivilege::SessionInvocations,
                InspectPrivilege::Source
            ],
        ),
        InspectDecision::Allowed {
            epoch_scope: InspectEpochScope::Session,
            requested: InspectPrivilege::Source,
        }
    );
    assert_eq!(
        authorise_inspect(
            SESSION,
            InspectPrivilege::RuntimeInternals,
            Some(FOREIGN),
            &[
                InspectPrivilege::AnyInvocation,
                InspectPrivilege::RuntimeInternals
            ],
        ),
        InspectDecision::Allowed {
            epoch_scope: InspectEpochScope::Foreign,
            requested: InspectPrivilege::RuntimeInternals,
        }
    );
    assert_eq!(
        authorise_inspect(
            SESSION,
            InspectPrivilege::Source,
            None,
            &[
                InspectPrivilege::SessionInvocations,
                InspectPrivilege::Values
            ],
        ),
        InspectDecision::Denied(InspectDenial::MissingPrivilege)
    );
}

fn state_cell() -> StateCellRow {
    let key = UserStateKeyWithoutPrincipal::new(
        FunctionId::from_bytes([0x31; 16]),
        "default".to_owned(),
        FunctionId::from_bytes([0x32; 16]),
        "instance".to_owned(),
        StateSlotId::from_bytes([0x33; 16]),
    )
    .expect("fixture key is valid");
    let value = InvokeValue::new(RuntimeValue::Integer(7)).expect("fixture value is valid");
    StateCellRow::new(
        key,
        TypeId::from_bytes([0x34; 16]),
        9,
        SystemTime::UNIX_EPOCH + Duration::from_secs(42),
        Some(value),
    )
}

fn epoch(options: InspectSnapshotOptions) -> InspectSnapshotEpoch {
    let epoch_id = InspectEpochId::from_bytes([0x41; 16]);
    let summary = InspectSnapshotSummary::new(
        3,
        InspectResultSummary::ValueBatch { value_count: 1 },
        Some(27),
    )
    .expect("fixture summary is valid");
    InspectSnapshotEpoch::new(
        epoch_id,
        InvocationId::from_bytes([0x42; 16]),
        SourceRevisionId::from_bytes([0x43; 16]),
        CatalogueRevisionId::from_bytes([0x44; 16]),
        SESSION,
        SystemTime::UNIX_EPOCH + Duration::from_secs(100),
        FunctionId::from_bytes([0x45; 16]),
        InspectOutcomeKind::Allowed,
        summary,
        &options,
        Vec::new(),
        Vec::new(),
        Vec::new(),
        vec![state_cell()],
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    )
    .expect("fixture epoch is valid")
}

#[test]
fn structural_epoch_redacts_values_without_erasing_cell_structure() {
    let structural = epoch(InspectSnapshotOptions::structural());
    let with_values = epoch(InspectSnapshotOptions::new(true, false, false, false));

    let redacted = &structural.state_cells()[0];
    let retained = &with_values.state_cells()[0];
    assert!(redacted.value().is_none());
    assert_eq!(redacted.key(), retained.key());
    assert_eq!(redacted.value_type(), retained.value_type());
    assert_eq!(redacted.revision(), retained.revision());
    assert_eq!(redacted.updated_at(), retained.updated_at());
    assert_eq!(
        retained.value().expect("values were requested").value(),
        &RuntimeValue::Integer(7)
    );
}

#[test]
fn epoch_exposes_pinned_metadata_and_rejects_empty_or_zero_value_batch_facts() {
    let inspected = epoch(InspectSnapshotOptions::structural());
    assert_eq!(inspected.id(), InspectEpochId::from_bytes([0x41; 16]));
    assert_eq!(
        inspected.invocation_id(),
        InvocationId::from_bytes([0x42; 16])
    );
    assert_eq!(
        inspected.source_revision_id(),
        SourceRevisionId::from_bytes([0x43; 16])
    );
    assert_eq!(
        inspected.catalogue_revision_id(),
        CatalogueRevisionId::from_bytes([0x44; 16])
    );
    assert_eq!(inspected.owner(), SESSION);
    assert_eq!(
        inspected.recorded_at(),
        SystemTime::UNIX_EPOCH + Duration::from_secs(100)
    );
    assert_eq!(inspected.root_target(), FunctionId::from_bytes([0x45; 16]));
    assert_eq!(inspected.outcome(), InspectOutcomeKind::Allowed);
    assert_eq!(inspected.summary().event_count(), 3);
    assert_eq!(
        inspected.summary().result(),
        InspectResultSummary::ValueBatch { value_count: 1 }
    );
    assert_eq!(inspected.summary().duration_nanoseconds(), Some(27));

    let empty_epoch_id = InspectEpochId::from_bytes([0x51; 16]);
    let summary = InspectSnapshotSummary::new(0, InspectResultSummary::NoValues, None)
        .expect("empty event summary is valid independently of epoch rows");
    let empty_epoch_result = InspectSnapshotEpoch::new(
        empty_epoch_id,
        InvocationId::from_bytes([0x52; 16]),
        SourceRevisionId::from_bytes([0x53; 16]),
        CatalogueRevisionId::from_bytes([0x54; 16]),
        SESSION,
        SystemTime::UNIX_EPOCH,
        FunctionId::from_bytes([0x55; 16]),
        InspectOutcomeKind::Denied,
        summary,
        &InspectSnapshotOptions::structural(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    );
    assert!(matches!(
        empty_epoch_result,
        Err(InspectError::EmptyEpoch { id }) if id == empty_epoch_id
    ));
    assert_eq!(
        InspectSnapshotSummary::new(1, InspectResultSummary::ValueBatch { value_count: 0 }, None,),
        Err(InspectError::EmptyValueBatch)
    );
}
