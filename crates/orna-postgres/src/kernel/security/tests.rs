use super::audit_writer::resource_parent_invocation_unavailable;
use super::raw_call::{classify_raw_server_insert_error, validate_raw_call_argument_shape};
use super::resource::MAX_RESOURCE_CREDIT;
use super::resource_producer::ResourceProducerStartGuard;
use super::*;
use orna_core::{
    CatalogueRevisionId, FieldId, ObjectId, ParameterId, TypeId,
    catalogue::{CatalogueSnapshot, EnumTypeDefinition, QualifiedSemanticName, SchemaDefinition},
    security::PrivilegeDecision,
    system::{SYS_SECURITY_CREATE_PRINCIPAL_FUNCTION_ID, SYS_SECURITY_GRANT_PRIVILEGE_FUNCTION_ID},
    value::{EnumValue, ResultColumn, ResultRow, ResultRows, RuntimeFloat},
};
use std::time::UNIX_EPOCH;

const RAW_CALL_FUNCTION: FunctionId = FunctionId::from_bytes([0x61; 16]);
const RAW_CALL_PARAMETER: ParameterId = ParameterId::from_bytes([0x62; 16]);

fn resource_request_lineage_fixture() -> ResourceRequest {
    ResourceRequest {
        stream_id: 1,
        request_id: InvocationId::from_bytes([0x01; 16]),
        parent_invocation_id: InvocationId::from_bytes([0x02; 16]),
        call_site_id: orna_core::CallSiteId::from_bytes([0x03; 16]),
        state_profile: String::new(),
        function_instance_key: String::new(),
        target_function_id: RAW_CALL_FUNCTION,
        target_revision: RevisionPair::new(
            SourceRevisionId::from_bytes([0x04; 16]),
            CatalogueRevisionId::from_bytes([0x05; 16]),
        ),
        generation: 1,
        resource_kind: ProtocolResourceKind::Single,
        arguments: Vec::new(),
        item_window: 1,
        byte_window: 1,
    }
}

#[test]
fn resource_lineage_validation_rejects_zero_parent_before_other_request_validation() {
    let mut request = resource_request_lineage_fixture();
    request.parent_invocation_id = InvocationId::from_bytes([0; 16]);
    request.state_profile = "invalid\0profile".to_owned();
    let before = request.clone();
    let record = request.request_id.canonical();

    assert!(matches!(
        validate_resource_lineage(&request),
        Err(PostgresKernelError::DurableInvariant {
            relation: "resource request",
            record: ref actual_record,
            rule: "resource parent invocation identity must be non-zero",
        }) if actual_record == &record
    ));
    assert_eq!(request, before);
}

#[test]
fn resource_lineage_validation_rejects_zero_request_id_before_other_lineage_checks() {
    let mut request = resource_request_lineage_fixture();
    request.request_id = InvocationId::from_bytes([0; 16]);
    request.state_profile = "invalid\0profile".to_owned();
    let before = request.clone();
    let record = request.request_id.canonical();

    assert!(matches!(
        validate_resource_lineage(&request),
        Err(PostgresKernelError::DurableInvariant {
            relation: "resource request",
            record: ref actual_record,
            rule: "resource request identity must be non-zero",
        }) if actual_record == &record
    ));
    assert_eq!(request, before);
}

#[test]
fn resource_lineage_validation_rejects_zero_call_site_before_other_request_validation() {
    let mut request = resource_request_lineage_fixture();
    request.call_site_id = orna_core::CallSiteId::from_bytes([0; 16]);
    request.function_instance_key = "invalid\0instance".to_owned();
    let before = request.clone();
    let record = request.request_id.canonical();

    assert!(matches!(
        validate_resource_lineage(&request),
        Err(PostgresKernelError::DurableInvariant {
            relation: "resource request",
            record: ref actual_record,
            rule: "resource call-site identity must be non-zero",
        }) if actual_record == &record
    ));
    assert_eq!(request, before);
}

#[test]
fn resource_parent_provenance_rejection_is_closed_without_mutation() {
    let request = resource_request_lineage_fixture();
    let before = request.clone();
    let error = resource_parent_invocation_unavailable(&request);

    assert!(matches!(
        error,
        PostgresKernelError::DurableInvariant {
            relation: "_orna_kernel.invocation_audit_events",
            record,
            rule: "resource parent invocation must belong to authenticated session",
        } if record == request.request_id.canonical()
    ));
    assert_eq!(request, before);
}

#[test]
fn resource_lineage_validation_accepts_non_zero_identities_without_mutation() {
    let request = resource_request_lineage_fixture();
    let before = request.clone();

    validate_resource_lineage(&request).expect("non-zero resource lineage must be accepted");
    assert_eq!(request, before);
}

#[test]
fn resource_audit_lineage_validation_rejects_zero_request_id() {
    let error =
        validate_resource_audit_lineage("7", [0; 16], Some([0x04; 16]), [0x02; 16], [0x03; 16])
            .expect_err("zero request identity must be rejected during recovery");

    assert!(matches!(
        error,
        PostgresKernelError::DurableInvariant {
            relation: "_orna_kernel.resource_audit_events",
            record,
            rule: "resource request identity must be non-zero",
        } if record == "7"
    ));
}

#[test]
fn resource_audit_lineage_validation_rejects_zero_parent_invocation_id() {
    let error =
        validate_resource_audit_lineage("7", [0x01; 16], Some([0x04; 16]), [0; 16], [0x03; 16])
            .expect_err("zero parent invocation identity must be rejected during recovery");

    assert!(matches!(
        error,
        PostgresKernelError::DurableInvariant {
            relation: "_orna_kernel.resource_audit_events",
            record,
            rule: "resource parent invocation identity must be non-zero",
        } if record == "7"
    ));
}

#[test]
fn resource_audit_lineage_validation_rejects_zero_call_site_id() {
    let error =
        validate_resource_audit_lineage("7", [0x01; 16], Some([0x04; 16]), [0x02; 16], [0; 16])
            .expect_err("zero call-site identity must be rejected during recovery");

    assert!(matches!(
        error,
        PostgresKernelError::DurableInvariant {
            relation: "_orna_kernel.resource_audit_events",
            record,
            rule: "resource call-site identity must be non-zero",
        } if record == "7"
    ));
}

#[test]
fn resource_audit_lineage_validation_rejects_zero_nested_invocation_id() {
    let error =
        validate_resource_audit_lineage("7", [0x01; 16], Some([0; 16]), [0x02; 16], [0x03; 16])
            .expect_err("zero nested invocation identity must be rejected during recovery");

    assert!(matches!(
        error,
        PostgresKernelError::DurableInvariant {
            relation: "_orna_kernel.resource_audit_events",
            record,
            rule: "resource nested invocation identity must be non-zero",
        } if record == "7"
    ));
}

#[test]
fn resource_audit_insertion_validation_rejects_zero_nested_invocation_id() {
    let request = resource_request_lineage_fixture();
    let error = validate_resource_audit_nested_invocation(
        "resource request",
        request.request_id.canonical(),
        Some([0; 16]),
    )
    .expect_err("zero nested invocation identity must be rejected before audit insertion");

    assert!(matches!(
        error,
        PostgresKernelError::DurableInvariant {
            relation: "resource request",
            record,
            rule: "resource nested invocation identity must be non-zero",
        } if record == request.request_id.canonical()
    ));
}

#[test]
fn resource_audit_lineage_validation_accepts_non_zero_identities() {
    validate_resource_audit_lineage("7", [0x01; 16], Some([0x04; 16]), [0x02; 16], [0x03; 16])
        .expect("non-zero resource audit lineage must be accepted");
}

#[test]
fn resource_audit_lineage_validation_accepts_absent_nested_identity() {
    validate_resource_audit_lineage("7", [0x01; 16], None, [0x02; 16], [0x03; 16])
        .expect("preaccept resource audit may omit nested identity");
}

#[test]
fn principal_kind_decoder_round_trips_closed_vocabulary() {
    for (expected, stored) in [
        (PrincipalKind::User, "user"),
        (PrincipalKind::Role, "role"),
        (PrincipalKind::Service, "service"),
    ] {
        assert_eq!(encode_principal_kind(expected), stored);
        assert_eq!(
            decode_principal_kind(stored.to_owned()).expect("closed principal kind must decode"),
            expected
        );
    }

    for stored in ["other", "User", "ROLE", "Service"] {
        assert!(matches!(
            decode_principal_kind(stored.to_owned()),
            Err(PostgresKernelError::DurableInvariant {
                relation: "_orna_kernel.security_principals",
                ref record,
                rule: "principal kind must be user, role, or service",
            }) if record == stored
        ));
    }
}

