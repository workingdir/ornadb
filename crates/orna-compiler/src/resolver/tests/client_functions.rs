use super::*;

#[test]
fn checks_client_boolean_constant_with_exact_model_and_literal_location() {
    let source = "CREATE SCHEMA examples; CREATE CLIENT FUNCTION examples.enabled() RETURNS BOOL RETURN tRuE;";
    let report = check(&bundle([("client.orna", source)]), &empty_catalogue());

    assert!(report.diagnostics().is_empty());
    let checked = report.checked_bundle().unwrap();
    assert!(checked.server_functions().is_empty());
    let function = &checked.client_functions()[0];
    assert_eq!(function.name().to_string(), "examples.enabled");
    assert_eq!(function.domain(), FunctionDomain::Client);
    assert!(function.id().is_provisional());
    assert!(function.parameters().is_empty());
    assert_eq!(
        function.return_type(),
        SemanticType::scalar(StandardScalar::Boolean)
    );
    assert_eq!(function.security(), FunctionSecurity::Invoker);
    assert_eq!(function.transaction(), None);
    assert_eq!(function.volatility(), FunctionVolatility::Immutable);
    assert!(function.references().is_empty());
    assert_eq!(
        function.location().span().start(),
        source.find("CREATE CLIENT").unwrap()
    );
    assert_eq!(function.location().span().end(), source.len());
    let literal_start = source.find("tRuE").unwrap();
    let (value, literal_location) = function.boolean_body().unwrap();
    assert!(value);
    assert_eq!(literal_location.logical_path(), "client.orna");
    assert_eq!(literal_location.span().start(), literal_start);
    assert_eq!(literal_location.span().end(), literal_start + 4);
}

#[test]
fn rejects_client_integer_literals_outside_i32_range_and_accepts_boundary() {
    let out_of_range = "CREATE SCHEMA examples; CREATE CLIENT FUNCTION examples.value() RETURNS INTEGER AS 2147483648;";
    let report = check(&bundle([("client.orna", out_of_range)]), &empty_catalogue());
    assert_eq!(report.diagnostics().len(), 1);
    assert_eq!(report.diagnostics()[0].code(), DiagnosticCode::TypeMismatch);
    assert_eq!(
        report.diagnostics()[0].message(),
        "CLIENT integer literal is outside the INTEGER range"
    );
    assert_eq!(
        report.diagnostics()[0].location().span().start(),
        out_of_range.find("2147483648").unwrap()
    );
    assert_no_checked_bundle(&report);

    let in_range = "CREATE SCHEMA examples; CREATE CLIENT FUNCTION examples.value() RETURNS INTEGER AS 2147483647;";
    let report = check(&bundle([("client.orna", in_range)]), &empty_catalogue());
    assert!(
        report.diagnostics().is_empty(),
        "{:?}",
        report.diagnostics()
    );
    let function = &report.checked_bundle().unwrap().client_functions()[0];
    assert!(matches!(
        function.body(),
        CheckedClientFunctionBody::Expression {
            expression: CheckedClientExpression::Integer {
                value: 2_147_483_647,
                ..
            }
        }
    ));
}

#[test]
fn rejects_out_of_range_control_flow_literals() {
    let source = "CREATE SCHEMA examples; CREATE CLIENT FUNCTION examples.value() RETURNS INTEGER IS \
            BEGIN IF TRUE THEN RETURN 2147483648; ELSE RETURN 0; END IF; END;";
    let report = check(&bundle([("client.orna", source)]), &empty_catalogue());

    assert_eq!(report.diagnostics().len(), 1, "{:?}", report.diagnostics());
    assert_eq!(report.diagnostics()[0].code(), DiagnosticCode::TypeMismatch);
    assert_eq!(
        report.diagnostics()[0].message(),
        "CLIENT integer literal is outside the INTEGER range"
    );
    assert_no_checked_bundle(&report);
}

#[test]
fn accepts_let_declarations_inside_while_bodies() {
    let source = "CREATE SCHEMA examples; CREATE CLIENT FUNCTION examples.value() RETURNS INTEGER IS \
            BEGIN \
                LET index INTEGER := 0; \
                WHILE index < 2 LOOP \
                    LET item INTEGER := index; \
                    index := item + 1; \
                END LOOP; \
                RETURN index; \
            END;";
    let report = check(&bundle([("client.orna", source)]), &empty_catalogue());

    assert!(
        report.diagnostics().is_empty(),
        "{:?}",
        report.diagnostics()
    );
    assert!(report.checked_bundle().is_some());
}

fn validate_capability_text(
    capability: &str,
    declared_parameters: &[&str],
) -> Vec<crate::CompilerDiagnostic> {
    let source = format!(
        "CREATE CLIENT FUNCTION examples.f() RETURNS BOOLEAN REQUIRES CAPABILITY {capability} RETURN TRUE;"
    );
    let parsed = parse(&source);
    assert!(parsed.diagnostics().is_empty(), "source: {source}");
    let declaration = &parsed.client_functions()[0];
    let mut diagnostics = Vec::new();
    validate_client_capability(
        &declaration.capabilities[0],
        declared_parameters.iter().copied(),
        "capability.orna",
        &declaration.span,
        &mut diagnostics,
    );
    diagnostics
}

#[test]
fn validates_closed_client_capability_vocabulary_and_argument_shapes() {
    for capability in [
        "std.fs.read('/tmp/input')",
        "std.fs.write('/tmp/output')",
        "std.net.connect('db.internal')",
        "std.secret.use('database-password')",
    ] {
        assert!(
            validate_capability_text(capability, &[]).is_empty(),
            "capability: {capability}"
        );
    }
    assert!(validate_capability_text("std.fs.read(p_file)", &["p_file"]).is_empty());
}

#[test]
fn rejects_invalid_client_capability_names_counts_arguments_and_references() {
    for capability in [
        "std.net.call('db.internal')",
        "std.fs.read()",
        "std.fs.read('/tmp/a', '/tmp/b')",
        "std.fs.read(42)",
    ] {
        let diagnostics = validate_capability_text(capability, &[]);
        assert_eq!(diagnostics.len(), 1, "capability: {capability}");
        assert_eq!(
            diagnostics[0].code(),
            DiagnosticCode::CapabilityRequirement,
            "capability: {capability}"
        );
    }

    let diagnostics = validate_capability_text("std.fs.read(p_file)", &[]);
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].code(), DiagnosticCode::CapabilityRequirement);
    assert!(diagnostics[0].message().contains("undeclared parameter"));
}

#[test]
fn rejects_capabilities_on_accepted_client_boolean_bodies() {
    let source = "CREATE SCHEMA examples; CREATE CLIENT FUNCTION examples.read() \
            RETURNS BOOLEAN REQUIRES CAPABILITY std.fs.read('/tmp/input') RETURN TRUE;";
    let report = check(&bundle([("client.orna", source)]), &empty_catalogue());

    assert_eq!(report.diagnostics().len(), 1);
    assert_eq!(
        report.diagnostics()[0].code(),
        DiagnosticCode::CapabilityRequirement
    );
    assert_eq!(
        report.diagnostics()[0].message(),
        "accepted CLIENT function bodies must not declare capabilities"
    );
    assert_no_checked_bundle(&report);
}

#[test]
fn rejects_client_parameter_defaults_before_expression_lowering() {
    let source = "CREATE SCHEMA examples; CREATE CLIENT FUNCTION examples.identity(p TEXT DEFAULT 'fallback') RETURNS TEXT AS p;";
    let report = check(&bundle([("client.orna", source)]), &empty_catalogue());

    assert_eq!(report.diagnostics().len(), 1);
    assert_eq!(report.diagnostics()[0].code(), DiagnosticCode::TypeMismatch);
    assert_eq!(
        report.diagnostics()[0].message(),
        "CLIENT function parameters do not yet support default values"
    );
    assert_eq!(
        report.diagnostics()[0].location().span().start(),
        source.find("'fallback'").unwrap()
    );
    assert_no_checked_bundle(&report);
}

