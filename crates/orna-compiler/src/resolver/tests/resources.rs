use super::*;

#[test]
fn accepts_scalar_resource_with_named_call_arguments() {
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
    let source = "CREATE SCHEMA ui; CREATE CLIENT FUNCTION ui.find(p_name TEXT) RETURNS TEXT IS BEGIN RETURN AWAIT std.data.resource(target => tasks.find, arguments => std.call.args(p_name => p_name)); END;";
    let report = check(&bundle([("resource.orna", source)]), &base);
    assert!(
        report.diagnostics().is_empty(),
        "{:?}",
        report.diagnostics()
    );
    let checked = report.checked_bundle().expect("resource source checks");
    let function = checked
        .client_functions()
        .iter()
        .find(|function| function.name().to_string() == "ui.find")
        .expect("checked CLIENT resource function");
    let CheckedClientFunctionBody::Expression { expression } = function.body() else {
        panic!("resource body must be an expression");
    };
    let super::super::CheckedClientExpression::Await {
        expression,
        location: await_location,
    } = expression
    else {
        panic!("resource body must await the resource");
    };
    let await_text = "AWAIT std.data.resource(target => tasks.find, arguments => std.call.args(p_name => p_name))";
    let await_start = source
        .find(await_text)
        .expect("await expression is present");
    assert_eq!(await_location.logical_path(), "resource.orna");
    assert_eq!(await_location.span().start(), await_start);
    assert_eq!(await_location.span().end(), await_start + await_text.len());
    let super::super::CheckedClientExpression::Resource { operation } = expression.as_ref() else {
        panic!("await operand must be a resource");
    };
    assert_eq!(
        operation.kind(),
        orna_artifact::client_plan::ResourceKind::Scalar
    );
    assert_eq!(operation.arguments().len(), 1);
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
}

#[test]
fn rejects_inline_row_resource_descriptors_in_both_procedural_local_paths() {
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
            Vec::new(),
            FunctionReturn::Single(ResolvedType::Scalar(StandardScalar::CharacterLargeObject)),
            FunctionRevisionId::from_bytes([0x44; 16]),
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::ReadOnly),
            FunctionVolatility::Stable,
        )],
    )
    .unwrap();

    for descriptor in [
        "TABLE (task_id UUID, title TEXT)",
        "RECORD (task_id UUID, title TEXT)",
    ] {
        for resource_type in ["Resource", "StreamResource"] {
            for (path, local) in [
                (
                    "state-less",
                    format!(
                        "LET rows std.data.{resource_type}<{descriptor}> := std.data.resource(target => tasks.find, arguments => std.call.args()); BEGIN RETURN AWAIT rows; END;"
                    ),
                ),
                (
                    "BEGIN LET",
                    format!(
                        "BEGIN LET rows std.data.{resource_type}<{descriptor}> := std.data.resource(target => tasks.find, arguments => std.call.args()); RETURN AWAIT rows; END;"
                    ),
                ),
            ] {
                let source = format!(
                    "CREATE SCHEMA ui; CREATE CLIENT FUNCTION ui.find() RETURNS TEXT IS {local}"
                );
                let source_bundle =
                    SourceBundle::new([SourceUnit::new("resource.orna", source)]).unwrap();
                let report = check(&source_bundle, &base);
                assert_eq!(report.diagnostics().len(), 1, "{path} {descriptor}");
                assert_eq!(report.diagnostics()[0].code(), DiagnosticCode::TypeMismatch);
                assert_eq!(
                    report.diagnostics()[0].message(),
                    "CLIENT local rows uses an inline TABLE/RECORD resource descriptor; row-resource transport is deferred"
                );
                assert_no_checked_bundle(&report);
            }
        }
    }
}

