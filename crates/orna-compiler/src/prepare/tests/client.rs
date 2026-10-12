//! Client expression, state, capability, and resource preparation tests.

use super::*;
use crate::DiagnosticCode;

#[test]
fn empty_client_state_block_uses_expression_plan_format() {
    let verified = invocation_carrier_standard();
    let standard = check_standard_library_source(&verified).unwrap();
    let active = empty_standard_application_active(&verified);
    let context = StandardApplicationCheckContext::try_new(active.catalogue(), &standard).unwrap();
    let source = "CREATE SCHEMA examples; \
            CREATE CLIENT FUNCTION examples.ready() RETURNS BOOLEAN IS \
            BEGIN RETURN TRUE; END;";
    let bundle = SourceBundle::new([SourceUnit::new("application.orna", source)]).unwrap();
    let report = check_standard_application(&bundle, &context);
    assert!(
        report.diagnostics().is_empty(),
        "{:?}",
        report.diagnostics()
    );

    let prepared = prepare_standard_application(&report, active.pair(), &active).unwrap();
    let revision = &prepared.new_function_revisions()[0];
    assert_eq!(
        revision.artifact().version(),
        CLIENT_PLAN_EXPRESSION_VERSION
    );
    let plan = ExpressionClientPlan::decode(revision.artifact().payload()).unwrap();
    assert_eq!(
        plan.expression(),
        &ClientExpressionNode::Boolean { value: true },
    );
}

#[test]
fn unreachable_warning_does_not_block_standard_preparation() {
    let verified = invocation_carrier_standard();
    let standard = check_standard_library_source(&verified).unwrap();
    let active = empty_standard_application_active(&verified);
    let context = StandardApplicationCheckContext::try_new(active.catalogue(), &standard).unwrap();
    let source = "CREATE SCHEMA examples; \
                  CREATE CLIENT FUNCTION examples.ready() RETURNS BOOLEAN IS \
                  BEGIN RETURN TRUE; LET ignored := FALSE; END;";
    let bundle = SourceBundle::new([SourceUnit::new("application.orna", source)]).unwrap();
    let report = check_standard_application(&bundle, &context);

    assert!(!report.has_errors());
    assert_eq!(report.warning_count(), 1);
    assert_eq!(
        report.diagnostics()[0].code(),
        DiagnosticCode::UnreachableCode
    );
    let prepared = prepare_standard_application(&report, active.pair(), &active)
        .expect("warnings must not block preparation");
    assert_eq!(prepared.candidate().functions().len(), 1);
}