#[test]
fn principal_status_decoder_round_trips_closed_vocabulary() {
    for (expected, stored) in [
        (PrincipalStatus::Active, "active"),
        (PrincipalStatus::Disabled, "disabled"),
    ] {
        assert_eq!(encode_principal_status(expected), stored);
        assert_eq!(
            decode_principal_status(stored.to_owned())
                .expect("closed principal status must decode"),
            expected
        );
    }

    for stored in ["other", "Active", "DISABLED"] {
        assert!(matches!(
            decode_principal_status(stored.to_owned()),
            Err(PostgresKernelError::DurableInvariant {
                relation: "_orna_kernel.security_principals",
                ref record,
                rule: "principal status must be active or disabled",
            }) if record == stored
        ));
    }
}

#[test]
fn resource_target_shape_matches_protocol_kind() {
    use orna_core::{
        catalogue::{
            FunctionReturn, FunctionReturnColumnDefinition, FunctionSecurity, FunctionTransaction,
            FunctionVolatility, QualifiedSemanticName,
        },
        types::{ResolvedType, StandardScalar},
    };

    let function = FunctionDefinition::new(
        RAW_CALL_FUNCTION,
        QualifiedSemanticName::new(["app", "resource"]).expect("function name"),
        FunctionDomain::Server,
        Vec::new(),
        FunctionReturn::Single(ResolvedType::scalar(StandardScalar::Integer)),
        FunctionRevisionId::new(),
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::ReadOnly),
        FunctionVolatility::Stable,
    );
    assert!(resource_target_shape_is_supported(
        &function,
        ProtocolResourceKind::Single,
    ));
    assert!(!resource_target_shape_is_supported(
        &function,
        ProtocolResourceKind::Stream,
    ));

    let stream = FunctionDefinition::new(
        RAW_CALL_FUNCTION,
        QualifiedSemanticName::new(["app", "stream"]).expect("function name"),
        FunctionDomain::Server,
        Vec::new(),
        FunctionReturn::Stream(ResolvedType::scalar(StandardScalar::Integer)),
        FunctionRevisionId::new(),
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::ReadOnly),
        FunctionVolatility::Stable,
    );
    assert!(resource_target_shape_is_supported(
        &stream,
        ProtocolResourceKind::Stream,
    ));
    assert!(!resource_target_shape_is_supported(
        &stream,
        ProtocolResourceKind::Single,
    ));
    let rows = FunctionDefinition::new(
        RAW_CALL_FUNCTION,
        QualifiedSemanticName::new(["app", "finite_rows"]).expect("function name"),
        FunctionDomain::Server,
        Vec::new(),
        FunctionReturn::Rows(vec![FunctionReturnColumnDefinition::new(
            "value",
            0,
            ResolvedType::scalar(StandardScalar::Integer),
        )]),
        FunctionRevisionId::new(),
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::ReadOnly),
        FunctionVolatility::Stable,
    );
    assert!(!resource_target_shape_is_supported(
        &rows,
        ProtocolResourceKind::Stream,
    ));

    let multi_rows = FunctionDefinition::new(
        RAW_CALL_FUNCTION,
        QualifiedSemanticName::new(["app", "multi_stream"]).expect("function name"),
        FunctionDomain::Server,
        Vec::new(),
        FunctionReturn::Rows(vec![
            FunctionReturnColumnDefinition::new(
                "first",
                0,
                ResolvedType::scalar(StandardScalar::Integer),
            ),
            FunctionReturnColumnDefinition::new(
                "second",
                1,
                ResolvedType::scalar(StandardScalar::Integer),
            ),
        ]),
        FunctionRevisionId::new(),
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::ReadOnly),
        FunctionVolatility::Stable,
    );
    assert!(!resource_target_shape_is_supported(
        &multi_rows,
        ProtocolResourceKind::Stream,
    ));
    let client_scalar = FunctionDefinition::new(
        RAW_CALL_FUNCTION,
        QualifiedSemanticName::new(["app", "client_scalar"]).expect("function name"),
        FunctionDomain::Client,
        Vec::new(),
        FunctionReturn::Single(ResolvedType::scalar(StandardScalar::Integer)),
        FunctionRevisionId::new(),
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::ReadOnly),
        FunctionVolatility::Stable,
    );
    let client_stream = FunctionDefinition::new(
        RAW_CALL_FUNCTION,
        QualifiedSemanticName::new(["app", "client_stream"]).expect("function name"),
        FunctionDomain::Client,
        Vec::new(),
        FunctionReturn::Stream(ResolvedType::scalar(StandardScalar::Integer)),
        FunctionRevisionId::new(),
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::ReadOnly),
        FunctionVolatility::Stable,
    );
    for (label, definition) in [
        ("CLIENT scalar", client_scalar),
        ("CLIENT stream", client_stream),
    ] {
        let before_definition = definition.clone();
        for kind in [ProtocolResourceKind::Single, ProtocolResourceKind::Stream] {
            let mut request = resource_request_lineage_fixture();
            request.resource_kind = kind;
            let before_request = request.clone();
            let mut dispatch_attempted = false;

            // Model the production branch: only a supported shape may
            // mutate the request or enter resource dispatch.
            if resource_target_shape_is_supported(&definition, kind) {
                dispatch_attempted = true;
                request.generation += 1;
            }

            assert!(!dispatch_attempted, "{label} must not dispatch as {kind:?}");
            assert_eq!(request, before_request, "{label} request was mutated");
            assert_eq!(definition, before_definition, "{label} target was mutated");
        }
    }
    assert_eq!(
        sealed_server_result_kind(rows.return_type()),
        Some(ProtocolResourceKind::Stream),
    );
}

#[test]
fn resource_result_rejects_rows_with_extra_columns() {
    use orna_core::types::{ResolvedType, StandardScalar};

    let pair = RevisionPair::new(
        SourceRevisionId::from_bytes([0x71; 16]),
        CatalogueRevisionId::from_bytes([0x72; 16]),
    );
    let rows = ResultRows::new(
        [
            ResultColumn::new(
                "first",
                ResolvedType::scalar(StandardScalar::Integer),
                false,
            )
            .expect("first column is valid"),
            ResultColumn::new(
                "second",
                ResolvedType::scalar(StandardScalar::Integer),
                false,
            )
            .expect("second column is valid"),
        ],
        [ResultRow::new([
            RuntimeValue::Integer(1),
            RuntimeValue::Integer(2),
        ])],
    )
    .expect("two-column result rows are valid");
    let result = ServerSelectResult::new(
        pair,
        RAW_CALL_FUNCTION,
        FunctionRevisionId::from_bytes([0x73; 16]),
        rows,
    );

    assert!(resource_values_from_server_result(ProtocolResourceKind::Stream, result).is_none());
}

#[test]
fn resource_arguments_require_canonical_complete_typed_set() {
    use orna_core::{
        catalogue::{
            FunctionSecurity, FunctionTransaction, FunctionVolatility, ParameterDefinition,
            QualifiedSemanticName,
        },
        types::{ResolvedType, StandardScalar},
    };

    let first = ParameterId::from_bytes([0x01; 16]);
    let second = ParameterId::from_bytes([0x02; 16]);
    let function = FunctionDefinition::new(
        RAW_CALL_FUNCTION,
        QualifiedSemanticName::new(["app", "resource"]).expect("function name"),
        FunctionDomain::Server,
        vec![
            ParameterDefinition::new(
                first,
                "first",
                0,
                ResolvedType::scalar(StandardScalar::Integer),
                None,
            ),
            ParameterDefinition::new(
                second,
                "second",
                1,
                ResolvedType::scalar(StandardScalar::Boolean),
                None,
            ),
        ],
        FunctionReturn::Rows(Vec::new()),
        FunctionRevisionId::new(),
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::ReadOnly),
        FunctionVolatility::Stable,
    );
    let canonical = vec![
        ResourceArgument {
            parameter: first,
            value: RuntimeValue::Integer(7),
        },
        ResourceArgument {
            parameter: second,
            value: RuntimeValue::Boolean(true),
        },
    ];
    assert!(
        bind_authenticated_resource_arguments(
            &CatalogueHashContext::version_one(),
            &function,
            &canonical,
        )
        .is_some()
    );

    let wrong_order = vec![canonical[1].clone(), canonical[0].clone()];
    assert!(
        bind_authenticated_resource_arguments(
            &CatalogueHashContext::version_one(),
            &function,
            &wrong_order,
        )
        .is_none()
    );
    let wrong_type = vec![
        ResourceArgument {
            parameter: first,
            value: RuntimeValue::Boolean(true),
        },
        canonical[1].clone(),
    ];
    assert!(
        bind_authenticated_resource_arguments(
            &CatalogueHashContext::version_one(),
            &function,
            &wrong_type,
        )
        .is_none()
    );
    assert!(
        bind_authenticated_resource_arguments(
            &CatalogueHashContext::version_one(),
            &function,
            &canonical[..1],
        )
        .is_none()
    );
}

