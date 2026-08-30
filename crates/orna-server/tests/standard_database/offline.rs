use super::*;

#[test]
fn checks_accepted_scalar_resource_fixture_offline() -> TestResult<()> {
    let snapshot = verify_standard_library_v2_snapshot(retained_standard_library_v2_snapshot()?)?;
    let standard = check_standard_library_source(&snapshot)?;
    let catalogue = CatalogueSnapshot::new(
        CatalogueRevisionId::from_bytes([0; 16]),
        Vec::new(),
        Vec::new(),
    )?;
    let context = StandardApplicationCheckContext::try_new(&catalogue, &standard)?;
    let source = SourceBundle::new([SourceUnit::new(
        "fixtures/scalar_resource_dogfood.orna",
        include_str!("../fixtures/scalar_resource_dogfood.orna"),
    )])?;
    let report = check_standard_application(&source, &context);
    require(
        report.diagnostics().is_empty(),
        "accepted scalar resource fixture did not check",
    )?;
    let checked = report
        .checked_bundle()
        .ok_or_else(|| failure("accepted scalar resource fixture produced no checked bundle"))?;
    require(
        checked
            .client_functions()
            .any(|function| function.name().parts() == ["scalar_fixture", "call"]),
        "accepted scalar resource fixture is missing scalar_fixture.call",
    )
}