#[test]
fn rejects_inline_row_resource_descriptor_in_control_flow_local_path() {
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

    for descriptor in [
        "TABLE (task_id UUID, title TEXT)",
        "RECORD (task_id UUID, title TEXT)",
    ] {
        let source = format!(
            "CREATE SCHEMA ui; CREATE CLIENT FUNCTION ui.find() RETURNS TEXT IS \
             BEGIN IF TRUE THEN LET rows std.data.Resource<{descriptor}> := \
             std.data.resource(target => tasks.find, arguments => std.call.args()); \
             RETURN AWAIT rows; ELSE RETURN 'fallback'; END IF; END;"
        );
        let parsed = parse(&source);
        assert!(
            parsed.diagnostics().is_empty(),
            "{source}: {:?}",
            parsed.diagnostics()
        );

        let source_bundle =
            SourceBundle::new([SourceUnit::new("resource-control-flow.orna", source)]).unwrap();
        let report = check(&source_bundle, &base);
        assert_eq!(
            report.diagnostics().len(),
            1,
            "{descriptor}: {:?}",
            report.diagnostics()
        );
        assert_eq!(report.diagnostics()[0].code(), DiagnosticCode::TypeMismatch);
        assert_eq!(
            report.diagnostics()[0].message(),
            "CLIENT local rows uses an inline TABLE/RECORD resource descriptor; row-resource transport is deferred"
        );
        assert_no_checked_bundle(&report);
    }
}

#[test]
fn accepts_scalar_resource_descriptor_in_control_flow_local_path() {
    let base = CatalogueSnapshot::new_with_functions(
        CatalogueRevisionId::from_bytes([0x61; 16]),
        vec![SchemaDefinition::new(
            SchemaId::from_bytes([0x62; 16]),
            QualifiedSemanticName::new(["tasks"]).unwrap(),
        )],
        Vec::new(),
        vec![FunctionDefinition::new(
            FunctionId::from_bytes([0x63; 16]),
            QualifiedSemanticName::new(["tasks", "find"]).unwrap(),
            FunctionDomain::Server,
            Vec::new(),
            FunctionReturn::Single(ResolvedType::Scalar(StandardScalar::CharacterLargeObject)),
            FunctionRevisionId::from_bytes([0x64; 16]),
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::ReadOnly),
            FunctionVolatility::Stable,
        )],
    )
    .unwrap();
    let source = "CREATE SCHEMA ui; CREATE CLIENT FUNCTION ui.find() RETURNS TEXT IS \
                  BEGIN IF TRUE THEN LET value std.data.Resource<TEXT> := \
                  std.data.resource(target => tasks.find, arguments => std.call.args()); \
                  RETURN AWAIT value; ELSE RETURN 'fallback'; END IF; END;";
    let parsed = parse(source);
    assert!(
        parsed.diagnostics().is_empty(),
        "{source}: {:?}",
        parsed.diagnostics()
    );
    let source_bundle =
        SourceBundle::new([SourceUnit::new("resource-control-flow.orna", source)]).unwrap();
    let report = check(&source_bundle, &base);
    assert!(
        report.diagnostics().is_empty(),
        "{:?}",
        report.diagnostics()
    );
    assert!(report.checked_bundle().is_some());
}

#[test]
fn rejects_client_resource_table_descriptor_with_deferred_row_diagnostic() {
    let source = "CREATE SCHEMA ui; CREATE CLIENT FUNCTION ui.find() RETURNS TEXT IS BEGIN LET rows std.data.Resource<TABLE (task_id UUID, title TEXT)> := std.data.resource(target => tasks.find, arguments => std.call.args()); RETURN AWAIT rows; END;";
    let parsed = parse(source);
    assert!(
        parsed.diagnostics().is_empty(),
        "{source}: {:?}",
        parsed.diagnostics()
    );

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
            Vec::new(),
            FunctionReturn::Single(ResolvedType::Scalar(StandardScalar::CharacterLargeObject)),
            FunctionRevisionId::from_bytes([0x44; 16]),
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::ReadOnly),
            FunctionVolatility::Stable,
        )],
    )
    .unwrap();
    let report = check(&bundle([("resource-table.orna", source)]), &base);
    assert_eq!(report.diagnostics().len(), 1, "{:?}", report.diagnostics());
    assert_eq!(report.diagnostics()[0].code(), DiagnosticCode::TypeMismatch);
    assert_eq!(
        report.diagnostics()[0].message(),
        "CLIENT local rows uses an inline TABLE/RECORD resource descriptor; row-resource transport is deferred"
    );
    assert_no_checked_bundle(&report);
}

