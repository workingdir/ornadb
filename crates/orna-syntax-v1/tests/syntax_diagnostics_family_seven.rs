use orna_syntax_v1::{Declaration, Expr, Pattern, StringSegment, parse_module};

fn assert_nested_list_pattern(pattern: &Pattern, remaining: usize) {
    if remaining == 0 {
        assert!(matches!(pattern, Pattern::Literal { text, .. } if text == "false"));
        return;
    }

    let Pattern::List { elements, .. } = pattern else {
        panic!("expected another list-pattern level ({remaining} left): {pattern:?}");
    };
    let [element] = elements.as_slice() else {
        panic!("each nested list level should contain one element: {elements:?}");
    };
    assert_nested_list_pattern(element, remaining - 1);
}

#[test]
fn interpolated_recovery_keeps_a_near_limit_compound_suffix() {
    let source = include_str!("fixtures/deep-compound-implicit-suffix.orna");
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
        panic!("expected the deep-nesting function");
    };
    let Expr::Block {
        tail: Some(root_tail),
        ..
    } = body
    else {
        panic!("expected interpolation at the root block tail: {body:?}");
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
    assert_eq!(prefix, "prefix ");
    assert_eq!(suffix, " suffix");

    let Expr::Control {
        arms: outer_arms, ..
    } = outer_case
    else {
        panic!("interpolation expression is not a case: {outer_case:?}");
    };
    assert_eq!(outer_arms.len(), 2, "{outer_arms:?}");
    assert!(matches!(
        &outer_arms[1].body,
        Expr::Literal { text, .. } if text == "\"outer fallback\""
    ));

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
    let [suffix_arm] = inner_arms.as_slice() else {
        panic!("deep compound suffix was lost: {inner_arms:?}");
    };
    assert_nested_list_pattern(&suffix_arm.pattern, 48);
    assert!(matches!(
        suffix_arm.guard.as_ref(),
        Some(Expr::Name { text, .. }) if text == "flag"
    ));

    let Expr::Block {
        tail: Some(deep_tail),
        ..
    } = &suffix_arm.body
    else {
        panic!(
            "deep suffix body lost its block tail: {:?}",
            suffix_arm.body
        );
    };
    let Expr::Control {
        arms: deep_arms, ..
    } = deep_tail.as_ref()
    else {
        panic!("deep suffix block lost its case: {deep_tail:?}");
    };
    assert_eq!(deep_arms.len(), 2, "{deep_arms:?}");
    assert!(matches!(
        &deep_arms[0].body,
        Expr::Literal { text, .. } if text == "\"deep match\""
    ));
    assert!(matches!(
        &deep_arms[1].body,
        Expr::Literal { text, .. } if text == "\"deep fallback\""
    ));
}

#[test]
fn deep_case_chain_keeps_compound_suffix_and_enclosing_fallbacks() {
    let source = include_str!("fixtures/deep-nested-case-compound-suffix.orna");
    let parsed = parse_module(source);

    assert_eq!(parsed.diagnostics.len(), 1, "{:?}", parsed.diagnostics);
    let diagnostic = &parsed.diagnostics[0];
    let start = source.find("true 0").expect("malformed deepest arm") + "true ".len();
    assert_eq!(diagnostic.code, "ORNA-PARSE-001");
    assert_eq!(diagnostic.message, "expected `:` after case pattern");
    assert_eq!(diagnostic.span.start, start, "{diagnostic:?}");
    assert_eq!(diagnostic.span.end, start + 1, "{diagnostic:?}");
    assert!(parsed.is_malformed());
    assert!(!parsed.is_incomplete());

    let Declaration::Function { body, .. } = &parsed.value.items[0].declaration else {
        panic!("expected the deep case-chain function");
    };
    let Expr::Block {
        tail: Some(root_case),
        ..
    } = body
    else {
        panic!("expected a case expression at the function tail: {body:?}");
    };

    let mut current_case = root_case.as_ref();
    for level in 0..16 {
        let Expr::Control { arms, .. } = current_case else {
            panic!("case chain ended at level {level}: {current_case:?}");
        };
        assert_eq!(arms.len(), 2, "level {level}: {arms:?}");

        if level == 15 {
            assert!(matches!(
                &arms[0].pattern,
                Pattern::Constructor { path, fields, .. }
                    if path.iter().map(|segment| segment.text.as_str()).collect::<Vec<_>>() == ["Deep"]
                        && fields.len() == 2
            ));
            assert!(matches!(
                arms[0].guard.as_ref(),
                Some(Expr::Name { text, .. }) if text == "flag"
            ));
            assert!(matches!(
                &arms[0].body,
                Expr::Literal { text, .. } if text == "\"deep recovered\""
            ));
            assert!(matches!(
                &arms[1].body,
                Expr::Literal { text, .. } if text == "\"deep fallback\""
            ));
        } else {
            let expected_fallback = format!("\"level {level} fallback\"");
            assert!(matches!(
                &arms[1].body,
                Expr::Literal { text, .. } if text == &expected_fallback
            ));
            current_case = &arms[0].body;
        }
    }
}
