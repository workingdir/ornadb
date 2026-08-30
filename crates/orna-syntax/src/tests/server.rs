//! Server function, SQL body, and DML parser tests.

use super::*;
#[test]
fn parses_server_functions_with_rows_returns_and_select_bodies() {
    let source = "CREATE SERVER FUNCTION tasks.overdue (\n\
            p_principal REF sys.security.principal DEFAULT sys.security.session_principal(),\n\
            p_before TIMESTAMP DEFAULT tasks.window(sys.time.now(), sys.time.plus(1, 2))\n\
        )\n\
        RETURNS ROWS (\n\
            task REF tasks.task,\n\
            title TEXT\n\
        )\n\
        SECURITY INVOKER\n\
        TRANSACTION READ ONLY\n\
        VOLATILITY STABLE\n\
        AS SELECT REF(t), t.title, t.assignee.name FROM tasks.task t WHERE t.completed = FALSE ORDER BY t.due_at;";
    let parsed = parse(source);

    assert!(parsed.diagnostics().is_empty());
    assert_eq!(parsed.syntax().text(), source);
    assert_eq!(parsed.server_functions().len(), 1);

    let function = &parsed.server_functions()[0];
    assert_eq!(function.name.parts[0].text, "tasks");
    assert_eq!(function.name.parts[1].text, "overdue");
    assert_eq!(function.parameters.len(), 2);
    assert_eq!(function.parameters[0].name.text, "p_principal");
    assert_eq!(function.parameters[0].order, 0);
    assert_reference_type(
        &function.parameters[0].type_specification,
        "sys",
        "security",
        "principal",
    );
    assert_eq!(
        function.parameters[0]
            .default_expression
            .as_ref()
            .map(|expression| expression.text.as_str()),
        Some("sys.security.session_principal()"),
    );
    assert_eq!(function.parameters[1].name.text, "p_before");
    assert_eq!(function.parameters[1].order, 1);
    assert_named_type(&function.parameters[1].type_specification, "TIMESTAMP");
    assert_eq!(
        function.parameters[1]
            .default_expression
            .as_ref()
            .map(|expression| expression.text.as_str()),
        Some("tasks.window(sys.time.now(), sys.time.plus(1, 2))"),
    );

    match &function.return_type {
        FunctionReturnType::Rows { columns, .. } => {
            assert_eq!(columns.len(), 2);
            assert_eq!(columns[0].name.text, "task");
            assert_eq!(columns[0].order, 0);
            assert_reference_type(&columns[0].type_specification, "tasks", "task", "");
            assert_eq!(columns[1].name.text, "title");
            assert_eq!(columns[1].order, 1);
            assert_named_type(&columns[1].type_specification, "TEXT");
        }
        FunctionReturnType::Single(_) | FunctionReturnType::Stream { .. } => {
            panic!("tasks.overdue must return rows")
        }
    }
    assert_eq!(function.security, Some(FunctionSecurity::Invoker));
    assert_eq!(function.transaction, Some(FunctionTransaction::ReadOnly));
    assert_eq!(function.volatility, Some(FunctionVolatility::Stable));
    match &function.body {
        ServerFunctionBody::SqlQuery(query) => {
            assert_eq!(
                query.source.text,
                "SELECT REF(t), t.title, t.assignee.name FROM tasks.task t WHERE t.completed = FALSE ORDER BY t.due_at",
            );
            assert_eq!(
                query.source.span.start,
                source.find("SELECT").expect("query exists")
            );
            assert_eq!(query.query.projections.len(), 3);
            assert!(matches!(
                query.query.projections[0],
                QueryExpression::ObjectReference { .. }
            ));
            match &query.query.projections[2] {
                QueryExpression::FieldPath { root, members, .. } => {
                    assert_eq!(root.text, "t");
                    assert_eq!(members[0].text, "assignee");
                    assert_eq!(members[1].text, "name");
                }
                _ => panic!("third projection must be a field path"),
            }
            assert!(matches!(
                query.query.predicate,
                Some(QueryExpression::Equality { .. })
            ));
            assert_eq!(query.query.ordering.len(), 1);
            assert_eq!(
                query.query.ordering[0].direction,
                OrderingDirection::Unspecified
            );
            assert_eq!(
                query.query.ordering[0].null_order,
                NullOrdering::Unspecified
            );
        }
        ServerFunctionBody::SqlInsert(_)
        | ServerFunctionBody::SqlUpdate(_)
        | ServerFunctionBody::SqlDelete(_)
        | ServerFunctionBody::NoInputParameterSelect(_) => {
            panic!("tasks.overdue must use a SELECT body")
        }
    }
}

#[test]
fn parses_distinct_losslessly_with_quoted_source_and_type_neutral_syntax() {
    let source = "CREATE SERVER FUNCTION tasks.values() RETURNS ROWS (value TEXT) \
            AS SELECT DiStInCt \"item\".\"value\" FROM \"tasks\".\"item\" AS \"item\";";
    let parsed = parse(source);

    assert!(parsed.diagnostics().is_empty());
    assert_eq!(parsed.syntax().text(), source);
    let ServerFunctionBody::SqlQuery(query) = &parsed.server_functions()[0].body else {
        panic!("DISTINCT source must parse as a SELECT query");
    };
    let SelectQuantifier::Distinct { source: distinct } = &query.query.quantifier else {
        panic!("query must retain DISTINCT instead of the implicit ALL form");
    };
    let distinct_start = source.find("DiStInCt").expect("DISTINCT exists");
    assert_eq!(distinct.text, "DiStInCt");
    assert_eq!(
        distinct.span,
        SourceSpan {
            start: distinct_start,
            end: distinct_start + "DiStInCt".len(),
        }
    );
    assert_eq!(
        query.source.text,
        "SELECT DiStInCt \"item\".\"value\" FROM \"tasks\".\"item\" AS \"item\""
    );
    assert_eq!(
        query.source.span.start,
        source.find("SELECT").expect("SELECT exists")
    );
    assert_eq!(
        query.query.source_object.alias.text, "\"item\"",
        "quoted aliases must remain lossless around DISTINCT"
    );
}

#[test]
fn select_without_distinct_retains_the_implicit_all_quantifier() {
    let source = "CREATE SERVER FUNCTION tasks.values() RETURNS ROWS (value INT) \
            AS SELECT item.value FROM tasks.item item;";
    let parsed = parse(source);

    assert!(parsed.diagnostics().is_empty());
    let ServerFunctionBody::SqlQuery(query) = &parsed.server_functions()[0].body else {
        panic!("ordinary SELECT source must parse as a query");
    };
    assert!(matches!(query.query.quantifier, SelectQuantifier::All));
}

#[test]
fn rejects_distinct_order_by_at_order_and_recovers_to_the_next_declaration() {
    let source = "CREATE SERVER FUNCTION tasks.bad() RETURNS ROWS (value INT) \
            AS SELECT DISTINCT item.value FROM tasks.item item ORDER BY item.value; \
            CREATE SCHEMA later;";
    let parsed = parse(source);

    assert_eq!(parsed.syntax().text(), source);
    assert_eq!(parsed.diagnostics().len(), 1);
    let diagnostic = &parsed.diagnostics()[0];
    assert_eq!(diagnostic.code, "ORNA0001");
    assert_eq!(
        diagnostic.message,
        "SELECT DISTINCT queries do not allow ORDER BY; remove the ORDER BY clause",
    );
    let order_start = source.find("ORDER BY").expect("ORDER exists");
    assert_eq!(
        diagnostic.span,
        SourceSpan {
            start: order_start,
            end: order_start + "ORDER".len(),
        }
    );
    assert!(parsed.server_functions().is_empty());
    assert_eq!(parsed.schemas().len(), 1);
    assert_eq!(parsed.schemas()[0].name.parts[0].text, "later");
}

#[test]
fn rejects_deferred_distinct_on_and_select_all_syntax() {
    let distinct_on = "CREATE SERVER FUNCTION tasks.bad() RETURNS ROWS (value INT) \
            AS SELECT DISTINCT ON (item.value) item.value FROM tasks.item item;";
    let parsed = parse(distinct_on);
    assert_eq!(parsed.diagnostics().len(), 1);
    assert_eq!(parsed.diagnostics()[0].code, "ORNA0001");
    assert_eq!(
        parsed.diagnostics()[0].message,
        "DISTINCT ON is not supported; use SELECT DISTINCT followed by the result columns",
    );
    let on_start = distinct_on.find("DISTINCT ON").expect("DISTINCT ON exists") + "DISTINCT ".len();
    assert_eq!(
        parsed.diagnostics()[0].span,
        SourceSpan {
            start: on_start,
            end: on_start + "ON".len(),
        }
    );

    let select_all = "CREATE SERVER FUNCTION tasks.bad() RETURNS ROWS (value INT) \
            AS SELECT ALL item.value FROM tasks.item item;";
    let parsed = parse(select_all);
    assert_eq!(parsed.diagnostics().len(), 1);
    assert_eq!(parsed.diagnostics()[0].code, "ORNA0001");
    assert_eq!(
        parsed.diagnostics()[0].message,
        "SELECT ALL is not supported; omit ALL to preserve duplicate rows",
    );
    let all_start = select_all.find("ALL").expect("ALL exists");
    assert_eq!(
        parsed.diagnostics()[0].span,
        SourceSpan {
            start: all_start,
            end: all_start + "ALL".len(),
        }
    );
}

#[test]
fn parses_single_return_types_and_all_server_execution_modifiers() {
    let source = "CREATE SERVER FUNCTION tasks.reopen()\n\
            RETURNS REF tasks.task\n\
            SECURITY DEFINER\n\
            TRANSACTION ATOMIC\n\
            VOLATILITY IMMUTABLE\n\
            AS SELECT REF(t) FROM tasks.task t;\n\
            CREATE SERVER FUNCTION tasks.audit()\n\
            RETURNS TEXT\n\
            TRANSACTION MANUAL\n\
            VOLATILITY VOLATILE\n\
            AS SELECT t.title FROM tasks.task t;";
    let parsed = parse(source);

    assert!(parsed.diagnostics().is_empty());
    assert_eq!(parsed.server_functions().len(), 2);
    match &parsed.server_functions()[0].return_type {
        FunctionReturnType::Single(type_specification) => {
            assert_reference_type(type_specification, "tasks", "task", "");
        }
        FunctionReturnType::Rows { .. } | FunctionReturnType::Stream { .. } => {
            panic!("tasks.reopen must return one reference")
        }
    }
    assert_eq!(
        parsed.server_functions()[0].security,
        Some(FunctionSecurity::Definer),
    );
    assert_eq!(
        parsed.server_functions()[0].transaction,
        Some(FunctionTransaction::Atomic),
    );
    assert_eq!(
        parsed.server_functions()[0].volatility,
        Some(FunctionVolatility::Immutable),
    );
    assert_eq!(parsed.server_functions()[1].security, None);
    assert_eq!(
        parsed.server_functions()[1].transaction,
        Some(FunctionTransaction::Manual),
    );
    assert_eq!(
        parsed.server_functions()[1].volatility,
        Some(FunctionVolatility::Volatile),
    );
}

#[test]
fn parses_server_function_capabilities_after_execution_modifiers() {
    let source = "CREATE SERVER FUNCTION security.rotate_key(p_key TEXT)\n\
            RETURNS BOOL\n\
            SECURITY DEFINER\n\
            TRANSACTION ATOMIC\n\
            VOLATILITY VOLATILE\n\
            REQUIRES CAPABILITY sys.secret.read(p_key, audit(sys.time.now(), p_actor)),\n\
                std.net.call(\n\
                    endpoint => p_endpoint,\n\
                    metadata => trace(request(1, 2))\n\
                ),\n\
                sys.job.submit,\n\
                sys.job.noop()\n\
            AS SELECT t.completed FROM tasks.task t;";
    let parsed = parse(source);

    assert!(parsed.diagnostics().is_empty());
    assert_eq!(parsed.syntax().text(), source);

    let capabilities = &parsed.server_functions()[0].capabilities;
    assert_eq!(capabilities.len(), 4);
    assert_eq!(capabilities[0].name.parts[0].text, "sys");
    assert_eq!(capabilities[0].name.parts[1].text, "secret");
    assert_eq!(capabilities[0].name.parts[2].text, "read");
    assert_eq!(
        capabilities[0]
            .arguments
            .as_ref()
            .map(|arguments| arguments.text.as_str()),
        Some("p_key, audit(sys.time.now(), p_actor)"),
    );
    assert_eq!(
        capabilities[1]
            .arguments
            .as_ref()
            .map(|arguments| arguments.text.as_str()),
        Some("\nendpoint => p_endpoint,\nmetadata => trace(request(1, 2))\n"),
    );
    assert!(capabilities[2].arguments.is_none());
    assert_eq!(
        capabilities[3]
            .arguments
            .as_ref()
            .map(|arguments| arguments.text.as_str()),
        Some(""),
    );
    assert_eq!(
        capabilities[1]
            .arguments
            .as_ref()
            .expect("arguments exist")
            .span
            .start,
        source.find("\nendpoint").expect("arguments exist"),
    );
}