#[test]
fn standard_integer_stream_resource_preparation_materialises_durable_operation_artifact() {
    let verified = crate::tests::verified_canonical_standard_source_fixture();
    let integer_type_id = verified
        .catalogue()
        .value_type_by_name(&semantic_name(&["std", "types", "integer"]))
        .unwrap()
        .id();
    let standard = check_standard_library_source(&verified).unwrap();
    let active = empty_standard_application_active(&verified);
    let context = StandardApplicationCheckContext::try_new(active.catalogue(), &standard).unwrap();
    let source = r#"CREATE SCHEMA stream_fixture;

CREATE TYPE stream_fixture.probe AS OBJECT (
    marker INTEGER NOT NULL
);

CREATE SERVER FUNCTION stream_fixture.events()
RETURNS STREAM<INTEGER>
SECURITY INVOKER
TRANSACTION READ ONLY
VOLATILITY STABLE
AS
    SELECT probe.marker FROM stream_fixture.probe probe;

CREATE CLIENT FUNCTION stream_fixture.read() RETURNS STREAM<INTEGER> IS
BEGIN
    RETURN AWAIT std.data.stream_resource(
        target => stream_fixture.events,
        arguments => std.call.args()
    );
END;"#;
    let bundle = SourceBundle::new([SourceUnit::new(
        "fixtures/stream_resource_integer.orna",
        source,
    )])
    .unwrap();
    let report = check_standard_application(&bundle, &context);
    assert!(
        report.diagnostics().is_empty(),
        "integer stream resource fixture did not check: {:?}",
        report.diagnostics()
    );

    let checked_call_site = {
        let checked = report.preparation_view().unwrap().checked();
        let client = checked
            .client_functions()
            .iter()
            .find(|function| function.name().parts() == ["stream_fixture", "read"])
            .unwrap();
        let CheckedClientFunctionBody::Expression { expression } = client.body() else {
            panic!("integer stream resource client must use an expression body");
        };
        let CheckedClientExpression::Await { expression, .. } = expression else {
            panic!("integer stream resource client must await its resource");
        };
        let CheckedClientExpression::Resource { operation } = expression.as_ref() else {
            panic!("integer stream resource client must retain its resource operation");
        };
        assert_eq!(
            operation.kind(),
            orna_artifact::client_plan::ResourceKind::Stream
        );
        assert_eq!(
            operation.result_type(),
            SemanticType::scalar(StandardScalar::Integer)
        );
        assert_eq!(operation.standard_result_type(), Some(integer_type_id));
        operation.call_site()
    };

    let prepared = prepare_standard_application(&report, active.pair(), &active).unwrap();
    let target = prepared
        .candidate()
        .functions()
        .iter()
        .find(|function| function.name().parts() == ["stream_fixture", "events"])
        .unwrap();
    let target_revision = prepared
        .new_function_revisions()
        .iter()
        .find(|revision| revision.function() == target.id())
        .unwrap();
    assert_eq!(target.current_revision(), target_revision.id());
    assert_eq!(
        target_revision.artifact().kind(),
        ExecutableArtifactKind::Server
    );
    assert_eq!(target_revision.artifact().format(), SERVER_PLAN_FORMAT);
    assert_eq!(target_revision.artifact().version(), SERVER_PLAN_VERSION);
    assert_eq!(
        target.return_type(),
        &FunctionReturn::Stream(ResolvedType::Value(integer_type_id))
    );
    let target_plan = ServerPlan::decode(target_revision.artifact().payload()).unwrap();
    let probe = prepared
        .candidate()
        .object_type_by_name(&semantic_name(&["stream_fixture", "probe"]))
        .unwrap();
    assert_eq!(target_plan.scan.object_type, probe.id());
    assert_eq!(target_plan.projections.len(), 1);
    assert_eq!(
        target_plan.projections[0].value_type.resolved_type,
        ResolvedType::scalar(StandardScalar::Integer)
    );
    let ExpressionKind::FieldPath { ref steps, .. } = target_plan.projections[0].kind else {
        panic!("integer stream SERVER plan projection must be a field path");
    };
    assert_eq!(steps.len(), 1);
    assert_eq!(steps[0].owner, probe.id());
    assert_eq!(steps[0].field, probe.field_by_name("marker").unwrap().id());

    let client = prepared
        .candidate()
        .functions()
        .iter()
        .find(|function| function.name().parts() == ["stream_fixture", "read"])
        .unwrap();
    let revision = prepared
        .new_function_revisions()
        .iter()
        .find(|revision| revision.function() == client.id())
        .unwrap();
    assert_eq!(revision.artifact().version(), CLIENT_PLAN_RESOURCE_VERSION);
    let plan = ResourceClientPlan::decode(revision.artifact().payload()).unwrap();
    let ClientExpressionNode::Await { expression } = plan.expression() else {
        panic!("prepared integer stream resource plan must keep AWAIT at the return expression");
    };
    let ClientExpressionNode::Resource { operation } = expression.as_ref() else {
        panic!(
            "prepared integer stream resource plan must contain a resource operation under AWAIT"
        );
    };
    assert_eq!(
        operation.kind(),
        orna_artifact::client_plan::ResourceKind::Stream
    );
    assert_eq!(operation.target(), target.id());
    assert_eq!(operation.target_revision(), prepared.candidate_pair());
    assert_eq!(operation.result_type(), integer_type_id);
    assert_eq!(operation.call_site(), checked_call_site);
    assert_ne!(operation.call_site().to_bytes(), [0; 16]);
    assert!(operation.arguments().is_empty());
}