#[test]
fn invocation_audit_decision_uses_only_closed_execute_evidence() {
    let target = InvocationTarget::new(
        FunctionId::from_bytes([0x81; 16]),
        RevisionPair::new(
            SourceRevisionId::from_bytes([0x82; 16]),
            CatalogueRevisionId::from_bytes([0x83; 16]),
        ),
    );
    let evidence = SecurityAuditEvent::new(
        SecurityAuditEventId::from_bytes([0x84; 16]),
        1,
        UNIX_EPOCH,
        SecurityAuditDecision::recover_execute_allowed(
            PrincipalId::from_bytes([0x85; 16]),
            PrincipalId::from_bytes([0x86; 16]),
            PrincipalId::from_bytes([0x87; 16]),
            target,
        ),
    );
    let decision = InvocationAuditDecision::from_execute_evidence(
        InvocationId::from_bytes([0x88; 16]),
        &evidence,
    )
    .expect("allowed EXECUTE evidence must create one invocation decision");
    assert_eq!(decision.outcome, SecurityAuditOutcome::Allowed);
    assert_eq!(decision.target, Some(target));
    assert_eq!(decision.security_audit_event, Some(evidence.id()));

    let authentication = SecurityAuditEvent::new(
        SecurityAuditEventId::from_bytes([0x89; 16]),
        2,
        UNIX_EPOCH,
        SecurityAuditDecision::recover_authentication_allowed(PrincipalId::from_bytes([0x85; 16])),
    );
    assert!(matches!(
        InvocationAuditDecision::from_execute_evidence(
            InvocationId::from_bytes([0x8a; 16]),
            &authentication,
        ),
        Err(PostgresKernelError::DurableInvariant {
            relation: "_orna_kernel.invocation_audit_events",
            rule: "invocation decision requires EXECUTE audit evidence",
            ..
        })
    ));

    let unresolved = InvocationAuditDecision::unresolved_denied(
        InvocationId::from_bytes([0x8b; 16]),
        PrincipalId::from_bytes([0x85; 16]),
    );
    validate_invocation_audit_decision_shape(&unresolved, "test")
        .expect("unresolved denied decision must remain closed");
}

#[test]
fn sealed_invocation_security_guard_rejects_security_definer_targets() {
    use orna_core::{
        catalogue::{
            FunctionSecurity, FunctionTransaction, FunctionVolatility, QualifiedSemanticName,
        },
        types::{ResolvedType, StandardScalar},
    };

    let definition = |security| {
        FunctionDefinition::new(
            FunctionId::from_bytes([0xf2; 16]),
            QualifiedSemanticName::new(["app", "definer_guard"]).expect("function name"),
            FunctionDomain::Server,
            Vec::new(),
            FunctionReturn::Single(ResolvedType::scalar(StandardScalar::Integer)),
            FunctionRevisionId::from_bytes([0xf3; 16]),
            security,
            Some(FunctionTransaction::ReadOnly),
            FunctionVolatility::Stable,
        )
    };

    let definer = definition(FunctionSecurity::Definer);
    let invoker = definition(FunctionSecurity::Invoker);
    assert!(!sealed_target_security_is_supported(
        SealedResolvedTarget::Application(&definer),
    ));
    assert!(sealed_target_security_is_supported(
        SealedResolvedTarget::Application(&invoker),
    ));
    assert!(!resource_target_security_is_supported(&definer));
    assert!(resource_target_security_is_supported(&invoker));
}

#[test]
fn sealed_invocation_audit_recovery_rejects_partial_target_tuple() {
    let invocation = InvocationId::from_bytes([0xb1; 16]);
    let decision = InvocationAuditDecision {
        invocation,
        outcome: SecurityAuditOutcome::Allowed,
        session_principal: PrincipalId::from_bytes([0xb2; 16]),
        effective_principal: Some(PrincipalId::from_bytes([0xb3; 16])),
        authorising_principal: Some(PrincipalId::from_bytes([0xb4; 16])),
        target: Some(InvocationTarget::new(
            FunctionId::from_bytes([0xb5; 16]),
            RevisionPair::new(
                SourceRevisionId::from_bytes([0xb6; 16]),
                CatalogueRevisionId::from_bytes([0xb7; 16]),
            ),
        )),
        security_audit_event: None,
    };
    let expected_record = invocation.canonical();

    assert!(matches!(
        validate_invocation_audit_decision_shape(&decision, &expected_record),
        Err(PostgresKernelError::DurableInvariant {
            relation: "_orna_kernel.invocation_audit_events",
            ref record,
            rule: "target, pinned revision, and security audit evidence must be present together",
        }) if record == &expected_record
    ));
}

#[test]
fn sealed_invocation_audit_recovery_rejects_each_malformed_target_tuple() {
    let function = FunctionId::from_bytes([0xb8; 16]);
    let source = SourceRevisionId::from_bytes([0xb9; 16]);
    let catalogue = CatalogueRevisionId::from_bytes([0xba; 16]);
    let expected_record = "sealed-invocation-target-tuple";

    for (function, source, catalogue, expected_rule) in [
        (
            None,
            Some(source),
            Some(catalogue),
            "EXECUTE requires a function",
        ),
        (
            Some(function),
            None,
            Some(catalogue),
            "EXECUTE requires a source revision",
        ),
        (
            Some(function),
            Some(source),
            None,
            "EXECUTE requires a catalogue revision",
        ),
        (None, None, Some(catalogue), "EXECUTE requires a function"),
        (None, Some(source), None, "EXECUTE requires a function"),
        (
            Some(function),
            None,
            None,
            "EXECUTE requires a source revision",
        ),
    ] {
        assert!(matches!(
            audit_target(function, source, catalogue, expected_record),
            Err(PostgresKernelError::DurableInvariant {
                relation: "_orna_kernel.security_audit_events",
                ref record,
                rule,
            }) if record == expected_record && rule == expected_rule
        ));
    }
}

#[test]
fn sealed_invocation_audit_recovery_rejects_malformed_outcome() {
    let expected_record = "sealed-invocation-137";

    assert!(matches!(
        decode_invocation_audit_outcome("corrupted".to_owned(), expected_record),
        Err(PostgresKernelError::DurableInvariant {
            relation: "_orna_kernel.invocation_audit_events",
            ref record,
            rule: "invocation outcome must be allowed or denied",
        }) if record == expected_record
    ));
}

#[test]
fn sealed_invocation_audit_recovery_rejects_missing_linked_security_evidence() {
    let target = InvocationTarget::new(
        FunctionId::from_bytes([0xc1; 16]),
        RevisionPair::new(
            SourceRevisionId::from_bytes([0xc2; 16]),
            CatalogueRevisionId::from_bytes([0xc3; 16]),
        ),
    );
    let evidence = SecurityAuditEvent::new(
        SecurityAuditEventId::from_bytes([0xc4; 16]),
        1,
        UNIX_EPOCH,
        SecurityAuditDecision::recover_execute_allowed(
            PrincipalId::from_bytes([0xc5; 16]),
            PrincipalId::from_bytes([0xc6; 16]),
            PrincipalId::from_bytes([0xc7; 16]),
            target,
        ),
    );
    let invocation = InvocationId::from_bytes([0xc8; 16]);
    let decision = InvocationAuditDecision::from_execute_evidence(invocation, &evidence)
        .expect("matching EXECUTE evidence must form a decision");
    let expected_record = invocation.canonical();

    assert!(matches!(
        validate_invocation_audit_evidence(&decision, &[], &expected_record),
        Err(PostgresKernelError::DurableInvariant {
            relation: "_orna_kernel.invocation_audit_events",
            ref record,
            rule: "linked security audit evidence is missing",
        }) if record == &expected_record
    ));
}

