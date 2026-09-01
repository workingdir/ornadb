//! Client function and procedural body parser tests.

use super::*;
#[test]
fn parses_client_boolean_constants_losslessly_with_exact_spans() {
    let source = "CREATE CLIENT FUNCTION examples.enabled()\n\
            RETURNS BOOLEAN\n\
            RETURN TRUE;\n\
            CREATE cLiEnT fUnCtIoN \"Examples\".\"Disabled\"() RETURNS BOOL RETURN fAlSe;";
    let parsed = parse(source);

    assert!(parsed.diagnostics().is_empty());
    assert_eq!(parsed.syntax().text(), source);
    assert!(parsed.server_functions().is_empty());
    assert_eq!(parsed.client_functions().len(), 2);

    let enabled = &parsed.client_functions()[0];
    assert_eq!(enabled.name.parts[0].text, "examples");
    assert_eq!(enabled.parameters.len(), 0);
    assert_eq!(enabled.span.start, 0);
    assert_eq!(
        enabled.span.end,
        source.find(';').expect("first terminator") + 1
    );
    let enabled_literal = enabled.body.as_boolean_literal().expect("Boolean body");
    assert!(enabled_literal.0);
    assert_eq!(enabled_literal.1.text, "TRUE");
    let enabled_start = source.find("TRUE").expect("TRUE literal");
    assert_eq!(
        enabled_literal.1.span,
        SourceSpan {
            start: enabled_start,
            end: enabled_start + 4,
        }
    );

    let disabled = &parsed.client_functions()[1];
    assert_eq!(disabled.name.parts[0].text, "\"Examples\"");
    assert_eq!(disabled.name.parts[1].text, "\"Disabled\"");
    let disabled_literal = disabled.body.as_boolean_literal().expect("Boolean body");
    assert!(!disabled_literal.0);
    assert_eq!(disabled_literal.1.text, "fAlSe");
    let disabled_start = source.find("fAlSe").expect("FALSE literal");
    assert_eq!(
        disabled_literal.1.span,
        SourceSpan {
            start: disabled_start,
            end: disabled_start + 5,
        }
    );
}

#[test]
fn parses_short_client_return_expressions_without_broadening_the_closed_surface() {
    let source = "CREATE CLIENT FUNCTION examples.ui() RETURNS UI RETURN std.ui.text('Example');\n\
            CREATE CLIENT FUNCTION examples.text() RETURNS TEXT RETURN 'ready';";
    let parsed = parse(source);

    assert!(
        parsed.diagnostics().is_empty(),
        "{:?}",
        parsed.diagnostics()
    );
    assert_eq!(parsed.client_functions().len(), 2);
    let ui = &parsed.client_functions()[0];
    let ClientFunctionBody::ReturnExpression { expression } = &ui.body else {
        panic!("expected a short RETURN expression body");
    };
    assert!(matches!(
        expression,
        ClientExpression::Call { callee, arguments, .. }
            if callee.parts.iter().map(|part| part.text.as_str()).eq(["std", "ui", "text"])
                && arguments.len() == 1
    ));
    let text = &parsed.client_functions()[1];
    assert!(matches!(
        text.body.as_expression(),
        Some(ClientExpression::StringLiteral { value, .. }) if value == "ready"
    ));
}

#[test]
fn parses_short_client_return_await_with_exact_expression_spans() {
    let source =
        "CREATE CLIENT FUNCTION examples.awaited() RETURNS TEXT RETURN AWAIT std.data.resource();";
    let parsed = parse(source);

    assert!(
        parsed.diagnostics().is_empty(),
        "{:?}",
        parsed.diagnostics()
    );
    assert_eq!(parsed.syntax().text(), source);
    let ClientFunctionBody::ReturnExpression { expression } = &parsed.client_functions()[0].body
    else {
        panic!("expected a short RETURN expression body");
    };
    let ClientExpression::Await {
        expression: awaited,
        span,
    } = expression
    else {
        panic!("expected an AWAIT expression");
    };
    let await_start = source.find("AWAIT").expect("AWAIT keyword");
    let expression_end = source.rfind(");").expect("resource call terminator") + 1;
    assert_eq!(
        span,
        &SourceSpan {
            start: await_start,
            end: expression_end,
        }
    );
    assert_eq!(&source[span.start..span.end], "AWAIT std.data.resource()");
    let ClientExpression::Call {
        callee,
        span: resource_span,
        ..
    } = awaited.as_ref()
    else {
        panic!("expected AWAIT to wrap a resource call expression");
    };
    let resource_start = source.find("std.data.resource").expect("resource callee");
    assert_eq!(
        resource_span,
        &SourceSpan {
            start: resource_start,
            end: expression_end,
        }
    );
    assert_eq!(
        &source[resource_span.start..resource_span.end],
        "std.data.resource()"
    );
    assert_eq!(
        callee.span,
        SourceSpan {
            start: resource_start,
            end: resource_start + "std.data.resource".len(),
        }
    );
}

#[test]
fn parses_canonical_accepted_dogfood_fixtures_losslessly() {
    let fixtures = [
        (
            "client_function_dogfood.orna",
            include_str!("../../../orna-server/tests/fixtures/client_function_dogfood.orna"),
        ),
        (
            "scalar_resource_dogfood.orna",
            include_str!("../../../orna-server/tests/fixtures/scalar_resource_dogfood.orna"),
        ),
        (
            "stream_resource_dogfood.orna",
            include_str!("../../../orna-server/tests/fixtures/stream_resource_dogfood.orna"),
        ),
        (
            "action_dogfood.orna",
            include_str!("../../../orna-server/tests/fixtures/action_dogfood.orna"),
        ),
        (
            "client_inspector_dogfood.orna",
            include_str!("../../../orna-server/tests/fixtures/client_inspector_dogfood.orna"),
        ),
        (
            "expression_client_dogfood.orna",
            include_str!("../../../orna-server/tests/fixtures/expression_client_dogfood.orna"),
        ),
        (
            "server_function_dogfood.orna",
            include_str!("../../../orna-server/tests/fixtures/server_function_dogfood.orna"),
        ),
        (
            "client_local_assignment_dogfood.orna",
            include_str!(
                "../../../orna-server/tests/fixtures/client_local_assignment_dogfood.orna"
            ),
        ),
    ];

    for (name, source) in fixtures {
        let parsed = parse(source);
        assert!(
            parsed.diagnostics().is_empty(),
            "{name}: {:?}",
            parsed.diagnostics()
        );
        assert_eq!(parsed.syntax().text(), source, "{name}");
    }
}