#[test]
fn accepted_server_function_dogfood_checks_and_prepares_catalogue_artifacts() {
    const SOURCE: &str = include_str!("../../../tests/fixtures/server-function-dogfood.orna");

    let verified = crate::tests::verified_canonical_standard_source_fixture();
    let standard = check_standard_library_source(&verified).unwrap();
    let active = empty_standard_application_active(&verified);
    let context = StandardApplicationCheckContext::try_new(active.catalogue(), &standard).unwrap();
    let bundle = SourceBundle::new([SourceUnit::new(
        "examples/server-function-dogfood.orna",
        SOURCE,
    )])
    .unwrap();

    let report = check_standard_application(&bundle, &context);
    assert!(
        report.diagnostics().is_empty(),
        "accepted dogfood application did not check: {:?}",
        report.diagnostics()
    );

    let prepared = prepare_standard_application(&report, active.pair(), &active).unwrap();
    let candidate = prepared.candidate();
    let functions = ["read", "distinct_values", "stream", "read_item", "update"];
    assert_eq!(prepared.new_function_revisions().len(), functions.len());
    for function_name in functions {
        let function = candidate
            .functions()
            .iter()
            .find(|function| function.name().parts() == ["dogfood", function_name])
            .unwrap_or_else(|| panic!("missing installed dogfood.{function_name}"));
        assert_eq!(function.domain(), FunctionDomain::Server);
        let revision = prepared
            .new_function_revisions()
            .iter()
            .find(|revision| revision.function() == function.id())
            .unwrap_or_else(|| panic!("missing prepared revision for dogfood.{function_name}"));
        assert_eq!(function.current_revision(), revision.id());
        assert_eq!(revision.artifact().kind(), ExecutableArtifactKind::Server);
    }
}

#[test]
fn named_record_stream_resource_preparation_preserves_nominal_result_identity() {
    let verified = resource_standard();
    let standard = check_standard_library_source(&verified).unwrap();
    let active = empty_standard_application_active(&verified);
    let context = StandardApplicationCheckContext::try_new(active.catalogue(), &standard).unwrap();
    let source = r#"CREATE SCHEMA stream_fixture;

CREATE TYPE stream_fixture.event AS VALUE (
    marker TEXT
) IMMUTABLE PERSISTABLE;

CREATE TYPE stream_fixture.other_event AS VALUE (
    marker TEXT
) IMMUTABLE PERSISTABLE;

CREATE TYPE stream_fixture.probe AS OBJECT (
    event stream_fixture.event NOT NULL
);

CREATE SERVER FUNCTION stream_fixture.events()
RETURNS STREAM<stream_fixture.event>
SECURITY INVOKER
TRANSACTION READ ONLY
VOLATILITY STABLE
AS
    SELECT probe.event FROM stream_fixture.probe probe;

CREATE CLIENT FUNCTION stream_fixture.read() RETURNS STREAM<stream_fixture.event> IS
BEGIN
    RETURN AWAIT std.data.stream_resource(
        target => stream_fixture.events,
        arguments => std.call.args()
    );
END;"#;
    let bundle = SourceBundle::new([SourceUnit::new(
        "fixtures/stream_resource_record.orna",
        source,
    )])
    .unwrap();
    let report = check_standard_application(&bundle, &context);
    assert!(
        report.diagnostics().is_empty(),
        "record stream resource fixture did not check: {:?}",
        report.diagnostics()
    );

    let checked = report.preparation_view().unwrap().checked();
    let checked_record = checked
        .record_value_types()
        .iter()
        .find(|record| record.name().to_string() == "stream_fixture.event")
        .unwrap();
    let checked_record_id = checked_record.id();
    let checked_call_site = {
        let client = checked
            .client_functions()
            .iter()
            .find(|function| function.name().parts() == ["stream_fixture", "read"])
            .unwrap();
        let CheckedClientFunctionBody::Expression { expression } = client.body() else {
            panic!("record stream resource client must use an expression body");
        };
        let CheckedClientExpression::Await { expression, .. } = expression else {
            panic!("record stream resource client must await its resource");
        };
        let CheckedClientExpression::Resource { operation } = expression.as_ref() else {
            panic!("record stream resource client must retain its resource operation");
        };
        assert_eq!(
            operation.kind(),
            orna_artifact::client_plan::ResourceKind::Stream
        );
        assert_eq!(
            operation.result_type(),
            SemanticType::Named(checked_record_id)
        );
        assert_eq!(operation.standard_result_type(), None);
        operation.call_site()
    };

    let prepared = prepare_standard_application(&report, active.pair(), &active).unwrap();
    let record = prepared
        .candidate()
        .record_value_types()
        .iter()
        .find(|record| record.name().to_string() == "stream_fixture.event")
        .unwrap();
    let record_type_id = record.id();
    let target = prepared
        .candidate()
        .functions()
        .iter()
        .find(|function| function.name().parts() == ["stream_fixture", "events"])
        .unwrap();
    let target_revision = prepared
        .new_function_revisions()
        .iter()
        .find(|revision| revision.function() == target.id())
        .unwrap();
    assert_eq!(target.current_revision(), target_revision.id());
    assert_eq!(
        target.return_type(),
        &FunctionReturn::Stream(ResolvedType::Named(record_type_id))
    );
    let target_plan = ServerPlan::decode(target_revision.artifact().payload()).unwrap();
    let probe = prepared
        .candidate()
        .object_type_by_name(&semantic_name(&["stream_fixture", "probe"]))
        .unwrap();
    assert_eq!(target_plan.scan.object_type, probe.id());
    assert_eq!(target_plan.projections.len(), 1);
    assert_eq!(
        target_plan.projections[0].value_type.resolved_type,
        ResolvedType::Named(record_type_id)
    );
    let ExpressionKind::FieldPath { ref steps, .. } = target_plan.projections[0].kind else {
        panic!("record stream SERVER plan projection must be a field path");
    };
    assert_eq!(steps.len(), 1);
    assert_eq!(steps[0].owner, probe.id());
    assert_eq!(steps[0].field, probe.field_by_name("event").unwrap().id());

    let client = prepared
        .candidate()
        .functions()
        .iter()
        .find(|function| function.name().parts() == ["stream_fixture", "read"])
        .unwrap();
    let revision = prepared
        .new_function_revisions()
        .iter()
        .find(|revision| revision.function() == client.id())
        .unwrap();
    assert_eq!(revision.artifact().version(), CLIENT_PLAN_RESOURCE_VERSION);
    let plan = ResourceClientPlan::decode(revision.artifact().payload()).unwrap();
    let ClientExpressionNode::Await { expression } = plan.expression() else {
        panic!("prepared record stream resource plan must keep AWAIT at the return expression");
    };
    let ClientExpressionNode::Resource { operation } = expression.as_ref() else {
        panic!("prepared record stream resource plan must contain a resource operation");
    };
    assert_eq!(
        operation.kind(),
        orna_artifact::client_plan::ResourceKind::Stream
    );
    assert_eq!(operation.target(), target.id());
    assert_eq!(operation.target_revision(), prepared.candidate_pair());
    assert_eq!(operation.result_type(), record_type_id);
    assert_eq!(operation.call_site(), checked_call_site);
    assert_ne!(operation.call_site().to_bytes(), [0; 16]);
    assert!(operation.arguments().is_empty());

    let mismatched_source = source.replace(
        "CREATE CLIENT FUNCTION stream_fixture.read() RETURNS STREAM<stream_fixture.event>",
        "CREATE CLIENT FUNCTION stream_fixture.read() RETURNS STREAM<stream_fixture.other_event>",
    );
    let mismatch_report = check_standard_application(
        &SourceBundle::new([SourceUnit::new(
            "fixtures/stream_resource_record.orna",
            mismatched_source,
        )])
        .unwrap(),
        &context,
    );
    assert!(
        mismatch_report
            .diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.code() == DiagnosticCode::TypeMismatch),
        "same-shaped nominal record stream mismatch must be rejected: {:?}",
        mismatch_report.diagnostics()
    );
    assert!(mismatch_report.checked_bundle().is_none());
}