#[test]
fn accepts_external_client_parameters_and_capabilities() {
    let source = "CREATE SCHEMA examples; \
            CREATE EXTERNAL CLIENT FUNCTION examples.connect(p_host TEXT) \
            RETURNS TEXT \
            RUNTIME CONTRACT 'std.net.connect@1' \
            REQUIRES CAPABILITY std.net.connect(p_host);";
    let report = check(&bundle([("client.orna", source)]), &empty_catalogue());

    assert!(
        report.diagnostics().is_empty(),
        "{:?}",
        report.diagnostics()
    );
    let function = &report.checked_bundle().unwrap().client_functions()[0];
    assert_eq!(function.parameters().len(), 1);
    assert_eq!(function.capabilities().len(), 1);
    assert_eq!(function.capabilities()[0].name(), "std.net.connect");
    assert_eq!(
        function.capabilities()[0].argument(),
        &super::super::CheckedClientCapabilityArgument::Parameter("p_host".to_owned())
    );
    assert!(matches!(
        function.body(),
        CheckedClientFunctionBody::ExternalContract { identity, .. }
            if identity == "std.net.connect@1"
    ));
}

#[test]
fn checks_client_state_slots_and_rejects_state_shape_type_errors() {
    let valid = "CREATE SCHEMA examples; \
            CREATE CLIENT FUNCTION examples.state() RETURNS TEXT IS \
            STATE filter TEXT SCOPE LOCAL DEFAULT ''; \
            STATE selected TEXT SCOPE SESSION DEFAULT NULL; \
            STATE count INTEGER; \
            STATE total BIGINT; \
            STATE ratio FLOAT; \
            STATE payload BYTES; \
            BEGIN RETURN 'ready'; END;";
    let report = check(&bundle([("client.orna", valid)]), &empty_catalogue());
    assert!(
        report.diagnostics().is_empty(),
        "{:?}",
        report.diagnostics()
    );
    let function = &report.checked_bundle().unwrap().client_functions()[0];
    let CheckedClientFunctionBody::StateBlock { states, .. } = function.body() else {
        panic!("expected checked CLIENT state block");
    };
    assert_eq!(states.len(), 6);
    assert_eq!(states[3].name(), "total");
    assert_eq!(states[4].name(), "ratio");
    assert_eq!(states[5].name(), "payload");
    assert!(matches!(
        states[0].default(),
        CheckedStateDefault::Expression(_)
    ));
    assert!(matches!(states[1].default(), CheckedStateDefault::Null));
    assert!(matches!(states[2].default(), CheckedStateDefault::Unset));
    assert!(matches!(states[3].default(), CheckedStateDefault::Unset));
    assert!(matches!(states[4].default(), CheckedStateDefault::Unset));
    assert!(matches!(states[5].default(), CheckedStateDefault::Unset));

    let sealed_session = "CREATE SCHEMA examples; CREATE CLIENT FUNCTION examples.state() RETURNS BOOLEAN IS STATE snapshot sys.inspect.snapshot SCOPE SESSION; BEGIN RETURN TRUE; END;";
    let report = check(
        &bundle([("client.orna", sealed_session)]),
        &empty_catalogue(),
    );
    assert_eq!(report.diagnostics().len(), 1);
    assert_eq!(
        report.diagnostics()[0].code(),
        DiagnosticCode::DomainIncompatible
    );
    assert!(
        report.diagnostics()[0]
            .message()
            .contains("sealed sys.inspect carriers are transient")
    );

    let sealed_user = "CREATE SCHEMA examples; CREATE CLIENT FUNCTION examples.state() RETURNS BOOLEAN IS STATE snapshot_options sys.inspect.snapshot_options SCOPE USER; BEGIN RETURN TRUE; END;";

    let report = check(&bundle([("client.orna", sealed_user)]), &empty_catalogue());
    assert_eq!(report.diagnostics().len(), 1);
    assert_eq!(
        report.diagnostics()[0].code(),
        DiagnosticCode::DomainIncompatible
    );
    assert!(
        report.diagnostics()[0]
            .message()
            .contains("sealed sys.inspect carriers are transient")
    );
    let sealed_local = "CREATE SCHEMA examples; CREATE CLIENT FUNCTION examples.state() RETURNS BOOLEAN IS STATE snapshot sys.inspect.snapshot SCOPE LOCAL; BEGIN RETURN TRUE; END;";
    let report = check(&bundle([("client.orna", sealed_local)]), &empty_catalogue());
    assert_eq!(report.diagnostics().len(), 1);
    assert_eq!(
        report.diagnostics()[0].code(),
        DiagnosticCode::DomainIncompatible
    );
    assert!(
        report.diagnostics()[0]
            .message()
            .contains("sealed sys.inspect carriers are transient")
    );

    let duplicate = "CREATE SCHEMA examples; CREATE CLIENT FUNCTION examples.state() RETURNS TEXT IS \
            STATE value TEXT; STATE value INTEGER; BEGIN RETURN 'ready'; END;";
    let report = check(&bundle([("client.orna", duplicate)]), &empty_catalogue());
    assert_eq!(report.diagnostics().len(), 1);
    assert_eq!(
        report.diagnostics()[0].code(),
        DiagnosticCode::DuplicateDefinition
    );
    assert!(
        report.diagnostics()[0]
            .message()
            .contains("duplicate state definition")
    );

    let bad_default = "CREATE SCHEMA examples; CREATE CLIENT FUNCTION examples.state() RETURNS TEXT IS \
            STATE value TEXT DEFAULT 1; BEGIN RETURN 'ready'; END;";

    let report = check(&bundle([("client.orna", bad_default)]), &empty_catalogue());
    assert_eq!(report.diagnostics().len(), 1);
    assert_eq!(report.diagnostics()[0].code(), DiagnosticCode::TypeMismatch);
    assert_eq!(
        report.diagnostics()[0].message(),
        "this CLIENT state default must have the declared state type"
    );

    let bad_return = "CREATE SCHEMA examples; CREATE CLIENT FUNCTION examples.state() RETURNS TEXT IS \
            STATE value TEXT; BEGIN RETURN 1; END;";
    let report = check(&bundle([("client.orna", bad_return)]), &empty_catalogue());
    assert_eq!(report.diagnostics().len(), 1);
    assert_eq!(report.diagnostics()[0].code(), DiagnosticCode::TypeMismatch);
    assert_eq!(
        report.diagnostics()[0].message(),
        "this CLIENT function must return the declared value type"
    );
}