#[test]
fn rejects_client_stream_resource_record_descriptor_with_deferred_row_diagnostic() {
    let source = "CREATE SCHEMA tasks; CREATE TYPE tasks.task AS OBJECT (title TEXT); CREATE SERVER FUNCTION tasks.events() RETURNS STREAM<TEXT> AS SELECT t.title FROM tasks.task t; CREATE SCHEMA ui; CREATE CLIENT FUNCTION ui.events() RETURNS STREAM<TEXT> IS BEGIN LET rows std.data.StreamResource<RECORD (task_id UUID, title TEXT)> := std.data.stream_resource(target => tasks.events, arguments => std.call.args()); RETURN AWAIT rows; END;";
    let parsed = parse(source);
    assert!(
        parsed.diagnostics().is_empty(),
        "{source}: {:?}",
        parsed.diagnostics()
    );

    let report = check(
        &bundle([("stream-resource-record.orna", source)]),
        &empty_catalogue(),
    );
    assert_eq!(report.diagnostics().len(), 1, "{:?}", report.diagnostics());
    assert_eq!(report.diagnostics()[0].code(), DiagnosticCode::TypeMismatch);
    assert_eq!(
        report.diagnostics()[0].message(),
        "CLIENT local rows uses an inline TABLE/RECORD resource descriptor; row-resource transport is deferred"
    );
    assert_no_checked_bundle(&report);
}

#[test]
fn rejects_malformed_client_resource_local_descriptors() {
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
    for (descriptor, source) in [
        (
            "",
            "CREATE SCHEMA ui; CREATE CLIENT FUNCTION ui.find(p_name TEXT) RETURNS TEXT IS \
                 LET rows std.data.Resource<> := std.data.resource(target => tasks.find, arguments => std.call.args(p_name => p_name)); \
                 BEGIN RETURN AWAIT rows; END;",
        ),
        (
            "not-a-type",
            "CREATE SCHEMA ui; CREATE CLIENT FUNCTION ui.find(p_name TEXT) RETURNS TEXT IS \
                 LET rows std.data.Resource<not-a-type> := std.data.resource(target => tasks.find, arguments => std.call.args(p_name => p_name)); \
                 BEGIN RETURN AWAIT rows; END;",
        ),
    ] {
        let report = check(&bundle([("resource.orna", source)]), &base);
        assert_eq!(report.diagnostics().len(), 1, "{descriptor:?}");
        assert_eq!(report.diagnostics()[0].code(), DiagnosticCode::TypeMismatch);
        assert_eq!(
            report.diagnostics()[0].message(),
            "CLIENT local rows must declare std.data.Resource<T> or std.data.StreamResource<T>"
        );
        assert_no_checked_bundle(&report);
    }
}

#[test]
fn rejects_client_resource_descriptor_beyond_type_depth() {
    let text = format!(
        "std.data.Resource<TEXT{}>",
        "?".repeat(ClientResourceTypeParser::MAX_TYPE_DEPTH + 1),
    );
    let source = SourceSlice {
        span: SourceSpan {
            start: 0,
            end: text.len(),
        },
        text,
    };

    assert!(super::super::client_local_resource_type(&source).is_none());
}