#[test]
fn procedural_stream_resource_preparation_preserves_local_and_operation_identity() {
    let verified = resource_standard();
    let text_type_id = verified
        .catalogue()
        .value_type_by_name(&semantic_name(&["std", "types", "character_large_object"]))
        .unwrap()
        .id();
    let standard = check_standard_library_source(&verified).unwrap();
    let active = empty_standard_application_active(&verified);
    let context = StandardApplicationCheckContext::try_new(active.catalogue(), &standard).unwrap();
    let source = r#"CREATE SCHEMA stream_fixture;

CREATE TYPE stream_fixture.probe AS OBJECT (
    marker TEXT NOT NULL
);

CREATE SERVER FUNCTION stream_fixture.events()
RETURNS STREAM<TEXT>
SECURITY INVOKER
TRANSACTION READ ONLY
VOLATILITY STABLE
AS
    SELECT probe.marker FROM stream_fixture.probe probe;

CREATE CLIENT FUNCTION stream_fixture.read_local() RETURNS STREAM<TEXT> IS
    LET events std.data.StreamResource<TEXT> := std.data.stream_resource(
        target => stream_fixture.events,
        arguments => std.call.args()
    );
BEGIN
    RETURN AWAIT events;
END;"#;
    let bundle = SourceBundle::new([SourceUnit::new(
        "fixtures/stream_resource_procedural.orna",
        source,
    )])
    .unwrap();
    let report = check_standard_application(&bundle, &context);
    assert!(
        report.diagnostics().is_empty(),
        "procedural stream resource fixture did not check: {:?}",
        report.diagnostics()
    );

    let checked_call_site = {
        let checked = report.preparation_view().unwrap().checked();
        let client = checked
            .client_functions()
            .iter()
            .find(|function| function.name().parts() == ["stream_fixture", "read_local"])
            .unwrap();
        let CheckedClientFunctionBody::Procedural {
            locals,
            statements,
            return_expression,
        } = client.body()
        else {
            panic!("procedural stream resource client must use its block body");
        };
        assert_eq!(locals.len(), 1);
        assert_eq!(statements.len(), 1);
        let CheckedClientStatement::Let { expression, .. } = &statements[0] else {
            panic!("procedural stream resource local must be initialized by LET");
        };
        let CheckedClientExpression::Resource { operation } = expression else {
            panic!("procedural stream resource client must retain its constructor");
        };
        assert_eq!(
            operation.kind(),
            orna_artifact::client_plan::ResourceKind::Stream
        );
        let CheckedClientExpression::Await { expression, .. } = return_expression else {
            panic!("procedural stream resource client must use its local");
        };
        assert!(matches!(
            expression.as_ref(),
            CheckedClientExpression::LocalRead { local: 0, .. }
        ));
        operation.call_site()
    };

    let prepared = prepare_standard_application(&report, active.pair(), &active).unwrap();
    let target = prepared
        .candidate()
        .functions()
        .iter()
        .find(|function| function.name().parts() == ["stream_fixture", "events"])
        .unwrap();
    let client = prepared
        .candidate()
        .functions()
        .iter()
        .find(|function| function.name().parts() == ["stream_fixture", "read_local"])
        .unwrap();
    let revision = prepared
        .new_function_revisions()
        .iter()
        .find(|revision| revision.function() == client.id())
        .unwrap();
    assert_eq!(
        revision.artifact().version(),
        CLIENT_PLAN_PROCEDURAL_VERSION
    );
    let plan = ProceduralClientPlan::decode(revision.artifact().payload()).unwrap();
    assert_eq!(plan.locals().len(), 1);
    let local_decl = &plan.locals()[0];
    assert_eq!(
        local_decl.local_id(),
        durable_client_local_id(client.id(), 0)
    );
    assert_eq!(local_decl.type_id(), text_type_id);
    assert_eq!(
        local_decl.kind(),
        ClientLocalKind::Resource(orna_artifact::client_plan::ResourceKind::Stream)
    );
    assert_eq!(plan.statements().len(), 1);
    assert_eq!(plan.statements()[0].local(), local_decl.local_id());
    let ClientExpressionNode::Resource { operation } = plan.statements()[0].expression() else {
        panic!("procedural plan LET must contain a stream resource operation");
    };
    assert_eq!(
        operation.kind(),
        orna_artifact::client_plan::ResourceKind::Stream
    );
    assert_eq!(operation.target(), target.id());
    assert_eq!(operation.target_revision(), prepared.candidate_pair());
    assert_eq!(operation.call_site(), checked_call_site);
    assert_eq!(operation.result_type(), text_type_id);
    assert!(operation.arguments().is_empty());
    let ClientExpressionNode::Await { expression } = plan.return_expression() else {
        panic!("procedural plan return must await the resource local");
    };
    let ClientExpressionNode::LocalRead { local } = expression.as_ref() else {
        panic!("procedural plan return AWAIT must read the resource local");
    };
    assert_eq!(*local, local_decl.local_id());
}
#[test]
fn rejects_legacy_client_state_plan_without_standard_type_identity() {
    let active = empty_active();
    let source = "CREATE SCHEMA examples; \
            CREATE CLIENT FUNCTION examples.state() RETURNS BOOLEAN IS \
            STATE flag BOOLEAN DEFAULT TRUE; \
            BEGIN RETURN TRUE; END;";
    let report = checked_report(source, active.catalogue());
    assert!(
        report.diagnostics().is_empty(),
        "{:?}",
        report.diagnostics()
    );

    let result = prepare(&report, active.pair(), &active);
    assert!(
        matches!(
            result,
            Err(PrepareError::InvalidCheckedBundle { reason })
                if reason == "checked CLIENT state declarations require standard-backed preparation"
        ),
        "result: {result:?}"
    );
}