#[test]
fn rejects_opaque_values_in_client_state() {
    let standard =
        check_standard_library_source(&verified_standard_library_with_action_for_test()).unwrap();
    let base = empty_catalogue();
    let context = StandardApplicationCheckContext::try_new(&base, &standard).unwrap();
    let source = "CREATE SCHEMA examples; \
            CREATE CLIENT FUNCTION examples.state() RETURNS INTEGER IS \
            STATE action std.Action; \
            BEGIN RETURN 1; END;";
    let report = check_standard_application(&bundle([("state.orna", source)]), &context);

    assert_eq!(report.diagnostics().len(), 1, "{:?}", report.diagnostics());
    assert_eq!(
        report.diagnostics()[0].code(),
        DiagnosticCode::DomainIncompatible,
        "{:?}",
        report.diagnostics()
    );
    assert_eq!(
        report.diagnostics()[0].message(),
        "opaque CLIENT values are transient and cannot be stored in state"
    );
    assert!(report.preparation_view().is_none());
}
#[test]
fn rejects_inspector_expressions_in_state_defaults_and_returns() {
    let cases = [
        (
            "CREATE SCHEMA devtools; CREATE CLIENT FUNCTION devtools.state(p_target REF sys.inspect.invocation) RETURNS BOOLEAN IS \
                    STATE snapshot TEXT SCOPE LOCAL DEFAULT sys.inspect.snapshot(p_target => p_target); \
                    BEGIN RETURN TRUE; END;",
            "CLIENT state defaults do not support Inspector expressions",
        ),
        (
            "CREATE SCHEMA devtools; CREATE CLIENT FUNCTION devtools.state(p_target REF sys.inspect.invocation) RETURNS sys.inspect.snapshot IS \
                    STATE value TEXT; BEGIN RETURN sys.inspect.snapshot(p_target => p_target); END;",
            "CLIENT state blocks do not support Inspector expressions",
        ),
    ];

    for (source, message) in cases {
        let report = check(
            &bundle([("inspector-state.orna", source)]),
            &empty_catalogue(),
        );
        assert_eq!(report.diagnostics().len(), 1, "{:?}", report.diagnostics());
        assert_eq!(
            report.diagnostics()[0].code(),
            DiagnosticCode::DomainIncompatible
        );
        assert_eq!(report.diagnostics()[0].message(), message);
        assert_no_checked_bundle(&report);
    }
}

#[test]
fn accepts_parameters_on_client_state_blocks() {
    let source = "CREATE SCHEMA examples; \
            CREATE CLIENT FUNCTION examples.state(p TEXT) RETURNS TEXT IS \
            STATE value TEXT; BEGIN RETURN p; END;";
    let report = check(&bundle([("client.orna", source)]), &empty_catalogue());
    assert!(
        report.diagnostics().is_empty(),
        "{:?}",
        report.diagnostics()
    );
    let function = &report.checked_bundle().unwrap().client_functions()[0];
    assert_eq!(function.parameters().len(), 1);
    assert!(matches!(
        function.body(),
        CheckedClientFunctionBody::StateBlock { states, .. } if states.len() == 1
    ));
}

#[test]
fn keeps_empty_no_state_client_blocks_as_expression_bodies() {
    let source = "CREATE SCHEMA examples;             CREATE CLIENT FUNCTION examples.identity(p TEXT) RETURNS TEXT IS             BEGIN RETURN p; END;";
    let report = check(&bundle([("client.orna", source)]), &empty_catalogue());
    assert!(
        report.diagnostics().is_empty(),
        "{:?}",
        report.diagnostics()
    );
    let function = &report.checked_bundle().unwrap().client_functions()[0];
    let CheckedClientFunctionBody::Expression { expression } = function.body() else {
        panic!("expected an expression CLIENT body");
    };
    assert!(matches!(
        expression,
        super::super::CheckedClientExpression::ParameterRead { .. }
    ));
}

#[test]
fn accepts_procedural_client_statements_without_state_declarations() {
    let source = "CREATE SCHEMA examples; \
            CREATE CLIENT FUNCTION examples.procedural() RETURNS TEXT IS \
            BEGIN LET value := 'first'; value := 'second'; RETURN value; END;";
    let report = check(&bundle([("client.orna", source)]), &empty_catalogue());
    assert!(
        report.diagnostics().is_empty(),
        "{:?}",
        report.diagnostics()
    );
    let function = &report.checked_bundle().unwrap().client_functions()[0];
    let CheckedClientFunctionBody::Procedural {
        locals,
        statements,
        return_expression,
    } = function.body()
    else {
        panic!("expected checked procedural CLIENT body");
    };
    assert_eq!(locals.len(), 1);
    assert_eq!(locals[0].ordinal(), 0);
    assert_eq!(statements.len(), 2);
    assert!(matches!(
        statements[0],
        super::super::CheckedClientStatement::Let { local: 0, .. }
    ));
    assert!(matches!(
        statements[1],
        super::super::CheckedClientStatement::Assignment { local: 0, .. }
    ));
    assert!(matches!(
        return_expression,
        super::super::CheckedClientExpression::LocalRead { local: 0, .. }
    ));
}

#[test]
fn accepts_procedural_scalar_resource_local_await() {
    let base = CatalogueSnapshot::new_with_functions(
        CatalogueRevisionId::from_bytes([0x41; 16]),
        vec![SchemaDefinition::new(
            SchemaId::from_bytes([0x42; 16]),
            QualifiedSemanticName::new(["tasks"]).unwrap(),
        )],
        Vec::new(),
        vec![FunctionDefinition::new(
            FunctionId::from_bytes([0x43; 16]),
            QualifiedSemanticName::new(["tasks", "find"]).unwrap(),
            FunctionDomain::Server,
            vec![parameter(
                0x44,
                "p_name",
                0,
                ResolvedType::Scalar(StandardScalar::CharacterLargeObject),
            )],
            FunctionReturn::Single(ResolvedType::Scalar(StandardScalar::CharacterLargeObject)),
            FunctionRevisionId::from_bytes([0x45; 16]),
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::ReadOnly),
            FunctionVolatility::Stable,
        )],
    )
    .unwrap();
    let source = "CREATE SCHEMA ui; CREATE CLIENT FUNCTION ui.find(p_name TEXT) RETURNS TEXT IS \
            LET rows std.data.Resource<TEXT> := std.data.resource(target => tasks.find, arguments => std.call.args(p_name => p_name)); \
            BEGIN RETURN AWAIT rows; END;";
    let report = check(&bundle([("resource.orna", source)]), &base);
    assert!(
        report.diagnostics().is_empty(),
        "{:#?}",
        report.diagnostics()
    );
    let function = &report
        .checked_bundle()
        .expect("resource source checks")
        .client_functions()[0];
    let CheckedClientFunctionBody::Procedural {
        locals,
        statements,
        return_expression,
    } = function.body()
    else {
        panic!("expected a checked procedural CLIENT body");
    };
    assert_eq!(locals.len(), 1);
    assert_eq!(locals[0].ordinal(), 0);
    assert_eq!(
        locals[0].kind(),
        super::super::CheckedClientLocalKind::Resource(
            orna_artifact::client_plan::ResourceKind::Scalar
        )
    );
    assert_eq!(statements.len(), 1);
    let super::super::CheckedClientStatement::Let {
        local: 0,
        expression: resource_expression,
    } = &statements[0]
    else {
        panic!("resource local must be initialized by a LET");
    };
    let super::super::CheckedClientExpression::Resource { operation } = resource_expression else {
        panic!("resource local initializer must be a resource constructor");
    };
    assert_eq!(
        operation.kind(),
        orna_artifact::client_plan::ResourceKind::Scalar
    );
    assert_eq!(
        operation.target(),
        super::super::CheckedFunctionId::Existing(FunctionId::from_bytes([0x43; 16]))
    );
    let resource_text =
        "std.data.resource(target => tasks.find, arguments => std.call.args(p_name => p_name))";
    let resource_start = source
        .find(resource_text)
        .expect("resource constructor is present");
    assert_eq!(operation.location().logical_path(), "resource.orna");
    assert_eq!(operation.location().span().start(), resource_start);
    assert_eq!(
        operation.location().span().end(),
        resource_start + resource_text.len()
    );
    let argument_location = match &operation.arguments()[0].1 {
        super::super::CheckedClientExpression::ParameterRead { location, .. } => location,
        _ => panic!("resource argument must retain its parameter-read span"),
    };
    let argument_start = source
        .rfind("p_name")
        .expect("argument parameter read is present");
    assert_eq!(argument_location.logical_path(), "resource.orna");
    assert_eq!(argument_location.span().start(), argument_start);
    assert_eq!(
        argument_location.span().end(),
        argument_start + "p_name".len()
    );

    let super::super::CheckedClientExpression::Await {
        expression: awaited_expression,
        location: await_location,
    } = return_expression
    else {
        panic!("procedural return must await the resource local");
    };
    let super::super::CheckedClientExpression::LocalRead {
        local: 0,
        location: local_location,
    } = awaited_expression.as_ref()
    else {
        panic!("await operand must read the resource local");
    };
    let await_text = "AWAIT rows";
    let await_start = source
        .find(await_text)
        .expect("await expression is present");
    assert_eq!(await_location.logical_path(), "resource.orna");
    assert_eq!(await_location.span().start(), await_start);
    assert_eq!(await_location.span().end(), await_start + await_text.len());
    let local_start = source.rfind("rows").expect("await local read is present");
    assert_eq!(local_location.logical_path(), "resource.orna");
    assert_eq!(local_location.span().start(), local_start);
    assert_eq!(local_location.span().end(), local_start + "rows".len());
}

