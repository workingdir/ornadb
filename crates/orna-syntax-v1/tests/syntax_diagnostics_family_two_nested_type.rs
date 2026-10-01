use orna_syntax_v1::{Declaration, Expr, StringSegment, parse_module};

#[test]
fn nested_type_trailing_comma_keeps_annotated_interpolated_closure_tail() {
    let source = include_str!("fixtures/trailing-nested-type-in-interpolated-closure.orna");
    let parsed = parse_module(source);

    assert_eq!(parsed.diagnostics.len(), 1, "{:?}", parsed.diagnostics);
    let diagnostic = &parsed.diagnostics[0];
    assert_eq!(diagnostic.code, "ORNA-PARSE-001");
    assert_eq!(
        diagnostic.message,
        "trailing commas are not allowed in generic type arguments"
    );
    let closing_angle = source.find("Int,").expect("nested trailing comma") + "Int,".len();
    assert_eq!(diagnostic.span.start, closing_angle);
    assert_eq!(diagnostic.span.end, closing_angle + 1);
    assert_eq!(parsed.value.items.len(), 2, "later declaration was lost");
    assert!(matches!(
        &parsed.value.items[1].declaration,
        Declaration::Function { .. }
    ));

    let Declaration::Function { body, .. } = &parsed.value.items[0].declaration else {
        panic!("expected the recovered function");
    };
    let Expr::InterpolatedString { segments, .. } = body else {
        panic!("outer interpolated string was lost: {body:?}");
    };
    let [
        StringSegment::Text { text: prefix, .. },
        StringSegment::Expression {
            value: Expr::Lambda { body: closure, .. },
            ..
        },
        StringSegment::Text { text: suffix, .. },
    ] = segments.as_slice()
    else {
        panic!("recovery lost the annotated closure or string tail: {segments:?}");
    };
    assert_eq!(prefix, "prefix ");
    assert_eq!(suffix, " suffix");
    let Expr::Block {
        tail: Some(closure_tail),
        ..
    } = closure.as_ref()
    else {
        panic!("annotated closure lost its case tail: {closure:?}");
    };
    let Expr::Control { arms, .. } = closure_tail.as_ref() else {
        panic!("annotated closure tail is not a case: {closure_tail:?}");
    };
    assert_eq!(arms.len(), 2, "closure case arms were lost: {arms:?}");

    // The reference does not define the recovery tree for a rejected trailing
    // generic-type comma. Keep the interpolation close and following text.
}
