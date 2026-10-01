use orna_syntax_v1::{Declaration, Expr, Pattern, StringSegment, parse_module};

#[test]
fn nested_block_recovery_keeps_a_guarded_list_pattern_suffix() {
    let source = include_str!("fixtures/nested-case-recovery-guarded-list-suffix.orna");
    let parsed = parse_module(source);

    assert_eq!(parsed.diagnostics.len(), 1, "{:?}", parsed.diagnostics);
    let diagnostic = &parsed.diagnostics[0];
    let start = source.find("true 0").expect("malformed nested arm") + "true ".len();
    assert_eq!(diagnostic.code, "ORNA-PARSE-001");
    assert_eq!(diagnostic.message, "expected `:` after case pattern");
    assert_eq!(diagnostic.span.start, start, "{diagnostic:?}");
    assert_eq!(diagnostic.span.end, start + 1, "{diagnostic:?}");
    assert!(parsed.is_malformed());
    assert!(!parsed.is_incomplete());

    let Declaration::Function { body, .. } = &parsed.value.items[0].declaration else {
        panic!("expected the nested-form function");
    };
    let Expr::Block {
        tail: Some(root_tail), ..
    } = body
    else {
        panic!("expected a string expression at the block tail: {body:?}");
    };
    let Expr::InterpolatedString { segments, .. } = root_tail.as_ref() else {
        panic!("outer interpolated string was lost: {root_tail:?}");
    };
    let [
        StringSegment::Text { text: prefix, .. },
        StringSegment::Expression {
            value: outer_case, ..
        },
        StringSegment::Text { text: suffix, .. },
    ] = segments.as_slice()
    else {
        panic!("outer interpolation boundaries or text were lost: {segments:?}");
    };
    assert_eq!(prefix, "λ before ");
    assert_eq!(suffix, " after");

    let Expr::Control {
        arms: outer_arms, ..
    } = outer_case
    else {
        panic!("outer interpolation expression is not a case: {outer_case:?}");
    };
    assert_eq!(outer_arms.len(), 2, "{outer_arms:?}");
    assert!(matches!(&outer_arms[1].body, Expr::Literal { text, .. } if text == "\"fallback\""));

    let Expr::Block {
        tail: Some(inner_tail), ..
    } = &outer_arms[0].body
    else {
        panic!("outer case lost its nested block: {:?}", outer_arms[0].body);
    };
    let Expr::Control {
        arms: inner_arms, ..
    } = inner_tail.as_ref()
    else {
        panic!("nested block lost its case tail: {inner_tail:?}");
    };
    let [arm] = inner_arms.as_slice() else {
        panic!("compound suffix arm was lost: {inner_arms:?}");
    };
    assert!(matches!(&arm.pattern, Pattern::List { elements, .. } if elements.len() == 2));
    assert!(arm.guard.is_some(), "guard was lost: {arm:?}");
    assert!(matches!(&arm.body, Expr::Literal { text, .. } if text == "\"winner\""));

    // Recovery accepts a later composite pattern only when its parsed header
    // (including a guard) reaches the arm colon without diagnostics.
}

#[test]
fn nested_record_suffix_uses_its_outer_arm_colon() {
    let source = include_str!("fixtures/nested-case-recovery-record-suffix.orna");
    let parsed = parse_module(source);

    assert_eq!(parsed.diagnostics.len(), 1, "{:?}", parsed.diagnostics);
    let diagnostic = &parsed.diagnostics[0];
    let start = source.find("true 0").expect("malformed case arm") + "true ".len();
    assert_eq!(diagnostic.code, "ORNA-PARSE-001");
    assert_eq!(diagnostic.message, "expected `:` after case pattern");
    assert_eq!(diagnostic.span.start, start, "{diagnostic:?}");
    assert_eq!(diagnostic.span.end, start + 1, "{diagnostic:?}");

    let Declaration::Function { body, .. } = &parsed.value.items[0].declaration else {
        panic!("expected the record-pattern function");
    };
    let Expr::Block {
        tail: Some(tail), ..
    } = body
    else {
        panic!("expected the case expression at the block tail: {body:?}");
    };
    let Expr::Control { arms, .. } = tail.as_ref() else {
        panic!("block tail is not a case: {tail:?}");
    };
    let [arm] = arms.as_slice() else {
        panic!("record suffix arm was lost: {arms:?}");
    };
    assert!(matches!(&arm.pattern, Pattern::Constructor { fields, .. } if fields.len() == 1));
    assert!(matches!(&arm.body, Expr::Literal { text, .. } if text == "7"));

    // The colon nested inside a record pattern is not the case-arm separator.
}