#[test]
fn accepts_scalar_resource_assignment_await_with_exact_spans_and_call_provenance() {
    let target_id = FunctionId::from_bytes([0x43; 16]);
    let base = catalogue(
        vec![schema(0x42, &["tasks"])],
        Vec::new(),
        vec![FunctionDefinition::new(
            target_id,
            QualifiedSemanticName::new(["tasks", "find"]).unwrap(),
            FunctionDomain::Server,
            vec![parameter(
                0x44,
                "p_name",
                0,
                ResolvedType::Scalar(StandardScalar::CharacterLargeObject),
            )],
            FunctionReturn::Single(ResolvedType::Scalar(StandardScalar::CharacterLargeObject)),
            FunctionRevisionId::from_bytes([0x45; 16]),
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::ReadOnly),
            FunctionVolatility::Stable,
        )],
    );
    let source = "CREATE SCHEMA ui; CREATE CLIENT FUNCTION ui.find(p_name TEXT) RETURNS TEXT IS \
            LET value TEXT := 'initial'; \
            BEGIN value := AWAIT std.data.resource(target => tasks.find, arguments => std.call.args(p_name => p_name)); RETURN value; END;";
    let report = check(&bundle([("resource-assignment.orna", source)]), &base);
    assert!(
        report.diagnostics().is_empty(),
        "{:#?}",
        report.diagnostics()
    );
    let function = &report
        .checked_bundle()
        .expect("resource assignment source checks")
        .client_functions()[0];
    let CheckedClientFunctionBody::Procedural { statements, .. } = function.body() else {
        panic!("expected a checked procedural CLIENT body");
    };
    assert_eq!(statements.len(), 2);
    let super::super::CheckedClientStatement::Assignment {
        local: 0,
        expression: assignment_expression,
    } = &statements[1]
    else {
        panic!("second procedural statement must assign the existing local");
    };
    let CheckedClientExpression::Await {
        expression: awaited_expression,
        location: await_location,
    } = assignment_expression
    else {
        panic!("assignment RHS must retain its AWAIT expression");
    };
    let await_text = "AWAIT std.data.resource(target => tasks.find, arguments => std.call.args(p_name => p_name))";
    let await_start = source
        .find(await_text)
        .expect("assignment AWAIT is present");
    assert_eq!(await_location.logical_path(), "resource-assignment.orna");
    assert_eq!(await_location.span().start(), await_start);
    assert_eq!(await_location.span().end(), await_start + await_text.len());

    let CheckedClientExpression::Resource { operation } = awaited_expression.as_ref() else {
        panic!("assignment AWAIT operand must retain its resource operation");
    };
    let resource_text =
        "std.data.resource(target => tasks.find, arguments => std.call.args(p_name => p_name))";
    let resource_start = source
        .find(resource_text)
        .expect("resource constructor is present");
    assert_eq!(
        operation.location().logical_path(),
        "resource-assignment.orna"
    );
    assert_eq!(operation.location().span().start(), resource_start);
    assert_eq!(
        operation.location().span().end(),
        resource_start + resource_text.len()
    );
    let argument_location = match &operation.arguments()[0].1 {
        CheckedClientExpression::ParameterRead { location, .. } => location,
        _ => panic!("resource argument must retain its parameter-read span"),
    };
    let argument_start = source
        .rfind("p_name")
        .expect("argument parameter read is present");
    assert_eq!(argument_location.logical_path(), "resource-assignment.orna");
    assert_eq!(argument_location.span().start(), argument_start);
    assert_eq!(
        argument_location.span().end(),
        argument_start + "p_name".len()
    );

    let call_references = function
        .references()
        .iter()
        .filter(|reference| reference.kind() == DefinitionReferenceKind::FunctionCall)
        .collect::<Vec<_>>();
    assert_eq!(call_references.len(), 1);
    let call_reference = call_references[0];
    assert_eq!(
        call_reference.target(),
        CheckedDefinitionReferenceTarget::Function(super::super::CheckedFunctionId::Existing(
            target_id,
        ))
    );
    assert_eq!(
        call_reference.location().logical_path(),
        "resource-assignment.orna"
    );
    assert_eq!(call_reference.location().span().start(), resource_start);
    assert_eq!(
        call_reference.location().span().end(),
        resource_start + resource_text.len()
    );
}

#[test]
fn rejects_resource_local_as_action_argument() {
    let resource_target_id = FunctionId::from_bytes([0x71; 16]);
    let action_target_id = FunctionId::from_bytes([0x72; 16]);
    let action_parameter_id = ParameterId::from_bytes([0x73; 16]);
    let integer_type = ResolvedType::Scalar(StandardScalar::Integer);
    let base = CatalogueSnapshot::new_with_functions(
        CatalogueRevisionId::from_bytes([0x74; 16]),
        vec![SchemaDefinition::new(
            SchemaId::from_bytes([0x75; 16]),
            QualifiedSemanticName::new(["tasks"]).unwrap(),
        )],
        Vec::new(),
        vec![
            FunctionDefinition::new(
                resource_target_id,
                QualifiedSemanticName::new(["tasks", "find"]).unwrap(),
                FunctionDomain::Server,
                Vec::new(),
                FunctionReturn::Single(integer_type),
                FunctionRevisionId::from_bytes([0x76; 16]),
                FunctionSecurity::Invoker,
                None,
                FunctionVolatility::Stable,
            ),
            FunctionDefinition::new(
                action_target_id,
                QualifiedSemanticName::new(["tasks", "run"]).unwrap(),
                FunctionDomain::Client,
                vec![ParameterDefinition::new(
                    action_parameter_id,
                    "p_value",
                    0,
                    integer_type,
                    None,
                )],
                FunctionReturn::Single(ResolvedType::Scalar(StandardScalar::Integer)),
                FunctionRevisionId::from_bytes([0x77; 16]),
                FunctionSecurity::Invoker,
                None,
                FunctionVolatility::Immutable,
            ),
        ],
    )
    .unwrap();
    let standard =
        check_standard_library_source(&verified_standard_library_with_action_for_test()).unwrap();
    let context = StandardApplicationCheckContext::try_new(&base, &standard).unwrap();
    let source = "CREATE SCHEMA ui; CREATE CLIENT FUNCTION ui.run() RETURNS std.Action IS \
            LET rows std.data.Resource<INTEGER> := std.data.resource(target => tasks.find, arguments => std.call.args()); \
            BEGIN RETURN std.action.call(target => tasks.run, arguments => std.call.args(p_value => rows)); END;";
    let report = check_standard_application(&bundle([("action-resource.orna", source)]), &context);
    assert_eq!(report.diagnostics().len(), 1, "{:?}", report.diagnostics());
    assert_eq!(report.diagnostics()[0].code(), DiagnosticCode::TypeMismatch);
    assert_eq!(
        report.diagnostics()[0].message(),
        "std.action.call argument for parameter p_value is not ORV3-encodable"
    );
    assert!(report.checked_bundle().is_none());
}