#[test]
fn rejects_malformed_server_function_capability_clauses() {
    let sources = [
        (
            "CREATE SERVER FUNCTION security.bad() RETURNS BOOL REQUIRES CAPABILITY AS SELECT TRUE;",
            "expected a capability",
        ),
        (
            "CREATE SERVER FUNCTION security.bad() RETURNS BOOL REQUIRES CAPABILITY sys.secret.read(), AS SELECT TRUE;",
            "trailing commas",
        ),
        (
            "CREATE SERVER FUNCTION security.bad() RETURNS BOOL REQUIRES CAPABILITY sys.secret.read(p_key AS SELECT TRUE;",
            "expected ')'",
        ),
    ];

    for (source, expected_message) in sources {
        let parsed = parse(source);

        assert_eq!(parsed.syntax().text(), source);
        assert!(parsed.server_functions().is_empty());
        assert!(
            parsed
                .diagnostics()
                .iter()
                .any(|diagnostic| diagnostic.code == "ORNA0001"),
        );
        assert!(
            parsed
                .diagnostics()
                .iter()
                .any(|diagnostic| diagnostic.message.contains(expected_message)),
        );
    }
}

#[test]
fn parses_canonical_multiword_scalar_types_in_every_type_position() {
    let source = "CREATE TYPE files.document AS OBJECT (body CHARACTER LARGE OBJECT, content BINARY LARGE OBJECT);\n\
            CREATE SERVER FUNCTION files.encode(input CHARACTER LARGE OBJECT)\n\
            RETURNS BINARY LARGE OBJECT\n\
            AS SELECT REF(d) FROM files.document d;\n\
            CREATE SERVER FUNCTION files.describe()\n\
            RETURNS ROWS (body CHARACTER LARGE OBJECT, content BINARY LARGE OBJECT)\n\
            AS SELECT REF(d) FROM files.document d;";
    let parsed = parse(source);

    assert!(parsed.diagnostics().is_empty());
    assert_eq!(parsed.syntax().text(), source);

    let fields = &parsed.object_types()[0].fields;
    assert_standard_large_object_type(
        &fields[0].type_specification,
        StandardLargeObjectKind::Character,
        "CHARACTER LARGE OBJECT",
    );
    assert_standard_large_object_type(
        &fields[1].type_specification,
        StandardLargeObjectKind::Binary,
        "BINARY LARGE OBJECT",
    );

    let encode = &parsed.server_functions()[0];
    assert_standard_large_object_type(
        &encode.parameters[0].type_specification,
        StandardLargeObjectKind::Character,
        "CHARACTER LARGE OBJECT",
    );
    match &encode.return_type {
        FunctionReturnType::Single(type_specification) => {
            assert_standard_large_object_type(
                type_specification,
                StandardLargeObjectKind::Binary,
                "BINARY LARGE OBJECT",
            );
        }
        FunctionReturnType::Rows { .. } | FunctionReturnType::Stream { .. } => {
            panic!("files.encode must return one value")
        }
    }

    let describe = &parsed.server_functions()[1];
    match &describe.return_type {
        FunctionReturnType::Rows { columns, .. } => {
            assert_standard_large_object_type(
                &columns[0].type_specification,
                StandardLargeObjectKind::Character,
                "CHARACTER LARGE OBJECT",
            );
            assert_standard_large_object_type(
                &columns[1].type_specification,
                StandardLargeObjectKind::Binary,
                "BINARY LARGE OBJECT",
            );
        }
        FunctionReturnType::Single(_) | FunctionReturnType::Stream { .. } => {
            panic!("files.describe must return rows")
        }
    }
}

#[test]
fn retains_exact_source_for_multiword_large_object_types() {
    let source = "CREATE TYPE files.document AS OBJECT (body cHaRaCtEr /* kept */ LaRgE ObJeCt);";
    let parsed = parse(source);

    assert!(parsed.diagnostics().is_empty());
    assert_eq!(parsed.syntax().text(), source);

    match &parsed.object_types()[0].fields[0].type_specification {
        TypeSpecification::StandardLargeObject { kind, source } => {
            assert_eq!(*kind, StandardLargeObjectKind::Character);
            assert_eq!(source.text, "cHaRaCtEr /* kept */ LaRgE ObJeCt");
        }
        _ => {
            panic!("body must use the standard large object AST form")
        }
    }
}

#[test]
fn parses_constructed_type_specifications_losslessly() {
    let source = "CREATE TYPE samples.container AS OBJECT (\
            listed LIST /* kept */ < TEXT >,\
            unique SET<REF tasks.task>,\
            indexed MAP<TEXT, OPTION<BOOL>>,\
            optional TEXT /* first */ ? /* second */ ?,\
            streamed STREAM<tasks.event>,\
            recursive REF LIST<TEXT>\
        );";
    let parsed = parse(source);

    assert!(parsed.diagnostics().is_empty());
    assert_eq!(parsed.syntax().text(), source);
    let fields = &parsed.object_types()[0].fields;

    let TypeSpecification::List { element, span } = &fields[0].type_specification else {
        panic!("listed must use LIST");
    };
    assert_eq!(&source[span.start..span.end], "LIST /* kept */ < TEXT >");
    assert_named_type(element, "TEXT");

    let TypeSpecification::Set { element, .. } = &fields[1].type_specification else {
        panic!("unique must use SET");
    };
    let TypeSpecification::Reference { target, .. } = element.as_ref() else {
        panic!("SET element must use REF");
    };
    assert_named_type(target, "tasks.task");

    let TypeSpecification::Map { key, value, .. } = &fields[2].type_specification else {
        panic!("indexed must use MAP");
    };
    assert_named_type(key, "TEXT");
    let TypeSpecification::Option {
        value,
        spelling: OptionTypeSpelling::Prefix,
        ..
    } = value.as_ref()
    else {
        panic!("MAP value must use prefix OPTION");
    };
    assert_named_type(value, "BOOL");

    let TypeSpecification::Option {
        value,
        spelling: OptionTypeSpelling::Postfix,
        span,
    } = &fields[3].type_specification
    else {
        panic!("optional must use postfix OPTION");
    };
    assert_eq!(
        &source[span.start..span.end],
        "TEXT /* first */ ? /* second */ ?"
    );
    let TypeSpecification::Option {
        value,
        spelling: OptionTypeSpelling::Postfix,
        ..
    } = value.as_ref()
    else {
        panic!("optional must retain both postfix markers");
    };
    assert_named_type(value, "TEXT");

    let TypeSpecification::Stream { element, .. } = &fields[4].type_specification else {
        panic!("streamed must use STREAM");
    };
    assert_named_type(element, "tasks.event");

    let TypeSpecification::Reference { target, .. } = &fields[5].type_specification else {
        panic!("recursive must use REF");
    };
    assert!(matches!(target.as_ref(), TypeSpecification::List { .. }));
}

#[test]
fn constructed_type_errors_recover_to_a_later_declaration() {
    let malformed = "CREATE TYPE samples.bad AS OBJECT (value MAP<TEXT OPTION<BOOL>>);\
            CREATE SCHEMA recovered;";
    let parsed = parse(malformed);

    assert_eq!(parsed.syntax().text(), malformed);
    assert!(parsed.object_types().is_empty());
    assert_eq!(parsed.schemas().len(), 1);
    assert_eq!(parsed.diagnostics().len(), 1);
    assert_eq!(
        parsed.diagnostics()[0].message,
        "expected ',' between MAP key and value types"
    );

    let nested = format!(
        "CREATE TYPE samples.deep AS OBJECT (value {}TEXT{});CREATE SCHEMA after_depth;",
        "OPTION<".repeat(33),
        ">".repeat(33)
    );
    let parsed = parse(&nested);
    assert_eq!(parsed.syntax().text(), nested);
    assert!(parsed.object_types().is_empty());
    assert_eq!(parsed.schemas().len(), 1);
    assert_eq!(parsed.diagnostics().len(), 1);
    assert_eq!(
        parsed.diagnostics()[0].message,
        "type specification exceeds the maximum depth of 32"
    );

    let mixed = format!(
        "CREATE TYPE samples.deep AS OBJECT (value LIST<TEXT{}>);CREATE SCHEMA after_mixed;",
        "?".repeat(32)
    );
    let parsed = parse(&mixed);
    assert_eq!(parsed.syntax().text(), mixed);
    assert!(parsed.object_types().is_empty());
    assert_eq!(parsed.schemas().len(), 1);
    assert_eq!(parsed.diagnostics().len(), 1);
    assert_eq!(
        parsed.diagnostics()[0].message,
        "type specification exceeds the maximum depth of 32"
    );
}

#[test]
fn every_constructed_type_delimiter_failure_is_direct_and_recoverable() {
    for (written_type, expected) in [
        ("LIST TEXT", "expected '<' after type constructor"),
        ("LIST<>", "expected a field type"),
        ("LIST<TEXT", "expected '>' to close type constructor"),
        ("MAP<, TEXT>", "expected a field type"),
        ("MAP<TEXT, >", "expected a field type"),
        (
            "MAP<TEXT TEXT>",
            "expected ',' between MAP key and value types",
        ),
    ] {
        let source = format!(
            "CREATE TYPE samples.bad AS OBJECT (value {written_type});CREATE SCHEMA recovered;"
        );
        let parsed = parse(&source);

        assert_eq!(parsed.syntax().text(), source, "{written_type}");
        assert!(parsed.object_types().is_empty(), "{written_type}");
        assert_eq!(parsed.schemas().len(), 1, "{written_type}");
        assert_eq!(
            parsed.diagnostics().len(),
            1,
            "{written_type}: {:?}",
            parsed.diagnostics()
        );
        assert_eq!(parsed.diagnostics()[0].message, expected, "{written_type}");
    }
}

#[test]
fn parses_stream_return_type_losslessly_with_complete_span() {
    let source = "CREATE SERVER FUNCTION tasks.events() RETURNS STREAM< /* kept */ REF tasks.event > AS SELECT REF(e) FROM tasks.event e;";
    let parsed = parse(source);

    assert!(parsed.diagnostics().is_empty());
    assert_eq!(parsed.syntax().text(), source);
    let FunctionReturnType::Stream { element, span } = &parsed.server_functions()[0].return_type
    else {
        panic!("events must return a stream");
    };
    assert_eq!(
        &source[span.start..span.end],
        "STREAM< /* kept */ REF tasks.event >"
    );
    assert_reference_type(element, "tasks", "event", "");
}

#[test]
fn malformed_stream_return_types_keep_source_and_recover() {
    for (written_type, expected) in [
        ("STREAM", "expected '<' after type constructor"),
        ("STREAM<>", "expected a field type"),
        ("STREAM<TEXT", "expected '>' to close type constructor"),
    ] {
        let source = format!(
            "CREATE SERVER FUNCTION tasks.bad() RETURNS {written_type} AS SELECT TRUE;CREATE SCHEMA recovered;"
        );
        let parsed = parse(&source);

        assert_eq!(parsed.syntax().text(), source, "{written_type}");
        assert!(parsed.server_functions().is_empty(), "{written_type}");
        assert_eq!(parsed.schemas().len(), 1, "{written_type}");
        assert_eq!(
            parsed.diagnostics().len(),
            1,
            "{written_type}: {:?}",
            parsed.diagnostics()
        );
        assert_eq!(parsed.diagnostics()[0].message, expected, "{written_type}");
    }
}