#[test]
fn checks_accepted_stream_resource_fixture_offline() -> TestResult<()> {
    let snapshot = verify_standard_library_v2_snapshot(retained_standard_library_v2_snapshot()?)?;
    let standard = check_standard_library_source(&snapshot)?;
    let catalogue = CatalogueSnapshot::new(
        CatalogueRevisionId::from_bytes([0; 16]),
        Vec::new(),
        Vec::new(),
    )?;
    let context = StandardApplicationCheckContext::try_new(&catalogue, &standard)?;
    let source = SourceBundle::new([SourceUnit::new(
        "fixtures/stream_resource_dogfood.orna",
        include_str!("../fixtures/stream_resource_dogfood.orna"),
    )])?;
    let report = check_standard_application(&source, &context);
    require(
        report.diagnostics().is_empty(),
        "accepted stream resource fixture did not check",
    )?;
    let checked = report
        .checked_bundle()
        .ok_or_else(|| failure("accepted stream resource fixture produced no checked bundle"))?;
    require(
        checked
            .client_functions()
            .any(|function| function.name().parts() == ["stream_fixture", "read"]),
        "accepted stream resource fixture is missing stream_fixture.read",
    )
}
#[test]
fn checks_accepted_expression_client_fixture_offline() -> TestResult<()> {
    let snapshot = verify_standard_library_v2_snapshot(retained_standard_library_v2_snapshot()?)?;
    let standard = check_standard_library_source(&snapshot)?;
    let base = offline_empty_version_two_active(standard.verified_snapshot())?;
    let context = StandardApplicationCheckContext::try_new(base.catalogue(), &standard)?;
    let source = SourceBundle::new([SourceUnit::new(
        "fixtures/expression_client_dogfood.orna",
        include_str!("../fixtures/expression_client_dogfood.orna"),
    )])?;
    let report = check_standard_application(&source, &context);
    if !report.diagnostics().is_empty() {
        return Err(failure(format!(
            "accepted expression CLIENT fixture did not check: {:?}",
            report.diagnostics(),
        )));
    }
    let prepared = prepare_standard_application(&report, base.pair(), &base)?;
    let active = offline_active_from_prepared(&prepared)?;
    let checked = report
        .checked_bundle()
        .ok_or_else(|| failure("accepted expression CLIENT fixture produced no checked bundle"))?;
    for name in [
        "literal",
        "composed",
        "ref_composed",
        "param_composed",
        "external",
    ] {
        require(
            checked
                .client_functions()
                .any(|function| function.name().parts() == ["expr", name]),
            "accepted expression CLIENT fixture is missing a declared function",
        )?;
    }
    let composed = checked
        .client_functions()
        .find(|function| function.name().parts() == ["expr", "composed"])
        .ok_or_else(|| failure("accepted expression CLIENT fixture is missing expr.composed"))?;
    require(
        composed
            .references()
            .iter()
            .any(|reference| reference.kind() == DefinitionReferenceKind::FunctionCall),
        "accepted expression CLIENT fixture did not retain expr.literal as a function call reference",
    )?;
    let external = checked
        .client_functions()
        .find(|function| function.name().parts() == ["expr", "external"])
        .ok_or_else(|| failure("accepted expression CLIENT fixture is missing expr.external"))?;
    require(
        external.references().is_empty(),
        "accepted external CLIENT contract unexpectedly retained executable references",
    )?;

    let ref_composed = active
        .catalogue()
        .functions()
        .iter()
        .find(|function| function.name().parts() == ["expr", "ref_composed"])
        .ok_or_else(|| {
            failure("prepared expression CLIENT fixture is missing expr.ref_composed")
        })?;
    let item_type = active
        .catalogue()
        .object_types()
        .iter()
        .find(|object| object.name().parts() == ["expr", "item"])
        .ok_or_else(|| failure("prepared expression CLIENT fixture is missing expr.item"))?;
    let title_field = item_type
        .field_by_name("title")
        .ok_or_else(|| failure("prepared expression CLIENT fixture is missing expr.item.title"))?;
    let ref_parameter = ref_composed
        .parameters()
        .first()
        .ok_or_else(|| failure("prepared expr.ref_composed is missing p_item"))?;
    require(
        ref_parameter.resolved_type() == ResolvedType::reference(item_type.id()),
        "prepared expr.ref_composed lost its REF expr.item parameter type",
    )?;
    require(
        active.references().iter().any(|reference| {
            reference.source_function() == ref_composed.id()
                && reference.kind() == DefinitionReferenceKind::ObjectReference
                && reference.target() == DefinitionReferenceTarget::ObjectType(item_type.id())
        }),
        "prepared expr.ref_composed lost its object-reference metadata",
    )?;
    let ref_revision = active
        .function_revisions()
        .iter()
        .find(|revision| revision.function() == ref_composed.id())
        .ok_or_else(|| failure("prepared expr.ref_composed is missing its function revision"))?;
    require(
        ref_revision.artifact().version() == EXPRESSION_FORMAT_VERSION,
        "prepared expr.ref_composed did not produce a version-three expression plan",
    )?;
    let ref_plan = ExpressionClientPlan::decode(ref_revision.artifact().payload())?;
    let ClientExpressionNode::Concat { left, right } = ref_plan.expression() else {
        return Err(failure(
            "expr.ref_composed plan lost its outer concatenation",
        ));
    };
    let ClientExpressionNode::Concat {
        left: field_path,
        right: bang,
    } = left.as_ref()
    else {
        return Err(failure(
            "expr.ref_composed plan lost its left-associative concatenation",
        ));
    };
    let ClientExpressionNode::FieldPath { root, fields } = field_path.as_ref() else {
        return Err(failure("expr.ref_composed plan lost its REF field path"));
    };
    require(
        *root == ref_parameter.id() && fields.len() == 1 && fields[0] == title_field.id(),
        "expr.ref_composed plan did not retain p_item.title field identity",
    )?;
    require(
        matches!(bang.as_ref(), ClientExpressionNode::String { value } if value == "!"),
        "expr.ref_composed plan lost the first suffix",
    )?;
    require(
        matches!(right.as_ref(), ClientExpressionNode::String { value } if value == "?"),
        "expr.ref_composed plan lost the second suffix",
    )?;
    let literal_id = active
        .catalogue()
        .functions()
        .iter()
        .find(|function| function.name().parts() == ["expr", "literal"])
        .ok_or_else(|| failure("prepared expression CLIENT fixture is missing expr.literal"))?
        .id();
    let composed_id = active
        .catalogue()
        .functions()
        .iter()
        .find(|function| function.name().parts() == ["expr", "composed"])
        .ok_or_else(|| failure("prepared expression CLIENT fixture is missing expr.composed"))?
        .id();
    let param_composed_id = active
        .catalogue()
        .functions()
        .iter()
        .find(|function| function.name().parts() == ["expr", "param_composed"])
        .ok_or_else(|| {
            failure("prepared expression CLIENT fixture is missing expr.param_composed")
        })?
        .id();
    let external_id = active
        .catalogue()
        .functions()
        .iter()
        .find(|function| function.name().parts() == ["expr", "external"])
        .ok_or_else(|| failure("prepared expression CLIENT fixture is missing expr.external"))?
        .id();
    let functions = active
        .catalogue()
        .functions()
        .iter()
        .map(FunctionDefinition::id)
        .collect::<Vec<_>>();
    let security = SecuritySnapshot::new(
        active.pair(),
        functions,
        vec![Principal::new(
            RAW_CLIENT_USER,
            PrincipalKind::User,
            PrincipalStatus::Active,
        )],
        vec![],
        vec![
            ExecuteGrant::new(RAW_CLIENT_USER, literal_id),
            ExecuteGrant::new(RAW_CLIENT_USER, composed_id),
            ExecuteGrant::new(RAW_CLIENT_USER, param_composed_id),
            ExecuteGrant::new(RAW_CLIENT_USER, external_id),
        ],
    )?;
    let session = security.bind_authenticated_session(RAW_CLIENT_USER, vec![])?;
    let composed_authorisation = match security
        .authorise_execute(&session, InvocationTarget::new(composed_id, active.pair()))
    {
        ExecuteDecision::Allowed(authorisation) => authorisation,
        ExecuteDecision::Denied(reason) => {
            return Err(failure(format!(
                "offline expression CLIENT composed authorisation was denied: {reason:?}"
            )));
        }
    };
    let composed_result = evaluate_client_function(&active, &composed_authorisation)?;
    require(
        composed_result.value() == &RuntimeValue::Text("hello world".to_owned()),
        "offline expression CLIENT composed evaluation returned the wrong value",
    )?;
    let param_authorisation = match security.authorise_execute(
        &session,
        InvocationTarget::new(param_composed_id, active.pair()),
    ) {
        ExecuteDecision::Allowed(authorisation) => authorisation,
        ExecuteDecision::Denied(reason) => {
            return Err(failure(format!(
                "offline expression CLIENT param_composed authorisation was denied: {reason:?}"
            )));
        }
    };
    let param_function = active
        .catalogue()
        .function_by_id(param_composed_id)
        .ok_or_else(|| failure("prepared expression CLIENT fixture lost expr.param_composed"))?;
    let param_parameter = param_function
        .parameters()
        .first()
        .ok_or_else(|| failure("prepared expr.param_composed is missing p_suffix"))?;
    let param_argument = FunctionArgument::new(
        param_parameter.id(),
        RuntimeValue::Text(" world".to_owned()),
    )?;
    let param_result = evaluate_client_function_with_grants_and_arguments(
        &active,
        &param_authorisation,
        std::slice::from_ref(&param_argument),
        &[],
        &LocalCapabilityGrantSet::new(),
    )?;
    require(
        param_result.value() == &RuntimeValue::Text("hello world".to_owned()),
        "offline parameterized CLIENT evaluation returned the wrong typed result",
    )?;
    let literal_authorisation = match security
        .authorise_execute(&session, InvocationTarget::new(literal_id, active.pair()))
    {
        ExecuteDecision::Allowed(authorisation) => authorisation,
        ExecuteDecision::Denied(reason) => {
            return Err(failure(format!(
                "offline expression CLIENT literal authorisation was denied: {reason:?}"
            )));
        }
    };
    let literal_result = evaluate_client_function(&active, &literal_authorisation)?;
    require(
        literal_result.value() == &RuntimeValue::Text("hello".to_owned()),
        "offline expression CLIENT literal evaluation returned the wrong value",
    )?;
    require(
        literal_result.context().function() == literal_id
            && literal_result.context().pair() == active.pair(),
        "offline expression CLIENT literal result retained the wrong invocation context",
    )?;
    let external_authorisation = match security
        .authorise_execute(&session, InvocationTarget::new(external_id, active.pair()))
    {
        ExecuteDecision::Allowed(authorisation) => authorisation,
        ExecuteDecision::Denied(reason) => {
            return Err(failure(format!(
                "offline external CLIENT authorisation was denied: {reason:?}"
            )));
        }
    };
    let external_error = evaluate_client_function(&active, &external_authorisation)
        .expect_err("offline external CLIENT evaluation unexpectedly completed");
    require(
        matches!(
            external_error,
            ClientExecutionError::ExternalContract { identity, .. }
                if identity == "expr.runtime@1"
        ),
        "offline external CLIENT evaluation did not fail closed on expr.runtime@1",
    )
}

