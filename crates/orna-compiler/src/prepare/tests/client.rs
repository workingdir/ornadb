//! Client expression, state, capability, and resource preparation tests.

use super::*;
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
fn accepted_client_action_preparation_preserves_durable_operation_identity_and_arguments() {
    let verified = action_standard();
    let standard = check_standard_library_source(&verified).unwrap();
    let active = empty_standard_application_active(&verified);
    let context = StandardApplicationCheckContext::try_new(active.catalogue(), &standard).unwrap();
    let source = r#"CREATE SCHEMA action_fixture;

CREATE CLIENT FUNCTION action_fixture.local(p_first TEXT, p_second TEXT)
    RETURNS TEXT AS p_first;

CREATE CLIENT FUNCTION action_fixture.call_local(p_first TEXT, p_second TEXT)
    RETURNS std.Action AS std.action.call(
        target => action_fixture.local,
        arguments => std.call.args(p_second => p_second, p_first => p_first)
    );"#;
    let bundle = SourceBundle::new([SourceUnit::new("action.orna", source)]).unwrap();
    let report = check_standard_application(&bundle, &context);
    assert!(
        report.diagnostics().is_empty(),
        "accepted action source did not check: {:?}",
        report.diagnostics()
    );

    let checked_call_site = {
        let checked = report.preparation_view().unwrap().checked();
        let caller = checked
            .client_functions()
            .iter()
            .find(|function| function.name().parts() == ["action_fixture", "call_local"])
            .unwrap();
        let CheckedClientFunctionBody::Expression { expression } = caller.body() else {
            panic!("action client must use an expression body");
        };
        let CheckedClientExpression::Action { operation } = expression else {
            panic!("action client must retain its action operation");
        };
        operation.call_site()
    };

    let prepared = prepare_standard_application(&report, active.pair(), &active).unwrap();
    let target = prepared
        .candidate()
        .functions()
        .iter()
        .find(|function| function.name().parts() == ["action_fixture", "local"])
        .unwrap();
    let caller = prepared
        .candidate()
        .functions()
        .iter()
        .find(|function| function.name().parts() == ["action_fixture", "call_local"])
        .unwrap();
    let revision = prepared
        .new_function_revisions()
        .iter()
        .find(|revision| revision.function() == caller.id())
        .unwrap();
    assert_eq!(revision.artifact().version(), CLIENT_PLAN_ACTION_VERSION);

    let plan = ActionClientPlan::decode(revision.artifact().payload()).unwrap();
    let operation = plan.operation();
    assert_eq!(operation.domain(), ActionTargetDomain::Client);
    assert_eq!(operation.target(), target.id());
    assert_eq!(operation.target_revision(), prepared.candidate_pair());
    assert_eq!(operation.call_site(), checked_call_site);
    assert_ne!(operation.call_site().to_bytes(), [0; 16]);
    let text_type_id = verified
        .catalogue()
        .value_type_by_name(&semantic_name(&["std", "types", "character_large_object"]))
        .unwrap()
        .id();
    assert_eq!(operation.result_type(), text_type_id);

    let mut expected_arguments: Vec<_> = target
        .parameters()
        .iter()
        .map(|target_parameter| {
            let caller_parameter = caller
                .parameters()
                .iter()
                .find(|parameter| parameter.name() == target_parameter.name())
                .unwrap();
            (
                target_parameter.id(),
                ClientExpressionNode::ParameterRead {
                    parameter: caller_parameter.id(),
                },
            )
        })
        .collect();
    expected_arguments.sort_by_key(|(parameter, _)| *parameter);
    assert_eq!(operation.arguments(), expected_arguments.as_slice());
}