#[test]
fn rejects_legacy_table_and_set_of_return_declarations() {
    let source = "CREATE SERVER FUNCTION tasks.table_result() RETURNS TABLE (task REF tasks.task) AS SELECT REF(t) FROM tasks.task t;\n\
            CREATE SERVER FUNCTION tasks.set_result() RETURNS SET OF REF tasks.task AS SELECT REF(t) FROM tasks.task t;";
    let parsed = parse(source);

    assert_eq!(parsed.syntax().text(), source);
    assert!(parsed.server_functions().is_empty());
    assert_eq!(parsed.diagnostics().len(), 2);
    assert!(
        parsed
            .diagnostics()
            .iter()
            .all(|diagnostic| diagnostic.code == "ORNA0001")
    );
    assert!(parsed.diagnostics()[0].message.contains("RETURNS TABLE"));
    assert!(parsed.diagnostics()[0].message.contains("RETURNS ROWS"));
    assert!(parsed.diagnostics()[1].message.contains("RETURNS SET OF"));
    assert!(parsed.diagnostics()[1].message.contains("RETURNS ROWS"));
}

#[test]
fn rejects_proposal_only_declarations_without_losing_following_schema() {
    let cases = [
        "CREATE APPLICATION app; CREATE SCHEMA recovered;",
        "CREATE COMPONENT app.widget; CREATE SCHEMA recovered;",
        "CREATE QUERY app.list; CREATE SCHEMA recovered;",
        "CREATE SCREEN app.home; CREATE SCHEMA recovered;",
        "CREATE PAGE app.home; CREATE SCHEMA recovered;",
        "CREATE SERVER FUNCTION tasks.list() RETURNS ROWS (task REF tasks.task) AS SELECT REF(t) FROM tasks.task t RUNS ON SERVER; CREATE SCHEMA recovered;",
    ];

    for source in cases {
        let parsed = parse(source);

        assert_eq!(parsed.syntax().text(), source, "{source}");
        assert_eq!(parsed.diagnostics().len(), 1, "{source}");
        assert_eq!(parsed.diagnostics()[0].code, "ORNA0001", "{source}");
        assert!(parsed.object_types().is_empty(), "{source}");
        assert!(parsed.enum_types().is_empty(), "{source}");
        assert!(parsed.record_value_types().is_empty(), "{source}");
        assert!(parsed.primitive_value_types().is_empty(), "{source}");
        assert!(parsed.opaque_value_types().is_empty(), "{source}");
        assert!(parsed.type_exports().is_empty(), "{source}");
        assert!(parsed.field_renames().is_empty(), "{source}");
        assert!(parsed.server_functions().is_empty(), "{source}");
        assert!(parsed.client_functions().is_empty(), "{source}");
        assert_eq!(parsed.schemas().len(), 1, "{source}");
        assert_eq!(
            parsed.schemas()[0].name.parts[0].text,
            "recovered",
            "{source}"
        );
    }
}

#[test]
fn rejects_nonstandard_trailing_commas_in_server_function_shapes() {
    let parameters =
        "CREATE SERVER FUNCTION tasks.bad(p_task REF tasks.task,) RETURNS TEXT AS SELECT 'bad';";
    let rows = "CREATE SERVER FUNCTION tasks.bad() RETURNS ROWS (task REF tasks.task,) AS SELECT REF(t) FROM tasks.task t;";

    for source in [parameters, rows] {
        let parsed = parse(source);

        assert_eq!(parsed.syntax().text(), source);
        assert!(parsed.server_functions().is_empty());
        assert_eq!(parsed.diagnostics().len(), 1);
        assert_eq!(parsed.diagnostics()[0].code, "ORNA0001");
        assert!(parsed.diagnostics()[0].message.contains("trailing commas"));
    }
}

#[test]
fn preserves_select_source_spans_and_trivia() {
    let source = "CREATE SERVER FUNCTION tasks.list() RETURNS ROWS (task REF tasks.task, title TEXT) AS\n\
            SELECT /* identity */ REF( t ), t /* title root */ . title, t.assignee /* member */ . name\n\
            FROM tasks /* object namespace */ . task AS t\n\
            WHERE t.completed /* equality */ = fAlSe\n\
            ORDER BY t.due_at DESC, t.title ASC;";
    let parsed = parse(source);

    assert!(parsed.diagnostics().is_empty());
    assert_eq!(parsed.syntax().text(), source);
    let body = parsed.server_functions()[0]
        .body
        .as_sql_query()
        .expect("tasks.list must use a SELECT body");
    let query_start = source.find("SELECT").expect("query exists");
    let query_end = source.rfind("ASC").expect("ordering direction exists") + "ASC".len();
    assert_eq!(&body.source.text, &source[query_start..query_end]);
    assert_eq!(body.source.span.start, query_start);
    assert_eq!(body.source.span.end, query_end);
    assert_eq!(body.query.span.start, query_start);
    assert_eq!(body.query.span.end, query_end);
    assert_eq!(body.query.source_object.object_type.parts[0].text, "tasks");
    assert_eq!(body.query.source_object.object_type.parts[1].text, "task");
    assert_eq!(body.query.source_object.alias.text, "t");
    assert_eq!(
        body.query.ordering[0].direction,
        OrderingDirection::Descending
    );
    assert_eq!(body.query.ordering[0].null_order, NullOrdering::Unspecified);
    assert_eq!(
        body.query.ordering[1].direction,
        OrderingDirection::Ascending
    );
    assert_eq!(body.query.ordering[1].null_order, NullOrdering::Unspecified);

    match &body.query.predicate {
        Some(QueryExpression::Equality { left, right, .. }) => {
            assert_eq!(&source[left.span().start..left.span().end], "t.completed");
            match right.as_ref() {
                QueryExpression::BooleanLiteral { value, source } => {
                    assert!(!value);
                    assert_eq!(source.text, "fAlSe");
                }
                _ => panic!("right equality expression must be a boolean literal"),
            }
        }
        _ => panic!("query must contain its equality predicate"),
    }
}

#[test]
fn retains_identity_selector_parameters_for_both_source_alias_forms() {
    for (source, selector) in [
        (
            "CREATE SERVER FUNCTION tasks.get(p_task REF tasks.task) RETURNS ROWS (task REF tasks.task) AS SELECT REF(selected) FROM tasks.task selected WHERE REF(selected) = p_task;",
            "p_task",
        ),
        (
            "CREATE SERVER FUNCTION tasks.get(\"p_Task\" REF tasks.task) RETURNS ROWS (task REF tasks.task) AS SELECT REF(selected) FROM tasks.task AS selected WHERE REF(selected) = \"p_Task\";",
            "\"p_Task\"",
        ),
    ] {
        let parsed = parse(source);

        assert!(parsed.diagnostics().is_empty(), "source: {source}");
        assert_eq!(parsed.syntax().text(), source);
        let body = parsed.server_functions()[0]
            .body
            .as_sql_query()
            .expect("function must retain its SELECT query");
        let parameter_start = source.rfind(selector).expect("selector parameter exists");
        let query_start = source.find("SELECT").expect("query exists");
        assert_eq!(body.source.text, &source[query_start..source.len() - 1]);
        assert_eq!(body.query.span.end, parameter_start + selector.len());

        match &body.query.predicate {
            Some(QueryExpression::Equality { left, right, span }) => {
                assert_eq!(&source[left.span().start..left.span().end], "REF(selected)");
                match right.as_ref() {
                    QueryExpression::ParameterRead { parameter } => {
                        assert_eq!(parameter.text, selector);
                        assert_eq!(parameter.span.start, parameter_start);
                        assert_eq!(parameter.span.end, parameter_start + selector.len());
                        assert_eq!(&source[parameter.span.start..parameter.span.end], selector);
                    }
                    _ => panic!("selector right operand must retain the parameter read"),
                }
                assert_eq!(span.start, left.span().start);
                assert_eq!(span.end, parameter_start + selector.len());
            }
            _ => panic!("query must contain the identity selector predicate"),
        }
    }
}

#[test]
fn retains_a_parameter_read_after_one_direct_field_path() {
    let source = "CREATE SERVER FUNCTION people.by_email(p_email TEXT) RETURNS ROWS (person REF people.person, name TEXT) AS SELECT REF(selected), selected.name FROM people.person selected WHERE selected.email = p_email;";
    let parsed = parse(source);

    assert!(parsed.diagnostics().is_empty());
    let body = parsed.server_functions()[0]
        .body
        .as_sql_query()
        .expect("people.by_email must use a SELECT query");
    let parameter_start = source.rfind("p_email").expect("parameter exists");
    match &body.query.predicate {
        Some(QueryExpression::Equality { left, right, span }) => {
            assert!(matches!(
                left.as_ref(),
                QueryExpression::FieldPath { root, members, .. }
                    if root.text == "selected" && members.len() == 1 && members[0].text == "email"
            ));
            match right.as_ref() {
                QueryExpression::ParameterRead { parameter } => {
                    assert_eq!(parameter.text, "p_email");
                    assert_eq!(parameter.span.start, parameter_start);
                    assert_eq!(parameter.span.end, parameter_start + "p_email".len());
                }
                _ => panic!("direct field selector must retain a parameter read"),
            }
            assert_eq!(span.start, left.span().start);
            assert_eq!(span.end, parameter_start + "p_email".len());
        }
        _ => panic!("query must contain a direct-field selector predicate"),
    }
}

#[test]
fn rejects_a_bare_selector_name_after_a_nested_field_path() {
    let source = "CREATE SERVER FUNCTION people.by_nested_email(p_email TEXT) RETURNS ROWS (person REF people.person) AS SELECT REF(selected) FROM people.person selected WHERE selected.owner.email = p_email;";
    let parsed = parse(source);

    assert!(parsed.server_functions().is_empty());
    assert_eq!(parsed.diagnostics().len(), 1);
    assert_eq!(parsed.diagnostics()[0].code, "ORNA0001");
    assert_eq!(
        parsed.diagnostics()[0].message,
        "the current Orna SELECT parser does not yet implement bare alias expressions; expected a field path"
    );
}

#[test]
fn retains_direct_field_selector_parser_closures() {
    let reversed = "CREATE SERVER FUNCTION people.reversed(p_email TEXT) RETURNS ROWS (person REF people.person) AS SELECT REF(selected) FROM people.person selected WHERE p_email = selected.email;";
    let parsed = parse(reversed);

    assert!(parsed.server_functions().is_empty());
    assert_eq!(parsed.diagnostics().len(), 1);
    let diagnostic = &parsed.diagnostics()[0];
    assert_eq!(diagnostic.code, "ORNA0001");
    assert_eq!(
        diagnostic.message,
        "the current Orna SELECT parser does not yet implement bare alias expressions; expected a field path",
        "{diagnostic:#?}"
    );
    let reversed_start = reversed.find(" = selected").expect("equality exists") + 1;
    assert_eq!(
        diagnostic.span,
        SourceSpan {
            start: reversed_start,
            end: reversed_start + "=".len(),
        }
    );

    let qualified = "CREATE SERVER FUNCTION people.qualified(p_email TEXT) RETURNS ROWS (person REF people.person) AS SELECT REF(selected) FROM people.person selected WHERE selected.email = owner.p_email;";
    let parsed = parse(qualified);

    assert!(parsed.diagnostics().is_empty());
    let body = parsed.server_functions()[0]
        .body
        .as_sql_query()
        .expect("people.qualified must use a SELECT query");
    match &body.query.predicate {
        Some(QueryExpression::Equality { right, .. }) => assert!(matches!(
            right.as_ref(),
            QueryExpression::FieldPath { root, members, .. }
                if root.text == "owner" && members.len() == 1 && members[0].text == "p_email"
        )),
        _ => panic!("query must retain its qualified right-hand path"),
    }

    let call = "CREATE SERVER FUNCTION people.call(p_email TEXT) RETURNS ROWS (person REF people.person) AS SELECT REF(selected) FROM people.person selected WHERE selected.email = find_email();";
    let parsed = parse(call);

    assert!(parsed.server_functions().is_empty());
    assert_eq!(parsed.diagnostics().len(), 1);
    let diagnostic = &parsed.diagnostics()[0];
    assert_eq!(diagnostic.code, "ORNA0001");
    assert_eq!(
        diagnostic.message,
        "the current Orna SELECT parser does not yet implement function calls as identity selector parameters; expected a selector parameter name by itself"
    );
    let call_start = call.find("find_email()").expect("function call exists") + "find_email".len();
    assert_eq!(
        diagnostic.span,
        SourceSpan {
            start: call_start,
            end: call_start + "(".len(),
        }
    );
}