#[test]
fn sealed_invocation_audit_recovery_rejects_mismatched_linked_security_evidence() {
    let expected_target = InvocationTarget::new(
        FunctionId::from_bytes([0xd1; 16]),
        RevisionPair::new(
            SourceRevisionId::from_bytes([0xd2; 16]),
            CatalogueRevisionId::from_bytes([0xd3; 16]),
        ),
    );
    let event_id = SecurityAuditEventId::from_bytes([0xd4; 16]);
    let expected_evidence = SecurityAuditEvent::new(
        event_id,
        1,
        UNIX_EPOCH,
        SecurityAuditDecision::recover_execute_allowed(
            PrincipalId::from_bytes([0xd5; 16]),
            PrincipalId::from_bytes([0xd6; 16]),
            PrincipalId::from_bytes([0xd7; 16]),
            expected_target,
        ),
    );
    let invocation = InvocationId::from_bytes([0xd8; 16]);
    let decision = InvocationAuditDecision::from_execute_evidence(invocation, &expected_evidence)
        .expect("matching EXECUTE evidence must form a decision");
    let mismatched_evidence = SecurityAuditEvent::new(
        event_id,
        2,
        UNIX_EPOCH,
        SecurityAuditDecision::recover_execute_allowed(
            PrincipalId::from_bytes([0xd5; 16]),
            PrincipalId::from_bytes([0xd6; 16]),
            PrincipalId::from_bytes([0xd7; 16]),
            InvocationTarget::new(
                FunctionId::from_bytes([0xe1; 16]),
                RevisionPair::new(
                    SourceRevisionId::from_bytes([0xe2; 16]),
                    CatalogueRevisionId::from_bytes([0xe3; 16]),
                ),
            ),
        ),
    );
    let expected_record = invocation.canonical();

    assert!(matches!(
        validate_invocation_audit_evidence(
            &decision,
            &[mismatched_evidence],
            &expected_record,
        ),
        Err(PostgresKernelError::DurableInvariant {
            relation: "_orna_kernel.invocation_audit_events",
            ref record,
            rule: "linked security audit evidence does not match the invocation decision",
        }) if record == &expected_record
    ));
}

#[test]
fn sealed_invocation_audit_recovery_maps_denied_execute_evidence_exactly() {
    let target = InvocationTarget::new(
        FunctionId::from_bytes([0xe5; 16]),
        RevisionPair::new(
            SourceRevisionId::from_bytes([0xe6; 16]),
            CatalogueRevisionId::from_bytes([0xe7; 16]),
        ),
    );
    let session_principal = PrincipalId::from_bytes([0xe8; 16]);
    let reason = ExecuteDenial::MissingExecuteGrant;
    let evidence_id = SecurityAuditEventId::from_bytes([0xe9; 16]);
    let evidence = SecurityAuditEvent::new(
        evidence_id,
        9,
        UNIX_EPOCH,
        SecurityAuditDecision::recover_execute_denied(session_principal, target, reason),
    );
    let invocation = InvocationId::from_bytes([0xea; 16]);

    let decision = InvocationAuditDecision::from_execute_evidence(invocation, &evidence)
        .expect("denied EXECUTE evidence must create one invocation decision");
    assert_eq!(decision.invocation, invocation);
    assert_eq!(decision.outcome, SecurityAuditOutcome::Denied);
    assert_eq!(decision.session_principal, session_principal);
    assert_eq!(decision.effective_principal, None);
    assert_eq!(decision.authorising_principal, None);
    assert_eq!(decision.target, Some(target));
    assert_eq!(decision.security_audit_event, Some(evidence_id));
    validate_invocation_audit_decision_shape(&decision, &invocation.canonical())
        .expect("denied target evidence must retain its closed shape");
    validate_invocation_audit_evidence(
        &decision,
        std::slice::from_ref(&evidence),
        &invocation.canonical(),
    )
    .expect("denied EXECUTE evidence must map back exactly");
}

#[test]
fn raw_call_argument_shape_accepts_zero_one_and_supported_pairs() {
    validate_raw_call_argument_shape(RAW_CALL_FUNCTION, &[])
        .expect("zero arguments must be accepted");
    for value in [
        RuntimeValue::Boolean(true),
        RuntimeValue::Integer(1),
        RuntimeValue::BigInt(2),
        RuntimeValue::Float(RuntimeFloat::new(3.5).expect("finite Float argument")),
        RuntimeValue::Text("text".to_string()),
        RuntimeValue::Bytes(vec![0x00, 0xff]),
    ] {
        let argument = FunctionArgument::new(RAW_CALL_PARAMETER, value)
            .expect("supported scalar argument is valid");
        validate_raw_call_argument_shape(RAW_CALL_FUNCTION, std::slice::from_ref(&argument))
            .expect("one supported scalar argument must be accepted");
    }
    let reference = FunctionArgument::new(
        RAW_CALL_PARAMETER,
        RuntimeValue::Reference {
            target: TypeId::from_bytes([0x65; 16]),
            object: ObjectId::from_bytes([0x66; 16]),
        },
    )
    .expect("Reference argument is valid");
    assert_eq!(reference.parameter(), RAW_CALL_PARAMETER);
    assert_eq!(
        reference.value(),
        &RuntimeValue::Reference {
            target: TypeId::from_bytes([0x65; 16]),
            object: ObjectId::from_bytes([0x66; 16]),
        }
    );
    validate_raw_call_argument_shape(RAW_CALL_FUNCTION, std::slice::from_ref(&reference))
        .expect("one Reference argument must be accepted");

    let supported = [
        RuntimeValue::Boolean(false),
        RuntimeValue::Integer(1),
        RuntimeValue::BigInt(2),
        RuntimeValue::Float(RuntimeFloat::new(3.5).expect("finite Float argument")),
        RuntimeValue::Text("text".to_string()),
        RuntimeValue::Bytes(vec![0x00, 0xff]),
        reference.value().clone(),
    ];
    for (index, value) in supported.into_iter().enumerate() {
        let pair = [
            FunctionArgument::new(RAW_CALL_PARAMETER, RuntimeValue::Boolean(true))
                .expect("Boolean argument is valid"),
            FunctionArgument::new(ParameterId::from_bytes([0x70 + index as u8; 16]), value)
                .expect("supported pair argument is valid"),
        ];
        validate_raw_call_argument_shape(RAW_CALL_FUNCTION, &pair)
            .expect("a pair of supported arguments must be accepted");
    }
}

#[test]
fn raw_call_argument_shape_rejects_other_argument_sets() {
    let enum_type = TypeId::from_bytes([0x67; 16]);
    let catalogue = CatalogueSnapshot::new_with_enum_types(
        CatalogueRevisionId::new(),
        vec![SchemaDefinition::new(
            orna_core::SchemaId::new(),
            QualifiedSemanticName::new(["app"]).expect("schema name"),
        )],
        Vec::new(),
        Vec::new(),
        vec![EnumTypeDefinition::new(
            enum_type,
            QualifiedSemanticName::new(["app", "stage"]).expect("qualified enum name"),
            ["lead"],
        )],
        Vec::new(),
    )
    .expect("enum catalogue");
    let enum_argument = FunctionArgument::new(
        RAW_CALL_PARAMETER,
        RuntimeValue::Enum(
            EnumValue::new(&catalogue, enum_type, "lead").expect("declared enum label"),
        ),
    )
    .expect("Enum argument is valid");
    assert!(matches!(
        validate_raw_call_argument_shape(RAW_CALL_FUNCTION, std::slice::from_ref(&enum_argument),)
            .expect_err("one Enum argument must be rejected"),
        PostgresKernelError::RawCallTargetUnavailable {
            function: RAW_CALL_FUNCTION,
            rule: "raw calls accept zero arguments, one supported value, or one supported argument pair",
        }
    ));

    let unsupported_pair = [
        FunctionArgument::new(RAW_CALL_PARAMETER, RuntimeValue::Boolean(true))
            .expect("Boolean argument is valid"),
        enum_argument.clone(),
    ];
    assert!(matches!(
        validate_raw_call_argument_shape(RAW_CALL_FUNCTION, &unsupported_pair)
            .expect_err("a pair with an Enum argument must be rejected"),
        PostgresKernelError::RawCallTargetUnavailable {
            function: RAW_CALL_FUNCTION,
            rule: "raw calls accept zero arguments, one supported value, or one supported argument pair",
        }
    ));

    let three = [
        FunctionArgument::new(RAW_CALL_PARAMETER, RuntimeValue::Boolean(true))
            .expect("Boolean argument is valid"),
        FunctionArgument::new(
            ParameterId::from_bytes([0x64; 16]),
            RuntimeValue::Boolean(false),
        )
        .expect("Boolean argument is valid"),
        FunctionArgument::new(
            ParameterId::from_bytes([0x65; 16]),
            RuntimeValue::Boolean(true),
        )
        .expect("Boolean argument is valid"),
    ];
    assert!(matches!(
        validate_raw_call_argument_shape(RAW_CALL_FUNCTION, &three)
            .expect_err("three arguments must be rejected"),
        PostgresKernelError::RawCallTargetUnavailable {
            function: RAW_CALL_FUNCTION,
            rule: "raw calls accept zero arguments, one supported value, or one supported argument pair",
        }
    ));
}