#[test]
fn rejects_bare_as_and_state_return_await_but_accepts_procedural_await() {
    let base = CatalogueSnapshot::new_with_functions(
        CatalogueRevisionId::from_bytes([0x51; 16]),
        vec![SchemaDefinition::new(
            SchemaId::from_bytes([0x52; 16]),
            QualifiedSemanticName::new(["tasks"]).unwrap(),
        )],
        Vec::new(),
        vec![FunctionDefinition::new(
            FunctionId::from_bytes([0x53; 16]),
            QualifiedSemanticName::new(["tasks", "find"]).unwrap(),
            FunctionDomain::Server,
            Vec::new(),
            FunctionReturn::Single(ResolvedType::Scalar(StandardScalar::CharacterLargeObject)),
            FunctionRevisionId::from_bytes([0x54; 16]),
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::ReadOnly),
            FunctionVolatility::Stable,
        )],
    )
    .unwrap();
    let source = "CREATE SCHEMA ui; CREATE CLIENT FUNCTION ui.bare() RETURNS TEXT AS \
            AWAIT std.data.resource(target => tasks.find, arguments => std.call.args()); \
            CREATE CLIENT FUNCTION ui.procedural() RETURNS TEXT IS \
            LET value std.data.Resource<TEXT> := std.data.resource(target => tasks.find, arguments => std.call.args()); \
            BEGIN RETURN AWAIT value; END; \
            CREATE CLIENT FUNCTION ui.stateful() RETURNS TEXT IS \
            STATE value TEXT; BEGIN RETURN AWAIT std.data.resource(target => tasks.find, arguments => std.call.args()); END;";
    let report = check(&bundle([("await-positions.orna", source)]), &base);
    assert_eq!(report.diagnostics().len(), 2, "{:?}", report.diagnostics());
    let await_starts = [
        source.find("AWAIT").unwrap(),
        source.rfind("AWAIT").unwrap(),
    ];
    for (diagnostic, start) in report.diagnostics().iter().zip(await_starts) {
        assert_eq!(diagnostic.code(), DiagnosticCode::UnexpectedToken);
        assert_eq!(diagnostic.message(), "expected a CLIENT expression");
        assert_eq!(diagnostic.location().logical_path(), "await-positions.orna");
        assert_eq!(diagnostic.location().span().start(), start);
        assert_eq!(diagnostic.location().span().end(), start + "AWAIT".len());
    }
    assert_no_checked_bundle(&report);

    let procedural = "CREATE SCHEMA ui; CREATE CLIENT FUNCTION ui.procedural() RETURNS TEXT IS \
            LET value TEXT := AWAIT std.data.resource(target => tasks.find, arguments => std.call.args()); \
            BEGIN value := AWAIT std.data.resource(target => tasks.find, arguments => std.call.args()); RETURN value; END;";
    let report = check(&bundle([("await-procedural.orna", procedural)]), &base);
    assert!(
        report.diagnostics().is_empty(),
        "{:?}",
        report.diagnostics()
    );
    assert!(report.checked_bundle().is_some());
}

#[test]
fn rejects_scalar_and_stream_resource_descriptor_mismatches() {
    let base = CatalogueSnapshot::new_with_functions(
        CatalogueRevisionId::from_bytes([0x61; 16]),
        vec![SchemaDefinition::new(
            SchemaId::from_bytes([0x62; 16]),
            QualifiedSemanticName::new(["tasks"]).unwrap(),
        )],
        Vec::new(),
        vec![
            FunctionDefinition::new(
                FunctionId::from_bytes([0x63; 16]),
                QualifiedSemanticName::new(["tasks", "find"]).unwrap(),
                FunctionDomain::Server,
                Vec::new(),
                FunctionReturn::Single(ResolvedType::Scalar(StandardScalar::CharacterLargeObject)),
                FunctionRevisionId::from_bytes([0x64; 16]),
                FunctionSecurity::Invoker,
                Some(FunctionTransaction::ReadOnly),
                FunctionVolatility::Stable,
            ),
            FunctionDefinition::new(
                FunctionId::from_bytes([0x65; 16]),
                QualifiedSemanticName::new(["tasks", "events"]).unwrap(),
                FunctionDomain::Server,
                Vec::new(),
                FunctionReturn::Stream(ResolvedType::Scalar(StandardScalar::CharacterLargeObject)),
                FunctionRevisionId::from_bytes([0x66; 16]),
                FunctionSecurity::Invoker,
                Some(FunctionTransaction::ReadOnly),
                FunctionVolatility::Stable,
            ),
        ],
    )
    .unwrap();
    let source = "CREATE SCHEMA ui; CREATE CLIENT FUNCTION ui.scalar() RETURNS TEXT IS \
            LET value std.data.Resource<INTEGER> := std.data.resource(target => tasks.find, arguments => std.call.args()); \
            BEGIN RETURN AWAIT value; END; \
            CREATE CLIENT FUNCTION ui.stream() RETURNS STREAM<TEXT> IS \
            LET rows std.data.StreamResource<INTEGER> := std.data.stream_resource(target => tasks.events, arguments => std.call.args()); \
            BEGIN RETURN AWAIT rows; END;";
    let report = check(
        &bundle([("resource-descriptor-mismatch.orna", source)]),
        &base,
    );
    assert_eq!(report.diagnostics().len(), 2, "{:?}", report.diagnostics());
    assert!(report.diagnostics().iter().all(|diagnostic| {
        diagnostic.code() == DiagnosticCode::TypeMismatch
            && diagnostic.message().contains("descriptor does not match")
    }));
    assert_no_checked_bundle(&report);
}

#[test]
fn rejects_state_block_pre_begin_let_locals_in_parser_shape() {
    let source = "CREATE SCHEMA examples; CREATE CLIENT FUNCTION examples.mixed() RETURNS BOOLEAN IS \
            STATE value TEXT; LET other TEXT := 'x'; BEGIN RETURN TRUE; END;";
    let report = check(&bundle([("state-shape.orna", source)]), &empty_catalogue());
    assert!(
        report.diagnostics().iter().any(|diagnostic| {
            diagnostic.code() == DiagnosticCode::UnexpectedToken
                && diagnostic.message() == "CLIENT state blocks cannot contain pre-BEGIN LET locals"
        }),
        "{:?}",
        report.diagnostics()
    );
    assert_no_checked_bundle(&report);
}

#[test]
fn rejects_state_blocks_mixed_with_procedural_declarations() {
    let source = "CREATE SCHEMA examples; CREATE CLIENT FUNCTION examples.mixed() RETURNS BOOLEAN IS STATE value TEXT; BEGIN LET other := 'x'; RETURN TRUE; END;";
    let report = check(&bundle([("client.orna", source)]), &empty_catalogue());
    assert!(
        report.diagnostics().iter().any(|diagnostic| {
            diagnostic.code() == DiagnosticCode::UnexpectedToken
                && diagnostic.message()
                    == "CLIENT state blocks accept only a single RETURN statement"
        }),
        "{:?}",
        report.diagnostics()
    );
    assert_no_checked_bundle(&report);
}

#[test]
fn rejects_procedural_unknown_locals_types_and_await_operands() {
    let unknown = "CREATE SCHEMA examples; CREATE CLIENT FUNCTION examples.bad() RETURNS TEXT IS BEGIN missing := 'x'; RETURN 'ok'; END;";
    let report = check(&bundle([("client.orna", unknown)]), &empty_catalogue());
    assert_eq!(report.diagnostics().len(), 1);
    assert_eq!(
        report.diagnostics()[0].code(),
        DiagnosticCode::UnknownQualifiedName
    );

    let wrong_type = "CREATE SCHEMA examples; CREATE CLIENT FUNCTION examples.bad() RETURNS INTEGER IS BEGIN LET value INTEGER := 'wrong'; RETURN value; END;";
    let report = check(&bundle([("client.orna", wrong_type)]), &empty_catalogue());
    assert!(
        report
            .diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.code() == DiagnosticCode::TypeMismatch)
    );

    let bad_await = "CREATE SCHEMA examples; CREATE CLIENT FUNCTION examples.bad() RETURNS TEXT IS BEGIN LET value := AWAIT 1; RETURN value; END;";
    let report = check(&bundle([("client.orna", bad_await)]), &empty_catalogue());
    assert!(
        report
            .diagnostics()
            .iter()
            .any(
                |diagnostic| diagnostic.code() == DiagnosticCode::DomainIncompatible
                    && diagnostic.message().contains("AWAIT requires")
            )
    );
}

