use super::*;

#[test]
fn accepts_client_action_call_with_canonical_target_and_argument_identities() {
    let target_id = FunctionId::from_bytes([0x51; 16]);
    let target_parameter_id = ParameterId::from_bytes([0x52; 16]);
    let argument_type = ResolvedType::Scalar(StandardScalar::Integer);
    let integer_type_id = TypeId::from_bytes([0x48; 16]);
    let base = CatalogueSnapshot::new_with_functions(
        CatalogueRevisionId::from_bytes([0x53; 16]),
        vec![SchemaDefinition::new(
            SchemaId::from_bytes([0x54; 16]),
            QualifiedSemanticName::new(["tasks"]).unwrap(),
        )],
        Vec::new(),
        vec![FunctionDefinition::new(
            target_id,
            QualifiedSemanticName::new(["tasks", "run"]).unwrap(),
            FunctionDomain::Client,
            vec![ParameterDefinition::new(
                target_parameter_id,
                "p_value",
                0,
                argument_type,
                None,
            )],
            FunctionReturn::Single(argument_type),
            FunctionRevisionId::from_bytes([0x55; 16]),
            FunctionSecurity::Invoker,
            None,
            FunctionVolatility::Immutable,
        )],
    )
    .unwrap();
    let standard =
        check_standard_library_source(&verified_standard_library_with_action_for_test()).unwrap();
    let context = StandardApplicationCheckContext::try_new(&base, &standard).unwrap();
    let source = "CREATE SCHEMA ui; CREATE CLIENT FUNCTION ui.run(p_value INTEGER) RETURNS std.Action AS std.action.call(target => tasks.run, arguments => std.call.args(p_value => p_value));";
    let report = check_standard_application(&bundle([("action.orna", source)]), &context);
    assert!(
        report.diagnostics().is_empty(),
        "{:?}",
        report.diagnostics()
    );

    let function = report
        .preparation_view()
        .unwrap()
        .checked()
        .client_functions()
        .iter()
        .find(|function| function.name().to_string() == "ui.run")
        .unwrap();
    let caller_parameter_id = function.parameters()[0].id();
    let CheckedClientFunctionBody::Expression { expression } = function.body() else {
        panic!("expected an expression CLIENT action body");
    };
    let super::super::CheckedClientExpression::Action { operation } = expression else {
        panic!("expected std.action.call to lower to an action operation");
    };
    assert_eq!(
        operation.target_domain(),
        orna_artifact::client_plan::ActionTargetDomain::Client
    );
    assert_eq!(
        operation.target(),
        super::super::CheckedFunctionId::Existing(target_id)
    );
    assert_eq!(operation.arguments().len(), 1);
    assert_eq!(
        operation.arguments()[0].0,
        super::super::CheckedParameterId::Existing(target_parameter_id)
    );
    assert!(matches!(
        &operation.arguments()[0].1,
        super::super::CheckedClientExpression::ParameterRead { parameter, .. }
            if *parameter == caller_parameter_id
    ));
    assert_eq!(
        operation.result_type(),
        super::super::SemanticType::Scalar(StandardScalar::Integer)
    );
    assert_eq!(operation.standard_result_type(), Some(integer_type_id));
}