#[test]
fn preserves_existing_ref_and_boolean_right_operands_after_object_references() {
    let cases = [
        (
            "CREATE SERVER FUNCTION tasks.ref_equal() RETURNS ROWS (task REF tasks.task) AS SELECT REF(t) FROM tasks.task t WHERE REF(t) = REF(t);",
            "REF(t)",
        ),
        (
            "CREATE SERVER FUNCTION tasks.ref_true() RETURNS ROWS (task REF tasks.task) AS SELECT REF(t) FROM tasks.task t WHERE REF(t) = TRUE;",
            "TRUE",
        ),
    ];

    for (source, right_source) in cases {
        let parsed = parse(source);

        assert!(parsed.diagnostics().is_empty(), "source: {source}");
        let body = parsed.server_functions()[0]
            .body
            .as_sql_query()
            .expect("function must retain its SELECT query");
        match &body.query.predicate {
            Some(QueryExpression::Equality { right, .. }) => {
                assert_eq!(&source[right.span().start..right.span().end], right_source,);
                if right_source == "REF(t)" {
                    assert!(matches!(
                        right.as_ref(),
                        QueryExpression::ObjectReference { alias, .. } if alias.text == "t"
                    ));
                } else {
                    assert!(matches!(
                        right.as_ref(),
                        QueryExpression::BooleanLiteral { value: true, .. }
                    ));
                }
            }
            _ => panic!("query must retain its equality predicate"),
        }
    }
}

#[test]
fn preserves_existing_boolean_left_equality_with_an_object_reference() {
    let source = "CREATE SERVER FUNCTION tasks.true_equal() RETURNS ROWS (task REF tasks.task) AS SELECT REF(t) FROM tasks.task t WHERE TRUE = REF(t);";
    let parsed = parse(source);

    assert!(parsed.diagnostics().is_empty());
    let body = parsed.server_functions()[0]
        .body
        .as_sql_query()
        .expect("function must retain its SELECT query");
    match &body.query.predicate {
        Some(QueryExpression::Equality { left, right, .. }) => {
            assert!(matches!(
                left.as_ref(),
                QueryExpression::BooleanLiteral { value: true, .. }
            ));
            assert!(matches!(
                right.as_ref(),
                QueryExpression::ObjectReference { alias, .. } if alias.text == "t"
            ));
        }
        _ => panic!("query must retain its equality predicate"),
    }
}

#[test]
fn retains_direct_boolean_where_predicates_for_implicit_all_losslessly() {
    let source = "CREATE SERVER FUNCTION tasks.by_field() RETURNS ROWS (completed BOOL) AS SELECT t.completed FROM tasks.task t WHERE t.completed;\n\
            CREATE SERVER FUNCTION tasks.by_true() RETURNS ROWS (completed BOOL) AS SELECT t.completed FROM tasks.task t WHERE TRUE;\n\
            CREATE SERVER FUNCTION tasks.by_false() RETURNS ROWS (completed BOOL) AS SELECT t.completed FROM tasks.task t WHERE fAlSe;";
    let parsed = parse(source);

    assert!(parsed.diagnostics().is_empty());
    assert_eq!(parsed.syntax().text(), source);
    assert_eq!(parsed.server_functions().len(), 3);

    let field = parsed.server_functions()[0]
        .body
        .as_sql_query()
        .expect("field predicate function must use a SELECT body");
    assert!(matches!(field.query.quantifier, SelectQuantifier::All));
    let field_start = source
        .find("WHERE t.completed")
        .expect("field predicate exists")
        + "WHERE ".len();
    match field.query.predicate.as_ref() {
        Some(QueryExpression::FieldPath {
            root,
            members,
            span,
        }) => {
            assert_eq!(root.text, "t");
            assert_eq!(members.len(), 1);
            assert_eq!(members[0].text, "completed");
            assert_eq!(
                span,
                &SourceSpan {
                    start: field_start,
                    end: field_start + "t.completed".len(),
                }
            );
            assert_eq!(&source[span.start..span.end], "t.completed");
        }
        _ => panic!("WHERE t.completed must remain a field predicate"),
    }

    for (function, source_text, value) in [(1, "TRUE", true), (2, "fAlSe", false)] {
        let query = parsed.server_functions()[function]
            .body
            .as_sql_query()
            .expect("boolean predicate function must use a SELECT body");
        assert!(matches!(query.query.quantifier, SelectQuantifier::All));
        let literal_start = source
            .find(&format!("WHERE {source_text}"))
            .expect("literal predicate exists")
            + "WHERE ".len();
        match query.query.predicate.as_ref() {
            Some(QueryExpression::BooleanLiteral {
                value: actual_value,
                source: literal,
            }) => {
                assert_eq!(*actual_value, value);
                assert_eq!(literal.text, source_text);
                assert_eq!(
                    literal.span,
                    SourceSpan {
                        start: literal_start,
                        end: literal_start + source_text.len(),
                    }
                );
                assert_eq!(&source[literal.span.start..literal.span.end], source_text,);
            }
            _ => panic!("WHERE {source_text} must remain a boolean predicate"),
        }
    }
}

#[test]
fn rejects_direct_ref_where_predicates_at_the_complete_predicate_span_and_recovers() {
    let source = "CREATE SERVER FUNCTION tasks.bad() RETURNS ROWS (task REF tasks.task) AS SELECT REF(t) FROM tasks.task t WHERE REF(t);\n\
            CREATE SERVER FUNCTION tasks.good() RETURNS ROWS (task REF tasks.task) AS SELECT REF(t) FROM tasks.task t WHERE TRUE;";
    let parsed = parse(source);

    assert_eq!(parsed.syntax().text(), source);
    assert_eq!(parsed.diagnostics().len(), 1);
    let diagnostic = &parsed.diagnostics()[0];
    assert_eq!(diagnostic.code, "ORNA0001");
    assert_eq!(
        diagnostic.message,
        "WHERE must use a BOOLEAN field, TRUE, FALSE, or an equality predicate",
    );
    let predicate_start =
        source.find("WHERE REF(t)").expect("predicate REF exists") + "WHERE ".len();
    assert_eq!(
        diagnostic.span,
        SourceSpan {
            start: predicate_start,
            end: predicate_start + "REF(t)".len(),
        }
    );
    assert_eq!(parsed.server_functions().len(), 1);
    assert_eq!(parsed.server_functions()[0].name.parts[1].text, "good");
}

#[test]
fn retains_direct_boolean_where_predicates_under_distinct_losslessly() {
    let source = "CREATE SERVER FUNCTION tasks.by_field() RETURNS ROWS (value INT) AS SELECT DISTINCT t.value FROM tasks.task t WHERE t.title;\n\
            CREATE SERVER FUNCTION tasks.by_true() RETURNS ROWS (value INT) AS SELECT DISTINCT t.value FROM tasks.task t WHERE TRUE;\n\
            CREATE SERVER FUNCTION tasks.by_false() RETURNS ROWS (value INT) AS SELECT DISTINCT t.value FROM tasks.task t WHERE fAlSe;";
    let parsed = parse(source);

    assert!(parsed.diagnostics().is_empty());
    assert_eq!(parsed.syntax().text(), source);
    assert_eq!(parsed.server_functions().len(), 3);

    let field = parsed.server_functions()[0]
        .body
        .as_sql_query()
        .expect("field predicate function must use a SELECT body");
    let distinct_start = source.find("DISTINCT").expect("DISTINCT exists");
    assert!(matches!(
        &field.query.quantifier,
        SelectQuantifier::Distinct { source: distinct }
            if distinct.text == "DISTINCT"
                && distinct.span == SourceSpan {
                    start: distinct_start,
                    end: distinct_start + "DISTINCT".len(),
                }
    ));
    let field_start = source
        .find("WHERE t.title")
        .expect("field predicate exists")
        + "WHERE ".len();
    match field.query.predicate.as_ref() {
        Some(QueryExpression::FieldPath {
            root,
            members,
            span,
        }) => {
            assert_eq!(root.text, "t");
            assert_eq!(members.len(), 1);
            assert_eq!(members[0].text, "title");
            assert_eq!(
                span,
                &SourceSpan {
                    start: field_start,
                    end: field_start + "t.title".len(),
                }
            );
            assert_eq!(&source[span.start..span.end], "t.title");
        }
        _ => panic!("WHERE t.title must remain a type-neutral field predicate"),
    }

    for (function, source_text, value) in [(1, "TRUE", true), (2, "fAlSe", false)] {
        let query = parsed.server_functions()[function]
            .body
            .as_sql_query()
            .expect("boolean predicate function must use a SELECT body");
        assert!(matches!(
            query.query.quantifier,
            SelectQuantifier::Distinct { .. }
        ));
        let literal_start = source
            .find(&format!("WHERE {source_text}"))
            .expect("literal predicate exists")
            + "WHERE ".len();
        match query.query.predicate.as_ref() {
            Some(QueryExpression::BooleanLiteral {
                value: actual_value,
                source: literal,
            }) => {
                assert_eq!(*actual_value, value);
                assert_eq!(literal.text, source_text);
                assert_eq!(
                    literal.span,
                    SourceSpan {
                        start: literal_start,
                        end: literal_start + source_text.len(),
                    }
                );
                assert_eq!(&source[literal.span.start..literal.span.end], source_text,);
            }
            _ => panic!("WHERE {source_text} must remain a Boolean predicate"),
        }
    }
}

#[test]
fn rejects_direct_ref_where_predicates_under_distinct_and_recovers() {
    let source = "CREATE SERVER FUNCTION tasks.bad() RETURNS ROWS (task REF tasks.task) AS SELECT DISTINCT REF(t) FROM tasks.task t WHERE REF(t);\n\
            CREATE SERVER FUNCTION tasks.good_field() RETURNS ROWS (value INT) AS SELECT DISTINCT t.value FROM tasks.task t WHERE t.title;\n\
            CREATE SERVER FUNCTION tasks.good_true() RETURNS ROWS (value INT) AS SELECT DISTINCT t.value FROM tasks.task t WHERE TRUE;\n\
            CREATE SERVER FUNCTION tasks.good_false() RETURNS ROWS (value INT) AS SELECT DISTINCT t.value FROM tasks.task t WHERE FALSE;";
    let parsed = parse(source);

    assert_eq!(parsed.syntax().text(), source);
    assert_eq!(parsed.diagnostics().len(), 1);
    let diagnostic = &parsed.diagnostics()[0];
    assert_eq!(diagnostic.code, "ORNA0001");
    assert_eq!(
        diagnostic.message,
        "WHERE must use a BOOLEAN field, TRUE, FALSE, or an equality predicate",
    );
    let predicate_start =
        source.find("WHERE REF(t)").expect("predicate REF exists") + "WHERE ".len();
    assert_eq!(
        diagnostic.span,
        SourceSpan {
            start: predicate_start,
            end: predicate_start + "REF(t)".len(),
        }
    );
    assert_eq!(parsed.server_functions().len(), 3);
    for (function, name, expected) in [
        (0, "good_field", "field"),
        (1, "good_true", "true"),
        (2, "good_false", "false"),
    ] {
        let declaration = &parsed.server_functions()[function];
        assert_eq!(declaration.name.parts[1].text, name);
        let query = declaration
            .body
            .as_sql_query()
            .expect("recovered function must use a SELECT body");
        assert!(matches!(
            query.query.quantifier,
            SelectQuantifier::Distinct { .. }
        ));
        match (expected, query.query.predicate.as_ref()) {
            ("field", Some(QueryExpression::FieldPath { .. }))
            | ("true", Some(QueryExpression::BooleanLiteral { value: true, .. }))
            | ("false", Some(QueryExpression::BooleanLiteral { value: false, .. })) => {}
            _ => panic!("recovered {name} predicate has the wrong shape"),
        }
    }
}