#[test]
fn parses_accepted_client_fixture_losslessly_with_expression_and_state_bodies() {
    let source = include_str!("../../testdata/accepted-client.orna");
    let parsed = parse(source);

    assert!(
        parsed.diagnostics().is_empty(),
        "{:?}",
        parsed.diagnostics()
    );
    assert_eq!(parsed.syntax().text(), source);
    assert_eq!(parsed.schemas().len(), 1);
    assert_eq!(parsed.schemas()[0].name.parts[0].text, "accepted_client");
    assert_eq!(parsed.client_functions().len(), 2);

    let expression = &parsed.client_functions()[0];
    assert_eq!(expression.name.parts[0].text, "accepted_client");
    assert_eq!(expression.name.parts[1].text, "enabled");
    let ClientFunctionBody::Expression { expression } = &expression.body else {
        panic!("expected an expression CLIENT body");
    };
    assert!(matches!(
        expression,
        ClientExpression::BooleanLiteral { value: true, .. }
    ));

    let stateful = &parsed.client_functions()[1];
    assert_eq!(stateful.name.parts[0].text, "accepted_client");
    assert_eq!(stateful.name.parts[1].text, "stateful");
    let ClientFunctionBody::StateBlock(block) = &stateful.body else {
        panic!("expected a state CLIENT body");
    };
    assert_eq!(block.states.len(), 1);
    assert!(block.locals.is_empty());
    assert!(block.statements.is_empty());
    let state = &block.states[0];
    assert_eq!(state.name.text, "ready");
    assert!(matches!(
        &state.type_specification,
        TypeSpecification::Named(name) if name.parts[0].text == "BOOLEAN"
    ));
    assert_eq!(state.scope, StateScope::Local);
    assert!(matches!(
        &state.default,
        StateDefault::Expression(ClientExpression::BooleanLiteral { value: true, .. })
    ));
    assert!(matches!(
        block.return_expression.as_ref(),
        Some(ClientExpression::BooleanLiteral { value: true, .. })
    ));
}

#[test]
fn retains_client_parameters_and_non_boolean_return_types_for_semantic_checks() {
    let source = "CREATE CLIENT FUNCTION examples.with_parameter(p_value TEXT) RETURNS BOOLEAN RETURN TRUE;\n\
            CREATE CLIENT FUNCTION examples.ui() RETURNS UI RETURN FALSE;\n\
            CREATE CLIENT FUNCTION examples.text() RETURNS TEXT RETURN TRUE;";
    let parsed = parse(source);

    assert!(parsed.diagnostics().is_empty());
    assert_eq!(parsed.client_functions().len(), 3);
    let with_parameter = &parsed.client_functions()[0];
    assert_eq!(with_parameter.parameters.len(), 1);
    assert_eq!(with_parameter.parameters[0].name.text, "p_value");
    assert_eq!(with_parameter.parameters[0].order, 0);
    assert_eq!(
        with_parameter.parameters[0].span.start,
        source.find("p_value").unwrap()
    );
    let parameter_list = "(p_value TEXT)";
    let parameter_list_start = source.find(parameter_list).unwrap();
    assert_eq!(
        with_parameter.parameter_list_span,
        SourceSpan {
            start: parameter_list_start,
            end: parameter_list_start + parameter_list.len(),
        }
    );
    let empty_parameter_list_start = source.find("examples.ui()").unwrap() + "examples.ui".len();
    assert_eq!(
        parsed.client_functions()[1].parameter_list_span,
        SourceSpan {
            start: empty_parameter_list_start,
            end: empty_parameter_list_start + 2,
        }
    );
    assert!(matches!(
        &parsed.client_functions()[1].return_type,
        FunctionReturnType::Single(TypeSpecification::Named(name))
            if name.parts[0].text == "UI"
    ));
    assert!(matches!(
        &parsed.client_functions()[2].return_type,
        FunctionReturnType::Single(TypeSpecification::Named(name))
            if name.parts[0].text == "TEXT"
    ));
}

#[test]
fn reports_closed_client_body_diagnostics_with_exact_public_messages() {
    let cases = [
        (
            "CREATE CLIENT FUNCTION examples.security() RETURNS BOOLEAN SECURITY INVOKER RETURN TRUE;",
            "CLIENT functions use RETURN before their result value",
            "SECURITY",
        ),
        (
            "CREATE CLIENT FUNCTION examples.transaction() RETURNS BOOLEAN TRANSACTION READ ONLY RETURN TRUE;",
            "CLIENT functions use RETURN before their result value",
            "TRANSACTION",
        ),
        (
            "CREATE CLIENT FUNCTION examples.volatility() RETURNS BOOLEAN VOLATILITY IMMUTABLE RETURN TRUE;",
            "CLIENT functions use RETURN before their result value",
            "VOLATILITY",
        ),
        (
            "CREATE CLIENT FUNCTION examples.table_result() RETURNS TABLE (value BOOLEAN) RETURN TRUE;",
            "CLIENT functions must name one return type after RETURNS",
            "TABLE",
        ),
        (
            "CREATE CLIENT FUNCTION examples.set_result() RETURNS SET OF BOOLEAN RETURN TRUE;",
            "CLIENT functions must name one return type after RETURNS",
            "SET",
        ),
        (
            "CREATE CLIENT FUNCTION examples.missing_type() RETURNS ;",
            "CLIENT functions must name one return type after RETURNS",
            ";",
        ),
        (
            "CREATE CLIENT FUNCTION examples.extra() RETURNS BOOLEAN RETURN TRUE FALSE;",
            "expected ';' after CLIENT function body",
            "FALSE",
        ),
        (
            "CREATE CLIENT FUNCTION examples.missing_semicolon() RETURNS BOOLEAN RETURN TRUE",
            "expected ';' after CLIENT function body",
            "",
        ),
    ];

    for (source, message, marker) in cases {
        let parsed = parse(source);
        assert!(parsed.client_functions().is_empty(), "source: {source}");
        assert_eq!(parsed.diagnostics().len(), 1, "source: {source}");
        let diagnostic = &parsed.diagnostics()[0];
        assert_eq!(diagnostic.code, "ORNA0001");
        assert_eq!(diagnostic.message, message);
        if marker.is_empty() {
            assert_eq!(diagnostic.span.start, source.len());
            assert_eq!(diagnostic.span.end, source.len());
        } else {
            let start = source.find(marker).expect("diagnostic marker");
            assert_eq!(diagnostic.span.start, start);
            assert_eq!(diagnostic.span.end, start + marker.len());
        }
    }
}