#[test]
fn sorts_resource_and_action_arguments_by_checked_parameter_id() {
    let integer = ResolvedType::Scalar(StandardScalar::Integer);
    let resource_target_id = FunctionId::from_bytes([0x71; 16]);
    let resource_high_parameter_id = ParameterId::from_bytes([0x72; 16]);
    let resource_low_parameter_id = ParameterId::from_bytes([0x70; 16]);
    let resource_base = CatalogueSnapshot::new_with_functions(
        CatalogueRevisionId::from_bytes([0x73; 16]),
        vec![SchemaDefinition::new(
            SchemaId::from_bytes([0x74; 16]),
            QualifiedSemanticName::new(["tasks"]).unwrap(),
        )],
        Vec::new(),
        vec![FunctionDefinition::new(
            resource_target_id,
            QualifiedSemanticName::new(["tasks", "find"]).unwrap(),
            FunctionDomain::Server,
            vec![
                ParameterDefinition::new(resource_high_parameter_id, "p_high", 0, integer, None),
                ParameterDefinition::new(resource_low_parameter_id, "p_low", 1, integer, None),
            ],
            FunctionReturn::Single(integer),
            FunctionRevisionId::from_bytes([0x75; 16]),
            FunctionSecurity::Invoker,
            None,
            FunctionVolatility::Immutable,
        )],
    )
    .unwrap();
    let resource_source = "CREATE SCHEMA ui; CREATE CLIENT FUNCTION ui.run() RETURNS INTEGER IS BEGIN RETURN AWAIT std.data.resource(target => tasks.find, arguments => std.call.args(p_low => 7, p_high => 8)); END;";
    let resource_report = check(
        &bundle([("resource-argument-order.orna", resource_source)]),
        &resource_base,
    );
    assert!(
        resource_report.diagnostics().is_empty(),
        "{:?}",
        resource_report.diagnostics()
    );
    let resource_function = resource_report
        .checked_bundle()
        .unwrap()
        .client_functions()
        .iter()
        .find(|function| function.name().to_string() == "ui.run")
        .unwrap();
    let CheckedClientFunctionBody::Expression { expression } = resource_function.body() else {
        panic!("resource body must be an expression");
    };
    let CheckedClientExpression::Await { expression, .. } = expression else {
        panic!("resource body must await the resource");
    };
    let CheckedClientExpression::Resource { operation } = expression.as_ref() else {
        panic!("await operand must be a resource");
    };
    assert_eq!(
        operation
            .arguments()
            .iter()
            .map(|(parameter, _)| *parameter)
            .collect::<Vec<_>>(),
        vec![
            super::super::CheckedParameterId::Existing(resource_low_parameter_id),
            super::super::CheckedParameterId::Existing(resource_high_parameter_id),
        ]
    );

    let action_target_id = FunctionId::from_bytes([0x76; 16]);
    let action_high_parameter_id = ParameterId::from_bytes([0x78; 16]);
    let action_low_parameter_id = ParameterId::from_bytes([0x77; 16]);
    let action_base = CatalogueSnapshot::new_with_functions(
        CatalogueRevisionId::from_bytes([0x79; 16]),
        vec![SchemaDefinition::new(
            SchemaId::from_bytes([0x7a; 16]),
            QualifiedSemanticName::new(["tasks"]).unwrap(),
        )],
        Vec::new(),
        vec![FunctionDefinition::new(
            action_target_id,
            QualifiedSemanticName::new(["tasks", "run"]).unwrap(),
            FunctionDomain::Client,
            vec![
                ParameterDefinition::new(action_high_parameter_id, "p_high", 0, integer, None),
                ParameterDefinition::new(action_low_parameter_id, "p_low", 1, integer, None),
            ],
            FunctionReturn::Single(integer),
            FunctionRevisionId::from_bytes([0x7b; 16]),
            FunctionSecurity::Invoker,
            None,
            FunctionVolatility::Immutable,
        )],
    )
    .unwrap();
    let standard =
        check_standard_library_source(&verified_standard_library_with_action_for_test()).unwrap();
    let action_context = StandardApplicationCheckContext::try_new(&action_base, &standard).unwrap();
    let action_source = "CREATE SCHEMA ui; CREATE CLIENT FUNCTION ui.run() RETURNS std.Action AS std.action.call(target => tasks.run, arguments => std.call.args(p_low => 7, p_high => 8));";
    let action_report = check_standard_application(
        &bundle([("action-argument-order.orna", action_source)]),
        &action_context,
    );
    assert!(
        action_report.diagnostics().is_empty(),
        "{:?}",
        action_report.diagnostics()
    );
    let action_function = action_report
        .preparation_view()
        .unwrap()
        .checked()
        .client_functions()
        .iter()
        .find(|function| function.name().to_string() == "ui.run")
        .unwrap();
    let CheckedClientFunctionBody::Expression { expression } = action_function.body() else {
        panic!("action body must be an expression");
    };
    let CheckedClientExpression::Action { operation } = expression else {
        panic!("action body must be an action");
    };
    assert_eq!(
        operation
            .arguments()
            .iter()
            .map(|(parameter, _)| *parameter)
            .collect::<Vec<_>>(),
        vec![
            super::super::CheckedParameterId::Existing(action_low_parameter_id),
            super::super::CheckedParameterId::Existing(action_high_parameter_id),
        ]
    );
}