#[test]
fn named_standard_resource_result_uses_catalogue_value_identity() {
    let verified = action_standard();
    let text_type_id = verified
        .catalogue()
        .value_type_by_name(&semantic_name(&["std", "types", "character_large_object"]))
        .unwrap()
        .id();
    let action_type_id = verified
        .catalogue()
        .value_type_by_name(&semantic_name(&["std", "action", "action"]))
        .unwrap()
        .id();
    let base_active = empty_standard_application_active(&verified);
    let target_schema_id = SchemaId::from_bytes([0xc0; 16]);
    let target_id = FunctionId::from_bytes([0xc1; 16]);
    let target_first_parameter_id = ParameterId::from_bytes([0xc3; 16]);
    let target_second_parameter_id = ParameterId::from_bytes([0xc2; 16]);
    let target = FunctionDefinition::new(
        target_id,
        semantic_name(&["resource_catalogue", "forward"]),
        FunctionDomain::Server,
        vec![
            ParameterDefinition::new(
                target_first_parameter_id,
                "p_first",
                0,
                ResolvedType::Value(text_type_id),
                None,
            ),
            ParameterDefinition::new(
                target_second_parameter_id,
                "p_second",
                1,
                ResolvedType::Value(text_type_id),
                None,
            ),
        ],
        FunctionReturn::Single(ResolvedType::Named(action_type_id)),
        FunctionRevisionId::from_bytes([0xc4; 16]),
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::ReadOnly),
        FunctionVolatility::Stable,
    );
    let target_artifact = ExecutableArtifact::new(
        ExecutableArtifactKind::Server,
        SERVER_PLAN_FORMAT,
        SERVER_PLAN_VERSION,
        vec![0],
        artifact_payload_digest(&[0]).unwrap(),
    )
    .unwrap();
    let target_semantic_hash = function_semantic_digest_with_version(
        FunctionSemanticHashVersion::Version1,
        &target,
        SERVER_PLAN_LANGUAGE_VERSION,
        &target_artifact,
        &[],
        &[],
    )
    .unwrap();
    let target_source_origin =
        SourceOrigin::new(base_active.source().units()[0].id(), 0, 0).unwrap();
    let target_revision = FunctionRevisionRecord::new(
        target_id,
        FunctionRevisionId::from_bytes([0xc4; 16]),
        1,
        target_source_origin,
        function_declaration_digest(b"resource_catalogue.forward").unwrap(),
        target_semantic_hash,
        SERVER_PLAN_LANGUAGE_VERSION,
        target_artifact,
    )
    .unwrap();
    let target_origins = vec![
        DefinitionOrigin::new(
            DefinitionIdentity::Schema(target_schema_id),
            target_source_origin,
        ),
        DefinitionOrigin::new(
            DefinitionIdentity::Function(target_id),
            target_source_origin,
        ),
        DefinitionOrigin::new(
            DefinitionIdentity::Parameter {
                owner: target_id,
                parameter: target_first_parameter_id,
            },
            target_source_origin,
        ),
        DefinitionOrigin::new(
            DefinitionIdentity::Parameter {
                owner: target_id,
                parameter: target_second_parameter_id,
            },
            target_source_origin,
        ),
    ];
    let catalogue = CatalogueSnapshot::new_with_functions_and_types(
        CatalogueRevisionId::from_bytes([0xc5; 16]),
        vec![SchemaDefinition::new(
            target_schema_id,
            semantic_name(&["resource_catalogue"]),
        )],
        Vec::new(),
        Vec::new(),
        Vec::new(),
        vec![target],
    )
    .unwrap();
    let hash_context = CatalogueHashContext::version_two(verified.clone());
    let catalogue_hash = catalogue_digest_with_context(
        &hash_context,
        &catalogue,
        std::slice::from_ref(&target_revision),
        &[],
        &target_origins,
        &[],
    )
    .unwrap();
    let active = ActiveDatabaseRevision::new_with_catalogue_hash_context(
        ActiveDatabaseRevisionInput::new(
            RevisionPair::new(base_active.source().id(), catalogue.revision()),
            base_active.source().clone(),
            catalogue,
            catalogue_hash,
            ActiveRevisionContent::new(
                Vec::new(),
                vec![target_revision],
                target_origins,
                Vec::new(),
            ),
        ),
        hash_context,
    )
    .unwrap();
    let standard = check_standard_library_source(&verified).unwrap();
    let context = StandardApplicationCheckContext::try_new(active.catalogue(), &standard).unwrap();
    let source = r#"CREATE SCHEMA resource_fixture;

CREATE CLIENT FUNCTION resource_fixture.call(p_first TEXT, p_second TEXT)
RETURNS std.Action IS

BEGIN
    RETURN AWAIT std.data.resource(
        target => resource_catalogue.forward,
        arguments => std.call.args(p_second => p_second, p_first => p_first)
    );