#[test]
fn parses_client_expression_bodies_and_external_contracts_with_exact_spans() {
    let source = "CREATE CLIENT FUNCTION examples.greeting(p_name TEXT)\n\
            RETURNS TEXT\n\
            AS std.strings.concat('Hello ', p_name);\n\
            CREATE CLIENT FUNCTION examples.qualified() RETURNS BOOLEAN AS TRUE;\n\
            CREATE EXTERNAL CLIENT FUNCTION std.ui.window (\n\
                title TEXT,\n\
                content std.ui.UI\n\
            )\n\
            RETURNS std.ui.UI\n\
            RUNTIME CONTRACT 'std.ui.window@1';";
    let parsed = parse(source);

    assert!(
        parsed.diagnostics().is_empty(),
        "{:?}",
        parsed.diagnostics()
    );
    assert_eq!(parsed.syntax().text(), source);
    assert_eq!(parsed.client_functions().len(), 3);

    let greeting = &parsed.client_functions()[0];
    assert!(!greeting.external);
    assert_eq!(greeting.runtime_contract, None);
    assert_eq!(greeting.parameters.len(), 1);
    let ClientFunctionBody::Expression { expression } = &greeting.body else {
        panic!("expected an expression body");
    };
    let ClientExpression::Call {
        callee,
        arguments,
        span,
    } = expression
    else {
        panic!("expected a call expression");
    };
    assert_eq!(callee.parts.len(), 3);
    assert_eq!(callee.parts[0].text, "std");
    assert_eq!(callee.parts[2].text, "concat");
    assert_eq!(arguments.len(), 2);
    assert_eq!(arguments[0].name, None);
    let ClientExpression::StringLiteral { value, .. } = &arguments[0].value else {
        panic!("expected a string literal argument");
    };
    assert_eq!(value, "Hello ");
    assert_eq!(
        arguments[1].name.as_ref().map(|name| name.text.as_str()),
        None
    );
    let ClientExpression::ParameterRead { parameter } = &arguments[1].value else {
        panic!("expected a parameter read argument");
    };
    assert_eq!(parameter.text, "p_name");
    assert_eq!(span.start, source.find("std.strings").expect("callee"));

    let qualified = &parsed.client_functions()[1];
    let ClientFunctionBody::Expression { expression } = &qualified.body else {
        panic!("expected an expression body");
    };
    let ClientExpression::BooleanLiteral { value, .. } = expression else {
        panic!("expected a boolean literal expression");
    };
    assert!(*value);

    let external = &parsed.client_functions()[2];
    assert!(external.external);
    let contract = external
        .runtime_contract
        .as_ref()
        .expect("external functions carry a contract");
    assert_eq!(contract.text, "'std.ui.window@1'");
    let contract_start = source.find("'std.ui.window@1'").expect("contract");
    assert_eq!(
        contract.span,
        SourceSpan {
            start: contract_start,
            end: contract_start + "'std.ui.window@1'".len(),
        }
    );
    let ClientFunctionBody::ExternalContract { identity } = &external.body else {
        panic!("expected an external-contract body");
    };
    assert_eq!(identity.text, "'std.ui.window@1'");
}

#[test]
fn parses_external_contract_with_capability_clause_in_source_order() {
    let source = "CREATE EXTERNAL CLIENT FUNCTION std.net.connect (\n\
            p_host TEXT\n\
        )\n\
        RETURNS BOOLEAN\n\
        RUNTIME CONTRACT 'std.net.connect@1'\n\
        REQUIRES CAPABILITY std.net.connect(p_host);";
    let parsed = parse(source);

    assert!(
        parsed.diagnostics().is_empty(),
        "{:?}",
        parsed.diagnostics()
    );
    assert_eq!(parsed.syntax().text(), source);
    let function = &parsed.client_functions()[0];
    assert!(function.external);
    assert_eq!(function.capabilities.len(), 1);
    let contract = function
        .runtime_contract
        .as_ref()
        .expect("external functions carry a contract");
    assert_eq!(contract.text, "'std.net.connect@1'");
    let ClientFunctionBody::ExternalContract { identity } = &function.body else {
        panic!("expected an external-contract body");
    };
    assert_eq!(identity.text, "'std.net.connect@1'");
}

#[test]
fn parses_client_concat_and_field_path_expressions() {
    let source = "CREATE CLIENT FUNCTION examples.label(p_item REF app.item)\n\
            RETURNS TEXT\n\
            AS p_item.name || ' #' || p_item.code;";
    let parsed = parse(source);

    assert!(
        parsed.diagnostics().is_empty(),
        "{:?}",
        parsed.diagnostics()
    );
    assert_eq!(parsed.syntax().text(), source);
    let function = &parsed.client_functions()[0];
    let ClientFunctionBody::Expression { expression } = &function.body else {
        panic!("expected an expression body");
    };
    let ClientExpression::Concat {
        left: outer_left,
        right: outer_right,
        ..
    } = expression
    else {
        panic!("expected a concatenation");
    };
    let ClientExpression::Concat { left, right, .. } = outer_left.as_ref() else {
        panic!("expected a left-nested concatenation");
    };
    let ClientExpression::FieldPath { root, members, .. } = left.as_ref() else {
        panic!("expected a field path on the left");
    };
    assert_eq!(root.text, "p_item");
    assert_eq!(members.len(), 1);
    assert_eq!(members[0].text, "name");
    let ClientExpression::StringLiteral { value, .. } = right.as_ref() else {
        panic!("expected the literal in the middle");
    };
    assert_eq!(value, " #");
    let ClientExpression::FieldPath { root, members, .. } = outer_right.as_ref() else {
        panic!("expected a field path on the right");
    };
    assert_eq!(root.text, "p_item");
    assert_eq!(members.len(), 1);
    assert_eq!(members[0].text, "code");
}