#[test]
fn rejects_reversed_identity_selector_operands_with_an_exact_diagnostic() {
    let source = "CREATE SERVER FUNCTION tasks.bad(p_task REF tasks.task) RETURNS ROWS (task REF tasks.task) AS SELECT REF(selected) FROM tasks.task selected WHERE p_task = REF(selected);";
    let parsed = parse(source);

    assert_eq!(parsed.syntax().text(), source);
    assert!(parsed.server_functions().is_empty());
    assert_eq!(parsed.diagnostics().len(), 1);
    let diagnostic = &parsed.diagnostics()[0];
    assert_eq!(diagnostic.code, "ORNA0001");
    assert_eq!(
        diagnostic.message,
        "the current Orna SELECT parser does not yet implement selector parameters on the left side of WHERE equality; expected WHERE REF(alias) = selector_parameter",
    );
    let parameter_start = source.find("WHERE p_task").expect("selector exists") + "WHERE ".len();
    assert_eq!(
        diagnostic.span,
        SourceSpan {
            start: parameter_start,
            end: parameter_start + "p_task".len(),
        }
    );
}

#[test]
fn keeps_the_existing_bare_projection_diagnostic() {
    let source = "CREATE SERVER FUNCTION tasks.bad(p_task REF tasks.task) RETURNS ROWS (task REF tasks.task) AS SELECT p_task FROM tasks.task selected;";
    let parsed = parse(source);

    assert_eq!(parsed.syntax().text(), source);
    assert!(parsed.server_functions().is_empty());
    assert_eq!(parsed.diagnostics().len(), 1);
    let diagnostic = &parsed.diagnostics()[0];
    assert_eq!(diagnostic.code, "ORNA0001");
    assert_eq!(
        diagnostic.message,
        "the current Orna SELECT parser does not yet implement bare alias expressions; expected a field path",
    );
    let from_start = source.find(" FROM").expect("FROM exists") + 1;
    assert_eq!(
        diagnostic.span,
        SourceSpan {
            start: from_start,
            end: from_start + "FROM".len(),
        }
    );
}

#[test]
fn parses_a_no_input_parameter_select_server_body() {
    let source = "CREATE SERVER FUNCTION f(p_value INTEGER) RETURNS INTEGER AS SELECT p_value;";
    let parsed = parse(source);

    assert!(parsed.diagnostics().is_empty());
    assert_eq!(parsed.syntax().text(), source);
    assert_eq!(parsed.server_functions().len(), 1);

    let function = &parsed.server_functions()[0];
    assert!(function.body.as_sql_query().is_none());
    let select = function
        .body
        .as_no_input_parameter_select()
        .expect("f must use a no-input parameter select body");
    let select_start = source.find("SELECT").expect("SELECT exists");
    assert_eq!(select.source.text, "SELECT p_value");
    assert_eq!(
        select.source.span,
        SourceSpan {
            start: select_start,
            end: select_start + "SELECT p_value".len(),
        }
    );
    assert_eq!(select.parameter.text, "p_value");
    let parameter_start = source.rfind("p_value").expect("parameter exists");
    assert_eq!(
        select.parameter.span,
        SourceSpan {
            start: parameter_start,
            end: parameter_start + "p_value".len(),
        }
    );
    assert_eq!(
        &source[select.parameter.span.start..select.parameter.span.end],
        "p_value"
    );
}

#[test]
fn keeps_rejecting_no_from_select_bodies_outside_the_exact_shape() {
    for (body, message) in [
        (
            "SELECT TRUE",
            "the current Orna SELECT parser does not yet implement SELECT query bodies without FROM; expected FROM followed by an aliased object source",
        ),
        (
            "SELECT NULL",
            "the current Orna SELECT parser does not yet implement bare alias expressions; expected a field path",
        ),
        (
            "SELECT p_value + 1",
            "the current Orna SELECT parser does not yet implement bare alias expressions; expected a field path",
        ),
        ("SELECT 1", "expected a query expression in SELECT query"),
        (
            "SELECT p_value, p_value",
            "the current Orna SELECT parser does not yet implement bare alias expressions; expected a field path",
        ),
        (
            "SELECT DISTINCT p_value",
            "the current Orna SELECT parser does not yet implement bare alias expressions; expected a field path",
        ),
        (
            "SELECT p_value WHERE p_value = 1",
            "the current Orna SELECT parser does not yet implement bare alias expressions; expected a field path",
        ),
        (
            "SELECT p_value ORDER BY p_value",
            "the current Orna SELECT parser does not yet implement bare alias expressions; expected a field path",
        ),
    ] {
        let source = format!("CREATE SERVER FUNCTION tasks.bad() RETURNS TEXT AS {body};");
        let parsed = parse(&source);

        assert_eq!(parsed.syntax().text(), source, "body: {body}");
        assert!(parsed.server_functions().is_empty(), "body: {body}");
        assert_eq!(parsed.diagnostics().len(), 1, "body: {body}");
        assert_eq!(parsed.diagnostics()[0].code, "ORNA0001", "body: {body}");
        assert_eq!(parsed.diagnostics()[0].message, message, "body: {body}");
    }
}

#[test]
fn keeps_parsing_from_queries_as_sql_query_bodies() {
    let source = "CREATE SERVER FUNCTION tasks.list() RETURNS ROWS (task REF tasks.task) AS SELECT t.title FROM tasks.task t;";
    let parsed = parse(source);

    assert!(parsed.diagnostics().is_empty());
    let body = &parsed.server_functions()[0].body;
    assert!(body.as_no_input_parameter_select().is_none());
    let query = body
        .as_sql_query()
        .expect("tasks.list must use a SELECT body");
    assert_eq!(query.source.text, "SELECT t.title FROM tasks.task t");
    assert_eq!(query.query.source_object.alias.text, "t");
}

#[test]
fn rejects_order_by_after_an_identity_selector_parameter() {
    let source = "CREATE SERVER FUNCTION tasks.bad(p_task REF tasks.task) RETURNS ROWS (task REF tasks.task) AS SELECT REF(selected) FROM tasks.task selected WHERE REF(selected) = p_task ORDER BY selected.title;";
    let parsed = parse(source);

    assert_eq!(parsed.syntax().text(), source);
    assert!(parsed.server_functions().is_empty());
    assert_eq!(parsed.diagnostics().len(), 1);
    let diagnostic = &parsed.diagnostics()[0];
    assert_eq!(diagnostic.code, "ORNA0001");
    assert_eq!(
        diagnostic.message,
        "identity-selected SELECT queries do not allow ORDER BY; remove the ORDER BY clause",
    );
    let order_start = source.find("ORDER BY").expect("ORDER BY exists");
    assert_eq!(
        diagnostic.span,
        SourceSpan {
            start: order_start,
            end: order_start + "ORDER".len(),
        }
    );
}

#[test]
fn recovers_to_later_declarations_after_an_invalid_identity_selector() {
    let source = "CREATE SERVER FUNCTION tasks.bad(p_task REF tasks.task) RETURNS ROWS (task REF tasks.task) AS SELECT REF(selected) FROM tasks.task selected WHERE p_task = REF(selected);\n\
            CREATE SERVER FUNCTION tasks.good(p_task REF tasks.task) RETURNS ROWS (task REF tasks.task) AS SELECT REF(selected) FROM tasks.task selected WHERE REF(selected) = p_task;";
    let parsed = parse(source);

    assert_eq!(parsed.syntax().text(), source);
    assert_eq!(parsed.diagnostics().len(), 1);
    assert_eq!(parsed.server_functions().len(), 1);
    assert_eq!(parsed.server_functions()[0].name.parts[1].text, "good");
}

#[test]
fn reports_malformed_and_unimplemented_select_bodies_without_losing_recovery() {
    let malformed =
        "CREATE SERVER FUNCTION tasks.bad() RETURNS TEXT AS SELECT REF() FROM tasks.task t;";
    let parsed = parse(malformed);
    assert_eq!(parsed.syntax().text(), malformed);
    assert!(parsed.server_functions().is_empty());
    assert!(parsed.diagnostics().iter().any(|diagnostic| {
        diagnostic.code == "ORNA0001" && diagnostic.message.contains("alias inside REF")
    }));

    let unsupported = "CREATE SERVER FUNCTION tasks.unsupported() RETURNS TEXT AS SELECT t.* FROM tasks.task t;\n\
            CREATE SERVER FUNCTION tasks.ok() RETURNS TEXT AS SELECT t.title FROM tasks.task t;";
    let parsed = parse(unsupported);
    assert_eq!(parsed.syntax().text(), unsupported);
    assert_eq!(parsed.server_functions().len(), 1);
    assert_eq!(parsed.server_functions()[0].name.parts[1].text, "ok");
    assert_eq!(parsed.diagnostics().len(), 1);
    let diagnostic = &parsed.diagnostics()[0];
    assert_eq!(diagnostic.code, "ORNA0001");
    assert_eq!(
        diagnostic.message,
        "the current Orna SELECT parser does not yet implement wildcard field paths; expected a field name after '.'"
    );
    let wildcard = unsupported.find('*').expect("wildcard exists");
    assert_eq!(diagnostic.span.start, wildcard);
    assert_eq!(diagnostic.span.end, wildcard + 1);
}

#[test]
fn defers_query_alias_resolution_to_later_semantic_stages() {
    let source = "CREATE SERVER FUNCTION tasks.unresolved() RETURNS TEXT AS\n\
            SELECT REF(other), other.title FROM tasks.task t;";
    let parsed = parse(source);

    assert!(parsed.diagnostics().is_empty());
    let body = parsed.server_functions()[0]
        .body
        .as_sql_query()
        .expect("tasks.unresolved must use a SELECT body");
    assert!(matches!(
        body.query.projections[0],
        QueryExpression::ObjectReference { ref alias, .. } if alias.text == "other"
    ));
    assert!(matches!(
        body.query.projections[1],
        QueryExpression::FieldPath { ref root, .. } if root.text == "other"
    ));
}

#[test]
fn parses_single_row_insert_bodies_losslessly() {
    let source = "CREATE SERVER FUNCTION tasks.create (\n\
            p_title TEXT,\n\
            p_done BOOL,\n\
            p_owner REF tasks.owner\n\
        )\n\
        RETURNS ROWS (created REF tasks.task)\n\
        SECURITY INVOKER\n\
        TRANSACTION ATOMIC\n\
        VOLATILITY VOLATILE\n\
        AS\n\
            INSERT /* target */ INTO tasks /* type */ . task AS created (\n\
                title, done, owner\n\
            ) VALUES (p_title, p_done, p_owner) RETURNING REF(created);";
    let parsed = parse(source);

    assert!(parsed.diagnostics().is_empty());
    assert_eq!(parsed.syntax().text(), source);
    let function = &parsed.server_functions()[0];
    let body = function
        .body
        .as_sql_insert()
        .expect("the function must have an INSERT body");
    let insert = &body.insert;
    assert!(function.body.as_sql_query().is_none());
    let insert_start = source.find("INSERT").expect("INSERT exists");
    let body_end = source.rfind(")").expect("RETURNING close exists") + 1;
    assert_eq!(body.source.text, &source[insert_start..body_end]);
    assert_eq!(body.source.span.start, insert_start);
    assert_eq!(body.source.span.end, body_end);
    assert_eq!(insert.span, body.source.span);
    assert_eq!(insert.target_object.parts[0].text, "tasks");
    assert_eq!(insert.target_object.parts[1].text, "task");
    assert_eq!(insert.target_alias.text, "created");
    assert_eq!(insert.target_fields.len(), 3);
    assert_eq!(insert.target_fields[0].text, "title");
    assert_eq!(insert.target_fields[1].text, "done");
    assert_eq!(insert.target_fields[2].text, "owner");
    assert!(matches!(
        &insert.values[0],
        InsertValue::Parameter(name) if name.text == "p_title"
    ));
    assert!(matches!(
        &insert.values[1],
        InsertValue::Parameter(name) if name.text == "p_done"
    ));
    assert!(matches!(
        &insert.values[2],
        InsertValue::Parameter(name) if name.text == "p_owner"
    ));
    assert_eq!(insert.returning_alias.text, "created");
    assert_eq!(
        insert.returning_alias.span.start,
        source.rfind("created").unwrap()
    );
    assert_eq!(
        insert.values[0].span().start,
        source.rfind("p_title").unwrap()
    );
}