#[test]
fn raw_insert_argument_errors_classify_to_generic_unavailable() {
    let argument_error = PostgresKernelError::ServerInsert(crate::ServerInsertError::Argument {
        parameter: Some(RAW_CALL_PARAMETER),
        rule: "an argument was supplied for a parameter that this function does not declare",
    });
    assert!(matches!(
        classify_raw_server_insert_error(argument_error, true, RAW_CALL_FUNCTION),
        PostgresKernelError::RawCallTargetUnavailable {
            function: RAW_CALL_FUNCTION,
            rule: "raw SERVER INSERT argument target is unavailable",
        }
    ));

    let missing_required = PostgresKernelError::ServerInsert(crate::ServerInsertError::Argument {
        parameter: Some(RAW_CALL_PARAMETER),
        rule: "a required argument is missing",
    });
    assert!(matches!(
        classify_raw_server_insert_error(missing_required, false, RAW_CALL_FUNCTION),
        PostgresKernelError::RawCallTargetUnavailable {
            function: RAW_CALL_FUNCTION,
            rule: "raw SERVER INSERT argument target is unavailable",
        }
    ));
}

#[test]
fn sealed_server_error_classification_preserves_internal_failures() {
    let pair = RevisionPair::new(
        SourceRevisionId::from_bytes([0x71; 16]),
        CatalogueRevisionId::from_bytes([0x72; 16]),
    );
    let target = PostgresKernelError::ServerUpdate(crate::ServerUpdateError::FunctionNotActive {
        pair,
        function: RAW_CALL_FUNCTION,
    });
    assert_eq!(
        classify_sealed_server_error(&target),
        SealedInvocationFailureClass::Target
    );

    let internal = PostgresKernelError::ServerUpdate(crate::ServerUpdateError::Unavailable {
        source: Box::new(PostgresKernelError::DurableInvariant {
            relation: "test relation",
            record: "test record".to_owned(),
            rule: "test rule",
        }),
    });
    assert_eq!(
        classify_sealed_server_error(&internal),
        SealedInvocationFailureClass::Internal
    );
}

#[test]
fn raw_insert_parameter_free_target_failure_stays_typed() {
    let pair = RevisionPair::new(
        SourceRevisionId::from_bytes([0x71; 16]),
        CatalogueRevisionId::from_bytes([0x72; 16]),
    );
    let target_error =
        PostgresKernelError::ServerInsert(crate::ServerInsertError::FunctionNotActive {
            pair,
            function: RAW_CALL_FUNCTION,
        });
    assert!(matches!(
        classify_raw_server_insert_error(target_error, false, RAW_CALL_FUNCTION),
        PostgresKernelError::RawServerTargetUnavailable {
            source: RawServerTargetError::Insert(
                crate::ServerInsertError::FunctionNotActive {
                    pair: actual_pair,
                    function: RAW_CALL_FUNCTION,
                },
            ),
        } if actual_pair == pair
    ));
}

#[test]
fn raw_insert_operational_error_stays_unchanged() {
    let operational = PostgresKernelError::ServerInsert(crate::ServerInsertError::Kernel {
        source: Box::new(PostgresKernelError::DurableInvariant {
            relation: "test relation",
            record: "test record".to_owned(),
            rule: "test rule",
        }),
    });
    assert!(matches!(
        classify_raw_server_insert_error(operational, true, RAW_CALL_FUNCTION),
        PostgresKernelError::ServerInsert(crate::ServerInsertError::Kernel {
            source,
        }) if matches!(
            *source,
            PostgresKernelError::DurableInvariant {
                relation: "test relation",
                ref record,
                rule: "test rule",
            } if record == "test record"
        )
    ));
}

#[test]
fn raw_insert_value_codec_error_stays_unchanged_with_arguments_present() {
    let unsupported = PostgresKernelError::ServerInsert(crate::ServerInsertError::ValueCodec(
        orna_protocol::ValueCodecError::UnsupportedValue,
    ));
    assert!(matches!(
        classify_raw_server_insert_error(unsupported, true, RAW_CALL_FUNCTION),
        PostgresKernelError::ServerInsert(crate::ServerInsertError::ValueCodec(
            orna_protocol::ValueCodecError::UnsupportedValue,
        ))
    ));
}

#[test]
fn raw_insert_unique_reference_conflict_stays_typed_with_arguments_present() {
    const CONFLICT_OWNER: TypeId = TypeId::from_bytes([0x41; 16]);
    const CONFLICT_FIELD: FieldId = FieldId::from_bytes([0x42; 16]);
    const CONFLICT_REFERENCED: TypeId = TypeId::from_bytes([0x43; 16]);
    let config_error = "port=invalid"
        .parse::<tokio_postgres::Config>()
        .expect_err("invalid port must fail to parse");
    let conflict =
        PostgresKernelError::ServerInsert(crate::ServerInsertError::UniqueReferenceConflict {
            owner: CONFLICT_OWNER,
            field: CONFLICT_FIELD,
            referenced_type: CONFLICT_REFERENCED,
            source: config_error,
        });
    assert!(matches!(
        classify_raw_server_insert_error(conflict, true, RAW_CALL_FUNCTION),
        PostgresKernelError::ServerInsert(crate::ServerInsertError::UniqueReferenceConflict {
            owner: CONFLICT_OWNER,
            field: CONFLICT_FIELD,
            referenced_type: CONFLICT_REFERENCED,
            source,
        }) if source.as_db_error().is_none()
    ));
}

#[test]
fn raw_call_results_transfer_owned_values_in_execution_order() {
    let client = AuthenticatedRawCallResult::Client(RuntimeValue::Boolean(true));
    assert_eq!(client.into_values(), vec![RuntimeValue::Boolean(true)]);

    let server = AuthenticatedRawCallResult::Server(vec![
        RuntimeValue::Integer(1),
        RuntimeValue::Integer(2),
    ]);
    assert_eq!(
        server.into_values(),
        vec![RuntimeValue::Integer(1), RuntimeValue::Integer(2)]
    );
}

#[tokio::test]
async fn empty_record_preflight_does_not_open_postgres() {
    let kernel = "host=127.0.0.1 port=1 dbname=absent"
        .parse::<PostgresKernel>()
        .expect("unavailable test configuration is valid");

    assert_eq!(
        kernel.preflight_record_arguments(Vec::new()).await.unwrap(),
        RecordArgumentPreflight::NotRequired,
    );
}
use orna_core::security::ExecuteDenial;

#[test]
fn audit_denial_decoder_maps_the_complete_closed_vocabulary() {
    let authentication = [
        (
            "authentication_unknown_uid",
            LocalPeerAuthenticationError::UnknownUid,
        ),
        (
            "authentication_unknown_session_principal",
            LocalPeerAuthenticationError::InvalidPrincipal(
                SessionBindingError::UnknownSessionPrincipal,
            ),
        ),
        (
            "authentication_disabled_session_principal",
            LocalPeerAuthenticationError::InvalidPrincipal(
                SessionBindingError::DisabledSessionPrincipal,
            ),
        ),
        (
            "authentication_role_cannot_authenticate",
            LocalPeerAuthenticationError::InvalidPrincipal(
                SessionBindingError::RoleCannotAuthenticate,
            ),
        ),
        (
            "authentication_duplicate_active_role",
            LocalPeerAuthenticationError::InvalidPrincipal(
                SessionBindingError::DuplicateActiveRole,
            ),
        ),
        (
            "authentication_unknown_active_role",
            LocalPeerAuthenticationError::InvalidPrincipal(SessionBindingError::UnknownActiveRole),
        ),
        (
            "authentication_disabled_active_role",
            LocalPeerAuthenticationError::InvalidPrincipal(SessionBindingError::DisabledActiveRole),
        ),
        (
            "authentication_active_principal_is_not_role",
            LocalPeerAuthenticationError::InvalidPrincipal(
                SessionBindingError::ActivePrincipalIsNotRole,
            ),
        ),
        (
            "authentication_unreachable_active_role",
            LocalPeerAuthenticationError::InvalidPrincipal(
                SessionBindingError::UnreachableActiveRole,
            ),
        ),
    ];
    for (stored, expected) in authentication {
        assert_eq!(encode_authentication_audit_denial(expected), stored);
        assert_eq!(
            decode_authentication_audit_denial(stored.to_owned(), "41")
                .expect("closed authentication reason must decode"),
            expected
        );
    }

    for (stored, expected) in [
        ("execute_invalid_session", ExecuteDenial::InvalidSession),
        ("execute_unknown_function", ExecuteDenial::UnknownFunction),
        ("execute_revision_mismatch", ExecuteDenial::RevisionMismatch),
        ("execute_missing_grant", ExecuteDenial::MissingExecuteGrant),
        (
            "execute_unsupported_security_definer",
            ExecuteDenial::UnsupportedSecurityDefiner,
        ),
    ] {
        assert_eq!(encode_execute_audit_denial(expected), stored);
        assert_eq!(
            decode_execute_audit_denial(stored.to_owned(), "42")
                .expect("closed EXECUTE reason must decode"),
            expected
        );
    }

    assert!(matches!(
        decode_authentication_audit_denial("authentication_other".to_owned(), "43"),
        Err(PostgresKernelError::DurableInvariant {
            relation: "_orna_kernel.security_audit_events",
            ref record,
            rule: "authentication denial reason is unsupported",
        }) if record == "43"
    ));
    assert!(matches!(
        decode_execute_audit_denial("execute_other".to_owned(), "44"),
        Err(PostgresKernelError::DurableInvariant {
            relation: "_orna_kernel.security_audit_events",
            ref record,
            rule: "EXECUTE denial reason is unsupported",
        }) if record == "44"
    ));
}