#[test]
fn parses_client_action_call_with_named_target_arguments_and_exact_spans() {
    let source = "CREATE CLIENT FUNCTION app.owner() RETURNS std.Action AS\n\
            std.action.call(\n\
                target => std.invoke.echo,\n\
                arguments => std.call.args(p_value => app.first())\n\
            );";
    let parsed = parse(source);

    assert!(
        parsed.diagnostics().is_empty(),
        "{:?}",
        parsed.diagnostics()
    );
    let ClientFunctionBody::Expression { expression } = &parsed.client_functions()[0].body else {
        panic!("expected an expression body");
    };
    let ClientExpression::Call {
        callee,
        arguments,
        span,
    } = expression
    else {
        panic!("expected std.action.call expression");
    };

    let action_start = source.find("std.action.call").expect("action callee");
    let action_end = source.rfind(')').expect("action closing parenthesis") + 1;
    assert_eq!(
        callee
            .parts
            .iter()
            .map(|part| part.text.as_str())
            .collect::<Vec<_>>(),
        ["std", "action", "call"]
    );
    assert_eq!(
        callee.span,
        SourceSpan {
            start: action_start,
            end: action_start + "std.action.call".len(),
        }
    );
    assert_eq!(
        span,
        &SourceSpan {
            start: action_start,
            end: action_end
        }
    );
    assert_eq!(arguments.len(), 2);

    let target_start = source.find("target").expect("target argument");
    let target_value_start = source.find("std.invoke.echo").expect("target value");
    let target_argument = &arguments[0];
    let target_name = target_argument.name.as_ref().expect("named target");
    assert_eq!(target_name.text, "target");
    assert_eq!(
        target_name.span,
        SourceSpan {
            start: target_start,
            end: target_start + "target".len(),
        }
    );
    assert_eq!(
        target_argument.span,
        SourceSpan {
            start: target_start,
            end: target_value_start + "std.invoke.echo".len(),
        }
    );
    let ClientExpression::FieldPath {
        root,
        members,
        span: target_span,
    } = &target_argument.value
    else {
        panic!("expected a qualified target");
    };
    assert_eq!(target_span.start, target_value_start);
    assert_eq!(
        target_span.end,
        target_value_start + "std.invoke.echo".len()
    );
    assert_eq!(root.text, "std");
    assert_eq!(
        members
            .iter()
            .map(|member| member.text.as_str())
            .collect::<Vec<_>>(),
        ["invoke", "echo"]
    );

    let arguments_start = source.find("arguments").expect("arguments argument");
    let nested_start = source
        .find("std.call.args")
        .expect("nested arguments callee");
    let nested_end = source[nested_start..]
        .find("))")
        .expect("nested arguments closing parenthesis")
        + nested_start
        + 2;
    let arguments_argument = &arguments[1];
    let arguments_name = arguments_argument.name.as_ref().expect("named arguments");
    assert_eq!(arguments_name.text, "arguments");
    assert_eq!(
        arguments_name.span,
        SourceSpan {
            start: arguments_start,
            end: arguments_start + "arguments".len(),
        }
    );
    assert_eq!(
        arguments_argument.span,
        SourceSpan {
            start: arguments_start,
            end: nested_end,
        }
    );
    let ClientExpression::Call {
        callee: nested_callee,
        arguments: nested_arguments,
        span: nested_span,
    } = &arguments_argument.value
    else {
        panic!("expected std.call.args expression");
    };
    assert_eq!(
        nested_callee
            .parts
            .iter()
            .map(|part| part.text.as_str())
            .collect::<Vec<_>>(),
        ["std", "call", "args"]
    );
    assert_eq!(
        nested_callee.span,
        SourceSpan {
            start: nested_start,
            end: nested_start + "std.call.args".len(),
        }
    );
    assert_eq!(
        nested_span,
        &SourceSpan {
            start: nested_start,
            end: nested_end
        }
    );
    assert_eq!(nested_arguments.len(), 1);

    let pair_start = source
        .find("p_value => app.first()")
        .expect("nested argument");
    let pair_value_start = pair_start + "p_value => ".len();
    let pair_value_end = pair_start + "p_value => app.first()".len();
    let nested_argument = &nested_arguments[0];
    let nested_name = nested_argument
        .name
        .as_ref()
        .expect("named nested argument");
    assert_eq!(nested_name.text, "p_value");
    assert_eq!(
        nested_name.span,
        SourceSpan {
            start: pair_start,
            end: pair_start + "p_value".len(),
        }
    );
    assert_eq!(
        nested_argument.span,
        SourceSpan {
            start: pair_start,
            end: pair_value_end,
        }
    );
    let ClientExpression::Call {
        callee: target_call_callee,
        span: target_call_span,
        ..
    } = &nested_argument.value
    else {
        panic!("expected nested target argument call");
    };
    assert_eq!(
        target_call_callee
            .parts
            .iter()
            .map(|part| part.text.as_str())
            .collect::<Vec<_>>(),
        ["app", "first"]
    );
    assert_eq!(
        target_call_span,
        &SourceSpan {
            start: pair_value_start,
            end: pair_value_end,
        }
    );
}
#[test]
fn rejects_trailing_commas_in_client_calls_without_partial_declarations() {
    let cases = [
        (
            "CREATE CLIENT FUNCTION examples.trailing() RETURNS TEXT AS app.first('ready',);",
            "'ready',)",
        ),
        (
            "CREATE CLIENT FUNCTION examples.trailing_resource() RETURNS TEXT AS\n\
                    std.data.resource(\n\
                        target => tasks.get,\n\
                        arguments => std.call.args(p_value => p_value,)\n\
                    );",
            "p_value,)",
        ),
    ];

    for (source, trailing_marker) in cases {
        let parsed = parse(source);
        assert_eq!(parsed.syntax().text(), source, "{trailing_marker}");
        assert!(
            parsed.client_functions().is_empty(),
            "{trailing_marker}: unexpected declaration"
        );
        assert_eq!(
            parsed.diagnostics().len(),
            1,
            "{trailing_marker}: {:?}",
            parsed.diagnostics()
        );
        let diagnostic = &parsed.diagnostics()[0];
        assert_eq!(diagnostic.code, "ORNA0001");
        assert_eq!(diagnostic.message, "expected a CLIENT expression");
        let close_start = source
            .find(trailing_marker)
            .expect("trailing comma marker")
            + trailing_marker.len()
            - 1;
        assert_eq!(
            diagnostic.span,
            SourceSpan {
                start: close_start,
                end: close_start + 1,
            }
        );
    }
}