#[test]
fn accepts_inline_table_resource_descriptor_shape() {
    let text = "std.data.Resource<TABLE (task_id UUID, title TEXT)>";
    let source = orna_syntax::SourceSlice {
        text: text.to_owned(),
        span: SourceSpan {
            start: 0,
            end: text.len(),
        },
    };
    let Some((kind, descriptor)) = super::super::client_local_resource_type(&source) else {
        panic!("inline table resource descriptor should parse");
    };
    assert_eq!(kind, orna_artifact::client_plan::ResourceKind::Scalar);
    assert!(descriptor.is_none());
}

#[test]
fn rejects_await_nested_in_non_suspending_expression() {
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
    let source = "CREATE SCHEMA ui; \
            CREATE CLIENT FUNCTION ui.wrap(p_value TEXT) RETURNS TEXT AS p_value; \
            CREATE CLIENT FUNCTION ui.find(p_name TEXT) RETURNS TEXT AS \
            ui.wrap(AWAIT std.data.resource(target => tasks.find, arguments => std.call.args(p_name => p_name))) || 'x';";
    let report = check(&bundle([("resource.orna", source)]), &base);
    assert_eq!(report.diagnostics().len(), 1, "{:?}", report.diagnostics());
    let diagnostic = &report.diagnostics()[0];
    assert_eq!(diagnostic.code(), DiagnosticCode::UnexpectedToken);
    assert_eq!(diagnostic.location().logical_path(), "resource.orna");
    let await_start = source.find("AWAIT").expect("await expression is present");
    assert_eq!(diagnostic.location().span().start(), await_start);
    assert_eq!(
        diagnostic.location().span().end(),
        await_start + "AWAIT".len()
    );
    assert_no_checked_bundle(&report);
}

#[test]
fn accepts_resource_constructor_arguments_in_reverse_named_order_and_derives_result_type() {
    let integer = ResolvedType::Scalar(StandardScalar::Integer);
    let server_target_id = FunctionId::from_bytes([0x81; 16]);
    let base = catalogue(
        vec![schema(0x82, &["tasks"])],
        Vec::new(),
        vec![FunctionDefinition::new(
            server_target_id,
            QualifiedSemanticName::new(["tasks", "find"]).unwrap(),
            FunctionDomain::Server,
            Vec::new(),
            FunctionReturn::Single(integer),
            FunctionRevisionId::from_bytes([0x83; 16]),
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::ReadOnly),
            FunctionVolatility::Stable,
        )],
    );
    let source = "CREATE SCHEMA ui; CREATE CLIENT FUNCTION ui.find() RETURNS INTEGER IS \
            BEGIN RETURN AWAIT std.data.resource(arguments => std.call.args(), target => tasks.find); END;";
    let report = check(&bundle([("resource-order.orna", source)]), &base);
    assert!(
        report.diagnostics().is_empty(),
        "{:#?}",
        report.diagnostics()
    );

    let function = &report.checked_bundle().unwrap().client_functions()[0];
    let CheckedClientFunctionBody::Expression { expression } = function.body() else {
        panic!("resource body must be a checked expression");
    };
    let CheckedClientExpression::Await { expression, .. } = expression else {
        panic!("resource body must await the constructor");
    };
    let CheckedClientExpression::Resource { operation } = expression.as_ref() else {
        panic!("await operand must be a resource constructor");
    };
    assert_eq!(
        operation.kind(),
        orna_artifact::client_plan::ResourceKind::Scalar
    );
    assert_eq!(
        operation.target(),
        super::super::CheckedFunctionId::Existing(server_target_id)
    );
    assert_eq!(
        operation.result_type(),
        SemanticType::Scalar(StandardScalar::Integer)
    );
    assert_eq!(
        function.return_type(),
        SemanticType::Scalar(StandardScalar::Integer)
    );
}