#[test]
fn checks_accepted_client_state_fixture_plan_metadata_offline() -> TestResult<()> {
    let snapshot = verify_standard_library_v2_snapshot(retained_standard_library_v2_snapshot()?)?;
    let standard = check_standard_library_source(&snapshot)?;
    let base = offline_empty_version_two_active(standard.verified_snapshot())?;
    let upgrade_v3 = orna_standard::prepare_standard_upgrade_v2_to_v3(&base)?;
    let version_three = offline_active_from_prepared(upgrade_v3.application_revision())?;
    let upgrade_v4 = orna_standard::prepare_standard_upgrade_v3_to_v4(&version_three)?;
    let version_four = offline_active_from_prepared(upgrade_v4.application_revision())?;
    let context = StandardApplicationCheckContext::try_new(
        version_four.catalogue(),
        upgrade_v4.checked_standard_library(),
    )?;
    let source = SourceBundle::new([SourceUnit::new(
        "fixtures/client_state_dogfood.orna",
        include_str!("../fixtures/client_state_dogfood.orna"),
    )])?;
    let report = check_standard_application(&source, &context);
    if !report.diagnostics().is_empty() {
        return Err(failure(format!(
            "accepted CLIENT state fixture did not check: {:?}",
            report.diagnostics(),
        )));
    }

    let prepared = prepare_standard_application(&report, version_four.pair(), &version_four)?;
    let active = offline_active_from_prepared(&prepared)?;
    let function = active
        .catalogue()
        .functions()
        .iter()
        .find(|function| function.name().parts() == ["state_fixture", "scalar"])
        .ok_or_else(|| failure("prepared CLIENT state fixture is missing state_fixture.scalar"))?;
    require(
        function.return_type() == &FunctionReturn::Single(ResolvedType::Value(BOOLEAN_TYPE_ID)),
        "prepared CLIENT state fixture did not retain its Boolean scalar return",
    )?;
    let revision = active
        .function_revisions()
        .iter()
        .find(|revision| revision.function() == function.id())
        .ok_or_else(|| failure("prepared CLIENT state fixture is missing its function revision"))?;
    let plan = StateClientPlan::decode(revision.artifact().payload())?;
    require(
        plan.format_version() == STATE_FORMAT_VERSION,
        "CLIENT state fixture did not produce a version-four plan",
    )?;
    let slots = plan.slots();
    require(
        slots.len() == 3,
        "CLIENT state fixture plan did not retain three declarations",
    )?;
    require(
        slots.iter().all(|slot| slot.type_id() == BOOLEAN_TYPE_ID),
        "CLIENT state fixture plan did not retain scalar Boolean slot types",
    )?;
    require(
        slots[0].scope() == StateScope::Local
            && matches!(
                slots[0].default(),
                StateDefault::Expression(ClientExpressionNode::Boolean { value: true })
            ),
        "CLIENT state fixture did not retain the LOCAL expression default in order",
    )?;
    require(
        slots[1].scope() == StateScope::Session && slots[1].default() == &StateDefault::Null,
        "CLIENT state fixture did not retain the SESSION NULL default in order",
    )?;
    require(
        slots[2].scope() == StateScope::User && slots[2].default() == &StateDefault::Unset,
        "CLIENT state fixture did not retain the USER unset default in order",
    )?;
    let slot_ids = slots
        .iter()
        .map(|slot| slot.state_slot_id())
        .collect::<Vec<_>>();
    require(
        slot_ids.iter().all(|id| id.to_bytes() != [0; 16])
            && slot_ids[0] != slot_ids[1]
            && slot_ids[0] != slot_ids[2]
            && slot_ids[1] != slot_ids[2],
        "CLIENT state fixture plan did not retain distinct non-zero state slot IDs",
    )?;

    Ok(())
}