#[test]
fn rejects_expression_returns_the_local_evaluator_cannot_execute() {
    let source = "CREATE SCHEMA examples; \
            CREATE TYPE examples.item AS OBJECT (); \
            CREATE CLIENT FUNCTION examples.read(p_item REF examples.item) \
            RETURNS REF examples.item AS p_item;";
    let report = check(&bundle([("client.orna", source)]), &empty_catalogue());

    assert_eq!(report.diagnostics().len(), 1);
    assert_eq!(report.diagnostics()[0].code(), DiagnosticCode::TypeMismatch);
    assert_eq!(
        report.diagnostics()[0].message(),
        "this CLIENT function return type is not supported by the local evaluator"
    );
    assert_no_checked_bundle(&report);
}

#[test]
fn checks_client_false_and_reuses_active_id_with_quoted_formatting() {
    let changed_source = "CREATE SCHEMA examples;\nCREATE CLIENT FUNCTION \"examples\".\"enabled\"() RETURNS BOOL RETURN false;";
    let existing_function = FunctionDefinition::new(
        FunctionId::from_bytes([8; 16]),
        QualifiedSemanticName::new(["examples", "enabled"]).unwrap(),
        FunctionDomain::Client,
        Vec::new(),
        FunctionReturn::Single(ResolvedType::scalar(StandardScalar::Boolean)),
        FunctionRevisionId::from_bytes([9; 16]),
        FunctionSecurity::Invoker,
        None,
        FunctionVolatility::Immutable,
    );
    let existing_report = check(
        &bundle([("client.orna", changed_source)]),
        &catalogue(
            vec![schema(1, &["examples"])],
            Vec::new(),
            vec![existing_function],
        ),
    );
    assert!(existing_report.diagnostics().is_empty());
    let changed = &existing_report.checked_bundle().unwrap().client_functions()[0];
    assert_eq!(
        changed.id().existing(),
        Some(FunctionId::from_bytes([8; 16]))
    );
    assert_eq!(changed.name().to_string(), "examples.enabled");
    assert!(!changed.boolean_body().unwrap().0);
}

#[test]
fn rejects_client_shape_in_deterministic_order_and_whole_bundle() {
    let source = "CREATE SCHEMA examples; CREATE CLIENT FUNCTION examples.bad(a TEXT) RETURNS TEXT RETURN TRUE; CREATE CLIENT FUNCTION examples.good() RETURNS BOOLEAN RETURN FALSE;";
    let report = check(&bundle([("client.orna", source)]), &empty_catalogue());

    assert_eq!(report.diagnostics().len(), 2);
    assert_eq!(
        report.diagnostics()[0].code(),
        DiagnosticCode::DomainIncompatible
    );
    assert_eq!(
        report.diagnostics()[0].message(),
        "this CLIENT function cannot declare parameters yet"
    );
    assert_eq!(
        report.diagnostics()[0].location().span().start(),
        source.find("(a TEXT)").unwrap()
    );
    assert_eq!(
        report.diagnostics()[0].location().span().end(),
        source.find("(a TEXT)").unwrap() + "(a TEXT)".len()
    );
    assert_eq!(report.diagnostics()[1].code(), DiagnosticCode::TypeMismatch);
    assert_eq!(
        report.diagnostics()[1].message(),
        "this CLIENT function must return BOOLEAN"
    );
    assert_eq!(
        report.diagnostics()[1].location().span().start(),
        source.find("RETURNS TEXT").unwrap() + "RETURNS ".len()
    );
    assert_eq!(
        report.diagnostics()[1].location().span().end(),
        source.find("RETURNS TEXT").unwrap() + "RETURNS TEXT".len()
    );
    assert_no_checked_bundle(&report);
}

#[test]
fn rejects_client_server_duplicates_and_active_domain_changes_at_name() {
    let duplicate_source = "CREATE SCHEMA examples; CREATE TYPE examples.flag AS OBJECT (value BOOLEAN); CREATE SERVER FUNCTION examples.enabled() RETURNS ROWS (value BOOLEAN) AS SELECT f.value FROM examples.flag f; CREATE CLIENT FUNCTION examples.ENABLED() RETURNS BOOLEAN RETURN TRUE;";
    let duplicate = check(
        &bundle([("client.orna", duplicate_source)]),
        &empty_catalogue(),
    );
    assert_eq!(duplicate.diagnostics().len(), 1);
    assert_eq!(
        duplicate.diagnostics()[0].code(),
        DiagnosticCode::DuplicateDefinition
    );
    assert_eq!(
        duplicate.diagnostics()[0].message(),
        "duplicate function definition examples.enabled"
    );
    let duplicate_name = duplicate_source.rfind("examples.ENABLED").unwrap();
    assert_eq!(
        duplicate.diagnostics()[0].location().span().start(),
        duplicate_name
    );
    assert_eq!(
        duplicate.diagnostics()[0].location().span().end(),
        duplicate_name + "examples.ENABLED".len()
    );
    assert_no_checked_bundle(&duplicate);

    let reverse_source = "CREATE SCHEMA examples; CREATE CLIENT FUNCTION examples.enabled() RETURNS BOOLEAN RETURN TRUE; CREATE SERVER FUNCTION examples.ENABLED() RETURNS TEXT TRANSACTION MANUAL AS SELECT TRUE FROM examples.flag f;";
    let reverse_duplicate = check(
        &bundle([("client.orna", reverse_source)]),
        &empty_catalogue(),
    );
    assert_eq!(reverse_duplicate.diagnostics().len(), 1);
    assert_eq!(
        reverse_duplicate.diagnostics()[0].code(),
        DiagnosticCode::DuplicateDefinition
    );
    assert_eq!(
        reverse_duplicate.diagnostics()[0].message(),
        "duplicate function definition examples.enabled"
    );
    let reverse_name = reverse_source.rfind("examples.ENABLED").unwrap();
    assert_eq!(
        reverse_duplicate.diagnostics()[0].location().span().start(),
        reverse_name
    );
    assert_eq!(
        reverse_duplicate.diagnostics()[0].location().span().end(),
        reverse_name + "examples.ENABLED".len()
    );
    assert_no_checked_bundle(&reverse_duplicate);

    let base = catalogue(
        vec![schema(1, &["examples"])],
        Vec::new(),
        vec![server_function(
            8,
            &["examples", "enabled"],
            Vec::new(),
            vec![rows_column(
                "value",
                0,
                ResolvedType::scalar(StandardScalar::Boolean),
            )],
            FunctionSecurity::Invoker,
            None,
            FunctionVolatility::Stable,
        )],
    );
    let changed = "CREATE SCHEMA examples; CREATE CLIENT FUNCTION examples.enabled() RETURNS BOOLEAN RETURN TRUE;";
    let report = check(&bundle([("client.orna", changed)]), &base);
    assert_eq!(report.diagnostics().len(), 1);
    assert_eq!(
        report.diagnostics()[0].code(),
        DiagnosticCode::DomainIncompatible
    );
    assert_eq!(
        report.diagnostics()[0].message(),
        "this function is already declared as a SERVER function"
    );
    assert_eq!(
        report.diagnostics()[0].location().span().start(),
        changed.find("examples.enabled").unwrap()
    );
    assert_eq!(
        report.diagnostics()[0].location().span().end(),
        changed.find("examples.enabled").unwrap() + "examples.enabled".len()
    );
    assert_no_checked_bundle(&report);
}