#[test]
fn capability_audit_denial_codec_round_trips_the_redacted_qualified_name() {
    assert_eq!(
        encode_security_audit_kind(SecurityAuditKind::Authentication),
        "authentication"
    );
    assert_eq!(
        encode_security_audit_kind(SecurityAuditKind::Execute),
        "execute"
    );
    assert_eq!(
        encode_security_audit_kind(SecurityAuditKind::Capability),
        "capability"
    );
    assert_eq!(
        encode_security_audit_kind(SecurityAuditKind::SecurityAdmin),
        "security_admin"
    );

    for name in [
        "std.fs.read",
        "std.fs.write",
        "std.net.connect",
        "std.secret.use",
    ] {
        let stored = encode_capability_audit_denial(name);
        assert_eq!(stored, format!("capability:{name}"));
        assert_eq!(
            decode_capability_audit_denial(stored, "50")
                .expect("redacted capability name must decode"),
            name
        );
    }

    assert!(matches!(
        decode_capability_audit_denial("execute_missing_grant".to_owned(), "51"),
        Err(PostgresKernelError::DurableInvariant {
            relation: "_orna_kernel.security_audit_events",
            ref record,
            rule: "capability denial reason is unsupported",
        }) if record == "51"
    ));
}

#[test]
fn capability_audit_decisions_encode_the_redacted_name_for_both_outcomes() {
    let target = InvocationTarget::new(
        FunctionId::from_bytes([0x91; 16]),
        RevisionPair::new(
            SourceRevisionId::from_bytes([0x92; 16]),
            CatalogueRevisionId::from_bytes([0x93; 16]),
        ),
    );
    let principal = PrincipalId::from_bytes([0x94; 16]);
    let allowed =
        SecurityAuditDecision::recover_capability_allowed(principal, target, "std.fs.read")
            .expect("closed capability name is valid");
    let denied =
        SecurityAuditDecision::recover_capability_denied(principal, target, "std.secret.use")
            .expect("closed capability name is valid");

    let encode = |decision: &SecurityAuditDecision| match decision.denial() {
        None => decision
            .capability_name()
            .map(encode_capability_audit_denial),
        Some(SecurityAuditDenial::Authentication(reason)) => {
            Some(encode_authentication_audit_denial(reason).to_owned())
        }
        Some(SecurityAuditDenial::Execute(reason)) => {
            Some(encode_execute_audit_denial(reason).to_owned())
        }
        Some(SecurityAuditDenial::Capability { capability }) => {
            Some(encode_capability_audit_denial(&capability))
        }
        Some(SecurityAuditDenial::Inspect(reason)) => {
            Some(encode_inspect_audit_denial(reason).to_owned())
        }
        Some(SecurityAuditDenial::SecurityAdmin(reason)) => {
            encode_security_admin_audit_denied_detail(decision, reason)
        }
    };

    assert_eq!(encode(&allowed), Some("capability:std.fs.read".to_owned()));
    assert_eq!(
        encode(&denied),
        Some("capability:std.secret.use".to_owned())
    );
    assert_eq!(allowed.kind(), SecurityAuditKind::Capability);
    assert_eq!(denied.kind(), SecurityAuditKind::Capability);
    assert_eq!(allowed.outcome(), SecurityAuditOutcome::Allowed);
    assert_eq!(denied.outcome(), SecurityAuditOutcome::Denied);
    assert_eq!(allowed.target(), Some(target));
    assert_eq!(denied.target(), Some(target));
}

#[test]
fn source_apply_audit_codec_preserves_candidate_pair_and_committed_detail() {
    let principal = PrincipalId::from_bytes([0xa5; 16]);
    let candidate = RevisionPair::new(
        SourceRevisionId::from_bytes([0xa6; 16]),
        CatalogueRevisionId::from_bytes([0xa7; 16]),
    );
    let decision = SecurityAuditDecision::recover_source_apply_allowed(principal, candidate);

    assert_eq!(encode_security_audit_kind(decision.kind()), "source_apply");
    assert_eq!(encode_source_apply_audit_detail(), "source_apply:committed");
    decode_source_apply_audit_detail("source_apply:committed", "source-apply")
        .expect("committed source apply detail must decode");
    assert_eq!(
        encode_security_audit_identity_columns(&decision),
        (
            None,
            Some(candidate.source().to_bytes().to_vec()),
            Some(candidate.catalogue().to_bytes().to_vec()),
        )
    );
    assert_eq!(decision.outcome(), SecurityAuditOutcome::Allowed);
    assert_eq!(decision.session_principal(), Some(principal));
    assert_eq!(decision.source_apply_candidate(), Some(candidate));
    assert_eq!(decision.target(), None);
    assert_eq!(decision.denial(), None);
}

#[test]
fn security_admin_audit_codec_round_trips_operation_and_denial() {
    let principal = PrincipalId::from_bytes([0x95; 16]);
    let snapshot = SecuritySnapshot::new(
        RevisionPair::new(
            SourceRevisionId::from_bytes([0x96; 16]),
            CatalogueRevisionId::from_bytes([0x97; 16]),
        ),
        vec![],
        vec![Principal::new(
            principal,
            PrincipalKind::User,
            PrincipalStatus::Active,
        )],
        vec![],
        vec![],
    )
    .expect("security-admin codec snapshot is valid");
    let session = snapshot
        .bind_authenticated_session(principal, vec![])
        .expect("security-admin codec session binds");
    let operation = SecurityAdminAuditOperation::GrantPrivilege;
    let target = SYS_SECURITY_GRANT_PRIVILEGE_FUNCTION_ID;

    let allowed = SecurityAuditDecision::security_admin_allowed(
        &session,
        PrivilegeDecision::Allowed {
            requested: PrivilegeClass::SecurityAdmin,
        },
        operation,
        target,
    )
    .expect("allowed security-admin decision must construct");
    assert_eq!(
        encode_security_admin_audit_detail(operation),
        "security_admin:grant_privilege"
    );
    assert_eq!(
        decode_security_admin_audit_detail("security_admin:grant_privilege", "60")
            .expect("allowed security-admin detail must decode"),
        operation
    );
    assert!(matches!(
        decode_security_admin_audit_detail("security_admin:grant_privilege:missing-privilege", "61"),
        Err(PostgresKernelError::DurableInvariant {
            relation: "_orna_kernel.security_audit_events",
            ref record,
            rule: "allowed security-admin audit detail must carry only the operation",
        }) if record == "61"
    ));
    assert!(matches!(
        decode_security_admin_audit_detail("execute_missing_grant", "62"),
        Err(PostgresKernelError::DurableInvariant {
            relation: "_orna_kernel.security_audit_events",
            ref record,
            rule: "security-admin audit detail must start with security_admin:",
        }) if record == "62"
    ));
    assert_eq!(allowed.kind(), SecurityAuditKind::SecurityAdmin);
    assert_eq!(allowed.outcome(), SecurityAuditOutcome::Allowed);
    assert_eq!(allowed.security_admin_operation(), Some(operation));
    assert_eq!(allowed.security_admin_target(), Some(target));

    let reason = PrivilegeDenial::MissingPrivilege {
        requested: PrivilegeClass::SecurityAdmin,
    };
    let denied = SecurityAuditDecision::security_admin_denied(
        &session,
        SecurityAdminAuditOperation::CreatePrincipal,
        SYS_SECURITY_CREATE_PRINCIPAL_FUNCTION_ID,
        reason,
    );
    let stored = encode_security_admin_audit_denied_detail(&denied, reason)
        .expect("denied security-admin decision must carry its operation");
    assert_eq!(stored, "security_admin:create_principal:missing-privilege");
    let (operation, decoded_reason) = decode_security_admin_audit_denial(&stored, "63")
        .expect("denied security-admin detail must decode");
    assert_eq!(operation, SecurityAdminAuditOperation::CreatePrincipal);
    assert_eq!(decoded_reason, reason);
    assert!(matches!(
        decode_security_admin_audit_denial("security_admin:create_principal:granted", "64"),
        Err(PostgresKernelError::DurableInvariant {
            relation: "_orna_kernel.security_audit_events",
            ref record,
            rule: "security-admin denial reason is unsupported",
        }) if record == "64"
    ));
    assert!(matches!(
        decode_security_admin_audit_denial("security_admin:create_principal", "65"),
        Err(PostgresKernelError::DurableInvariant {
            relation: "_orna_kernel.security_audit_events",
            ref record,
            rule: "security-admin denial reason must carry an operation and a reason",
        }) if record == "65"
    ));
    assert_eq!(denied.kind(), SecurityAuditKind::SecurityAdmin);
    assert_eq!(denied.outcome(), SecurityAuditOutcome::Denied);
    assert_eq!(denied.security_admin_denial(), Some(reason));
    assert_eq!(
        denied.denial(),
        Some(SecurityAuditDenial::SecurityAdmin(reason))
    );
}