#[test]
fn checks_and_evaluates_accepted_client_local_assignment_fixture_offline() -> TestResult<()> {
    let snapshot = verify_standard_library_v2_snapshot(retained_standard_library_v2_snapshot()?)?;
    let standard = check_standard_library_source(&snapshot)?;
    let base = offline_empty_version_two_active(standard.verified_snapshot())?;
    let context = StandardApplicationCheckContext::try_new(base.catalogue(), &standard)?;
    let source = SourceBundle::new([SourceUnit::new(
        "fixtures/client_local_assignment_dogfood.orna",
        include_str!("../fixtures/client_local_assignment_dogfood.orna"),
    )])?;
    let report = check_standard_application(&source, &context);
    if !report.diagnostics().is_empty() {
        return Err(failure(format!(
            "accepted CLIENT local assignment fixture did not check: {:?}",
            report.diagnostics(),
        )));
    }
    let prepared = prepare_standard_application(&report, base.pair(), &base)?;
    let active = offline_active_from_prepared(&prepared)?;
    let checked = report.checked_bundle().ok_or_else(|| {
        failure("accepted CLIENT local assignment fixture produced no checked bundle")
    })?;
    let function = checked
        .client_functions()
        .find(|function| function.name().parts() == ["local_assignment_fixture", "assigned"])
        .ok_or_else(|| failure("accepted CLIENT local assignment fixture is missing assigned"))?;
    let function_id = active
        .catalogue()
        .functions()
        .iter()
        .find(|candidate| candidate.name() == function.name())
        .map(FunctionDefinition::id)
        .ok_or_else(|| {
            failure("prepared CLIENT local assignment fixture is missing its function definition")
        })?;
    let revision = active
        .function_revisions()
        .iter()
        .find(|revision| revision.function() == function_id)
        .ok_or_else(|| {
            failure("prepared CLIENT local assignment fixture is missing its function revision")
        })?;
    let plan = ProceduralClientPlan::decode(revision.artifact().payload())?;
    require(
        plan.format_version() == 7
            && plan.locals().len() == 1
            && plan.locals()[0].type_id() == orna_standard::INTEGER_TYPE_ID
            && plan.statements().len() == 2,
        "CLIENT local assignment artifact did not retain version-seven LET and assignment",
    )?;
    let local = plan.locals()[0].local_id();
    require(
        matches!(
            &plan.statements()[0],
            orna_artifact::client_plan::ClientStatement::Let {
                local: statement_local,
                ..
            } if *statement_local == local
        ),
        "CLIENT local assignment artifact did not retain the typed LET",
    )?;
    require(
        matches!(
            &plan.statements()[1],
            orna_artifact::client_plan::ClientStatement::Assignment {
                local: statement_local,
                ..
            } if *statement_local == local
        ),
        "CLIENT local assignment artifact did not retain the plain assignment",
    )?;
    require(
        matches!(
            plan.return_expression(),
            ClientExpressionNode::LocalRead { local: return_local } if *return_local == local
        ),
        "CLIENT local assignment artifact did not return the assigned local",
    )?;

    let functions = active
        .catalogue()
        .functions()
        .iter()
        .map(FunctionDefinition::id)
        .collect::<Vec<_>>();
    let security = SecuritySnapshot::new(
        active.pair(),
        functions,
        vec![Principal::new(
            RAW_CLIENT_USER,
            PrincipalKind::User,
            PrincipalStatus::Active,
        )],
        vec![],
        vec![ExecuteGrant::new(RAW_CLIENT_USER, function_id)],
    )?;
    let session = security.bind_authenticated_session(RAW_CLIENT_USER, vec![])?;
    let authorisation = match security
        .authorise_execute(&session, InvocationTarget::new(function_id, active.pair()))
    {
        ExecuteDecision::Allowed(authorisation) => authorisation,
        ExecuteDecision::Denied(reason) => {
            return Err(failure(format!(
                "offline CLIENT local assignment authorisation was denied: {reason:?}"
            )));
        }
    };
    let result = evaluate_client_function(&active, &authorisation)?;
    require(
        result.value() == &RuntimeValue::Integer(42),
        "offline CLIENT local assignment evaluation returned the wrong value",
    )
}

#[test]
fn exposes_checked_client_body_kind_for_rust_introspection() -> TestResult<()> {
    let snapshot = verify_standard_library_v2_snapshot(retained_standard_library_v2_snapshot()?)?;
    let standard = check_standard_library_source(&snapshot)?;
    let base = offline_empty_version_two_active(standard.verified_snapshot())?;
    let context = StandardApplicationCheckContext::try_new(base.catalogue(), &standard)?;
    let source = SourceBundle::new([SourceUnit::new(
        "fixtures/client_introspection_dogfood.orna",
        include_str!("../fixtures/client_introspection_dogfood.orna"),
    )])?;
    let report = check_standard_application(&source, &context);
    if !report.diagnostics().is_empty() {
        return Err(failure(format!(
            "introspection fixture did not check: {:?}",
            report.diagnostics()
        )));
    }
    let checked = report
        .checked_bundle()
        .ok_or_else(|| failure("missing checked bundle"))?;
    let function = checked
        .client_functions()
        .find(|function| function.name().parts() == ["introspection_demo", "compute"])
        .ok_or_else(|| failure("missing checked introspection function"))?;
    require(
        function.body_kind() == orna_compiler::CheckedClientBodyKind::ControlFlow,
        "Rust introspection did not expose the checked control-flow body kind",
    )
}

