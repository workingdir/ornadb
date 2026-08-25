use orna_syntax::{ClientExpression, ClientProceduralStatement, parse};

const ACCEPTED_SOURCE: &str = "CREATE SERVER FUNCTION tasks.list(p_title TEXT)\n\
RETURNS ROWS (title TEXT)\n\
AS SELECT t.title FROM tasks.task t WHERE t.title = p_title;\n\
CREATE CLIENT FUNCTION examples.procedural() RETURNS INTEGER IS\n\
BEGIN\n\
    LET value := AWAIT std.data.resource();\n\
    RETURN value;\n\
END;";

#[test]
fn accepts_server_query_and_client_procedural_shapes_through_public_api() {
    let parsed = parse(ACCEPTED_SOURCE);

    assert!(
        parsed.diagnostics().is_empty(),
        "{:?}",
        parsed.diagnostics()
    );
    assert_eq!(parsed.syntax().text(), ACCEPTED_SOURCE);
    assert_eq!(parsed.server_functions().len(), 1);
    assert_eq!(parsed.client_functions().len(), 1);

    let server = &parsed.server_functions()[0];
    assert_eq!(server.name.parts[0].text, "tasks");
    assert_eq!(server.name.parts[1].text, "list");
    let query = server
        .body
        .as_sql_query()
        .expect("accepted SERVER function should retain its SELECT body");
    assert_eq!(query.query.source_object.alias.text, "t");

    let client = &parsed.client_functions()[0];
    let block = client
        .body
        .as_state_block()
        .expect("accepted CLIENT procedural function should retain its block");
    assert_eq!(block.statements.len(), 1);
    assert!(matches!(
        &block.statements[0],
        ClientProceduralStatement::Let(statement)
            if statement.name.text == "value"
                && matches!(&statement.expression, ClientExpression::Await { .. })
    ));
    assert!(matches!(
        block.return_expression.as_ref(),
        Some(ClientExpression::LocalRead { local }) if local.text == "value"
    ));
}

#[test]
fn rejects_proposal_only_table_return_shape() {
    let source = "CREATE SERVER FUNCTION tasks.table_result() RETURNS TABLE (task REF tasks.task) AS SELECT REF(t) FROM tasks.task t;";
    let parsed = parse(source);

    assert_eq!(parsed.syntax().text(), source);
    assert!(parsed.server_functions().is_empty());
    assert!(!parsed.diagnostics().is_empty());
}

#[test]
fn rejects_client_procedural_plsql_exception_tail() {
    let source = "CREATE CLIENT FUNCTION examples.procedural() RETURNS INTEGER IS\n\
BEGIN\n\
    RETURN 1;\n\
EXCEPTION\n\
    WHEN OTHERS THEN\n\
END;";
    let parsed = parse(source);

    assert_eq!(parsed.syntax().text(), source);
    assert_eq!(parsed.diagnostics().len(), 1, "{:?}", parsed.diagnostics());
    assert_eq!(parsed.diagnostics()[0].code, "ORNA0001");
    assert_eq!(parsed.diagnostics()[0].message, "expected keyword END");
    let exception_start = source.find("EXCEPTION").unwrap();
    assert_eq!(parsed.diagnostics()[0].span.start, exception_start);
    assert_eq!(
        parsed.diagnostics()[0].span.end,
        exception_start + "EXCEPTION".len()
    );
    assert!(parsed.client_functions().is_empty());
}