#[test]
fn parses_client_await_expression_losslessly_with_complete_span() {
    let source = "CREATE CLIENT FUNCTION examples.awaited(p_value TEXT) RETURNS TEXT IS\n\
            BEGIN\n\
                RETURN AWAIT /* preserve */ std.data.resource(\n\
                    target => tasks.get,\n\
                    arguments => std.call.args(p_value => p_value)\n\
                );\n\
            END;";
    let parsed = parse(source);

    assert!(
        parsed.diagnostics().is_empty(),
        "{:?}",
        parsed.diagnostics()
    );
    assert_eq!(parsed.syntax().text(), source);
    assert_eq!(
        parsed
            .syntax()
            .root()
            .descendants()
            .filter(|node| node.kind() == SyntaxKind::ClientAwaitExpression)
            .count(),
        1
    );
    let ClientFunctionBody::StateBlock(block) = &parsed.client_functions()[0].body else {
        panic!("expected a procedural body");
    };
    let expression = block
        .return_expression
        .as_ref()
        .expect("expected a return expression");
    let ClientExpression::Await {
        expression: awaited,
        span,
    } = expression
    else {
        panic!("expected an AWAIT expression");
    };
    let expected_start = source.find("AWAIT").expect("AWAIT keyword");
    let expected_end = source[expected_start..]
        .find(");")
        .expect("resource statement terminator")
        + expected_start
        + 1;
    assert_eq!(
        span,
        &SourceSpan {
            start: expected_start,
            end: expected_end,
        }
    );
    let resource_start = source.find("std.data.resource").expect("resource callee");
    let resource_end = source.rfind(')').expect("resource closing parenthesis") + 1;
    let ClientExpression::Call {
        callee: resource_callee,
        arguments: resource_arguments,
        span: resource_span,
    } = awaited.as_ref()
    else {
        panic!("expected AWAIT to wrap a resource call expression");
    };
    assert_eq!(
        resource_span,
        &SourceSpan {
            start: resource_start,
            end: resource_end,
        }
    );
    assert_eq!(
        resource_callee.span,
        SourceSpan {
            start: resource_start,
            end: resource_start + "std.data.resource".len(),
        }
    );
    assert_eq!(
        resource_callee
            .parts
            .iter()
            .map(|part| part.text.as_str())
            .collect::<Vec<_>>(),
        ["std", "data", "resource"]
    );

    let target_start = source.find("target").expect("target argument");
    let target_name = resource_arguments[0]
        .name
        .as_ref()
        .expect("named target argument");
    assert_eq!(target_name.text, "target");
    assert_eq!(
        target_name.span,
        SourceSpan {
            start: target_start,
            end: target_start + "target".len(),
        }
    );
    let target_value_start = source.find("tasks.get").expect("resource target");
    assert_eq!(
        resource_arguments[0].span,
        SourceSpan {
            start: target_start,
            end: target_value_start + "tasks.get".len(),
        }
    );
    let ClientExpression::FieldPath {
        root: target_root,
        members: target_members,
        span: target_span,
    } = &resource_arguments[0].value
    else {
        panic!("expected a qualified target name");
    };
    assert_eq!(
        target_span,
        &SourceSpan {
            start: target_value_start,
            end: target_value_start + "tasks.get".len(),
        }
    );
    assert_eq!(target_root.text, "tasks");
    assert_eq!(
        target_root.span,
        SourceSpan {
            start: target_value_start,
            end: target_value_start + "tasks".len(),
        }
    );
    assert_eq!(target_members.len(), 1);
    assert_eq!(target_members[0].text, "get");
    assert_eq!(
        target_members[0].span,
        SourceSpan {
            start: target_value_start + "tasks.".len(),
            end: target_value_start + "tasks.get".len(),
        }
    );

    let arguments_start = source.find("arguments").expect("arguments argument");
    let arguments_name = resource_arguments[1]
        .name
        .as_ref()
        .expect("named arguments argument");
    assert_eq!(arguments_name.text, "arguments");
    assert_eq!(
        arguments_name.span,
        SourceSpan {
            start: arguments_start,
            end: arguments_start + "arguments".len(),
        }
    );
    let nested_start = source.find("std.call.args").expect("arguments call");
    let nested_end = source[nested_start..]
        .find(')')
        .expect("arguments closing parenthesis")
        + nested_start
        + 1;
    assert_eq!(
        resource_arguments[1].span,
        SourceSpan {
            start: arguments_start,
            end: nested_end,
        }
    );
    let ClientExpression::Call {
        callee: arguments_callee,
        arguments: nested_arguments,
        span: arguments_span,
    } = &resource_arguments[1].value
    else {
        panic!("expected std.call.args expression");
    };
    assert_eq!(
        arguments_span,
        &SourceSpan {
            start: nested_start,
            end: nested_end,
        }
    );
    assert_eq!(
        arguments_callee.span,
        SourceSpan {
            start: nested_start,
            end: nested_start + "std.call.args".len(),
        }
    );
    assert_eq!(
        arguments_callee
            .parts
            .iter()
            .map(|part| part.text.as_str())
            .collect::<Vec<_>>(),
        ["std", "call", "args"]
    );
    assert_eq!(nested_arguments.len(), 1);

    let nested_argument = &nested_arguments[0];
    let pair_start = source
        .find("p_value => p_value")
        .expect("nested named argument");
    let pair_value_start = pair_start + "p_value => ".len();
    let pair_value_end = pair_start + "p_value => p_value".len();
    let pair_name = nested_argument.name.as_ref().expect("nested argument name");
    assert_eq!(pair_name.text, "p_value");
    assert_eq!(
        pair_name.span,
        SourceSpan {
            start: pair_start,
            end: pair_start + "p_value".len(),
        }
    );
    assert_eq!(
        nested_argument.span,
        SourceSpan {
            start: pair_start,
            end: pair_value_end,
        }
    );
    let ClientExpression::ParameterRead { parameter } = &nested_argument.value else {
        panic!("expected the nested argument value to read p_value");
    };
    assert_eq!(parameter.text, "p_value");
    assert_eq!(
        parameter.span,
        SourceSpan {
            start: pair_value_start,
            end: pair_value_end,
        }
    );
}
#[test]
fn rejects_client_await_in_state_declaration_positions() {
    let source = "CREATE CLIENT FUNCTION examples.awaited() RETURNS TEXT IS\n\
            STATE value TEXT DEFAULT AWAIT std.data.resource();\n\
        BEGIN\n\
            RETURN value;\n\
        END;";
    let parsed = parse(source);

    assert_eq!(parsed.syntax().text(), source);
    assert!(parsed.client_functions().is_empty());
    assert!(
        parsed
            .diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.code == "ORNA0001"),
        "expected an ORNA0001 diagnostic, got {:?}",
        parsed.diagnostics()
    );
}

#[test]
fn parses_client_local_resource_binding_and_await_return() {
    let source = "CREATE CLIENT FUNCTION studio.overdue_rows(p_owner REF studio.owner)\n\
            RETURNS TEXT IS\n\
            LET rows std.data.Resource<TABLE(task_id UUID, title TEXT)> :=\n\
                std.data.resource(\n\
                    target => tasks.overdue,\n\
                    arguments => std.call.args(p_owner => p_owner)\n\
                );\n\
            BEGIN\n\
                RETURN AWAIT rows;\n\
            END;";
    let parsed = parse(source);

    assert!(
        parsed.diagnostics().is_empty(),
        "{:?}",
        parsed.diagnostics()
    );
    assert_eq!(parsed.syntax().text(), source);
    assert_eq!(
        parsed
            .syntax()
            .root()
            .descendants()
            .filter(|node| node.kind() == SyntaxKind::ClientLocalBinding)
            .count(),
        1
    );
    let ClientFunctionBody::StateBlock(block) = &parsed.client_functions()[0].body else {
        panic!("expected a procedural CLIENT block");
    };
    assert!(block.states.is_empty());
    assert_eq!(block.locals.len(), 1);
    let local = &block.locals[0];
    assert_eq!(local.name.text, "rows");
    assert_eq!(
        local.type_source.text,
        "std.data.Resource<TABLE(task_id UUID, title TEXT)>"
    );
    assert_eq!(
        local.type_source.span,
        SourceSpan {
            start: source.find("std.data.Resource").expect("local type"),
            end: source[..source.find(":=").expect("initializer marker")]
                .trim_end()
                .len(),
        }
    );
    let ClientExpression::Call { callee, .. } = &local.expression else {
        panic!("expected a resource constructor call");
    };
    assert_eq!(
        callee
            .parts
            .iter()
            .map(|part| part.text.as_str())
            .collect::<Vec<_>>(),
        ["std", "data", "resource"]
    );
    let Some(ClientExpression::Await { expression, .. }) = block.return_expression.as_ref() else {
        panic!("expected an AWAIT return expression");
    };
    assert!(matches!(
        expression.as_ref(),
        ClientExpression::LocalRead { local } if local.text == "rows"
    ));
}

#[test]
fn parses_post_begin_client_procedural_statements_losslessly() {
    let source = "CREATE CLIENT FUNCTION examples.procedural() RETURNS INTEGER IS\n\
            BEGIN\n\
                LET x std.data.Resource<INTEGER> := AWAIT std.data.resource();\n\
                x := AWAIT std.data.resource();\n\
                RETURN x;\n\
            END;";
    let parsed = parse(source);

    assert!(
        parsed.diagnostics().is_empty(),
        "{:?}",
        parsed.diagnostics()
    );
    assert_eq!(parsed.syntax().text(), source);
    let ClientFunctionBody::StateBlock(block) = &parsed.client_functions()[0].body else {
        panic!("expected a procedural CLIENT block");
    };
    assert!(block.states.is_empty());
    assert!(block.locals.is_empty());
    assert_eq!(block.statements.len(), 2);
    let ClientProceduralStatement::Let(let_statement) = &block.statements[0] else {
        panic!("expected a procedural LET statement");
    };
    assert_eq!(let_statement.name.text, "x");
    assert_eq!(
        let_statement
            .type_source
            .as_ref()
            .map(|source| source.text.as_str()),
        Some("std.data.Resource<INTEGER>")
    );
    assert!(matches!(
        let_statement.expression,
        ClientExpression::Await { .. }
    ));
    let ClientProceduralStatement::Assignment(assignment) = &block.statements[1] else {
        panic!("expected a procedural assignment statement");
    };
    assert_eq!(assignment.target.text, "x");
    assert!(matches!(
        assignment.expression,
        ClientExpression::Await { .. }
    ));
    assert!(matches!(
        block.return_expression,
        Some(ClientExpression::LocalRead { .. })
    ));
    assert_eq!(
        &source[let_statement.span.start..let_statement.span.end],
        "LET x std.data.Resource<INTEGER> := AWAIT std.data.resource();"
    );
    assert_eq!(
        &source[assignment.span.start..assignment.span.end],
        "x := AWAIT std.data.resource();"
    );
}