#[test]
fn checks_and_evaluates_accepted_client_control_flow_fixture_offline() -> TestResult<()> {
    let snapshot = verify_standard_library_v2_snapshot(retained_standard_library_v2_snapshot()?)?;
    let standard = check_standard_library_source(&snapshot)?;
    let base = offline_empty_version_two_active(standard.verified_snapshot())?;
    let context = StandardApplicationCheckContext::try_new(base.catalogue(), &standard)?;
    let source = SourceBundle::new([SourceUnit::new(
        "fixtures/client_control_flow_dogfood.orna",
        include_str!("../fixtures/client_control_flow_dogfood.orna"),
    )])?;
    let report = check_standard_application(&source, &context);
    if !report.diagnostics().is_empty() {
        return Err(failure(format!(
            "accepted CLIENT control-flow fixture did not check: {:?}",
            report.diagnostics(),
        )));
    }
    let prepared = prepare_standard_application(&report, base.pair(), &base)?;
    let active = offline_active_from_prepared(&prepared)?;
    let checked = report.checked_bundle().ok_or_else(|| {
        failure("accepted CLIENT control-flow fixture produced no checked bundle")
    })?;
    let function = checked
        .client_functions()
        .find(|function| function.name().parts() == ["console_demo", "bounded_counter"])
        .ok_or_else(|| {
            failure("accepted CLIENT control-flow fixture is missing console_demo.bounded_counter")
        })?;
    let function_id = active
        .catalogue()
        .functions()
        .iter()
        .find(|candidate| candidate.name() == function.name())
        .map(FunctionDefinition::id)
        .ok_or_else(|| {
            failure("prepared CLIENT control-flow fixture is missing its function definition")
        })?;
    let functions = active
        .catalogue()
        .functions()
        .iter()
        .map(FunctionDefinition::id)
        .collect::<Vec<_>>();
    let security = SecuritySnapshot::new(
        active.pair(),
        functions,
        vec![Principal::new(
            RAW_CLIENT_USER,
            PrincipalKind::User,
            PrincipalStatus::Active,
        )],
        vec![],
        vec![ExecuteGrant::new(RAW_CLIENT_USER, function_id)],
    )?;
    let session = security.bind_authenticated_session(RAW_CLIENT_USER, vec![])?;
    let authorisation = match security
        .authorise_execute(&session, InvocationTarget::new(function_id, active.pair()))
    {
        ExecuteDecision::Allowed(authorisation) => authorisation,
        ExecuteDecision::Denied(reason) => {
            return Err(failure(format!(
                "offline CLIENT control-flow authorisation was denied: {reason:?}"
            )));
        }
    };
    let result = evaluate_client_function(&active, &authorisation)?;
    require(
        result.value() == &RuntimeValue::Integer(5),
        "offline CLIENT control-flow evaluation returned the wrong value",
    )
}
#[test]
fn checks_and_evaluates_accepted_ui_constructor_showcase_roots_offline() -> TestResult<()> {
    let snapshot = verify_standard_library_v10_snapshot(retained_standard_library_v10_snapshot()?)?;
    let standard = check_standard_library_source(&snapshot)?;
    let base = offline_empty_version_two_active(standard.verified_snapshot())?;
    let context = StandardApplicationCheckContext::try_new(base.catalogue(), &standard)?;
    let source = SourceBundle::new([SourceUnit::new(
        "fixtures/ui_constructor_showcase_dogfood.orna",
        include_str!("../fixtures/ui_constructor_showcase_dogfood.orna"),
    )])?;
    let report = check_standard_application(&source, &context);
    if !report.diagnostics().is_empty() {
        return Err(failure(format!(
            "accepted UI constructor showcase did not check: {:?}",
            report.diagnostics()
        )));
    }
    let prepared = prepare_standard_application(&report, base.pair(), &base)?;
    let active = offline_active_from_prepared(&prepared)?;

    let function_ids = active
        .catalogue()
        .functions()
        .iter()
        .map(FunctionDefinition::id)
        .collect::<Vec<_>>();
    let security = SecuritySnapshot::new(
        active.pair(),
        function_ids.iter().copied().collect(),
        vec![Principal::new(
            RAW_CLIENT_USER,
            PrincipalKind::User,
            PrincipalStatus::Active,
        )],
        vec![],
        function_ids
            .iter()
            .copied()
            .map(|function| ExecuteGrant::new(RAW_CLIENT_USER, function))
            .collect(),
    )?;
    let session = security.bind_authenticated_session(RAW_CLIENT_USER, vec![])?;

    let roots: [(&str, &str, &[&str]); 3] = [
        (
            "main",
            "UI Constructor Showcase",
            &[
                "std.ui.tabs",
                "std.ui.column",
                "std.ui.panel",
                "std.ui.text",
            ],
        ),
        (
            "input_window",
            "Input Constructor Showcase",
            &["std.ui.text_input"],
        ),
        (
            "control_window",
            "Button Constructor Showcase",
            &["std.ui.row", "std.ui.button"],
        ),
    ];
    for (root_name, expected_title, expected_contracts) in roots {
        let function = active
            .catalogue()
            .functions()
            .iter()
            .find(|function| function.name().parts() == ["ui_constructor_showcase", root_name])
            .ok_or_else(|| {
                failure(format!(
                    "prepared UI constructor showcase is missing {root_name}"
                ))
            })?;
        let authorisation = match security.authorise_execute(
            &session,
            InvocationTarget::new(function.id(), active.pair()),
        ) {
            ExecuteDecision::Allowed(authorisation) => authorisation,
            ExecuteDecision::Denied(reason) => {
                return Err(failure(format!(
                    "UI constructor showcase root {root_name} authorisation was denied: {reason:?}"
                )));
            }
        };

        let expected_title = expected_title.to_owned();
        let expected_contracts = expected_contracts
            .iter()
            .map(|contract| (*contract).to_owned())
            .collect::<Vec<_>>();
        let window_calls = std::rc::Rc::new(std::cell::Cell::new(0_u32));
        let provider_window_calls = window_calls.clone();
        let mut executor = orna_client::DeterministicClientResourceExecutor::new(
            |_request: &ClientResourceRequest| -> Result<RuntimeValue, String> {
                Err("resource executor was not used".to_owned())
            },
        )
        .with_external_contract(
            move |request: &ClientExternalContractRequest| -> Result<RuntimeValue, String> {
                provider_window_calls.set(provider_window_calls.get() + 1);
                assert_eq!(
                    request.identity(),
                    orna_standard::STD_UI_WINDOW_RUNTIME_CONTRACT
                );
                assert_eq!(request.arguments().len(), 2);
                assert_eq!(
                    request.arguments()[0].0,
                    orna_standard::STD_UI_WINDOW_TITLE_PARAMETER_ID
                );
                assert_eq!(
                    request.arguments()[0].1,
                    RuntimeValue::Text(expected_title.clone())
                );
                assert_eq!(
                    request.arguments()[1].0,
                    orna_standard::STD_UI_WINDOW_CONTENT_PARAMETER_ID
                );
                let RuntimeValue::Opaque(content) = &request.arguments()[1].1 else {
                    panic!("std.ui.window content argument was not an opaque UI value");
                };
                assert_eq!(content.opaque_type(), orna_standard::STD_UI_TYPE_ID);

                let payload = content.canonical_payload();
                let magic = orna_standard::UI_MAGIC.as_bytes();
                let prefix_length = magic.len() + 4;
                assert!(
                    payload.len() >= prefix_length && payload.starts_with(magic),
                    "std.ui.window content did not use canonical ORNA-UI/1 framing"
                );
                let body_length = u32::from_be_bytes(
                    payload[magic.len()..prefix_length]
                        .try_into()
                        .expect("the UI body length is exactly four bytes"),
                ) as usize;
                assert_eq!(
                    payload.len(),
                    prefix_length + body_length,
                    "std.ui.window content framing had trailing or truncated bytes"
                );
                let body_bytes = &payload[prefix_length..];
                let body: serde_json::Value =
                    serde_json::from_slice(body_bytes).expect("UI content body must be JSON");
                assert_eq!(
                    serde_json::to_vec(&body).expect("UI content body must re-encode"),
                    body_bytes,
                    "std.ui.window content body was not canonical JSON"
                );
                let mut node = &body;
                for (index, expected_contract) in expected_contracts.iter().enumerate() {
                    assert_eq!(
                        node.get("kind").and_then(serde_json::Value::as_str),
                        Some("node")
                    );
                    let contract = node
                        .get("contract")
                        .and_then(serde_json::Value::as_object)
                        .expect("UI content node must carry a contract");
                    assert_eq!(
                        contract.get("id").and_then(serde_json::Value::as_str),
                        Some(expected_contract.as_str())
                    );
                    assert_eq!(
                        contract.get("name").and_then(serde_json::Value::as_str),
                        Some(expected_contract.as_str())
                    );
                    assert_eq!(
                        contract.get("version").and_then(serde_json::Value::as_str),
                        Some("1.0")
                    );
                    if index + 1 < expected_contracts.len() {
                        let children = node
                            .get("slots")
                            .and_then(serde_json::Value::as_object)
                            .and_then(|slots| slots.get("content"))
                            .and_then(serde_json::Value::as_array)
                            .expect("container UI node must carry a content slot");
                        assert_eq!(children.len(), 1);
                        node = children
                            .first()
                            .expect("container UI content slot must have one child");
                    }
                }
                Ok(RuntimeValue::Opaque(content.clone()))
            },
        );
        let result = evaluate_client_function_with_arguments_and_executor(
            &active,
            &authorisation,
            &[],
            &mut executor,
        )?;
        require(
            window_calls.get() == 1,
            "UI constructor showcase root did not reach std.ui.window exactly once",
        )?;
        let RuntimeValue::Opaque(ui) = result.value() else {
            return Err(failure(format!(
                "UI constructor showcase root {root_name} did not return an opaque UI value"
            )));
        };
        require(
            ui.opaque_type() == orna_standard::STD_UI_TYPE_ID,
            "UI constructor showcase root returned the wrong opaque type",
        )?;
        let payload = ui.canonical_payload();
        let magic = orna_standard::UI_MAGIC.as_bytes();
        let prefix_length = magic.len() + 4;
        require(
            payload.len() >= prefix_length && payload.starts_with(magic),
            "UI constructor showcase root returned a non-canonical UI frame",
        )?;
        let body_length = u32::from_be_bytes(
            payload[magic.len()..prefix_length]
                .try_into()
                .map_err(|_| failure("UI result body length was truncated"))?,
        ) as usize;
        require(
            payload.len() == prefix_length + body_length,
            "UI constructor showcase root returned trailing or truncated UI bytes",
        )?;
        let body = &payload[prefix_length..];
        let decoded: serde_json::Value = serde_json::from_slice(body)
            .map_err(|error| failure(format!("UI result body was not JSON: {error}")))?;
        require(
            serde_json::to_vec(&decoded)
                .map_err(|error| failure(format!("UI result body did not re-encode: {error}")))?
                == body,
            "UI constructor showcase root returned non-canonical JSON",
        )?;
    }
    Ok(())
}