#[test]
fn rejects_actions_in_client_state_returns_before_preparation() {
    let target_id = FunctionId::from_bytes([0x51; 16]);
    let argument_type = ResolvedType::Scalar(StandardScalar::Integer);
    let base = CatalogueSnapshot::new_with_functions(
        CatalogueRevisionId::from_bytes([0x53; 16]),
        vec![SchemaDefinition::new(
            SchemaId::from_bytes([0x54; 16]),
            QualifiedSemanticName::new(["tasks"]).unwrap(),
        )],
        Vec::new(),
        vec![FunctionDefinition::new(
            target_id,
            QualifiedSemanticName::new(["tasks", "run"]).unwrap(),
            FunctionDomain::Client,
            Vec::new(),
            FunctionReturn::Single(argument_type),
            FunctionRevisionId::from_bytes([0x55; 16]),
            FunctionSecurity::Invoker,
            None,
            FunctionVolatility::Immutable,
        )],
    )
    .unwrap();
    let standard =
        check_standard_library_source(&verified_standard_library_with_action_for_test()).unwrap();
    let context = StandardApplicationCheckContext::try_new(&base, &standard).unwrap();
    let source = "CREATE SCHEMA ui; CREATE CLIENT FUNCTION ui.run() RETURNS std.Action IS \
            STATE ready INTEGER; \
            BEGIN RETURN std.action.call(target => tasks.run, arguments => std.call.args()); END;";
    let report = check_standard_application(&bundle([("state-action.orna", source)]), &context);
    assert_eq!(report.diagnostics().len(), 1, "{:?}", report.diagnostics());
    assert_eq!(
        report.diagnostics()[0].code(),
        DiagnosticCode::DomainIncompatible,
        "{:?}",
        report.diagnostics()
    );
    assert_eq!(
        report.diagnostics()[0].message(),
        "CLIENT state blocks do not support action expressions"
    );
    assert!(report.preparation_view().is_none());
}