#[test]
fn standard_client_state_plan_uses_resolved_slot_type_and_default() {
    let verified = invocation_carrier_standard();
    let standard = check_standard_library_source(&verified).unwrap();
    let active = empty_standard_application_active(&verified);
    let context = StandardApplicationCheckContext::try_new(active.catalogue(), &standard).unwrap();
    let source = "CREATE SCHEMA examples; \
            CREATE CLIENT FUNCTION examples.ready() RETURNS BOOLEAN IS \
            STATE flag BOOLEAN DEFAULT TRUE; \
            STATE session_flag BOOLEAN SCOPE SESSION DEFAULT NULL; \
            STATE user_flag BOOLEAN SCOPE USER; \
            BEGIN RETURN TRUE; END;";
    let bundle = SourceBundle::new([SourceUnit::new("application.orna", source)]).unwrap();
    let report = check_standard_application(&bundle, &context);
    assert!(
        report.diagnostics().is_empty(),
        "{:?}",
        report.diagnostics()
    );

    let prepared = prepare_standard_application(&report, active.pair(), &active).unwrap();
    let revision = &prepared.new_function_revisions()[0];
    assert_eq!(revision.artifact().version(), CLIENT_PLAN_STATE_VERSION);
    let plan = StateClientPlan::decode(revision.artifact().payload()).unwrap();
    assert_eq!(
        plan.expression(),
        &ClientExpressionNode::Boolean { value: true }
    );
    assert_eq!(plan.slots().len(), 3);
    let function_id = prepared.candidate().functions()[0].id();
    let flag = &plan.slots()[0];
    assert_eq!(
        flag.state_slot_id(),
        durable_state_slot_id(function_id, "flag")
    );
    assert_eq!(flag.type_id(), TypeId::from_bytes([3; 16]));
    assert_eq!(flag.scope(), StateScope::Local);
    assert_eq!(
        flag.default(),
        &StateDefault::Expression(ClientExpressionNode::Boolean { value: true })
    );
    let session_flag = &plan.slots()[1];
    assert_eq!(
        session_flag.state_slot_id(),
        durable_state_slot_id(function_id, "session_flag")
    );
    assert_eq!(session_flag.type_id(), TypeId::from_bytes([3; 16]));
    assert_eq!(session_flag.scope(), StateScope::Session);
    assert_eq!(session_flag.default(), &StateDefault::Null);
    let user_flag = &plan.slots()[2];
    assert_eq!(
        user_flag.state_slot_id(),
        durable_state_slot_id(function_id, "user_flag")
    );
    assert_eq!(user_flag.type_id(), TypeId::from_bytes([3; 16]));
    assert_eq!(user_flag.scope(), StateScope::User);
    assert_eq!(user_flag.default(), &StateDefault::Unset);
}

