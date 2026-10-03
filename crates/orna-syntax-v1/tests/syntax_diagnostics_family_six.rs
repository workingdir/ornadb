use orna_syntax_v1::{Declaration, Expr, Pattern, StringSegment, parse_module};

#[test]
fn nested_interpolation_recovery_keeps_self_and_compound_suffixes() {
    let source = include_str!("fixtures/nested-case-recovery-self-compound-suffix.orna");
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
        panic!("expected the nested interpolation function");
    };
    let Expr::Block {
        tail: Some(root_tail),
        ..
    } = body
    else {
        panic!("expected the root interpolation tail: {body:?}");
    };
    let Expr::InterpolatedString { segments, .. } = root_tail.as_ref() else {
        panic!("root interpolation was lost: {root_tail:?}");
    };
    let [
        StringSegment::Text { text: prefix, .. },
        StringSegment::Expression {
            value: outer_case, ..
        },
        StringSegment::Text { text: suffix, .. },
    ] = segments.as_slice()
    else {
        panic!("outer interpolation segments changed: {segments:?}");
    };
    assert_eq!(prefix, "before ");
    assert_eq!(suffix, " after");

    let Expr::Control {
        arms: outer_arms, ..
    } = outer_case
    else {
        panic!("outer interpolation expression is not a case: {outer_case:?}");
    };
    assert_eq!(outer_arms.len(), 2, "{outer_arms:?}");
    let Expr::Block {
        tail: Some(inner_tail),
        ..
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
    assert_eq!(inner_arms.len(), 2, "{inner_arms:?}");
    assert!(matches!(&inner_arms[0].pattern, Pattern::Name(name, _) if name == "self"));
    assert!(matches!(&inner_arms[0].body, Expr::Literal { text, .. } if text == "\"self arm\""));
    assert!(matches!(
        &inner_arms[1].pattern,
        Pattern::Constructor { path, fields, .. }
            if path.iter().map(|segment| segment.text.as_str()).collect::<Vec<_>>() == ["Packet"]
                && fields.len() == 2
    ));
    assert!(
        matches!(inner_arms[1].guard.as_ref(), Some(Expr::Name { text, .. }) if text == "flag")
    );

    let Expr::InterpolatedString {
        segments: packet_segments,
        ..
    } = &inner_arms[1].body
    else {
        panic!(
            "compound suffix body lost its interpolation: {:?}",
            inner_arms[1].body
        );
    };
    let [
        StringSegment::Text {
            text: packet_prefix,
            ..
        },
        StringSegment::Expression {
            value: deep_case, ..
        },
    ] = packet_segments.as_slice()
    else {
        panic!("nested suffix interpolation changed: {packet_segments:?}");
    };
    assert_eq!(packet_prefix, "packet ");
    let Expr::Control {
        arms: deep_arms, ..
    } = deep_case
    else {
        panic!("compound suffix lost its nested case body: {deep_case:?}");
    };
    assert_eq!(deep_arms.len(), 2, "{deep_arms:?}");
    assert!(matches!(&outer_arms[1].body, Expr::Literal { text, .. } if text == "\"fallback\""));
}

#[test]
fn nested_guard_recovery_keeps_a_deep_compound_suffix() {
    let source = include_str!("fixtures/nested-case-recovery-compound-suffix-in-guard.orna");
    let parsed = parse_module(source);

    assert_eq!(parsed.diagnostics.len(), 1, "{:?}", parsed.diagnostics);
    let diagnostic = &parsed.diagnostics[0];
    let start = source.find("true 0").expect("malformed guarded case arm") + "true ".len();
    assert_eq!(diagnostic.code, "ORNA-PARSE-001");
    assert_eq!(diagnostic.message, "expected `:` after case pattern");
    assert_eq!(diagnostic.span.start, start, "{diagnostic:?}");
    assert_eq!(diagnostic.span.end, start + 1, "{diagnostic:?}");
    assert!(parsed.is_malformed());
    assert!(!parsed.is_incomplete());

    let Declaration::Function { body, .. } = &parsed.value.items[0].declaration else {
        panic!("expected the nested-guard function");
    };
    let Expr::Block {
        tail: Some(outer_tail),
        ..
    } = body
    else {
        panic!("expected the outer case at the block tail: {body:?}");
    };
    let Expr::Control {
        arms: outer_arms, ..
    } = outer_tail.as_ref()
    else {
        panic!("block tail is not a case: {outer_tail:?}");
    };
    assert_eq!(outer_arms.len(), 2, "{outer_arms:?}");
    let Some(Expr::Control {
        arms: guard_arms, ..
    }) = outer_arms[0].guard.as_ref()
    else {
        panic!("outer arm lost its case guard: {:?}", outer_arms[0].guard);
    };
    let [arm] = guard_arms.as_slice() else {
        panic!("recovered guard suffix is missing: {guard_arms:?}");
    };
    assert!(matches!(
        &arm.pattern,
        Pattern::Constructor { path, arguments, .. }
            if path.iter().map(|segment| segment.text.as_str()).collect::<Vec<_>>() == ["Packet"]
                && arguments.len() == 2
                && matches!(&arguments[0], Pattern::List { elements, .. } if elements.len() == 2)
                && matches!(&arguments[1], Pattern::Constructor { fields, .. } if fields.len() == 1)
    ));
    assert!(matches!(arm.guard.as_ref(), Some(Expr::Name { text, .. }) if text == "flag"));
    assert!(matches!(&arm.body, Expr::Literal { text, .. } if text == "true"));
    assert!(matches!(
        &outer_arms[0].body,
        Expr::Literal { text, .. } if text == "\"nested compound matched\""
    ));
    assert!(matches!(
        &outer_arms[1].body,
        Expr::Literal { text, .. } if text == "\"fallback\""
    ));
}