#[test]
fn parses_untyped_post_begin_await_let_and_local_read_return() {
    let source = "CREATE CLIENT FUNCTION examples.untyped_procedural() RETURNS INTEGER IS\n\
            BEGIN\n\
                LET value := AWAIT std.data.resource();\n\
                RETURN value;\n\
            END;";
    let parsed = parse(source);

    assert!(
        parsed.diagnostics().is_empty(),
        "{:?}",
        parsed.diagnostics()
    );
    assert_eq!(parsed.syntax().text(), source);
    let ClientFunctionBody::StateBlock(block) = &parsed.client_functions()[0].body else {
        panic!("expected a procedural CLIENT block");
    };
    assert_eq!(block.statements.len(), 1);
    let ClientProceduralStatement::Let(let_statement) = &block.statements[0] else {
        panic!("expected a procedural LET statement");
    };
    assert_eq!(let_statement.name.text, "value");
    assert!(let_statement.type_source.is_none());
    assert!(matches!(
        let_statement.expression,
        ClientExpression::Await { .. }
    ));
    assert!(matches!(
        &block.return_expression,
        Some(ClientExpression::LocalRead { local }) if local.text == "value"
    ));
}

#[test]
fn rejects_client_await_in_expression_bodies() {
    let source = "CREATE CLIENT FUNCTION examples.awaited() RETURNS TEXT AS\n\
            AWAIT std.data.resource();";
    let parsed = parse(source);

    assert_eq!(parsed.syntax().text(), source);
    assert!(parsed.client_functions().is_empty());
    assert_eq!(parsed.diagnostics().len(), 1, "{:?}", parsed.diagnostics());
    let diagnostic = &parsed.diagnostics()[0];
    assert_eq!(diagnostic.code, "ORNA0001");
    let await_start = source.find("AWAIT").expect("AWAIT keyword");
    assert_eq!(
        diagnostic.span,
        SourceSpan {
            start: await_start,
            end: await_start + "AWAIT".len(),
        }
    );
}

#[test]
fn reports_malformed_client_await_operands_without_widening_expression_syntax() {
    for expression in ["AWAIT;", "AWAIT (value);", "AWAIT AWAIT;"] {
        let source =
            format!("CREATE CLIENT FUNCTION examples.awaited() RETURNS TEXT AS {expression}");
        let parsed = parse(&source);

        assert_eq!(parsed.syntax().text(), source);
        assert!(parsed.client_functions().is_empty(), "{expression:?}");
        assert!(
            parsed
                .diagnostics()
                .iter()
                .any(|diagnostic| diagnostic.message == "expected a CLIENT expression"),
            "{expression:?}: {:?}",
            parsed.diagnostics()
        );
    }
}
#[test]
fn rejects_await_in_non_suspending_contexts_with_lossless_later_recovery() {
    let cases = [
        (
            "CREATE CLIENT FUNCTION examples.awaited() RETURNS TEXT AS \
             std.outer(AWAIT std.data.resource());\n",
            "nested call argument",
        ),
        (
            "CREATE CLIENT FUNCTION examples.awaited() RETURNS TEXT IS\n\
                 STATE value TEXT DEFAULT AWAIT std.data.resource();\n\
             BEGIN\n\
                 RETURN value;\n\
             END;\n",
            "state declaration default",
        ),
        (
            "CREATE CLIENT FUNCTION examples.awaited() RETURNS TEXT IS\n\
                 STATE value TEXT;\n\
             BEGIN\n\
                 RETURN AWAIT std.data.resource();\n\
             END;\n",
            "state block return",
        ),
    ];

    for (invalid, context) in cases {
        let source =
            format!("{invalid}CREATE CLIENT FUNCTION examples.recovered() RETURNS TEXT AS 'ok';");
        let parsed = parse(&source);

        assert_eq!(parsed.syntax().text(), source, "{context}");
        assert_eq!(parsed.client_functions().len(), 1, "{context}");
        assert_eq!(
            parsed.client_functions()[0].name.parts[1].text,
            "recovered",
            "{context}"
        );
        assert!(
            parsed
                .diagnostics()
                .iter()
                .any(|diagnostic| diagnostic.code == "ORNA0001"),
            "{context}: expected an ORNA0001 diagnostic, got {:?}",
            parsed.diagnostics()
        );
        assert_eq!(
            parsed
                .syntax()
                .root()
                .descendants()
                .filter(|node| node.kind() == SyntaxKind::ClientAwaitExpression)
                .count(),
            0,
            "{context}: forbidden AWAIT must not produce an Await AST node"
        );
    }
}

#[test]
fn rejects_client_expression_trailing_dots() {
    for expression in ["p.", "p_item.name."] {
        let source = format!(
            "CREATE CLIENT FUNCTION examples.read(p_item REF app.item) \
                 RETURNS TEXT AS {expression};"
        );
        let parsed = parse(&source);

        assert!(
            parsed
                .diagnostics()
                .iter()
                .any(|diagnostic| diagnostic.message
                    == "expected an identifier after a CLIENT expression dot"),
            "{expression:?}: {:?}",
            parsed.diagnostics()
        );
    }
}