#[test]
fn retains_insert_returning_ref_span_with_trivia() {
    let source = "cReAtE sErVeR fUnCtIoN t.i(p TEXT) ReTuRnS rOwS (r REF t.o) aS iNsErT /* target */ iNtO t.o aS r (x) vAlUeS (p) rEtUrNiNg rEf( /* before close */ r /* after */ );";
    let parsed = parse(source);

    assert!(parsed.diagnostics().is_empty());
    assert_eq!(parsed.syntax().text(), source);
    let insert = &parsed.server_functions()[0]
        .body
        .as_sql_insert()
        .expect("the function must have an INSERT body")
        .insert;
    assert_eq!(insert.target_alias.text, "r");
    assert_eq!(insert.values.len(), 1);
    assert_eq!(insert.returning_alias.text, "r");
    assert_eq!(
        insert.returning_ref_span,
        SourceSpan {
            start: 122,
            end: 161,
        }
    );
    assert_eq!(
        &source[insert.returning_ref_span.start..insert.returning_ref_span.end],
        "rEf( /* before close */ r /* after */ )"
    );
}

#[test]
fn parses_record_constructors_in_insert_values_losslessly() {
    let source = "CREATE SERVER FUNCTION tasks.create(p_x INT, p_stage tasks.stage) RETURNS ROWS (result REF tasks.item) AS INSERT INTO tasks.item AS made (point) VALUES (tasks.point{stage: p_stage, /* reordered */ x: p_x, ready: TRUE,}) RETURNING REF(made);";
    let parsed = parse(source);

    assert!(
        parsed.diagnostics().is_empty(),
        "{:?}",
        parsed.diagnostics()
    );
    assert_eq!(parsed.syntax().text(), source);
    let insert = &parsed.server_functions()[0]
        .body
        .as_sql_insert()
        .expect("the function must have an INSERT body")
        .insert;
    let InsertValue::RecordConstructor(constructor) = &insert.values[0] else {
        panic!("the INSERT value must be a record constructor");
    };
    assert_eq!(constructor.record_type.parts[0].text, "tasks");
    assert_eq!(constructor.record_type.parts[1].text, "point");
    assert_eq!(constructor.fields.len(), 3);
    assert_eq!(constructor.fields[0].name.text, "stage");
    assert!(matches!(
        &constructor.fields[0].value,
        RecordConstructorFieldValue::Parameter(parameter) if parameter.text == "p_stage"
    ));
    assert_eq!(constructor.fields[1].name.text, "x");
    assert!(matches!(
        &constructor.fields[1].value,
        RecordConstructorFieldValue::Parameter(parameter) if parameter.text == "p_x"
    ));
    assert_eq!(constructor.fields[2].name.text, "ready");
    assert!(matches!(
        &constructor.fields[2].value,
        RecordConstructorFieldValue::BooleanLiteral { value: true, source }
            if source.text == "TRUE"
    ));
    let constructor_start = source.find("tasks.point{").unwrap();
    let constructor_end = source.find("}) RETURNING").unwrap() + 1;
    assert_eq!(
        constructor.span,
        SourceSpan {
            start: constructor_start,
            end: constructor_end,
        }
    );
    assert_eq!(insert.values[0].span(), &constructor.span);
    assert_eq!(
        constructor.fields[1].span,
        SourceSpan {
            start: source.find("x: p_x").unwrap(),
            end: source.find("p_x, ready").unwrap() + "p_x".len(),
        }
    );
}

#[test]
fn record_constructor_diagnostics_close_the_initial_expression_subset() {
    let cases = [
        (
            "tasks.point{x: NULL}",
            "record constructor fields accept only a declared parameter, TRUE, or FALSE",
            "NULL",
        ),
        (
            "tasks.point{x: make_x()}",
            "record constructor fields do not support function calls",
            "(",
        ),
        (
            "tasks.point{x: other.value}",
            "record constructor fields do not support field paths or qualified values",
            ".",
        ),
        (
            "tasks.point{x: tasks.inner{x: p_x}}",
            "record constructor fields do not support nested record constructors",
            "{x: p_x}",
        ),
        (
            "tasks.point{x: p_x, X: p_x}",
            "record constructor field x appears more than once",
            "X: p_x",
        ),
    ];

    for (value, message, marker) in cases {
        let source = format!(
            "CREATE SERVER FUNCTION tasks.bad(p_x INT) RETURNS ROWS (result REF tasks.item) AS INSERT INTO tasks.item AS made (point) VALUES ({value}) RETURNING REF(made);"
        );
        let parsed = parse(&source);
        assert!(parsed.server_functions().is_empty(), "{value}");
        assert_eq!(parsed.diagnostics().len(), 1, "{value}");
        assert_eq!(parsed.diagnostics()[0].code, "ORNA0001", "{value}");
        assert_eq!(parsed.diagnostics()[0].message, message, "{value}");
        let value_start = source.find(value).unwrap();
        let marker_start = value_start + value.rfind(marker).unwrap();
        assert_eq!(parsed.diagnostics()[0].span.start, marker_start, "{value}");
    }
}

#[test]
fn update_values_do_not_accept_record_constructors() {
    let source = "CREATE SERVER FUNCTION tasks.update(p_item REF tasks.item, p_x INT) RETURNS ROWS (result REF tasks.item) AS UPDATE tasks.item AS item SET point = tasks.point{x: p_x} WHERE REF(item) = p_item RETURNING REF(item);";
    let parsed = parse(source);

    assert!(parsed.server_functions().is_empty());
    assert_eq!(parsed.diagnostics().len(), 1);
    assert_eq!(
        parsed.diagnostics()[0].message,
        "this UPDATE does not support record constructors in UPDATE values; expected a declared parameter name by itself"
    );
    assert_eq!(
        parsed.diagnostics()[0].span.start,
        source.find('{').unwrap()
    );
}

#[test]
fn empty_record_constructor_recovers_to_a_later_function() {
    let source = "CREATE SERVER FUNCTION tasks.bad() RETURNS ROWS (result REF tasks.item) AS INSERT INTO tasks.item AS made (point) VALUES (tasks.point{}) RETURNING REF(made);\n\
            CREATE SERVER FUNCTION tasks.good(p_x INT) RETURNS ROWS (result REF tasks.item) AS INSERT INTO tasks.item AS made (point) VALUES (p_x) RETURNING REF(made);";
    let parsed = parse(source);

    assert_eq!(parsed.syntax().text(), source);
    assert_eq!(parsed.server_functions().len(), 1);
    assert_eq!(parsed.server_functions()[0].name.parts[1].text, "good");
    assert_eq!(parsed.diagnostics().len(), 1);
    assert_eq!(
        parsed.diagnostics()[0].message,
        "record constructor must supply at least one field"
    );
    let close = source.find("{}").unwrap() + 1;
    assert_eq!(
        parsed.diagnostics()[0].span,
        SourceSpan {
            start: close,
            end: close + 1,
        }
    );
}

#[test]
fn rejects_closed_insert_forms_and_recovers_to_a_valid_declaration() {
    let invalid = [
        "INSERT INTO tasks.task created (title) VALUES (p_title) RETURNING REF(created)",
        "INSERT INTO tasks.task AS created () VALUES (p_title) RETURNING REF(created)",
        "INSERT INTO tasks.task AS created (title,) VALUES (p_title) RETURNING REF(created)",
        "INSERT INTO tasks.task AS created (title, title) VALUES (p_title, p_title) RETURNING REF(created)",
        "INSERT INTO tasks.task AS created (title, \"title\") VALUES (p_title, p_title) RETURNING REF(created)",
        "INSERT INTO tasks.task AS created (tasks.title) VALUES (p_title) RETURNING REF(created)",
        "INSERT INTO tasks.task AS created (title, done) VALUES (p_title) RETURNING REF(created)",
        "INSERT INTO tasks.task AS created (title) VALUES (p_title, TRUE) RETURNING REF(created)",
        "INSERT INTO tasks.task AS created (title) VALUES () RETURNING REF(created)",
        "INSERT INTO tasks.task AS created (title) VALUES (p_title,) RETURNING REF(created)",
        "INSERT INTO tasks.task AS created (title) VALUES (p_title), (p_title) RETURNING REF(created)",
        "INSERT INTO tasks.task AS created (title) VALUES ('title') RETURNING REF(created)",
        "INSERT INTO tasks.task AS created (title) VALUES (make_title()) RETURNING REF(created)",
        "INSERT INTO tasks.task AS created (title) VALUES (other.title) RETURNING REF(created)",
        "INSERT INTO tasks.task AS created (title) VALUES (p_title) RETURNING created",
        "INSERT INTO tasks.task AS created (title) VALUES (p_title) RETURNING REF(created.title)",
        "INSERT INTO tasks.task AS created (title) VALUES (p_title) RETURNING REF(other)",
    ];
    for body in invalid {
        let source = format!(
            "CREATE SERVER FUNCTION tasks.bad(p_title TEXT) RETURNS ROWS (created REF tasks.task) AS {body};"
        );
        let parsed = parse(&source);
        assert_eq!(parsed.syntax().text(), source);
        assert!(parsed.server_functions().is_empty(), "invalid body: {body}");
        assert!(!parsed.diagnostics().is_empty(), "invalid body: {body}");
    }

    let source = "CREATE SERVER FUNCTION tasks.bad(p_title TEXT) RETURNS ROWS (created REF tasks.task) AS INSERT INTO tasks.task AS Created (title) VALUES (p_title) RETURNING REF(OTHER);\n\
            CREATE SERVER FUNCTION tasks.good(p_title TEXT) RETURNS ROWS (result REF tasks.task) AS INSERT INTO tasks.task AS created (title) VALUES (p_title) RETURNING REF(created);";
    let parsed = parse(source);
    assert_eq!(parsed.syntax().text(), source);
    assert_eq!(parsed.server_functions().len(), 1);
    assert_eq!(parsed.server_functions()[0].name.parts[1].text, "good");
    assert_eq!(parsed.diagnostics().len(), 1);
    let diagnostic = &parsed.diagnostics()[0];
    assert_eq!(diagnostic.code, "ORNA0001");
    assert_eq!(
        diagnostic.message,
        "RETURNING REF must use the INSERT target alias created, not other"
    );
    let other = source.find("OTHER").expect("wrong alias exists");
    assert_eq!(diagnostic.span.start, other);
    assert_eq!(diagnostic.span.end, other + "OTHER".len());
}

#[test]
fn insert_keywords_and_unquoted_aliases_are_case_insensitive() {
    let source = "CREATE SERVER FUNCTION tasks.create() RETURNS ROWS (result REF tasks.task) AS iNsErT iNtO tasks.task aS Created (done, note) vAlUeS (fAlSe, nUlL) rEtUrNiNg rEf(created);";
    let parsed = parse(source);

    assert!(parsed.diagnostics().is_empty());
    let body = parsed.server_functions()[0]
        .body
        .as_sql_insert()
        .expect("the function must have an INSERT body");
    assert_eq!(body.insert.target_alias.text, "Created");
    assert_eq!(body.insert.returning_alias.text, "created");
    assert!(matches!(
        &body.insert.values[0],
        InsertValue::BooleanLiteral { value: false, source } if source.text == "fAlSe"
    ));
    assert!(matches!(
        &body.insert.values[1],
        InsertValue::NullLiteral { source } if source.text == "nUlL"
    ));
}

#[test]
fn duplicate_insert_field_diagnostic_uses_the_normalised_name_and_exact_span() {
    let source = "CREATE SERVER FUNCTION tasks.bad(p_title TEXT) RETURNS ROWS (result REF tasks.task) AS INSERT INTO tasks.task AS created (Title, \"title\") VALUES (p_title, p_title) RETURNING REF(created);";
    let parsed = parse(source);

    assert!(parsed.server_functions().is_empty());
    assert_eq!(parsed.diagnostics().len(), 1);
    assert_eq!(parsed.diagnostics()[0].code, "ORNA0001");
    assert_eq!(
        parsed.diagnostics()[0].message,
        "field title appears more than once in this INSERT"
    );
    let duplicate = source.find("\"title\"").expect("duplicate field exists");
    assert_eq!(parsed.diagnostics()[0].span.start, duplicate);
    assert_eq!(
        parsed.diagnostics()[0].span.end,
        duplicate + "\"title\"".len()
    );
}