#[test]
fn assigns_function_ids_in_shared_source_order_across_domains() {
    let source = "CREATE SCHEMA examples; CREATE TYPE examples.flag AS OBJECT (value BOOLEAN); CREATE CLIENT FUNCTION examples.enabled() RETURNS BOOLEAN RETURN TRUE; CREATE SERVER FUNCTION examples.read() RETURNS ROWS (value BOOLEAN) AS SELECT f.value FROM examples.flag f;";
    let report = check(&bundle([("client.orna", source)]), &empty_catalogue());

    assert!(report.diagnostics().is_empty());
    let checked = report.checked_bundle().unwrap();
    assert_eq!(
        checked.client_functions()[0].id().to_string(),
        "provisional:function:0"
    );
    assert_eq!(
        checked.server_functions()[0].id().to_string(),
        "provisional:function:1"
    );
}

#[test]
fn rejects_client_duplicates_with_normalised_names() {
    let source = "CREATE SCHEMA examples; CREATE CLIENT FUNCTION examples.enabled() RETURNS BOOLEAN RETURN TRUE; CREATE CLIENT FUNCTION examples.ENABLED(p_value TEXT) RETURNS TEXT RETURN FALSE;";
    let report = check(&bundle([("client.orna", source)]), &empty_catalogue());

    assert_eq!(report.diagnostics().len(), 1);
    assert_eq!(
        report.diagnostics()[0].code(),
        DiagnosticCode::DuplicateDefinition
    );
    assert_eq!(
        report.diagnostics()[0].message(),
        "duplicate client function definition examples.enabled"
    );
    let duplicate_name = source.rfind("examples.ENABLED").unwrap();
    assert_eq!(
        report.diagnostics()[0].location().span().start(),
        duplicate_name
    );
    assert_eq!(
        report.diagnostics()[0].location().span().end(),
        duplicate_name + "examples.ENABLED".len()
    );
    assert_no_checked_bundle(&report);
}

#[test]
fn rejects_active_client_to_server_domain_change_at_the_function_name() {
    let base = catalogue(
        vec![schema(1, &["examples"])],
        Vec::new(),
        vec![FunctionDefinition::new(
            FunctionId::from_bytes([8; 16]),
            QualifiedSemanticName::new(["examples", "enabled"]).unwrap(),
            FunctionDomain::Client,
            Vec::new(),
            FunctionReturn::Single(ResolvedType::scalar(StandardScalar::Boolean)),
            FunctionRevisionId::from_bytes([9; 16]),
            FunctionSecurity::Invoker,
            None,
            FunctionVolatility::Immutable,
        )],
    );
    let source = "CREATE SCHEMA examples; CREATE TYPE examples.flag AS OBJECT (value BOOLEAN); CREATE SERVER FUNCTION examples.enabled() RETURNS ROWS (value BOOLEAN) AS SELECT f.value FROM examples.flag f;";
    let report = check(&bundle([("client.orna", source)]), &base);

    assert_eq!(report.diagnostics().len(), 1);
    assert_eq!(
        report.diagnostics()[0].code(),
        DiagnosticCode::DomainIncompatible
    );
    assert_eq!(
        report.diagnostics()[0].message(),
        "this function is already declared as a CLIENT function"
    );
    assert_eq!(
        report.diagnostics()[0].location().span().start(),
        source.find("examples.enabled").unwrap()
    );
    assert_eq!(
        report.diagnostics()[0].location().span().end(),
        source.find("examples.enabled").unwrap() + "examples.enabled".len()
    );
    assert_no_checked_bundle(&report);
}

#[test]
fn reports_non_boolean_client_returns_at_the_written_return_shape() {
    let source = "CREATE SCHEMA examples; CREATE CLIENT FUNCTION examples.ui() RETURNS UI RETURN TRUE; CREATE CLIENT FUNCTION examples.rows() RETURNS ROWS () RETURN FALSE;";
    let report = check(&bundle([("client.orna", source)]), &empty_catalogue());

    assert_eq!(report.diagnostics().len(), 2);
    for diagnostic in report.diagnostics() {
        assert_eq!(diagnostic.code(), DiagnosticCode::TypeMismatch);
        assert_eq!(
            diagnostic.message(),
            "this CLIENT function must return BOOLEAN"
        );
    }
    assert_eq!(
        report.diagnostics()[0].location().span().start(),
        source.find("UI").unwrap()
    );
    assert_eq!(
        report.diagnostics()[0].location().span().end(),
        source.find("UI").unwrap() + "UI".len()
    );
    assert_eq!(
        report.diagnostics()[1].location().span().start(),
        source.find("ROWS ()").unwrap()
    );
    assert_eq!(
        report.diagnostics()[1].location().span().end(),
        source.find("ROWS ()").unwrap() + "ROWS ()".len()
    );
    assert_no_checked_bundle(&report);
}

#[test]
fn client_boolean_return_spellings_are_closed_to_boolean_and_bool() {
    let cases = [
        (
            "CREATE SCHEMA examples; CREATE CLIENT FUNCTION examples.value() RETURNS \"BOOLEAN\" RETURN TRUE;",
            "\"BOOLEAN\"",
        ),
        (
            "CREATE SCHEMA examples; CREATE CLIENT FUNCTION examples.value() RETURNS boolean_alias RETURN TRUE;",
            "boolean_alias",
        ),
        (
            "CREATE SCHEMA examples; CREATE CLIENT FUNCTION examples.value() RETURNS std.BOOLEAN RETURN TRUE;",
            "std.BOOLEAN",
        ),
    ];
    for (source, spelling) in cases {
        let report = check(&bundle([("client.orna", source)]), &empty_catalogue());

        assert_eq!(report.diagnostics().len(), 1, "spelling: {spelling}");
        assert_eq!(report.diagnostics()[0].code(), DiagnosticCode::TypeMismatch);
        assert_eq!(
            report.diagnostics()[0].message(),
            "this CLIENT function must return BOOLEAN"
        );
        let start = source.find(spelling).unwrap();
        assert_eq!(
            report.diagnostics()[0].location().logical_path(),
            "client.orna"
        );
        assert_eq!(report.diagnostics()[0].location().span().start(), start);
        assert_eq!(
            report.diagnostics()[0].location().span().end(),
            start + spelling.len()
        );
        assert_no_checked_bundle(&report);
    }
}