#[test]
fn standard_client_state_declaration_evidence_rejects_tampered_owner_or_ordinal() {
    let verified = invocation_carrier_standard();
    let standard = check_standard_library_source(&verified).unwrap();
    let active = empty_standard_application_active(&verified);
    let context = StandardApplicationCheckContext::try_new(active.catalogue(), &standard).unwrap();
    let source = "CREATE SCHEMA examples; \
            CREATE CLIENT FUNCTION examples.ready() RETURNS BOOLEAN IS \
            STATE flag BOOLEAN DEFAULT TRUE; \
            BEGIN RETURN TRUE; END;";
    let bundle = SourceBundle::new([SourceUnit::new("application.orna", source)]).unwrap();
    let report = check_standard_application(&bundle, &context);
    assert!(
        report.diagnostics().is_empty(),
        "{:?}",
        report.diagnostics()
    );
    let state_index = report
        .checked_bundle()
        .unwrap()
        .uses()
        .iter()
        .position(|type_use| matches!(type_use.kind(), crate::CheckedTypeUseKind::State { .. }))
        .unwrap();
    let state_kind = report.checked_bundle().unwrap().uses()[state_index].kind();
    let crate::CheckedTypeUseKind::State { owner, ordinal } = state_kind else {
        unreachable!();
    };

    for tampered_kind in [
        crate::CheckedTypeUseKind::State {
            owner: crate::CheckedFunctionId::Existing(FunctionId::from_bytes([0xf1; 16])),
            ordinal,
        },
        crate::CheckedTypeUseKind::State {
            owner,
            ordinal: ordinal + 1,
        },
    ] {
        let mut tampered = report.clone();
        assert!(tampered.replace_type_use_kind_for_test(state_index, tampered_kind));
        let error = prepare_standard_application(&tampered, active.pair(), &active)
            .expect_err("tampered state declaration evidence must fail closed");
        assert!(matches!(
            error,
            PrepareStandardApplicationError::DeclarationTypeEvidenceMismatch {
                kind: crate::CheckedTypeUseKind::State { .. },
            }
        ));
    }
}