#[test]
fn security_admin_audit_target_binding_covers_all_operations_for_both_outcomes() {
    let principal = PrincipalId::from_bytes([0xf1; 16]);
    let missing_privilege = PrivilegeDenial::MissingPrivilege {
        requested: PrivilegeClass::SecurityAdmin,
    };
    let operations = [
        (
            SecurityAdminAuditOperation::CreatePrincipal,
            SYS_SECURITY_CREATE_PRINCIPAL_FUNCTION_ID,
        ),
        (
            SecurityAdminAuditOperation::DisablePrincipal,
            SYS_SECURITY_DISABLE_PRINCIPAL_FUNCTION_ID,
        ),
        (
            SecurityAdminAuditOperation::CreateRole,
            SYS_SECURITY_CREATE_ROLE_FUNCTION_ID,
        ),
        (
            SecurityAdminAuditOperation::GrantRole,
            SYS_SECURITY_GRANT_ROLE_FUNCTION_ID,
        ),
        (
            SecurityAdminAuditOperation::RevokeRole,
            SYS_SECURITY_REVOKE_ROLE_FUNCTION_ID,
        ),
        (
            SecurityAdminAuditOperation::GrantPrivilege,
            SYS_SECURITY_GRANT_PRIVILEGE_FUNCTION_ID,
        ),
        (
            SecurityAdminAuditOperation::RevokePrivilege,
            SYS_SECURITY_REVOKE_PRIVILEGE_FUNCTION_ID,
        ),
    ];

    for (operation, target) in operations {
        require_security_admin_audit_target(target, operation, "security-admin-all-operations")
            .expect("every SecurityAdmin operation must bind to its sealed target");

        let allowed =
            SecurityAuditDecision::recover_security_admin_allowed(principal, operation, target);
        assert_eq!(allowed.security_admin_operation(), Some(operation));
        assert_eq!(allowed.security_admin_target(), Some(target));

        let denied = SecurityAuditDecision::recover_security_admin_denied(
            principal,
            operation,
            target,
            missing_privilege,
        );
        assert_eq!(denied.security_admin_operation(), Some(operation));
        assert_eq!(denied.security_admin_target(), Some(target));
    }
}

#[test]
fn security_admin_audit_target_binding_rejects_tampering() {
    let operation = SecurityAdminAuditOperation::GrantPrivilege;
    require_security_admin_audit_target(SYS_SECURITY_GRANT_PRIVILEGE_FUNCTION_ID, operation, "66")
        .expect("the matching sealed SecurityAdmin target must recover");

    assert!(matches!(
        require_security_admin_audit_target(
            SYS_SECURITY_CREATE_PRINCIPAL_FUNCTION_ID,
            operation,
            "67",
        ),
        Err(PostgresKernelError::DurableInvariant {
            relation: "_orna_kernel.security_audit_events",
            ref record,
            rule: "security-admin audit target must match operation",
        }) if record == "67"
    ));
    assert!(matches!(
        require_security_admin_audit_target(
            SYS_SECURITY_SESSION_PRINCIPAL_FUNCTION_ID,
            operation,
            "68",
        ),
        Err(PostgresKernelError::DurableInvariant {
            relation: "_orna_kernel.security_audit_events",
            ref record,
            rule: "security-admin audit target must be a sealed SecurityAdmin function",
        }) if record == "68"
    ));
}

#[test]
fn expected_security_result_does_not_hide_session_shutdown_failure() {
    let operation: Result<Result<(), LocalPeerAuthenticationError>, PostgresKernelError> =
        Ok(Err(LocalPeerAuthenticationError::UnknownUid));
    let shutdown = PostgresKernelError::DurableInvariant {
        relation: "test session",
        record: "shutdown".to_owned(),
        rule: "driver failed during shutdown",
    };

    assert!(matches!(
        finish_security_session(operation, Err(shutdown)),
        Err(PostgresKernelError::DurableInvariant {
            relation: "test session",
            ref record,
            rule: "driver failed during shutdown",
        }) if record == "shutdown"
    ));

    let operation: Result<Result<(), PostgresKernelError>, PostgresKernelError> =
        Ok(Err(PostgresKernelError::ClientExecuteDenied {
            pair: RevisionPair::new(
                SourceRevisionId::from_bytes([0x11; 16]),
                CatalogueRevisionId::from_bytes([0x12; 16]),
            ),
            function: FunctionId::from_bytes([0x13; 16]),
            reason: ExecuteDenial::MissingExecuteGrant,
        }));
    let shutdown = PostgresKernelError::DurableInvariant {
        relation: "test session",
        record: "shutdown".to_owned(),
        rule: "driver failed during shutdown",
    };
    assert!(matches!(
        finish_security_session(operation, Err(shutdown)),
        Err(PostgresKernelError::DurableInvariant {
            relation: "test session",
            ref record,
            rule: "driver failed during shutdown",
        }) if record == "shutdown"
    ));
}

#[test]
fn sealed_completed_events_carry_the_value_in_the_final_value_batch() {
    let principal = PrincipalId::from_bytes([0x51; 16]);
    let invocation = InvocationId::from_bytes([0x52; 16]);
    let events = sealed_completed_events(principal, invocation, RuntimeValue::Integer(41))
        .expect("the completed events are valid");
    let records = events.records();
    assert_eq!(records.len(), 3);
    assert_eq!(records[0].outer_sequence(), 1);
    assert_eq!(records[1].outer_sequence(), 2);
    assert_eq!(records[2].outer_sequence(), 3);
    assert!(matches!(
        records[0].event().body(),
        InvocationEventBody::Started {
            visible_principal: None
        }
    ));
    match records[1].event().body() {
        InvocationEventBody::ValueBatch { schema, values } => {
            assert!(schema.is_none());
            let [value] = values.as_slice() else {
                panic!("the ValueBatch must carry exactly one value");
            };
            assert_eq!(value.value(), &RuntimeValue::Integer(41));
        }
        other => panic!("expected a ValueBatch event, got {other:?}"),
    }
    assert!(matches!(
        records[2].event().body(),
        InvocationEventBody::Completed {
            duration_nanoseconds: 0
        }
    ));
}

#[test]
fn sealed_completed_events_carry_all_server_values_in_one_batch() {
    let events = sealed_completed_events_from_values(
        PrincipalId::from_bytes([0x61; 16]),
        InvocationId::from_bytes([0x62; 16]),
        vec![RuntimeValue::Integer(1), RuntimeValue::Integer(2)],
    )
    .expect("server values form a valid event batch");
    assert_eq!(events.records().len(), 3);
    assert_eq!(events.records()[0].event().sequence(), 0);
    assert_eq!(events.records()[1].event().sequence(), 1);
    assert_eq!(events.records()[2].event().sequence(), 2);
    let InvocationEventBody::ValueBatch { schema, values } = events.records()[1].event().body()
    else {
        panic!("expected one server ValueBatch event");
    };
    assert!(schema.is_none());
    assert_eq!(values.len(), 2);
    assert_eq!(values[0].value(), &RuntimeValue::Integer(1));
    assert_eq!(values[1].value(), &RuntimeValue::Integer(2));
}

#[test]
fn sealed_completed_events_allow_an_empty_server_result() {
    let events = sealed_completed_events_from_values(
        PrincipalId::from_bytes([0x63; 16]),
        InvocationId::from_bytes([0x64; 16]),
        Vec::new(),
    )
    .expect("an empty server result still completes the invocation");
    assert_eq!(events.records().len(), 2);
    assert_eq!(events.records()[0].event().sequence(), 0);
    assert_eq!(events.records()[1].event().sequence(), 1);
    assert!(matches!(
        events.records()[1].event().body(),
        InvocationEventBody::Completed {
            duration_nanoseconds: 0
        }
    ));
}