#[test]
fn rejects_protected_type_source_in_global_category_order() {
    let z_source = "EXPORT TYPE app.source TO PRELUDE AS SECOND;\n\
            CREATE TYPE app.second AS VALUE PRIMITIVE KERNEL CONTRACT 'app.second@1' IMMUTABLE PERSISTABLE;\n\
            EXPORT TYPE app.source AS app.second_binding;\n\
            CREATE SCHEMA std;";
    let a_source = "EXPORT TYPE app.source TO PRELUDE AS FIRST;\n\
            CREATE TYPE app.first AS VALUE PRIMITIVE KERNEL CONTRACT 'app.first@1' IMMUTABLE TRANSIENT;\n\
            EXPORT TYPE app.source AS app.first_binding;\n\
            CREATE TYPE StD.first AS OBJECT ();";
    let report = check(
        &bundle([("z.orna", z_source), ("a.orna", a_source)]),
        &empty_catalogue(),
    );

    assert_eq!(report.diagnostics().len(), 8);
    let expected = [
        (
            "z.orna",
            "only the standard library can export a type to the prelude",
            z_source.find("TO PRELUDE").unwrap(),
            "TO PRELUDE".len(),
        ),
        (
            "z.orna",
            "KERNEL CONTRACT is available only to the standard library",
            z_source.find("KERNEL CONTRACT").unwrap(),
            "KERNEL CONTRACT".len(),
        ),
        (
            "z.orna",
            "qualified type exports are available only to the standard library",
            z_source.find("app.second_binding").unwrap(),
            "app.second_binding".len(),
        ),
        (
            "z.orna",
            "the std namespace is owned by the standard library",
            z_source.find("std").unwrap(),
            "std".len(),
        ),
        (
            "a.orna",
            "only the standard library can export a type to the prelude",
            a_source.find("TO PRELUDE").unwrap(),
            "TO PRELUDE".len(),
        ),
        (
            "a.orna",
            "KERNEL CONTRACT is available only to the standard library",
            a_source.find("KERNEL CONTRACT").unwrap(),
            "KERNEL CONTRACT".len(),
        ),
        (
            "a.orna",
            "qualified type exports are available only to the standard library",
            a_source.find("app.first_binding").unwrap(),
            "app.first_binding".len(),
        ),
        (
            "a.orna",
            "the std namespace is owned by the standard library",
            a_source.find("StD.first").unwrap(),
            "StD.first".len(),
        ),
    ];
    for (diagnostic, (path, message, start, length)) in report.diagnostics().iter().zip(expected) {
        assert_eq!(diagnostic.code(), DiagnosticCode::DomainIncompatible);
        assert_eq!(diagnostic.message(), message);
        assert_eq!(diagnostic.location().logical_path(), path);
        assert_eq!(diagnostic.location().span().start(), start);
        assert_eq!(diagnostic.location().span().end(), start + length);
    }
    assert_no_checked_bundle(&report);
}

#[test]
fn rejects_opaque_value_declarations_outside_the_standard_library() {
    let source =
        "CREATE TYPE app.token AS VALUE OPAQUE KERNEL CONTRACT 'app.token@1' IMMUTABLE TRANSIENT;";
    let report = check(&bundle([("opaque.orna", source)]), &empty_catalogue());

    assert_eq!(report.diagnostics().len(), 1);
    let diagnostic = &report.diagnostics()[0];
    assert_eq!(diagnostic.code(), DiagnosticCode::DomainIncompatible);
    assert_eq!(
        diagnostic.message(),
        "KERNEL CONTRACT is available only to the standard library"
    );
    assert_eq!(diagnostic.location().logical_path(), "opaque.orna");
    assert_eq!(
        diagnostic.location().span().start(),
        source.find("KERNEL CONTRACT").unwrap()
    );
    assert_eq!(
        diagnostic.location().span().end(),
        source.find("KERNEL CONTRACT").unwrap() + "KERNEL CONTRACT".len()
    );
    assert_no_checked_bundle(&report);
}

#[test]
fn syntax_errors_precede_protected_primitive_and_export_diagnostics() {
    let source = "CREATE TYPE std.broken AS VALUE PRIMITIVE KERNEL CONTRACT 'std.broken@1' IMMUTABLE;\n\
            CREATE TYPE app.value AS VALUE PRIMITIVE KERNEL CONTRACT 'app.value@1' IMMUTABLE PERSISTABLE;\n\
            EXPORT TYPE app.value AS app.binding;\n\
            EXPORT TYPE app.value TO PRELUDE AS VALUE;";
    let report = check(&bundle([("precedence.orna", source)]), &empty_catalogue());

    assert_eq!(report.diagnostics().len(), 1);
    let diagnostic = &report.diagnostics()[0];
    assert_eq!(diagnostic.code(), DiagnosticCode::UnexpectedToken);
    assert_eq!(
        diagnostic.message(),
        "expected PERSISTABLE or TRANSIENT after IMMUTABLE"
    );
    assert_eq!(diagnostic.location().logical_path(), "precedence.orna");
    assert_eq!(
        diagnostic.location().span().start(),
        source.find(";").unwrap()
    );
    assert_no_checked_bundle(&report);
}

#[test]
fn protects_quoted_std_but_not_uppercase_quoted_std() {
    let source = "CREATE SCHEMA \"std\"; CREATE SCHEMA \"STD\";";
    let report = check(&bundle([("quoted.orna", source)]), &empty_catalogue());

    assert_eq!(report.diagnostics().len(), 1);
    let diagnostic = &report.diagnostics()[0];
    assert_eq!(diagnostic.code(), DiagnosticCode::DomainIncompatible);
    assert_eq!(
        diagnostic.message(),
        "the std namespace is owned by the standard library"
    );
    assert_eq!(
        diagnostic.location().span().start(),
        source.find("\"std\"").unwrap()
    );
    assert_no_checked_bundle(&report);
}

#[test]
fn rejects_every_std_owner_form_at_its_complete_name() {
    let source = "CREATE SCHEMA std;\n\
            CREATE TYPE std.object AS OBJECT ();\n\
            CREATE TYPE std.primitive AS VALUE PRIMITIVE KERNEL CONTRACT 'app.contract@1' IMMUTABLE PERSISTABLE;\n\
            ALTER TYPE std.object RENAME FIELD old TO new;\n\
            CREATE SERVER FUNCTION std.server() RETURNS ROWS (value BOOLEAN) AS SELECT o.value FROM std.object o;\n\
            CREATE CLIENT FUNCTION std.client() RETURNS BOOLEAN RETURN TRUE;\n\
            EXPORT TYPE app.value AS std.binding;";
    let report = check(&bundle([("owners.orna", source)]), &empty_catalogue());

    let expected = [
        (
            "std",
            source.find("CREATE SCHEMA std").unwrap() + "CREATE SCHEMA ".len(),
        ),
        (
            "std.object",
            source.find("CREATE TYPE std.object").unwrap() + "CREATE TYPE ".len(),
        ),
        (
            "std.primitive",
            source.find("CREATE TYPE std.primitive").unwrap() + "CREATE TYPE ".len(),
        ),
        (
            "std.object",
            source.find("ALTER TYPE std.object").unwrap() + "ALTER TYPE ".len(),
        ),
        (
            "std.server",
            source.find("CREATE SERVER FUNCTION std.server").unwrap()
                + "CREATE SERVER FUNCTION ".len(),
        ),
        (
            "std.client",
            source.find("CREATE CLIENT FUNCTION std.client").unwrap()
                + "CREATE CLIENT FUNCTION ".len(),
        ),
        (
            "std.binding",
            source.find("AS std.binding").unwrap() + "AS ".len(),
        ),
    ];
    assert_eq!(report.diagnostics().len(), expected.len());
    for (diagnostic, (name, start)) in report.diagnostics().iter().zip(expected) {
        assert_eq!(diagnostic.code(), DiagnosticCode::DomainIncompatible);
        assert_eq!(
            diagnostic.message(),
            "the std namespace is owned by the standard library"
        );
        assert_eq!(diagnostic.location().span().start(), start);
        assert_eq!(diagnostic.location().span().end(), start + name.len());
    }
    assert_no_checked_bundle(&report);
}

pub(super) const STD_INVOKE_SOURCE: &str = "CREATE SCHEMA std.invoke;\nCREATE SERVER FUNCTION std.invoke.echo(\n    p_value INTEGER\n)\nRETURNS INTEGER\nSECURITY INVOKER\nTRANSACTION READ ONLY\nVOLATILITY STABLE\nAS\n    SELECT p_value;";
/// The exact retained V2 `std/types.orna` source: the retained
/// `orna.std/1`-shape type declarations for the fixed INTEGER value type.
pub(super) const STANDARD_V2_TYPES_SOURCE: &str = "CREATE SCHEMA std;CREATE SCHEMA std.types;CREATE TYPE std.types.INTEGER AS VALUE PRIMITIVE KERNEL CONTRACT 'orna.kernel.value.integer@1' IMMUTABLE PERSISTABLE;EXPORT TYPE std.types.INTEGER AS std.INTEGER;EXPORT TYPE std.INTEGER TO PRELUDE AS INTEGER;";