#[test]
fn insert_count_diagnostics_use_grammatical_nouns_and_exact_spans() {
    let cases = [
        (
            "INSERT INTO tasks.task AS created (title, done) VALUES (p_title) RETURNING REF(created)",
            "INSERT lists 2 fields but 1 value; each field requires one value",
            ") RETURNING",
            1,
        ),
        (
            "INSERT INTO tasks.task AS created (title) VALUES (p_title, p_done) RETURNING REF(created)",
            "INSERT lists 1 field but 2 values; each field requires one value",
            "p_done) RETURNING",
            "p_done".len(),
        ),
    ];

    for (body, message, marker, span_length) in cases {
        assert_insert_diagnostic(body, message, marker, 0, span_length);
    }
}

#[test]
fn qualified_insert_names_report_guidance_at_the_dot() {
    let cases = [
        (
            "INSERT INTO tasks.task AS created (tasks.title) VALUES (p_title) RETURNING REF(created)",
            "write only the field name in the INSERT field list; do not add an object or alias",
            "tasks.title",
            "tasks".len(),
        ),
        (
            "INSERT INTO tasks.task AS created (title) VALUES (other.p_title) RETURNING REF(created)",
            "use the declared parameter name by itself in VALUES; do not add an object or alias",
            "other.p_title",
            "other".len(),
        ),
    ];

    for (body, message, marker, dot_offset) in cases {
        assert_insert_diagnostic(body, message, marker, dot_offset, 1);
    }
}

#[test]
fn insert_implementation_gap_diagnostics_use_exact_copy_and_spans() {
    let cases = [
        (
            "INSERT INTO tasks.task AS created (title) VALUES (p_title), (p_title) RETURNING REF(created)",
            "this INSERT does not support multiple VALUES rows; expected RETURNING after one VALUES row",
            ", (p_title) RETURNING",
            0,
            1,
        ),
        (
            "INSERT INTO tasks.task AS created (title) VALUES (p_title) RETURNING REF(created) EXTRA",
            "this INSERT does not support EXTRA; expected the end of the INSERT body",
            "EXTRA",
            0,
            "EXTRA".len(),
        ),
        (
            "INSERT INTO tasks.task AS created (title) VALUES (make_title()) RETURNING REF(created)",
            "this INSERT does not support function calls in INSERT values; expected a declared parameter name by itself",
            "make_title()",
            "make_title".len(),
            1,
        ),
    ];

    for (body, message, marker, span_offset, span_length) in cases {
        assert_insert_diagnostic(body, message, marker, span_offset, span_length);
    }
}

#[test]
fn malformed_insert_quotes_report_diagnostics_without_panicking() {
    let source = "CREATE SERVER FUNCTION tasks.bad(p_title TEXT) RETURNS ROWS (result REF tasks.task) AS INSERT INTO tasks.task AS created (title) VALUES (p_title) RETURNING REF(\"";
    let parsed = parse(source);

    assert_eq!(parsed.syntax().text(), source);
    assert!(parsed.server_functions().is_empty());
    assert!(parsed.diagnostics().iter().any(|diagnostic| {
        diagnostic.code == "ORNA0002" && diagnostic.message == "unterminated quoted identifier"
    }));
}

#[test]
fn malformed_insert_parentheses_do_not_consume_later_declarations() {
    let source = "CREATE SERVER FUNCTION tasks.bad(p_title TEXT) RETURNS ROWS (result REF tasks.task) AS INSERT INTO tasks.task AS created (title) VALUES (p_title;
            CREATE SERVER FUNCTION tasks.good(p_title TEXT) RETURNS ROWS (result REF tasks.task) AS INSERT INTO tasks.task AS created (title) VALUES (p_title) RETURNING REF(created);";
    let parsed = parse(source);

    assert_eq!(parsed.syntax().text(), source);
    assert_eq!(parsed.server_functions().len(), 1);
    assert_eq!(parsed.server_functions()[0].name.parts[1].text, "good");
    assert!(parsed.diagnostics().iter().any(|diagnostic| {
        diagnostic.code == "ORNA0001"
            && diagnostic
                .message
                .contains("expected ',' or ')' after an INSERT value")
    }));
}

#[test]
fn parses_single_object_update_bodies_losslessly() {
    let source = "CREATE SERVER FUNCTION tasks.update(
            p_task REF tasks.task,
            p_title TEXT
        )
        RETURNS ROWS (updated REF tasks.task)
        SECURITY INVOKER
        TRANSACTION ATOMIC
        VOLATILITY VOLATILE
        AS UPDATE /* target */ tasks.task AS Updated
            SET title = p_title, done = FALSE, note = NULL
            WHERE REF(updated) = p_task
            RETURNING REF(UPDATED);";
    let parsed = parse(source);

    assert!(parsed.diagnostics().is_empty());
    assert_eq!(parsed.syntax().text(), source);
    let function = &parsed.server_functions()[0];
    let body = function
        .body
        .as_sql_update()
        .expect("the function must have an UPDATE body");
    assert!(function.body.as_sql_query().is_none());
    assert!(function.body.as_sql_insert().is_none());
    let update_start = source.find("UPDATE /* target */").unwrap();
    let body_end = source.rfind(')').unwrap() + 1;
    assert_eq!(body.source.text, &source[update_start..body_end]);
    assert_eq!(
        body.source.span,
        SourceSpan {
            start: update_start,
            end: body_end
        }
    );
    assert_eq!(body.update.span, body.source.span);
    assert_eq!(body.update.target_object.parts[0].text, "tasks");
    assert_eq!(body.update.target_object.parts[1].text, "task");
    assert_eq!(body.update.target_alias.text, "Updated");
    assert_eq!(body.update.assignments.len(), 3);
    assert_eq!(body.update.assignments[0].target_field.text, "title");
    assert!(matches!(
        &body.update.assignments[0].value,
        MutationValue::Parameter(name) if name.text == "p_title"
    ));
    assert!(matches!(
        &body.update.assignments[1].value,
        MutationValue::BooleanLiteral { value: false, source } if source.text == "FALSE"
    ));
    assert!(matches!(
        &body.update.assignments[2].value,
        MutationValue::NullLiteral { source } if source.text == "NULL"
    ));
    assert_eq!(body.update.selector_alias.text, "updated");
    assert_eq!(body.update.selector_parameter.text, "p_task");
    assert_eq!(body.update.returning_alias.text, "UPDATED");
    assert_eq!(
        body.update.assignments[0].span.start,
        source.find("title = p_title").unwrap()
    );
    assert_eq!(
        body.update.assignments[0].span.end,
        source.find("p_title, done").unwrap() + "p_title".len()
    );
}

#[test]
fn retains_update_selector_and_returning_ref_spans_with_trivia() {
    let source = "cReAtE sErVeR fUnCtIoN t.u(p REF t.o, x TEXT) ReTuRnS rOwS (r REF t.o) aS uPdAtE /* target */ t.o aS r SeT x = p wHeRe rEf( /* selector */ r /* close */ ) /* equals */ = /* parameter */ p rEtUrNiNg rEf( /* returning */ r /* close */ );";
    let parsed = parse(source);

    assert!(parsed.diagnostics().is_empty());
    assert_eq!(parsed.syntax().text(), source);
    let update = &parsed.server_functions()[0]
        .body
        .as_sql_update()
        .expect("the function must have an UPDATE body")
        .update;
    assert_eq!(update.target_alias.text, "r");
    assert_eq!(update.assignments.len(), 1);
    assert_eq!(update.selector_alias.text, "r");
    assert_eq!(update.selector_parameter.text, "p");
    assert_eq!(update.returning_alias.text, "r");
    assert_eq!(
        update.selector_ref_span,
        SourceSpan {
            start: 119,
            end: 154,
        }
    );
    assert_eq!(
        update.selector_equality_span,
        SourceSpan {
            start: 119,
            end: 187,
        }
    );
    assert_eq!(
        update.returning_ref_span,
        SourceSpan {
            start: 198,
            end: 234,
        }
    );
    assert_eq!(
        &source[update.selector_ref_span.start..update.selector_ref_span.end],
        "rEf( /* selector */ r /* close */ )"
    );
    assert_eq!(
        &source[update.selector_equality_span.start..update.selector_equality_span.end],
        "rEf( /* selector */ r /* close */ ) /* equals */ = /* parameter */ p"
    );
    assert_eq!(
        &source[update.returning_ref_span.start..update.returning_ref_span.end],
        "rEf( /* returning */ r /* close */ )"
    );
}

#[test]
fn update_diagnostics_are_direct_and_select_the_offending_source() {
    let cases = [
        (
            "UPDATE tasks.task AS updated SET Title = p_title, \"title\" = p_title WHERE REF(updated) = p_task RETURNING REF(updated)",
            "field title appears more than once in this UPDATE",
            "\"title\" =",
            0,
            "\"title\"".len(),
        ),
        (
            "UPDATE tasks.task AS updated SET tasks.title = p_title WHERE REF(updated) = p_task RETURNING REF(updated)",
            "write only the field name in SET; do not add an object or alias",
            "tasks.title",
            "tasks".len(),
            1,
        ),
        (
            "UPDATE tasks.task AS updated SET title = input.p_title WHERE REF(updated) = p_task RETURNING REF(updated)",
            "use the declared parameter name by itself after '='; do not add an object or alias",
            "input.p_title",
            "input".len(),
            1,
        ),
        (
            "UPDATE tasks.task AS updated SET title = p_title WHERE REF(other) = p_task RETURNING REF(updated)",
            "WHERE REF must use the UPDATE target alias updated, not other",
            "REF(other)",
            "REF(".len(),
            "other".len(),
        ),
        (
            "UPDATE tasks.task AS updated SET title = p_title WHERE REF(updated) = p_task RETURNING REF(other)",
            "RETURNING REF must use the UPDATE target alias updated, not other",
            "REF(other)",
            "REF(".len(),
            "other".len(),
        ),
        (
            "UPDATE tasks.task AS updated SET title = make_title() WHERE REF(updated) = p_task RETURNING REF(updated)",
            "this UPDATE does not support function calls in UPDATE values; expected a declared parameter name by itself",
            "make_title()",
            "make_title".len(),
            1,
        ),
        (
            "UPDATE tasks.task AS updated SET title = p_title WHERE REF(updated) = p_task RETURNING REF(updated) EXTRA",
            "this UPDATE does not support EXTRA; expected the end of the UPDATE body",
            "EXTRA",
            0,
            "EXTRA".len(),
        ),
    ];

    for (body, message, marker, span_offset, span_length) in cases {
        assert_update_diagnostic(body, message, marker, span_offset, span_length);
    }
}