#[test]
fn checks_and_evaluates_accepted_static_studio_shell_offline() -> TestResult<()> {
    let snapshot = verify_standard_library_v10_snapshot(retained_standard_library_v10_snapshot()?)?;
    let standard = check_standard_library_source(&snapshot)?;
    let base = offline_empty_version_two_active(standard.verified_snapshot())?;
    let context = StandardApplicationCheckContext::try_new(base.catalogue(), &standard)?;
    let source = SourceBundle::new([SourceUnit::new(
        "fixtures/studio_static_app_dogfood.orna",
        include_str!("../fixtures/studio_static_app_dogfood.orna"),
    )])?;
    let report = check_standard_application(&source, &context);
    if !report.diagnostics().is_empty() {
        return Err(failure(format!(
            "static Studio shell did not check: {:?}",
            report.diagnostics()
        )));
    }
    let prepared = prepare_standard_application(&report, base.pair(), &base)?;
    let active = offline_active_from_prepared(&prepared)?;
    let function = active
        .catalogue()
        .functions()
        .iter()
        .find(|function| function.name().parts() == ["studio_static_app", "main"])
        .ok_or_else(|| failure("prepared static Studio shell is missing studio_static_app.main"))?;
    let function_ids = active
        .catalogue()
        .functions()
        .iter()
        .map(FunctionDefinition::id)
        .collect::<Vec<_>>();
    let security = SecuritySnapshot::new(
        active.pair(),
        function_ids.iter().copied().collect(),
        vec![Principal::new(
            RAW_CLIENT_USER,
            PrincipalKind::User,
            PrincipalStatus::Active,
        )],
        vec![],
        function_ids
            .iter()
            .copied()
            .map(|function| ExecuteGrant::new(RAW_CLIENT_USER, function))
            .collect(),
    )?;
    let session = security.bind_authenticated_session(RAW_CLIENT_USER, vec![])?;
    let authorisation = match security.authorise_execute(
        &session,
        InvocationTarget::new(function.id(), active.pair()),
    ) {
        ExecuteDecision::Allowed(authorisation) => authorisation,
        ExecuteDecision::Denied(reason) => {
            return Err(failure(format!(
                "static Studio shell authorisation was denied: {reason:?}"
            )));
        }
    };

    let window_calls = std::rc::Rc::new(std::cell::Cell::new(0_u32));
    let provider_window_calls = window_calls.clone();
    let mut executor = orna_client::DeterministicClientResourceExecutor::new(
        |_request: &ClientResourceRequest| -> Result<RuntimeValue, String> {
            Err("static Studio shell used an unexpected resource executor".to_owned())
        },
    )
    .with_external_contract(
        move |request: &ClientExternalContractRequest| -> Result<RuntimeValue, String> {
            provider_window_calls.set(provider_window_calls.get() + 1);
            assert_eq!(
                request.identity(),
                orna_standard::STD_UI_WINDOW_RUNTIME_CONTRACT
            );
            assert_eq!(request.arguments().len(), 2);
            assert_eq!(
                request.arguments()[0].0,
                orna_standard::STD_UI_WINDOW_TITLE_PARAMETER_ID
            );
            assert_eq!(
                request.arguments()[0].1,
                RuntimeValue::Text("Orna Studio".to_owned())
            );
            assert_eq!(
                request.arguments()[1].0,
                orna_standard::STD_UI_WINDOW_CONTENT_PARAMETER_ID
            );
            let RuntimeValue::Opaque(content) = &request.arguments()[1].1 else {
                panic!("static Studio shell content was not an opaque UI value");
            };
            assert_eq!(content.opaque_type(), orna_standard::STD_UI_TYPE_ID);

            let payload = content.canonical_payload();
            let magic = orna_standard::UI_MAGIC.as_bytes();
            let prefix_length = magic.len() + 4;
            assert!(payload.starts_with(magic));
            let body_length = u32::from_be_bytes(
                payload[magic.len()..prefix_length]
                    .try_into()
                    .expect("the UI body length is exactly four bytes"),
            ) as usize;
            assert_eq!(payload.len(), prefix_length + body_length);
            let body_bytes = &payload[prefix_length..];
            let body: serde_json::Value =
                serde_json::from_slice(body_bytes).expect("static Studio UI body must be JSON");
            assert_eq!(
                serde_json::to_vec(&body).expect("static Studio UI body must re-encode"),
                body_bytes
            );

            let mut node = &body;
            for (index, expected_contract) in ["std.ui.column", "std.ui.row", "std.ui.text"]
                .iter()
                .enumerate()
            {
                let contract = node
                    .get("contract")
                    .and_then(serde_json::Value::as_object)
                    .and_then(|contract| contract.get("id"))
                    .and_then(serde_json::Value::as_str);
                assert_eq!(contract, Some(*expected_contract));
                assert_eq!(
                    node.get("contract")
                        .and_then(serde_json::Value::as_object)
                        .and_then(|contract| contract.get("name"))
                        .and_then(serde_json::Value::as_str),
                    Some(*expected_contract)
                );
                assert_eq!(
                    node.get("contract")
                        .and_then(serde_json::Value::as_object)
                        .and_then(|contract| contract.get("version"))
                        .and_then(serde_json::Value::as_str),
                    Some("1.0")
                );
                if index < 2 {
                    node = node
                        .get("slots")
                        .and_then(serde_json::Value::as_object)
                        .and_then(|slots| slots.get("content"))
                        .and_then(serde_json::Value::as_array)
                        .and_then(|children| children.first())
                        .expect("static Studio container must have one content child");
                }
            }
            Ok(RuntimeValue::Opaque(content.clone()))
        },
    );
    let result = evaluate_client_function_with_arguments_and_executor(
        &active,
        &authorisation,
        &[],
        &mut executor,
    )?;
    require(
        window_calls.get() == 1,
        "static Studio shell did not reach std.ui.window exactly once",
    )?;
    let RuntimeValue::Opaque(ui) = result.value() else {
        return Err(failure(
            "static Studio shell did not return an opaque UI value",
        ));
    };
    require(
        ui.opaque_type() == orna_standard::STD_UI_TYPE_ID,
        "static Studio shell returned the wrong opaque type",
    )?;
    let payload = ui.canonical_payload();
    require(
        payload.starts_with(orna_standard::UI_MAGIC.as_bytes()),
        "static Studio shell returned a non-canonical UI frame",
    )?;
    Ok(())
}

