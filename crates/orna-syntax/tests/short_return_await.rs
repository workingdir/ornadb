use orna_syntax::{ClientExpression, ClientFunctionBody, SourceSpan, parse};

const SOURCE: &str =
    "CREATE CLIENT FUNCTION examples.awaited() RETURNS TEXT RETURN AWAIT std.data.resource();";

#[test]
fn parses_short_return_await_resource_losslessly_with_public_spans() {
    let parsed = parse(SOURCE);

    assert!(
        parsed.diagnostics().is_empty(),
        "{:?}",
        parsed.diagnostics()
    );
    assert_eq!(parsed.syntax().text(), SOURCE);
    assert_eq!(parsed.client_functions().len(), 1);

    let client = &parsed.client_functions()[0];
    assert_eq!(client.name.parts.len(), 2);
    assert_eq!(client.name.parts[0].text, "examples");
    assert_eq!(client.name.parts[1].text, "awaited");
    assert_eq!(&SOURCE[client.span.start..client.span.end], SOURCE);
    assert!(matches!(
        &client.body,
        ClientFunctionBody::ReturnExpression { .. }
    ));
    let expression = client
        .body
        .as_expression()
        .expect("expected the short CLIENT RETURN body expression");

    let ClientExpression::Await {
        expression: awaited,
        span: await_span,
    } = expression
    else {
        panic!("expected RETURN expression to be AWAIT");
    };

    let await_start = SOURCE.find("AWAIT").expect("AWAIT keyword");
    let resource_start = SOURCE
        .find("std.data.resource")
        .expect("resource constructor expression");
    let expression_end = SOURCE.rfind(");").expect("resource call terminator") + 1;
    assert_eq!(
        await_span,
        &SourceSpan {
            start: await_start,
            end: expression_end,
        }
    );
    assert_eq!(
        &SOURCE[await_span.start..await_span.end],
        "AWAIT std.data.resource()"
    );

    let ClientExpression::Call {
        callee,
        span: resource_span,
        ..
    } = awaited.as_ref()
    else {
        panic!("expected AWAIT to wrap the resource call");
    };
    assert_eq!(
        resource_span,
        &SourceSpan {
            start: resource_start,
            end: expression_end,
        }
    );
    assert_eq!(
        &SOURCE[resource_span.start..resource_span.end],
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