#[test]
fn sealed_failure_events_are_redacted_and_closed() {
    let invocation = InvocationId::from_bytes([0x71; 16]);
    let events = sealed_failure_events(invocation, SealedInvocationFailureClass::Target)
        .expect("the failure events are valid");
    assert_eq!(events.records().len(), 2);
    assert!(matches!(
        events.records()[0].event().body(),
        InvocationEventBody::Started {
            visible_principal: None
        }
    ));
    let InvocationEventBody::Failed(failure) = events.records()[1].event().body() else {
        panic!("expected an InvocationFailed event");
    };
    assert_eq!(failure.phase(), InvocationFailurePhase::Target);
    assert_eq!(failure.code(), "INVOKE_TARGET_FAILED");
    assert_eq!(failure.message(), "invocation target failed");
    assert!(failure.details().is_none());
    assert_eq!(failure.retryability(), InvocationRetryability::Unknown);
}
#[test]
fn resource_targets_resolve_and_authorize_with_closed_class_pins() {
    use orna_core::{
        SchemaId,
        catalogue::{FunctionSecurity, FunctionTransaction, FunctionVolatility, SchemaDefinition},
        security::{ExecuteGrant, Principal, PrincipalKind, PrincipalStatus},
        types::{ResolvedType, StandardScalar},
    };

    let pair = RevisionPair::new(
        SourceRevisionId::from_bytes([0xa1; 16]),
        CatalogueRevisionId::from_bytes([0xa2; 16]),
    );
    let principal = PrincipalId::from_bytes([0xa3; 16]);
    let application_function = FunctionId::from_bytes([0xa4; 16]);
    let application_revision = FunctionRevisionId::from_bytes([0xa5; 16]);
    let application = FunctionDefinition::new(
        application_function,
        QualifiedSemanticName::new(["app", "resource"]).expect("application name"),
        FunctionDomain::Server,
        Vec::new(),
        FunctionReturn::Single(ResolvedType::scalar(StandardScalar::Integer)),
        application_revision,
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::ReadOnly),
        FunctionVolatility::Stable,
    );
    let application_catalogue = CatalogueSnapshot::new_with_functions(
        pair.catalogue(),
        vec![SchemaDefinition::new(
            SchemaId::from_bytes([0xa6; 16]),
            QualifiedSemanticName::new(["app"]).expect("application schema"),
        )],
        Vec::new(),
        vec![application],
    )
    .expect("application catalogue");
    let application_target = resolve_resource_target_in_catalogues(
        pair,
        &application_catalogue,
        None,
        application_function,
    )
    .expect("application resource target");
    assert_eq!(
        application_target.target(),
        InvocationTarget::new(application_function, pair)
    );
    let application_security = SecuritySnapshot::new_with_function_targets(
        pair,
        vec![SecurityFunctionTarget::application(application_function)],
        vec![Principal::new(
            principal,
            PrincipalKind::User,
            PrincipalStatus::Active,
        )],
        Vec::new(),
        vec![ExecuteGrant::new(principal, application_function)],
    )
    .expect("application security snapshot");
    let session = application_security
        .bind_authenticated_session(principal, Vec::new())
        .expect("application session");
    assert!(matches!(
        application_security.authorise_execute(&session, application_target.target()),
        ExecuteDecision::Allowed(_)
    ));

    let standard = orna_standard::verify_standard_library_v2_snapshot(
        orna_standard::retained_standard_library_v2_snapshot().expect("standard fixture"),
    )
    .expect("verified standard fixture");
    let standard_function = STD_INVOKE_ECHO_FUNCTION_ID;
    let standard_definition = standard
        .catalogue()
        .function_by_id(standard_function)
        .expect("standard echo definition");
    let standard_executable = standard
        .executables()
        .iter()
        .find(|executable| executable.function() == standard_function)
        .expect("standard echo executable");
    assert_eq!(
        standard_executable.revision().id(),
        standard_definition.current_revision()
    );
    let empty_catalogue =
        CatalogueSnapshot::new_with_functions(pair.catalogue(), Vec::new(), Vec::new(), Vec::new())
            .expect("empty application catalogue");
    let standard_target = resolve_resource_target_in_catalogues(
        pair,
        &empty_catalogue,
        Some(&standard),
        standard_function,
    )
    .expect("verified standard resource target");
    let expected_standard_target = InvocationTarget::verified_standard(
        standard_function,
        pair,
        standard.revision(),
        standard_executable.revision().id(),
    );
    assert_eq!(standard_target.target(), expected_standard_target);
    let standard_security = SecuritySnapshot::new_with_function_targets(
        pair,
        vec![SecurityFunctionTarget::verified_standard(
            standard_function,
            standard.revision(),
            standard_executable.revision().id(),
        )],
        vec![Principal::new(
            principal,
            PrincipalKind::User,
            PrincipalStatus::Active,
        )],
        Vec::new(),
        vec![ExecuteGrant::new(principal, standard_function)],
    )
    .expect("standard security snapshot");
    let session = standard_security
        .bind_authenticated_session(principal, Vec::new())
        .expect("standard session");
    assert!(matches!(
        standard_security.authorise_execute(&session, expected_standard_target),
        ExecuteDecision::Allowed(_)
    ));
    assert_eq!(
        standard_security
            .authorise_execute(&session, InvocationTarget::new(standard_function, pair),),
        ExecuteDecision::Denied(ExecuteDenial::UnknownFunction)
    );
}

#[test]
fn dropping_resource_producer_requests_cancellation() {
    let cancellation = ResourceCancellation::new();
    let (commands, _receiver) = tokio::sync::mpsc::channel(1);
    let producer = AuthenticatedServerResourceProducer {
        accepted: AuthenticatedServerResourceAccepted {
            stream_id: 1,
            request_id: InvocationId::from_bytes([0x91; 16]),
            nested_invocation_id: InvocationId::from_bytes([0x92; 16]),
            target_revision: RevisionPair::new(
                SourceRevisionId::from_bytes([0x93; 16]),
                CatalogueRevisionId::from_bytes([0x94; 16]),
            ),
            resource_kind: AuthenticatedServerResourceKind::Stream,
        },
        commands,
        cancellation: cancellation.clone(),
    };

    drop(producer);

    assert!(cancellation.is_requested());
}

#[test]
fn dropping_resource_producer_start_guard_requests_cancellation() {
    let cancellation = ResourceCancellation::new();
    {
        let _guard = ResourceProducerStartGuard::new(cancellation.clone());
    }

    assert!(cancellation.is_requested());
}

#[test]
fn resource_credit_is_nonzero_and_bounded() {
    assert!(ResourceCredit::new(1, 1).is_some());
    assert!(ResourceCredit::new(0, 1).is_none());
    assert!(ResourceCredit::new(1, 0).is_none());
    assert!(ResourceCredit::new(MAX_RESOURCE_CREDIT + 1, 1).is_none());
    assert!(ResourceCredit::new(1, MAX_RESOURCE_CREDIT + 1).is_none());
}

#[test]
fn resource_cancellation_wins_before_terminal_commit() {
    let cancellation = ResourceCancellation::new();

    assert!(cancellation.request_cancel());
    assert!(!cancellation.try_begin_commit());
    assert!(!cancellation.request_cancel());
}

#[test]
fn resource_acceptance_commit_preserves_cancellation() {
    let cancellation = ResourceCancellation::new();

    assert!(cancellation.try_begin_acceptance_commit());
    assert!(!cancellation.request_cancel());
    assert!(cancellation.is_acceptance_cancellation_requested());
    assert!(cancellation.is_requested());
    cancellation.acceptance_commit_finished();
    assert!(!cancellation.is_acceptance_cancellation_requested());
    assert!(cancellation.is_requested());
    assert!(!cancellation.try_begin_commit());
}

#[test]
fn resource_terminal_commit_wins_over_late_cancellation() {
    let cancellation = ResourceCancellation::new();

    assert!(cancellation.try_begin_commit());
    assert!(!cancellation.request_cancel());
    cancellation.commit_finished();
    assert!(!cancellation.try_begin_commit());
}
#[test]
fn sealed_server_stream_completion_preserves_batch_metadata() {
    assert_eq!(
        sealed_server_stream_completed_event(17, 23, 41),
        AuthenticatedServerResourceEvent::Completed {
            final_batch_sequence: 17,
            total_items: 23,
            total_bytes: 41,
        },
    );
}