#[test]
fn checks_and_prepares_server_function_dogfood_fixture_offline() -> TestResult<()> {
    let snapshot = verify_standard_library_v2_snapshot(retained_standard_library_v2_snapshot()?)?;
    let standard = check_standard_library_source(&snapshot)?;
    let base = offline_empty_version_two_active(standard.verified_snapshot())?;
    let context = StandardApplicationCheckContext::try_new(base.catalogue(), &standard)?;
    let source = SourceBundle::new([SourceUnit::new(
        "fixtures/server_function_dogfood.orna",
        include_str!("../fixtures/server_function_dogfood.orna"),
    )])?;
    let report = check_standard_application(&source, &context);
    if !report.diagnostics().is_empty() {
        return Err(failure(format!(
            "accepted SERVER dogfood fixture did not check: {:?}",
            report.diagnostics(),
        )));
    }
    let checked = report
        .checked_bundle()
        .ok_or_else(|| failure("accepted SERVER dogfood fixture produced no checked bundle"))?;
    require(
        checked.server_functions().len() == 5,
        "accepted SERVER dogfood fixture produced an unexpected function count",
    )?;
    for name in ["read", "distinct_values", "stream", "read_item", "update"] {
        require(
            checked
                .server_functions()
                .any(|function| function.name().parts() == ["dogfood", name]),
            "accepted SERVER dogfood fixture is missing a declared function",
        )?;
    }

    let prepared = prepare_standard_application(&report, base.pair(), &base)?;
    require(
        prepared.expected_base() == base.pair(),
        "prepared SERVER dogfood fixture retained the wrong expected base pair",
    )?;
    let expected_candidate_pair =
        RevisionPair::new(prepared.source().id(), prepared.candidate().revision());
    require(
        prepared.candidate_pair() == expected_candidate_pair,
        "prepared SERVER dogfood fixture produced the wrong candidate pair",
    )?;
    require(
        prepared.candidate_pair() != base.pair(),
        "prepared SERVER dogfood fixture did not advance the revision pair",
    )?;
    let active = offline_active_from_prepared(&prepared)?;
    require(
        active.pair() == expected_candidate_pair,
        "prepared SERVER dogfood fixture could not become the expected active pair",
    )
}