#[test]
fn parses_client_function_capability_clauses_with_exact_names_arguments_and_spans() {
    let source = "CREATE CLIENT FUNCTION examples.hash_file(p_file std.fs.Path)\n\
            RETURNS BYTES\n\
            REQUIRES CAPABILITY std.fs.read(p_file), std.fs.write(p_file), std.net.call, std.secret.use()\n\
            RETURN TRUE;\n\
            CREATE CLIENT FUNCTION examples.bare() RETURNS BOOLEAN RETURN FALSE;";
    let parsed = parse(source);

    assert!(parsed.diagnostics().is_empty());
    assert_eq!(parsed.syntax().text(), source);
    assert_eq!(parsed.client_functions().len(), 2);

    let hash_file = &parsed.client_functions()[0];
    let capabilities = &hash_file.capabilities;
    assert_eq!(capabilities.len(), 4);
    assert_eq!(capabilities[0].name.parts[0].text, "std");
    assert_eq!(capabilities[0].name.parts[1].text, "fs");
    assert_eq!(capabilities[0].name.parts[2].text, "read");
    assert_eq!(
        capabilities[0]
            .arguments
            .as_ref()
            .map(|arguments| arguments.text.as_str()),
        Some("p_file"),
    );
    let read_clause = "std.fs.read(p_file)";
    let read_clause_start = source.find(read_clause).expect("read clause");
    assert_eq!(
        capabilities[0].span,
        SourceSpan {
            start: read_clause_start,
            end: read_clause_start + read_clause.len(),
        }
    );
    let read_arguments = capabilities[0].arguments.as_ref().expect("read arguments");
    assert_eq!(
        read_arguments.span,
        SourceSpan {
            start: read_clause_start + "std.fs.read(".len(),
            end: read_clause_start + "std.fs.read(p_file".len(),
        }
    );
    assert_eq!(
        capabilities[1]
            .arguments
            .as_ref()
            .map(|arguments| arguments.text.as_str()),
        Some("p_file"),
    );
    assert!(capabilities[2].arguments.is_none());
    assert_eq!(
        capabilities[3]
            .arguments
            .as_ref()
            .map(|arguments| arguments.text.as_str()),
        Some(""),
    );
    let bare = &parsed.client_functions()[1];
    assert!(bare.capabilities.is_empty());
}

#[test]
fn rejects_malformed_client_function_capability_clauses() {
    let cases = [
        (
            "CREATE CLIENT FUNCTION examples.bad() RETURNS BOOLEAN REQUIRES CAPABILITY RETURN TRUE;",
            "expected a capability after REQUIRES CAPABILITY",
            "RETURN",
        ),
        (
            "CREATE CLIENT FUNCTION examples.bad() RETURNS BOOLEAN REQUIRES CAPABILITY std.fs.read, RETURN TRUE;",
            "trailing commas are not allowed in capability requirements",
            "RETURN",
        ),
        (
            "CREATE CLIENT FUNCTION examples.bad() RETURNS BOOLEAN REQUIRES CAPABILITY std.fs.read REQUIRES CAPABILITY std.fs.write RETURN TRUE;",
            "expected ',' or a body keyword after a capability requirement",
            "REQUIRES",
        ),
        (
            "CREATE CLIENT FUNCTION examples.bad() RETURNS BOOLEAN REQUIRES CAPABILITY std.fs.read(p_file RETURN TRUE;",
            "expected ')' to close capability arguments",
            ";",
        ),
        (
            "CREATE CLIENT FUNCTION examples.is_form() RETURNS BOOLEAN REQUIRES CAPABILITY std.fs.read(p_file);",
            "CLIENT functions use RETURN before their result value",
            ";",
        ),
    ];

    for (source, message, marker) in cases {
        let parsed = parse(source);
        assert!(parsed.client_functions().is_empty(), "source: {source}");
        assert_eq!(parsed.diagnostics().len(), 1, "source: {source}");
        let diagnostic = &parsed.diagnostics()[0];
        assert_eq!(diagnostic.code, "ORNA0001");
        assert_eq!(diagnostic.message, message);
        let start = source
            .match_indices(marker)
            .last()
            .map(|(index, _)| index)
            .expect("diagnostic marker");
        assert_eq!(diagnostic.span.start, start);
        assert_eq!(diagnostic.span.end, start + marker.len());
    }
}

#[test]
fn recovers_after_client_function_errors_to_all_later_declarations() {
    let source = "CREATE CLIENT FUNCTION examples.bad() RETURNS BOOLEAN SECURITY INVOKER RETURN TRUE;\n\
            CREATE SCHEMA later;\n\
            CREATE TYPE later.item AS OBJECT (name TEXT);\n\
            CREATE SERVER FUNCTION later.server() RETURNS ROWS (value BOOL) AS SELECT t.value FROM later.item t;\n\
            CREATE CLIENT FUNCTION later.good() RETURNS BOOLEAN RETURN FALSE;";
    let parsed = parse(source);

    assert_eq!(parsed.diagnostics().len(), 1);
    assert_eq!(
        parsed.diagnostics()[0].message,
        "CLIENT functions use RETURN before their result value"
    );
    let rejected_form = source.find("SECURITY").expect("rejected CLIENT body form");
    assert_eq!(
        parsed.diagnostics()[0].span,
        SourceSpan {
            start: rejected_form,
            end: rejected_form + 8,
        }
    );
    assert_eq!(parsed.schemas().len(), 1);
    assert_eq!(parsed.object_types().len(), 1);
    assert_eq!(parsed.server_functions().len(), 1);
    assert_eq!(parsed.client_functions().len(), 1);
    assert_eq!(parsed.client_functions()[0].name.parts[0].text, "later");
    assert_eq!(parsed.client_functions()[0].name.parts[1].text, "good");
}

#[test]
fn parses_client_state_blocks_with_scopes_defaults_and_single_return() {
    let source = "CREATE CLIENT FUNCTION studio.connections()\n\
            RETURNS TEXT\n\
            IS\n\
                STATE filter TEXT SCOPE LOCAL DEFAULT '';\n\
                STATE selected TEXT SCOPE SESSION DEFAULT NULL;\n\
                STATE count INTEGER SCOPE USER;\n\
            BEGIN\n\
                RETURN filter || selected;\n\
            END;";
    let parsed = parse(source);

    assert!(
        parsed.diagnostics().is_empty(),
        "{:?}",
        parsed.diagnostics()
    );
    assert_eq!(parsed.syntax().text(), source);
    assert_eq!(parsed.client_functions().len(), 1);
    let function = &parsed.client_functions()[0];
    let ClientFunctionBody::StateBlock(block) = &function.body else {
        panic!("expected a state block body");
    };
    assert_eq!(block.states.len(), 3);

    let filter = &block.states[0];
    assert_eq!(filter.name.text, "filter");
    assert_eq!(filter.scope, StateScope::Local);
    assert!(matches!(
        filter.default,
        StateDefault::Expression(ClientExpression::StringLiteral { .. })
    ));
    assert!(matches!(
        &filter.type_specification,
        TypeSpecification::Named(name) if name.parts[0].text == "TEXT"
    ));

    let selected = &block.states[1];
    assert_eq!(selected.name.text, "selected");
    assert_eq!(selected.scope, StateScope::Session);
    // `DEFAULT NULL` represents an explicit null initial value.
    assert!(matches!(selected.default, StateDefault::Null));

    let count = &block.states[2];
    assert_eq!(count.name.text, "count");
    assert_eq!(count.scope, StateScope::User);
    assert!(matches!(count.default, StateDefault::Unset));

    let ClientExpression::Concat { .. } =
        block.return_expression.as_ref().expect("return expression")
    else {
        panic!("expected a concatenation return expression");
    };

    let filter_start = source.find("STATE filter").expect("filter declaration");
    let filter_end = source.find("'';").expect("filter terminator") + "'';".len();
    assert_eq!(
        filter.span,
        SourceSpan {
            start: filter_start,
            end: filter_end,
        }
    );
    let block_start = source.find("IS").expect("IS keyword");
    let block_end = source.find("END").expect("END keyword") + "END".len();
    assert_eq!(
        block.span,
        SourceSpan {
            start: block_start,
            end: block_end,
        }
    );
}