#[test]
fn accepts_client_action_call_with_canonical_server_target() {
    let target_id = FunctionId::from_bytes([0x56; 16]);
    let target_parameter_id = ParameterId::from_bytes([0x57; 16]);
    let argument_type = ResolvedType::Scalar(StandardScalar::Integer);
    let integer_type_id = TypeId::from_bytes([0x48; 16]);
    let base = CatalogueSnapshot::new_with_functions(
        CatalogueRevisionId::from_bytes([0x58; 16]),
        vec![SchemaDefinition::new(
            SchemaId::from_bytes([0x59; 16]),
            QualifiedSemanticName::new(["tasks"]).unwrap(),
        )],
        Vec::new(),
        vec![FunctionDefinition::new(
            target_id,
            QualifiedSemanticName::new(["tasks", "rebuild"]).unwrap(),
            FunctionDomain::Server,
            vec![ParameterDefinition::new(
                target_parameter_id,
                "p_value",
                0,
                argument_type,
                None,
            )],
            FunctionReturn::Single(argument_type),
            FunctionRevisionId::from_bytes([0x5a; 16]),
            FunctionSecurity::Invoker,
            None,
            FunctionVolatility::Immutable,
        )],
    )
    .unwrap();
    let standard =
        check_standard_library_source(&verified_standard_library_with_action_for_test()).unwrap();
    let context = StandardApplicationCheckContext::try_new(&base, &standard).unwrap();
    let source = "CREATE SCHEMA ui; CREATE CLIENT FUNCTION ui.run(p_value INTEGER) RETURNS std.Action AS std.action.call(target => tasks.rebuild, arguments => std.call.args(p_value => p_value));";
    let report = check_standard_application(&bundle([("action-server.orna", source)]), &context);
    assert!(
        report.diagnostics().is_empty(),
        "{:?}",
        report.diagnostics()
    );

    let function = report
        .preparation_view()
        .unwrap()
        .checked()
        .client_functions()
        .iter()
        .find(|function| function.name().to_string() == "ui.run")
        .unwrap();
    let caller_parameter_id = function.parameters()[0].id();
    let CheckedClientFunctionBody::Expression { expression } = function.body() else {
        panic!("expected an expression CLIENT action body");
    };
    let super::super::CheckedClientExpression::Action { operation } = expression else {
        panic!("expected std.action.call to lower to an action operation");
    };
    assert_eq!(
        operation.target_domain(),
        orna_artifact::client_plan::ActionTargetDomain::Server
    );
    assert_eq!(
        operation.target(),
        super::super::CheckedFunctionId::Existing(target_id)
    );
    assert_eq!(operation.arguments().len(), 1);
    assert_eq!(
        operation.arguments()[0].0,
        super::super::CheckedParameterId::Existing(target_parameter_id)
    );
    assert!(matches!(
        &operation.arguments()[0].1,
        super::super::CheckedClientExpression::ParameterRead { parameter, .. }
            if *parameter == caller_parameter_id
    ));
    assert_eq!(
        operation.result_type(),
        super::super::SemanticType::Scalar(StandardScalar::Integer)
    );
    assert_eq!(operation.standard_result_type(), Some(integer_type_id));
}

