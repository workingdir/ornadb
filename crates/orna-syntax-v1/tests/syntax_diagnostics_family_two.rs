use orna_syntax_v1::{Declaration, Expr, StringSegment, parse_module};

fn assert_trailing_comma_diagnostic(source: &str, marker: &str) {
    let parsed = parse_module(source);
    assert_eq!(parsed.diagnostics.len(), 1, "{:?}", parsed.diagnostics);
    let diagnostic = &parsed.diagnostics[0];
    assert_eq!(diagnostic.code, "ORNA-PARSE-001");
    assert_eq!(
        diagnostic.message,
        "trailing commas are not allowed in generic type arguments"
    );
    let closing_angle = source.find(marker).expect("generic trailing comma marker") + marker.len();
    assert_eq!(diagnostic.span.start, closing_angle);
    assert_eq!(diagnostic.span.end, closing_angle + 1);
    assert_eq!(parsed.value.items.len(), 2, "later declaration was lost");
    assert!(matches!(
        &parsed.value.items[1].declaration,
        Declaration::Function { .. }
    ));
}

#[test]
fn generic_call_trailing_comma_keeps_interpolation_closure_and_text_tails() {
    let source = include_str!("fixtures/trailing-generic-call-in-interpolated-closure.orna");
    let parsed = parse_module(source);
    assert_trailing_comma_diagnostic(source, "<Int,");

    let Declaration::Function { body, .. } = &parsed.value.items[0].declaration else {
        panic!("expected the recovered function");
    };
    let Expr::InterpolatedString { segments, .. } = body else {
        panic!("outer interpolated string was lost: {body:?}");
    };
    let [
        StringSegment::Text { text: head, .. },
        StringSegment::Expression {
            value: Expr::Call { .. },
            ..
        },
        StringSegment::Text { text: middle, .. },
        StringSegment::Expression {
            value: Expr::Lambda { body: closure, .. },
            ..
        },
        StringSegment::Text { text: tail, .. },
    ] = segments.as_slice()
    else {
        panic!("recovery lost an interpolation boundary or text tail: {segments:?}");
    };
    assert_eq!(head, "head ");
    assert_eq!(middle, " middle ");
    assert_eq!(tail, " tail");
    let Expr::Block {
        tail: Some(closure_tail),
        ..
    } = closure.as_ref()
    else {
        panic!("interpolated closure lost its case tail: {closure:?}");
    };
    let Expr::Control { arms, .. } = closure_tail.as_ref() else {
        panic!("interpolated closure tail is not a case: {closure_tail:?}");
    };
    assert_eq!(arms.len(), 2, "closure case arms were lost: {arms:?}");

    // The reference specifies interpolation delimiters, but not the recovery
    // tree after a rejected generic-call trailing comma. Keep later segments.
}