#[test]
fn checked_client_capability_maps_to_the_artifact_requirement_carrier() {
    let read = crate::CheckedClientCapability::new(
        "std.fs.read",
        crate::CheckedClientCapabilityArgument::Text("/home/bob".to_owned()),
    );
    let requirement = client_capability_requirement(&read);
    assert_eq!(requirement.name(), "std.fs.read");
    assert_eq!(
        requirement.argument(),
        &CapabilityArgumentSource::Text("/home/bob".to_owned())
    );

    let secret = crate::CheckedClientCapability::new(
        "std.secret.use",
        crate::CheckedClientCapabilityArgument::Parameter("p_secret".to_owned()),
    );
    let requirement = client_capability_requirement(&secret);
    assert_eq!(requirement.name(), "std.secret.use");
    assert_eq!(
        requirement.argument(),
        &CapabilityArgumentSource::Parameter("p_secret".to_owned())
    );
}

#[test]
fn capability_client_plan_round_trips_the_emitted_version_five_envelope() {
    let requirements = vec![
        CapabilityRequirement::new(
            "std.fs.read",
            CapabilityArgumentSource::Text("/home/bob".to_owned()),
        ),
        CapabilityRequirement::new(
            "std.secret.use",
            CapabilityArgumentSource::Parameter("p_secret".to_owned()),
        ),
    ];
    let plan = CapabilityClientPlan::new(
        InnerClientPlan::Expression(ExpressionClientPlan::new(ClientExpressionNode::String {
            value: "ready".to_owned(),
        })),
        requirements,
    );
    assert_eq!(plan.format_version(), CLIENT_PLAN_CAPABILITY_VERSION);
    assert_eq!(plan.inner_plan_version(), CLIENT_PLAN_EXPRESSION_VERSION);

    let bytes = plan.encode().unwrap();
    assert_eq!(&bytes[8..12], &CLIENT_PLAN_CAPABILITY_VERSION.to_be_bytes());
    let decoded = CapabilityClientPlan::decode(&bytes).unwrap();
    assert_eq!(decoded.format_version(), CLIENT_PLAN_CAPABILITY_VERSION);
    assert_eq!(decoded.inner_plan_version(), CLIENT_PLAN_EXPRESSION_VERSION);
    let InnerClientPlan::Expression(inner) = decoded.inner_plan() else {
        panic!("inner plan must round-trip as an expression plan");
    };
    assert_eq!(
        inner.expression(),
        &ClientExpressionNode::String {
            value: "ready".to_owned()
        }
    );
    assert_eq!(decoded.requirements().len(), 2);
    assert_eq!(decoded.requirements()[0].name(), "std.fs.read");
    assert_eq!(
        decoded.requirements()[0].argument(),
        &CapabilityArgumentSource::Text("/home/bob".to_owned())
    );
    assert_eq!(decoded.requirements()[1].name(), "std.secret.use");
    assert_eq!(
        decoded.requirements()[1].argument(),
        &CapabilityArgumentSource::Parameter("p_secret".to_owned())
    );
}

#[test]
fn prepares_generic_client_expression_without_standard_catalogue() {
    let active = empty_active();
    let source = "CREATE SCHEMA examples; CREATE CLIENT FUNCTION examples.add(p_value INTEGER) RETURNS INTEGER RETURN p_value + 1;";
    let report = checked_report(source, active.catalogue());
    assert!(
        report.diagnostics().is_empty(),
        "{:?}",
        report.diagnostics()
    );
    let prepared = prepare(&report, active.pair(), &active).unwrap();
    let function = prepared
        .candidate()
        .function_by_name(&semantic_name(&["examples", "add"]))
        .unwrap();
    assert_eq!(function.domain(), FunctionDomain::Client);
    assert_eq!(
        function.return_type(),
        &FunctionReturn::Single(ResolvedType::Scalar(StandardScalar::Integer)),
    );
    assert_eq!(prepared.new_function_revisions().len(), 1);
}