#[test]
fn rejects_resource_constructor_duplicate_missing_and_client_targets() {
    let integer = ResolvedType::Scalar(StandardScalar::Integer);
    let server_target_id = FunctionId::from_bytes([0x84; 16]);
    let client_target_id = FunctionId::from_bytes([0x85; 16]);
    let base = catalogue(
        vec![schema(0x86, &["tasks"])],
        Vec::new(),
        vec![
            FunctionDefinition::new(
                server_target_id,
                QualifiedSemanticName::new(["tasks", "find"]).unwrap(),
                FunctionDomain::Server,
                vec![parameter(0x89, "p_value", 0, integer)],
                FunctionReturn::Single(integer),
                FunctionRevisionId::from_bytes([0x87; 16]),
                FunctionSecurity::Invoker,
                Some(FunctionTransaction::ReadOnly),
                FunctionVolatility::Stable,
            ),
            FunctionDefinition::new(
                client_target_id,
                QualifiedSemanticName::new(["tasks", "client_find"]).unwrap(),
                FunctionDomain::Client,
                Vec::new(),
                FunctionReturn::Single(integer),
                FunctionRevisionId::from_bytes([0x88; 16]),
                FunctionSecurity::Invoker,
                None,
                FunctionVolatility::Immutable,
            ),
        ],
    );
    let cases = [
        (
            "duplicate constructor argument",
            "std.data.resource(target => tasks.find, target => tasks.find)",
            DiagnosticCode::DuplicateDefinition,
            "duplicate resource constructor argument",
        ),
        (
            "missing constructor argument",
            "std.data.resource(target => tasks.find)",
            DiagnosticCode::TypeMismatch,
            "resource constructor requires exactly one target and one arguments value",
        ),
        (
            "single abbreviated positional constructor argument",
            "std.data.resource(tasks.find)",
            DiagnosticCode::TypeMismatch,
            "resource constructor requires exactly one target and one arguments value",
        ),
        (
            "positional constructor arguments",
            "std.data.resource(tasks.find, std.call.args())",
            DiagnosticCode::TypeMismatch,
            "resource constructor arguments must be named target and arguments",
        ),
        (
            "mixed positional and named constructor arguments",
            "std.data.resource(tasks.find, arguments => std.call.args())",
            DiagnosticCode::TypeMismatch,
            "resource constructor arguments must be named target and arguments",
        ),
        (
            "CLIENT resource target",
            "std.data.resource(target => tasks.client_find, arguments => std.call.args())",
            DiagnosticCode::DomainIncompatible,
            "resource target tasks.client_find must be a SERVER function",
        ),
        (
            "unknown resource argument name",
            "std.data.resource(target => tasks.find, arguments => std.call.args(p_unknown => 7))",
            DiagnosticCode::UnknownQualifiedName,
            "unknown SERVER resource parameter p_unknown",
        ),
        (
            "trailing positional resource argument",
            "std.data.resource(target => tasks.find, arguments => std.call.args(7, 8))",
            DiagnosticCode::TypeMismatch,
            "too many arguments for SERVER resource target tasks.find",
        ),
        (
            "mistyped resource argument value",
            "std.data.resource(target => tasks.find, arguments => std.call.args(p_value => TRUE))",
            DiagnosticCode::TypeMismatch,
            "resource argument does not match SERVER parameter p_value",
        ),
    ];

    for (label, constructor, code, message) in cases {
        let source = format!(
            "CREATE SCHEMA ui; CREATE CLIENT FUNCTION ui.find() RETURNS INTEGER IS BEGIN RETURN AWAIT {constructor}; END;"
        );
        let report = check(
            &SourceBundle::new([SourceUnit::new("resource-rejections.orna", source)]).unwrap(),
            &base,
        );
        assert_eq!(
            report.diagnostics().len(),
            1,
            "{label}: {:?}",
            report.diagnostics()
        );
        assert_eq!(report.diagnostics()[0].code(), code, "{label}");
        assert_eq!(report.diagnostics()[0].message(), message, "{label}");
        assert_no_checked_bundle(&report);
    }
}
