use orna_syntax_v1::{Declaration, Expr, StringSegment, parse_module};

#[test]
fn nested_case_recovery_inside_interpolation_keeps_suffix_arms_and_text() {
    let source = include_str!("fixtures/nested-case-recovery-inside-interpolation.orna");
    let parsed = parse_module(source);

    assert_eq!(parsed.diagnostics.len(), 1, "{:?}", parsed.diagnostics);
    let diagnostic = &parsed.diagnostics[0];
    let start = source.find("true 0").expect("malformed inner case arm") + "true ".len();
    assert_eq!(diagnostic.code, "ORNA-PARSE-001");
    assert_eq!(diagnostic.message, "expected `:` after case pattern");
    assert_eq!(diagnostic.span.start, start, "{diagnostic:?}");
    assert_eq!(diagnostic.span.end, start + 1, "{diagnostic:?}");
    assert!(parsed.is_malformed());
    assert!(!parsed.is_incomplete());

    let Declaration::Function { body, .. } = &parsed.value.items[0].declaration else {
        panic!("expected the interpolation function");
    };
    let Expr::Block {
        statements,
        tail: Some(root_tail),
        ..
    } = body
    else {
        panic!("expected a string expression at the block tail: {body:?}");
    };
    assert!(statements.is_empty(), "{statements:?}");
    let Expr::InterpolatedString {
        segments: outer_segments,
        ..
    } = root_tail.as_ref()
    else {
        panic!("outer interpolated string was lost: {root_tail:?}");
    };
    let [
        StringSegment::Text { text: prefix, .. },
        StringSegment::Expression {
            value: outer_case, ..
        },
        StringSegment::Text { text: suffix, .. },
    ] = outer_segments.as_slice()
    else {
        panic!("outer interpolation boundaries or text were lost: {outer_segments:?}");
    };
    assert_eq!(prefix, "λ🐑 before ");
    assert_eq!(suffix, " after");

    let Expr::Control {
        arms: outer_arms, ..
    } = outer_case
    else {
        panic!("outer interpolation expression is not a case: {outer_case:?}");
    };
    assert_eq!(outer_arms.len(), 2, "{outer_arms:?}");
    assert!(matches!(&outer_arms[1].body, Expr::Literal { text, .. } if text == "\"fallback\""));

    let Expr::InterpolatedString {
        segments: nested_segments,
        ..
    } = &outer_arms[0].body
    else {
        panic!("nested interpolation string was lost: {:?}", outer_arms[0].body);
    };
    let [
        StringSegment::Text { text: nested_prefix, .. },
        StringSegment::Expression {
            value: nested_case, ..
        },
        StringSegment::Text { text: nested_suffix, .. },
    ] = nested_segments.as_slice()
    else {
        panic!("nested interpolation boundaries or text were lost: {nested_segments:?}");
    };
    assert_eq!(nested_prefix, "inner ");
    assert_eq!(nested_suffix, " middle");

    let Expr::Control {
        arms: nested_arms, ..
    } = nested_case
    else {
        panic!("inner interpolation expression is not a case: {nested_case:?}");
    };
    assert_eq!(nested_arms.len(), 1, "{nested_arms:?}");
    assert!(matches!(&nested_arms[0].body, Expr::Literal { text, .. } if text == "1"));

    // The reference specifies interpolation syntax, not recovery inside a
    // nested case expression. Keep local diagnostics and preserve both tails.
}