END;"#;
    let bundle = SourceBundle::new([SourceUnit::new("named-resource.orna", source)]).unwrap();
    let report = check_standard_application(&bundle, &context);
    assert!(
        report.diagnostics().is_empty(),
        "named resource source did not check: {:?}",
        report.diagnostics()
    );

    let checked = report.preparation_view().unwrap().checked();
    let checked_caller = checked
        .client_functions()
        .iter()
        .find(|function| function.name().parts() == ["resource_fixture", "call"])
        .unwrap();
    let checked_call_site = {
        let CheckedClientFunctionBody::Expression { expression } = checked_caller.body() else {
            panic!("named resource client must use an expression body");
        };
        let CheckedClientExpression::Await { expression, .. } = expression else {
            panic!("named resource client must await its resource");
        };
        let CheckedClientExpression::Resource { operation } = expression.as_ref() else {
            panic!("named resource client must retain its resource operation");
        };
        assert_eq!(operation.target(), CheckedFunctionId::Existing(target_id));
        assert_eq!(operation.standard_result_type(), None);
        assert_eq!(
            operation.result_type(),
            SemanticType::Named(CheckedTypeId::Existing(action_type_id))
        );
        operation.call_site()
    };

    let prepared = prepare_standard_application(&report, active.pair(), &active).unwrap();
    let caller = prepared
        .candidate()
        .functions()
        .iter()
        .find(|function| function.name().parts() == ["resource_fixture", "call"])
        .unwrap();
    let revision = prepared
        .new_function_revisions()
        .iter()
        .find(|revision| revision.function() == caller.id())
        .unwrap();
    assert_eq!(revision.artifact().version(), CLIENT_PLAN_RESOURCE_VERSION);
    let plan = ResourceClientPlan::decode(revision.artifact().payload()).unwrap();
    let ClientExpressionNode::Await { expression } = plan.expression() else {
        panic!("prepared named resource plan must await the resource");
    };
    let ClientExpressionNode::Resource { operation } = expression.as_ref() else {
        panic!("prepared named resource plan must contain a resource operation");
    };
    assert_eq!(
        operation.kind(),
        orna_artifact::client_plan::ResourceKind::Scalar
    );
    assert_eq!(operation.target(), target_id);
    assert_eq!(operation.target_revision(), prepared.candidate_pair());
    assert_eq!(operation.result_type(), action_type_id);
    assert_eq!(operation.call_site(), checked_call_site);
    assert_ne!(operation.call_site().to_bytes(), [0; 16]);
    let caller_parameter = |name: &str| {
        caller
            .parameters()
            .iter()
            .find(|parameter| parameter.name() == name)
            .unwrap()
            .id()
    };
    let mut expected_arguments = vec![
        (
            target_second_parameter_id,
            ClientExpressionNode::ParameterRead {
                parameter: caller_parameter("p_second"),
            },
        ),
        (
            target_first_parameter_id,
            ClientExpressionNode::ParameterRead {
                parameter: caller_parameter("p_first"),
            },
        ),
    ];
    expected_arguments.sort_by_key(|(parameter, _)| *parameter);
    assert_eq!(operation.arguments(), expected_arguments.as_slice());
}

#[test]
fn standard_stream_resource_preparation_materialises_durable_operation_artifact() {
    let verified = resource_standard();
    let text_type_id = verified
        .catalogue()
        .value_type_by_name(&semantic_name(&["std", "types", "character_large_object"]))
        .unwrap()
        .id();
    let standard = check_standard_library_source(&verified).unwrap();
    let active = empty_standard_application_active(&verified);
    let context = StandardApplicationCheckContext::try_new(active.catalogue(), &standard).unwrap();
    let bundle = SourceBundle::new([SourceUnit::new(
        "fixtures/stream_resource_dogfood.orna",
        include_str!("../../../../orna-server/tests/fixtures/stream_resource_dogfood.orna"),
    )])
    .unwrap();
    let report = check_standard_application(&bundle, &context);
    assert!(
        report.diagnostics().is_empty(),
        "accepted stream resource fixture did not check: {:?}",
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
            panic!("stream resource client must use an expression body");
        };
        let CheckedClientExpression::Await { expression, .. } = expression else {
            panic!("stream resource client must await its resource");
        };
        let CheckedClientExpression::Resource { operation } = expression.as_ref() else {
            panic!("stream resource client must retain its resource operation");
        };
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
        &FunctionReturn::Stream(ResolvedType::Value(text_type_id))
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
        ResolvedType::scalar(StandardScalar::CharacterLargeObject)
    );
    let ExpressionKind::FieldPath { ref steps, .. } = target_plan.projections[0].kind else {
        panic!("stream SERVER plan projection must be a field path");
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
        panic!("prepared stream resource plan must keep AWAIT at the return expression");
    };
    let ClientExpressionNode::Resource {
        operation: artifact,
    } = expression.as_ref()
    else {
        panic!("prepared stream resource plan must contain a resource operation under AWAIT");
    };
    assert_eq!(
        artifact.kind(),
        orna_artifact::client_plan::ResourceKind::Stream
    );
    assert_eq!(artifact.target(), target.id());
    assert_eq!(artifact.target_revision(), prepared.candidate_pair());
    assert_eq!(artifact.result_type(), text_type_id);
    assert_eq!(artifact.call_site(), checked_call_site);
    assert_ne!(artifact.call_site().to_bytes(), [0; 16]);
    assert!(artifact.arguments().is_empty());
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