#[test]
fn rejects_closed_update_forms_and_recovers_to_a_later_declaration() {
    let cases = [
        (
            "UPDATE tasks.task updated SET title = p_title WHERE REF(updated) = p_task RETURNING REF(updated)",
            "expected AS before the UPDATE target alias in UPDATE body",
            "updated SET",
            0,
            "updated".len(),
        ),
        (
            "UPDATE tasks.task AS updated SET WHERE REF(updated) = p_task RETURNING REF(updated)",
            "expected at least one field assignment after SET in UPDATE body",
            "WHERE",
            0,
            "WHERE".len(),
        ),
        (
            "UPDATE tasks.task AS updated SET title p_title WHERE REF(updated) = p_task RETURNING REF(updated)",
            "expected '=' after the UPDATE field name in UPDATE body",
            "p_title WHERE",
            0,
            "p_title".len(),
        ),
        (
            "UPDATE tasks.task AS updated SET title = p_title, WHERE REF(updated) = p_task RETURNING REF(updated)",
            "expected a field assignment after ',' in UPDATE body",
            "WHERE",
            0,
            "WHERE".len(),
        ),
        (
            "UPDATE tasks.task AS updated SET title = p_title WHERE updated.id = p_task RETURNING REF(updated)",
            "expected REF(target_alias) after WHERE in UPDATE body",
            "updated.id",
            0,
            "updated".len(),
        ),
        (
            "UPDATE tasks.task AS updated SET title = p_title WHERE REF(updated) = TRUE RETURNING REF(updated)",
            "expected a declared REF parameter after '=' in UPDATE body",
            "TRUE",
            0,
            "TRUE".len(),
        ),
        (
            "UPDATE tasks.task AS updated SET title = p_title WHERE REF(updated) = owner.p_task RETURNING REF(updated)",
            "use the selector parameter name by itself after '='; do not add an object or alias",
            "owner.p_task",
            "owner".len(),
            1,
        ),
        (
            "UPDATE tasks.task AS updated SET title = p_title WHERE REF(updated) = find_task() RETURNING REF(updated)",
            "this UPDATE does not support function calls as UPDATE selectors; expected a declared REF parameter name by itself",
            "find_task()",
            "find_task".len(),
            1,
        ),
        (
            "UPDATE tasks.task AS updated SET title = p_title WHERE REF(updated) = p_task RETURNING updated",
            "expected REF in the RETURNING expression in UPDATE body",
            "RETURNING updated",
            "RETURNING ".len(),
            "updated".len(),
        ),
    ];
    for (body, message, marker, span_offset, span_length) in cases {
        assert_update_diagnostic(body, message, marker, span_offset, span_length);
    }

    let source = "CREATE SERVER FUNCTION tasks.bad(p_task REF tasks.task, p_title TEXT) RETURNS ROWS (updated REF tasks.task) AS UPDATE tasks.task AS updated SET title = p_title WHERE REF(other) = p_task RETURNING REF(updated);
            CREATE SERVER FUNCTION tasks.good(p_task REF tasks.task, p_title TEXT) RETURNS ROWS (updated REF tasks.task) AS UPDATE tasks.task AS updated SET title = p_title WHERE REF(updated) = p_task RETURNING REF(updated);";
    let parsed = parse(source);
    assert_eq!(parsed.server_functions().len(), 1);
    assert_eq!(parsed.server_functions()[0].name.parts[1].text, "good");
    assert!(parsed.server_functions()[0].body.as_sql_update().is_some());
    assert_eq!(parsed.diagnostics().len(), 1);
}

#[test]
fn malformed_update_parentheses_do_not_consume_later_declarations() {
    let source = "CREATE SERVER FUNCTION tasks.bad(p_task REF tasks.task, p_title TEXT) RETURNS ROWS (updated REF tasks.task) AS UPDATE tasks.task AS updated SET title = p_title WHERE REF(updated = p_task RETURNING REF(updated);
            CREATE SERVER FUNCTION tasks.good(p_task REF tasks.task, p_title TEXT) RETURNS ROWS (updated REF tasks.task) AS UPDATE tasks.task AS updated SET title = p_title WHERE REF(updated) = p_task RETURNING REF(updated);";
    let parsed = parse(source);

    assert_eq!(parsed.syntax().text(), source);
    assert_eq!(parsed.server_functions().len(), 1);
    assert_eq!(parsed.server_functions()[0].name.parts[1].text, "good");
    assert!(parsed.diagnostics().iter().any(|diagnostic| {
        diagnostic.code == "ORNA0001"
            && diagnostic.message == "expected ')' after the WHERE REF alias in UPDATE body"
    }));
}

#[test]
fn parses_single_object_delete_bodies_losslessly() {
    let source = "CREATE SERVER FUNCTION tasks.remove(p_task REF tasks.task)
            RETURNS ROWS (deleted BOOL)
            SECURITY INVOKER TRANSACTION ATOMIC VOLATILITY VOLATILE
            AS DELETE /* target */ FROM tasks.task AS \"Gone\"
            WHERE REF(\"Gone\") = p_task
            RETURNING TrUe;";
    let parsed = parse(source);

    assert!(parsed.diagnostics().is_empty());
    assert_eq!(parsed.syntax().text(), source);
    let function = &parsed.server_functions()[0];
    let body = function
        .body
        .as_sql_delete()
        .expect("the function must have a DELETE body");
    assert!(function.body.as_sql_query().is_none());
    assert!(function.body.as_sql_insert().is_none());
    assert!(function.body.as_sql_update().is_none());
    let delete_start = source.find("DELETE /* target */").unwrap();
    let body_end = source.rfind("TrUe").unwrap() + "TrUe".len();
    assert_eq!(body.source.text, &source[delete_start..body_end]);
    assert_eq!(
        body.source.span,
        SourceSpan {
            start: delete_start,
            end: body_end,
        }
    );
    assert_eq!(body.delete.span, body.source.span);
    assert_eq!(body.delete.target_object.parts[0].text, "tasks");
    assert_eq!(body.delete.target_object.parts[1].text, "task");
    assert_eq!(body.delete.target_alias.text, "\"Gone\"");
    assert_eq!(body.delete.selector_alias.text, "\"Gone\"");
    assert_eq!(body.delete.selector_parameter.text, "p_task");
    assert_eq!(body.delete.returning_true.text, "TrUe");
    assert_eq!(
        body.delete.returning_true.span.start,
        source.rfind("TrUe").unwrap()
    );
    assert_eq!(body.delete.returning_true.span.end, body_end);
}

#[test]
fn retains_delete_selector_spans_with_trivia() {
    let source = "cReAtE sErVeR fUnCtIoN t.d(p REF t.o) ReTuRnS rOwS (d bOoL) aS dElEtE /* target */ fRoM t.o aS r wHeRe rEf( /* selector */ r /* close */ ) /* equals */ = /* parameter */ p rEtUrNiNg tRuE;";
    let parsed = parse(source);

    assert!(parsed.diagnostics().is_empty());
    assert_eq!(parsed.syntax().text(), source);
    let delete = &parsed.server_functions()[0]
        .body
        .as_sql_delete()
        .expect("the function must have a DELETE body")
        .delete;
    assert_eq!(delete.target_alias.text, "r");
    assert_eq!(delete.selector_alias.text, "r");
    assert_eq!(delete.selector_parameter.text, "p");
    assert_eq!(delete.returning_true.text, "tRuE");
    assert_eq!(
        delete.selector_ref_span,
        SourceSpan {
            start: 103,
            end: 138,
        }
    );
    assert_eq!(
        delete.selector_equality_span,
        SourceSpan {
            start: 103,
            end: 171,
        }
    );
    assert_eq!(
        &source[delete.selector_ref_span.start..delete.selector_ref_span.end],
        "rEf( /* selector */ r /* close */ )"
    );
    assert_eq!(
        &source[delete.selector_equality_span.start..delete.selector_equality_span.end],
        "rEf( /* selector */ r /* close */ ) /* equals */ = /* parameter */ p"
    );
}

#[test]
fn delete_diagnostics_are_exact_and_select_the_offending_source() {
    let cases = [
        (
            "DELETE tasks.task AS deleted_task WHERE REF(deleted_task) = p_task RETURNING TRUE",
            "expected FROM after DELETE in DELETE body",
            "DELETE tasks.task",
            "DELETE ".len(),
            "tasks".len(),
        ),
        (
            "DELETE FROM tasks.task deleted_task WHERE REF(deleted_task) = p_task RETURNING TRUE",
            "expected AS before the DELETE target alias in DELETE body",
            "deleted_task WHERE",
            0,
            "deleted_task".len(),
        ),
        (
            "DELETE FROM tasks.task AS deleted_task RETURNING TRUE",
            "expected WHERE after the DELETE target alias in DELETE body",
            "RETURNING",
            0,
            "RETURNING".len(),
        ),
        (
            "DELETE FROM tasks.task AS deleted_task WHERE deleted_task.id = p_task RETURNING TRUE",
            "expected REF(target_alias) after WHERE in DELETE body",
            "deleted_task.id",
            0,
            "deleted_task".len(),
        ),
        (
            "DELETE FROM tasks.task AS deleted_task WHERE REF(other) = p_task RETURNING TRUE",
            "WHERE REF must use the DELETE target alias deleted_task, not other",
            "REF(other)",
            "REF(".len(),
            "other".len(),
        ),
        (
            "DELETE FROM tasks.task AS deleted_task WHERE REF(deleted_task) p_task RETURNING TRUE",
            "expected '=' after WHERE REF(target_alias) in DELETE body",
            "p_task RETURNING",
            0,
            "p_task".len(),
        ),
        (
            "DELETE FROM tasks.task AS deleted_task WHERE REF(deleted_task) = TRUE RETURNING TRUE",
            "expected a declared REF parameter after '=' in DELETE body",
            "TRUE RETURNING",
            0,
            "TRUE".len(),
        ),
        (
            "DELETE FROM tasks.task AS deleted_task WHERE REF(deleted_task) = owner.p_task RETURNING TRUE",
            "use the selector parameter name by itself after '='; do not add an object or alias",
            "owner.p_task",
            "owner".len(),
            1,
        ),
        (
            "DELETE FROM tasks.task AS deleted_task WHERE REF(deleted_task) = find_task() RETURNING TRUE",
            "this DELETE does not support function calls as DELETE selectors; expected a declared REF parameter name by itself",
            "find_task()",
            "find_task".len(),
            1,
        ),
        (
            "DELETE FROM tasks.task AS deleted_task WHERE REF(deleted_task) = p_task EXTRA",
            "expected RETURNING after the DELETE selector in DELETE body",
            "EXTRA",
            0,
            "EXTRA".len(),
        ),
        (
            "DELETE FROM tasks.task AS deleted_task WHERE REF(deleted_task) = p_task RETURNING FALSE",
            "expected TRUE after RETURNING in DELETE body",
            "FALSE",
            0,
            "FALSE".len(),
        ),
        (
            "DELETE FROM tasks.task AS deleted_task WHERE REF(deleted_task) = p_task RETURNING REF(deleted_task)",
            "expected TRUE after RETURNING in DELETE body",
            "RETURNING REF(deleted_task)",
            "RETURNING ".len(),
            "REF".len(),
        ),
        (
            "DELETE FROM tasks.task AS deleted_task WHERE REF(deleted_task) = p_task RETURNING TRUE EXTRA",
            "this DELETE does not support EXTRA; expected the end of the DELETE body",
            "EXTRA",
            0,
            "EXTRA".len(),
        ),
    ];

    for (body, message, marker, span_offset, span_length) in cases {
        assert_delete_diagnostic(body, message, marker, span_offset, span_length);
    }
}

#[test]
fn malformed_delete_parentheses_do_not_consume_later_declarations() {
    let source = "CREATE SERVER FUNCTION tasks.bad(p_task REF tasks.task) RETURNS ROWS (deleted BOOL) AS DELETE FROM tasks.task AS deleted_task WHERE REF(deleted_task = p_task RETURNING TRUE;
            CREATE SERVER FUNCTION tasks.good(p_task REF tasks.task) RETURNS ROWS (deleted BOOL) AS DELETE FROM tasks.task AS deleted_task WHERE REF(deleted_task) = p_task RETURNING TRUE;";
    let parsed = parse(source);

    assert_eq!(parsed.syntax().text(), source);
    assert_eq!(parsed.server_functions().len(), 1);
    assert_eq!(parsed.server_functions()[0].name.parts[1].text, "good");
    assert!(parsed.server_functions()[0].body.as_sql_delete().is_some());
    assert_eq!(parsed.diagnostics().len(), 1);
    assert_eq!(parsed.diagnostics()[0].code, "ORNA0001");
    assert_eq!(
        parsed.diagnostics()[0].message,
        "expected ')' after the WHERE REF alias in DELETE body"
    );
}
fn assert_insert_diagnostic(
    body: &str,
    message: &str,
    marker: &str,
    span_offset: usize,
    span_length: usize,
) {
    assert_body_diagnostic(
        "p_title TEXT, p_done BOOL",
        "result REF tasks.task",
        body,
        message,
        marker,
        span_offset,
        span_length,
    );
}

fn assert_update_diagnostic(
    body: &str,
    message: &str,
    marker: &str,
    span_offset: usize,
    span_length: usize,
) {
    assert_body_diagnostic(
        "p_task REF tasks.task, p_title TEXT",
        "result REF tasks.task",
        body,
        message,
        marker,
        span_offset,
        span_length,
    );
}

fn assert_delete_diagnostic(
    body: &str,
    message: &str,
    marker: &str,
    span_offset: usize,
    span_length: usize,
) {
    assert_body_diagnostic(
        "p_task REF tasks.task",
        "deleted BOOL",
        body,
        message,
        marker,
        span_offset,
        span_length,
    );
}