#[test]
fn parses_client_state_blocks_with_bare_return_and_omitted_clauses() {
    let source = "CREATE CLIENT FUNCTION examples.reset() RETURNS BOOLEAN IS BEGIN RETURN; END;\n\
            CREATE CLIENT FUNCTION examples.touched() RETURNS TEXT IS\n\
                STATE stamp TEXT;\n\
            BEGIN\n\
                RETURN stamp;\n\
            END;";
    let parsed = parse(source);

    assert!(
        parsed.diagnostics().is_empty(),
        "{:?}",
        parsed.diagnostics()
    );
    assert_eq!(parsed.syntax().text(), source);
    assert_eq!(parsed.client_functions().len(), 2);

    let reset = &parsed.client_functions()[0];
    assert!(matches!(
        &reset.body,
        ClientFunctionBody::StateBlock(block) if block.states.is_empty()
    ));
    assert!(
        reset
            .body
            .procedural_statements()
            .is_some_and(|statements| { statements.is_empty() })
    );
    assert!(reset.body.as_expression().is_none());
    assert!(reset.body.as_boolean_literal().is_none());

    let touched = &parsed.client_functions()[1];
    let ClientFunctionBody::StateBlock(block) = &touched.body else {
        panic!("expected a state block body");
    };
    assert_eq!(block.states.len(), 1);
    let stamp = &block.states[0];
    assert_eq!(stamp.name.text, "stamp");
    assert_eq!(stamp.scope, StateScope::Local);
    assert!(matches!(stamp.default, StateDefault::Unset));
    let ClientExpression::ParameterRead { parameter } =
        block.return_expression.as_ref().expect("return expression")
    else {
        panic!("expected a parameter read return expression");
    };
    assert_eq!(parameter.text, "stamp");
}

#[test]
fn keeps_duplicate_state_names_for_the_compiler_to_reject() {
    let source = "CREATE CLIENT FUNCTION examples.dup() RETURNS TEXT IS\n\
            STATE stamp TEXT;\n\
            STATE stamp TEXT;\n\
        BEGIN\n\
            RETURN stamp;\n\
        END;";
    let parsed = parse(source);

    assert!(
        parsed.diagnostics().is_empty(),
        "{:?}",
        parsed.diagnostics()
    );
    let ClientFunctionBody::StateBlock(block) = &parsed.client_functions()[0].body else {
        panic!("expected a state block body");
    };
    assert_eq!(block.states.len(), 2);
    assert_eq!(block.states[0].name.text, "stamp");
    assert_eq!(block.states[1].name.text, "stamp");
}

#[test]
fn rejects_malformed_and_unsupported_procedural_statements_in_client_state_blocks() {
    let cases = [
        (
            "CREATE CLIENT FUNCTION examples.bad() RETURNS BOOLEAN IS LET x = 1; BEGIN RETURN TRUE; END;",
            "CLIENT local bindings require a declared type and ':=' initializer",
            "=",
        ),
        (
            "CREATE CLIENT FUNCTION examples.bad() RETURNS BOOLEAN IS STATE count TEXT; BEGIN RETURN 1; RETURN 2; END;",
            "CLIENT blocks accept only a single RETURN statement",
            "RETURN 2",
        ),
        (
            "CREATE CLIENT FUNCTION examples.bad() RETURNS BOOLEAN IS STATE count TEXT; BEGIN END;",
            "CLIENT state blocks accept only a single RETURN statement",
            "END",
        ),
        (
            "CREATE CLIENT FUNCTION examples.bad() RETURNS BOOLEAN IS BEGIN IF x THEN RETURN TRUE; END;",
            "expected keyword IF",
            "END",
        ),
    ];

    for (source, message, marker) in cases {
        let parsed = parse(source);
        assert!(parsed.client_functions().is_empty(), "source: {source}");
        let diagnostic = parsed
            .diagnostics()
            .iter()
            .find(|diagnostic| diagnostic.message == message)
            .unwrap_or_else(|| panic!("source: {source}: {:?}", parsed.diagnostics()));
        assert_eq!(diagnostic.code, "ORNA0001");
        let start = source.find(marker).expect("diagnostic marker");
        assert_eq!(diagnostic.span.start, start, "source: {source}");
        // The diagnostic names the offending keyword token, which is
        // the first word of the marker.
        let token = marker.split_whitespace().next().expect("marker token");
        assert_eq!(diagnostic.span.end, start + token.len(), "source: {source}");
    }
}

#[test]
fn accepts_multiple_no_state_returns_and_trivia_before_block_terminators() {
    let source = "CREATE CLIENT FUNCTION examples.control() RETURNS INTEGER IS\n\
            BEGIN\n\
                IF TRUE THEN\n\
                    RETURN 1;\n\
                END -- conditional terminator\n\
                IF -- keyword and semicolon trivia\n\
                ;\n\
                RETURN 2;\n\
                RETURN 3;\n\
            END;";
    let parsed = parse(source);

    assert!(
        parsed.diagnostics().is_empty(),
        "{:?}",
        parsed.diagnostics()
    );
    let ClientFunctionBody::StateBlock(block) = &parsed.client_functions()[0].body else {
        panic!("expected a procedural body");
    };
    assert_eq!(block.statements.len(), 2);
    assert!(matches!(
        block.statements[0],
        ClientProceduralStatement::If(_)
    ));
    assert!(matches!(
        block.statements[1],
        ClientProceduralStatement::Return(_)
    ));
    assert!(matches!(
        block.return_expression,
        Some(ClientExpression::IntegerLiteral { value: 3, .. })
    ));
}

#[test]
fn keeps_server_and_client_function_reports_separate() {
    let source = "CREATE SERVER FUNCTION examples.server() RETURNS ROWS (value BOOL) AS SELECT t.value FROM examples.item t;\n\
            CREATE CLIENT FUNCTION examples.client() RETURNS BOOLEAN RETURN TRUE;";
    let parsed = parse(source);

    assert!(parsed.diagnostics().is_empty());
    assert_eq!(parsed.server_functions().len(), 1);
    assert_eq!(parsed.server_functions()[0].name.parts[1].text, "server");
    assert_eq!(parsed.client_functions().len(), 1);
    assert_eq!(parsed.client_functions()[0].name.parts[1].text, "client");
}

#[test]
fn client_return_type_diagnostics_do_not_change_server_parsing() {
    let source = "CREATE SERVER FUNCTION tasks.bad() RETURNS RETURN TRUE;";
    let parsed = parse(source);

    assert!(parsed.server_functions().is_empty());
    assert!(parsed.client_functions().is_empty());
    assert_eq!(parsed.diagnostics().len(), 1);
    assert_eq!(parsed.diagnostics()[0].code, "ORNA0001");
    assert_eq!(parsed.diagnostics()[0].message, "expected keyword AS");
    let start = source.find("TRUE").expect("offending SERVER body token");
    assert_eq!(
        parsed.diagnostics()[0].span,
        SourceSpan {
            start,
            end: start + 4,
        }
    );
}