#[test]
fn excludes_stream_and_one_column_rows_action_targets() {
    let integer = ResolvedType::Scalar(StandardScalar::Integer);
    let base = CatalogueSnapshot::new_with_functions(
        CatalogueRevisionId::from_bytes([0x5a; 16]),
        vec![SchemaDefinition::new(
            SchemaId::from_bytes([0x5b; 16]),
            QualifiedSemanticName::new(["tasks"]).unwrap(),
        )],
        Vec::new(),
        vec![
            FunctionDefinition::new(
                FunctionId::from_bytes([0x5c; 16]),
                QualifiedSemanticName::new(["tasks", "events"]).unwrap(),
                FunctionDomain::Server,
                Vec::new(),
                FunctionReturn::Stream(integer),
                FunctionRevisionId::from_bytes([0x5d; 16]),
                FunctionSecurity::Invoker,
                None,
                FunctionVolatility::Immutable,
            ),
            FunctionDefinition::new(
                FunctionId::from_bytes([0x5e; 16]),
                QualifiedSemanticName::new(["tasks", "rows"]).unwrap(),
                FunctionDomain::Server,
                Vec::new(),
                FunctionReturn::Rows(vec![rows_column("value", 0, integer)]),
                FunctionRevisionId::from_bytes([0x5f; 16]),
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
    let source = "CREATE SCHEMA ui; \
            CREATE CLIENT FUNCTION ui.stream() RETURNS STREAM<INTEGER> IS \
            BEGIN RETURN AWAIT std.data.stream_resource(target => tasks.events, arguments => std.call.args()); END; \
            CREATE CLIENT FUNCTION ui.stream_action() RETURNS std.Action AS \
            std.action.call(target => ui.stream, arguments => std.call.args()); \
            CREATE CLIENT FUNCTION ui.rows_action() RETURNS std.Action AS \
            std.action.call(target => tasks.rows, arguments => std.call.args());";
    let report = check_standard_application(&bundle([("action-shapes.orna", source)]), &context);
    let messages = report
        .diagnostics()
        .iter()
        .map(|diagnostic| diagnostic.message())
        .collect::<Vec<_>>();
    assert!(
        messages.contains(&"unknown std.action.call target ui.stream"),
        "{messages:?}"
    );
    assert_eq!(
        messages
            .iter()
            .filter(|message| message.ends_with("does not return one durable value"))
            .count(),
        1,
        "{messages:?}"
    );
}

#[test]
fn accepts_transient_standard_opaque_action_target_result() {
    let target_id = FunctionId::from_bytes([0x5a; 16]);
    let action_type = ResolvedType::Named(STD_ACTION_TYPE_ID);
    let base = CatalogueSnapshot::new_with_functions(
        CatalogueRevisionId::from_bytes([0x5b; 16]),
        vec![SchemaDefinition::new(
            SchemaId::from_bytes([0x5c; 16]),
            QualifiedSemanticName::new(["tasks"]).unwrap(),
        )],
        Vec::new(),
        vec![FunctionDefinition::new(
            target_id,
            QualifiedSemanticName::new(["tasks", "run"]).unwrap(),
            FunctionDomain::Client,
            Vec::new(),
            FunctionReturn::Single(action_type),
            FunctionRevisionId::from_bytes([0x5d; 16]),
            FunctionSecurity::Invoker,
            None,
            FunctionVolatility::Immutable,
        )],
    )
    .unwrap();
    let standard =
        check_standard_library_source(&verified_standard_library_with_action_for_test()).unwrap();
    let context = StandardApplicationCheckContext::try_new(&base, &standard).unwrap();
    let source = "CREATE SCHEMA ui; CREATE CLIENT FUNCTION ui.run() RETURNS std.Action AS std.action.call(target => tasks.run, arguments => std.call.args());";
    let report = check_standard_application(&bundle([("action-transient.orna", source)]), &context);
    assert!(
        report.diagnostics().is_empty(),
        "{:?}",
        report.diagnostics()
    );
    let function = report
        .preparation_view()
        .unwrap()
        .checked()
        .client_functions()
        .iter()
        .find(|function| function.name().to_string() == "ui.run")
        .unwrap();
    let CheckedClientFunctionBody::Expression { expression } = function.body() else {
        panic!("expected an expression CLIENT action body");
    };
    let super::super::CheckedClientExpression::Action { operation } = expression else {
        panic!("expected std.action.call to lower to an action operation");
    };
    assert_eq!(
        operation.target(),
        super::super::CheckedFunctionId::Existing(target_id)
    );
    assert_eq!(
        operation.result_type(),
        super::super::SemanticType::Named(super::super::CheckedTypeId::Existing(
            STD_ACTION_TYPE_ID
        ))
    );
    assert_eq!(operation.standard_result_type(), Some(STD_ACTION_TYPE_ID));
}

#[test]
fn accepts_orv3_enum_and_record_action_arguments() {
    let standard =
        check_standard_library_source(&verified_standard_library_with_action_for_test()).unwrap();
    let base = empty_catalogue();
    let context = StandardApplicationCheckContext::try_new(&base, &standard).unwrap();
    let source = "CREATE SCHEMA app; \
            CREATE TYPE app.phase AS ENUM ('ready', 'done'); \
            CREATE TYPE app.status AS VALUE (active INTEGER) IMMUTABLE PERSISTABLE; \
            CREATE CLIENT FUNCTION app.target(p_phase app.phase, p_status app.status) RETURNS INTEGER AS 1; \
            CREATE CLIENT FUNCTION app.run(p_phase app.phase, p_status app.status) RETURNS std.Action AS \
                std.action.call(target => app.target, arguments => std.call.args(p_phase => p_phase, p_status => p_status));";
    let report = check_standard_application(&bundle([("action-orv3.orna", source)]), &context);
    assert!(
        report.diagnostics().is_empty(),
        "{:?}",
        report.diagnostics()
    );
    let function = report
        .preparation_view()
        .unwrap()
        .checked()
        .client_functions()
        .iter()
        .find(|function| function.name().to_string() == "app.run")
        .unwrap();
    let CheckedClientFunctionBody::Expression { expression } = function.body() else {
        panic!("expected an expression CLIENT action body");
    };
    let super::super::CheckedClientExpression::Action { operation } = expression else {
        panic!("expected std.action.call to lower to an action operation");
    };
    assert_eq!(operation.arguments().len(), 2);
}

#[test]
fn rejects_action_call_targets_reserved_combinators_and_argument_errors() {
    let target_id = FunctionId::from_bytes([0x61; 16]);
    let target_parameter_id = ParameterId::from_bytes([0x62; 16]);
    let argument_type = ResolvedType::Scalar(StandardScalar::Integer);
    let target_bad_id = FunctionId::from_bytes([0x66; 16]);
    let bad_parameter_id = ParameterId::from_bytes([0x67; 16]);
    let target_bad_result_id = FunctionId::from_bytes([0x69; 16]);
    let application_enum_type_id = TypeId::from_bytes([0x6b; 16]);
    let action_type = ResolvedType::Named(STD_ACTION_TYPE_ID);
    let bad_result_type = ResolvedType::Named(application_enum_type_id);
    let base = CatalogueSnapshot::new_with_functions_and_enum_types(
        CatalogueRevisionId::from_bytes([0x63; 16]),
        vec![SchemaDefinition::new(
            SchemaId::from_bytes([0x64; 16]),
            QualifiedSemanticName::new(["tasks"]).unwrap(),
        )],
        Vec::new(),
        Vec::new(),
        vec![EnumTypeDefinition::new(
            application_enum_type_id,
            QualifiedSemanticName::new(["tasks", "status"]).unwrap(),
            ["ready", "done"],
        )],
        Vec::new(),
        vec![
            FunctionDefinition::new(
                target_id,
                QualifiedSemanticName::new(["tasks", "run"]).unwrap(),
                FunctionDomain::Client,
                vec![ParameterDefinition::new(
                    target_parameter_id,
                    "p_value",
                    0,
                    argument_type,
                    None,
                )],
                FunctionReturn::Single(argument_type),
                FunctionRevisionId::from_bytes([0x65; 16]),
                FunctionSecurity::Invoker,
                None,
                FunctionVolatility::Immutable,
            ),
            FunctionDefinition::new(
                target_bad_id,
                QualifiedSemanticName::new(["tasks", "bad"]).unwrap(),
                FunctionDomain::Client,
                vec![ParameterDefinition::new(
                    bad_parameter_id,
                    "p_action",
                    0,
                    action_type,
                    None,
                )],
                FunctionReturn::Single(argument_type),
                FunctionRevisionId::from_bytes([0x68; 16]),
                FunctionSecurity::Invoker,
                None,
                FunctionVolatility::Immutable,
            ),
            FunctionDefinition::new(
                target_bad_result_id,
                QualifiedSemanticName::new(["tasks", "bad_return"]).unwrap(),
                FunctionDomain::Client,
                Vec::new(),
                FunctionReturn::Single(bad_result_type),
                FunctionRevisionId::from_bytes([0x6a; 16]),
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
    let cases = [
        (
            "std.action.call(target => missing.run, arguments => std.call.args(p_value => p_value))",
            DiagnosticCode::UnknownQualifiedName,
            "unknown std.action.call target missing.run",
        ),
        (
            "std.action.call(target => TRUE, arguments => std.call.args(p_value => p_value))",
            DiagnosticCode::TypeMismatch,
            "std.action.call target must be a qualified function name",
        ),
        (
            "std.action.call(target => tasks.run, arguments => TRUE)",
            DiagnosticCode::TypeMismatch,
            "std.action.call arguments must be a std.call.args value",
        ),
        (
            "std.action.call(target => tasks.bad, arguments => std.call.args(p_action => p_value))",
            DiagnosticCode::TypeMismatch,
            "std.action.call target tasks.bad has a parameter that is not ORV3-encodable",
        ),
        (
            "std.action.call(target => tasks.bad_return, arguments => std.call.args())",
            DiagnosticCode::UnknownQualifiedName,
            "std.action.call target tasks.bad_return does not return one durable value",
        ),
        (
            "std.action.sequence()",
            DiagnosticCode::UnknownQualifiedName,
            "unknown CLIENT function std.action.sequence",
        ),
        (
            "std.action.parallel()",
            DiagnosticCode::UnknownQualifiedName,
            "unknown CLIENT function std.action.parallel",
        ),
        (
            "std.action.call(target => tasks.run, arguments => std.call.args())",
            DiagnosticCode::TypeMismatch,
            "missing argument for std.action.call target tasks.run",
        ),
        (
            "std.action.call(target => tasks.run, arguments => std.call.args(missing => p_value))",
            DiagnosticCode::UnknownQualifiedName,
            "unknown std.action.call parameter missing",
        ),
        (
            "std.action.call(target => tasks.run, arguments => std.call.args(p_value => p_value, p_value => p_value))",
            DiagnosticCode::DuplicateDefinition,
            "duplicate std.action.call parameter p_value",
        ),
        (
            "std.action.call(target => tasks.run, arguments => std.call.args(p_value => 'wrong'))",
            DiagnosticCode::TypeMismatch,
            "std.action.call argument does not match parameter p_value",
        ),
    ];
    for (index, (expression, code, message)) in cases.into_iter().enumerate() {
        let source = format!(
            "CREATE SCHEMA ui; CREATE CLIENT FUNCTION ui.run(p_value INTEGER) RETURNS std.Action AS {expression};"
        );
        let report = check_standard_application(
            &SourceBundle::new([SourceUnit::new("action-reject.orna", source)]).unwrap(),
            &context,
        );
        assert_eq!(
            report.diagnostics().len(),
            1,
            "case {index}: {:?}",
            report.diagnostics()
        );
        assert_eq!(report.diagnostics()[0].code(), code, "case {index}");
        assert_eq!(report.diagnostics()[0].message(), message, "case {index}");
        assert!(report.checked_bundle().is_none(), "case {index}");
    }
}

#[test]
fn checked_opaque_standard_remains_definition_only_for_applications() {
    let snapshot = verified_standard_library_with_opaque_for_test();
    let standard = check_standard_library_source(&snapshot).unwrap();
    assert_eq!(standard.value_types().len(), 2);
    assert_eq!(standard.value_types()[0].kind(), ValueTypeKind::Primitive);
    assert_eq!(standard.value_types()[1].kind(), ValueTypeKind::Opaque);
    assert_eq!(
        standard.value_types()[1].representation_contract(),
        "std.token@1"
    );
    let application = empty_catalogue();
    let context = StandardApplicationCheckContext::try_new(&application, &standard).unwrap();

    let source = "CREATE SCHEMA app;CREATE TYPE app.item AS OBJECT (token std.TOKEN NOT NULL);";
    let report = check_standard_application(&bundle([("opaque-use.orna", source)]), &context);
    assert!(report.checked_bundle().is_none());
    assert_eq!(report.diagnostics().len(), 1);
    assert_eq!(
        report.diagnostics()[0].code(),
        DiagnosticCode::UnknownQualifiedName
    );
    assert_eq!(
        report.diagnostics()[0].message(),
        "unknown type name std.token"
    );
}

pub(super) fn empty_version_two_active(
    standard: &orna_core::revision::VerifiedStandardLibrarySnapshot,
) -> ActiveDatabaseRevision {
    let source_unit = StoredSourceUnit::new(
        SourceUnitId::from_bytes([0x41; 16]),
        0,
        "active.orna",
        "",
        source_unit_content_digest("").unwrap(),
    )
    .unwrap();
    let bundle_hash = source_bundle_digest(std::slice::from_ref(&source_unit)).unwrap();
    let source = StoredSourceRevision::new(
        SourceBundleId::from_bytes([0x42; 16]),
        SourceRevisionId::from_bytes([0x43; 16]),
        None,
        vec![source_unit],
        bundle_hash,
        source_revision_record_digest(SourceBundleId::from_bytes([0x42; 16]), None, bundle_hash)
            .unwrap(),
    )
    .unwrap();
    let catalogue = CatalogueSnapshot::new_with_types(
        CatalogueRevisionId::from_bytes([0x44; 16]),
        vec![],
        vec![],
        vec![],
        vec![],
    )
    .unwrap();
    let context = CatalogueHashContext::version_two(standard.clone());
    let catalogue_hash =
        catalogue_digest_with_context(&context, &catalogue, &[], &[], &[], &[]).unwrap();
    ActiveDatabaseRevision::new_with_catalogue_hash_context(
        ActiveDatabaseRevisionInput::new(
            RevisionPair::new(source.id(), catalogue.revision()),
            source,
            catalogue,
            catalogue_hash,
            ActiveRevisionContent::new(Vec::new(), Vec::new(), Vec::new(), Vec::new()),
        ),
        context,
    )
    .unwrap()
}

pub(super) fn active_from_prepared(prepared: &DeployableRevision) -> ActiveDatabaseRevision {
    ActiveDatabaseRevision::new_with_catalogue_hash_context(
        ActiveDatabaseRevisionInput::new(
            prepared.candidate_pair(),
            prepared.source().clone(),
            prepared.candidate().clone(),
            prepared.catalogue_hash(),
            ActiveRevisionContent::new(
                prepared.expressions().to_vec(),
                prepared
                    .current_function_revisions()
                    .map_or_else(Vec::new, ToOwned::to_owned),
                prepared.origins().to_vec(),
                prepared.references().to_vec(),
            ),
        ),
        prepared.catalogue_hash_context().clone(),
    )
    .unwrap()
}

pub(super) fn expression_use<'a>(
    uses: &[&'a CheckedApplicationTypeUse],
    ordinal: u32,
) -> &'a CheckedApplicationTypeUse {
    let matches = uses
        .iter()
        .copied()
        .filter(|type_use| {
            matches!(
                type_use.kind(),
                CheckedTypeUseKind::Expression {
                    ordinal: candidate,
                    ..
                } if candidate == ordinal
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(matches.len(), 1, "expected expression ordinal {ordinal}");
    matches[0]
}

pub(super) fn result_use<'a>(
    uses: &[&'a CheckedApplicationTypeUse],
    ordinal: u32,
) -> &'a CheckedApplicationTypeUse {
    let matches = uses
        .iter()
        .copied()
        .filter(|type_use| {
            matches!(
                type_use.kind(),
                CheckedTypeUseKind::Result {
                    ordinal: candidate,
                    ..
                } if candidate == ordinal
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(matches.len(), 1, "expected result ordinal {ordinal}");
    matches[0]
}

pub(super) fn assert_type_use_span(type_use: &CheckedApplicationTypeUse, start: usize, text: &str) {
    assert_eq!(type_use.location().span().start(), start);
    assert_eq!(type_use.location().span().end(), start + text.len());
}

pub(super) fn checked_use_index(
    uses: &[CheckedApplicationTypeUse],
    kind: CheckedTypeUseKind,
    start: usize,
    end: usize,
) -> usize {
    let matches = uses
        .iter()
        .enumerate()
        .filter(|(_, type_use)| {
            type_use.kind() == kind
                && type_use.location().span().start() == start
                && type_use.location().span().end() == end
        })
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    assert_eq!(matches.len(), 1, "expected one exact arena use");
    matches[0]
}