#[test]
fn checks_accepted_action_fixture_offline() -> TestResult<()> {
    let snapshot = verify_standard_library_v6_snapshot(retained_standard_library_v6_snapshot()?)?;
    let standard = check_standard_library_source(&snapshot)?;
    let catalogue = CatalogueSnapshot::new(
        CatalogueRevisionId::from_bytes([0; 16]),
        Vec::new(),
        Vec::new(),
    )?;
    let context = StandardApplicationCheckContext::try_new(&catalogue, &standard)?;
    let source = SourceBundle::new([SourceUnit::new(
        "fixtures/action_dogfood.orna",
        RAW_ACTION_SOURCE.to_owned(),
    )])?;
    let report = check_standard_application(&source, &context);
    if !report.diagnostics().is_empty() {
        return Err(failure(format!(
            "accepted V6 action fixture did not check: {:?}",
            report.diagnostics()
        )));
    }
    let checked = report
        .checked_bundle()
        .ok_or_else(|| failure("accepted V6 action fixture produced no checked bundle"))?;
    let call = checked
        .client_functions()
        .find(|function| function.name().parts() == ["action_fixture", "call"])
        .ok_or_else(|| failure("accepted V6 action fixture is missing action_fixture.call"))?;
    let call_local = checked
        .client_functions()
        .find(|function| function.name().parts() == ["action_fixture", "call_local"])
        .ok_or_else(|| {
            failure("accepted V6 action fixture is missing action_fixture.call_local")
        })?;
    if !(matches!(call.return_type().named_type(), Some(CheckedTypeId::Existing(type_id)) if type_id == STD_ACTION_TYPE_ID)
        && matches!(call_local.return_type().named_type(), Some(CheckedTypeId::Existing(type_id)) if type_id == STD_ACTION_TYPE_ID))
    {
        return Err(failure(format!(
            "accepted V6 action fixture did not retain std.Action return shape: call={:?}, local={:?}",
            call.return_type(),
            call_local.return_type(),
        )));
    }
    require(
        checked
            .client_functions()
            .any(|function| function.name().parts() == ["action_fixture", "local"]),
        "accepted V6 action fixture is missing local CLIENT action target",
    )
}
