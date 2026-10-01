use orna_syntax_v1::{Declaration, Expr, Statement, parse_module};

#[test]
fn nested_case_recovery_reports_local_diagnostics_and_keeps_enclosing_suffix() {
    let source = include_str!("fixtures/nested-case-recovery-diagnostics.orna");
    let parsed = parse_module(source);

    let malformed_arms = [
        ("true 0", "true "),
        ("false 1", "false "),
        ("false 4", "false "),
    ];
    assert_eq!(
        parsed.diagnostics.len(),
        malformed_arms.len(),
        "{:?}",
        parsed.diagnostics
    );
    for (diagnostic, (arm, prefix)) in parsed.diagnostics.iter().zip(malformed_arms) {
        let start = source.find(arm).expect("malformed nested arm") + prefix.len();
        assert_eq!(diagnostic.code, "ORNA-PARSE-001");
        assert_eq!(diagnostic.message, "expected `:` after case pattern");
        assert_eq!(diagnostic.span.start, start, "{diagnostic:?}");
        assert_eq!(diagnostic.span.end, start + 1, "{diagnostic:?}");
    }
    assert!(parsed.is_malformed());
    assert!(!parsed.is_incomplete());

    let Declaration::Function { body, .. } = &parsed.value.items[0].declaration else {
        panic!("expected the nested-recovery function");
    };
    let Expr::Block {
        tail: Some(outer_tail),
        ..
    } = body
    else {
        panic!("expected the root block: {body:?}");
    };
    let Expr::Control {
        arms: outer_arms, ..
    } = outer_tail.as_ref()
    else {
        panic!("outer case was lost: {body:?}");
    };
    assert_eq!(outer_arms.len(), 2, "{outer_arms:?}");
    assert!(matches!(&outer_arms[1].body, Expr::Literal { text, .. } if text == "5"));

    let Expr::Control {
        arms: nested_arms, ..
    } = &outer_arms[0].body
    else {
        panic!("nested case was lost: {:?}", outer_arms[0].body);
    };
    assert_eq!(nested_arms.len(), 2, "{nested_arms:?}");
    assert!(matches!(&nested_arms[1].body, Expr::Literal { text, .. } if text == "3"));

    let Expr::Control {
        arms: deepest_arms, ..
    } = &nested_arms[0].body
    else {
        panic!(
            "deep case after the first recovery was lost: {:?}",
            nested_arms[0].body
        );
    };
    assert_eq!(deepest_arms.len(), 1, "{deepest_arms:?}");
    assert!(matches!(&deepest_arms[0].body, Expr::Literal { text, .. } if text == "2"));

    // The reference specifies case-arm syntax, but leaves malformed nested
    // recovery trees undefined. Drop each bad arm, report its colon span, and
    // continue through the enclosing valid suffixes.
}

#[test]
fn nested_case_recovery_reports_each_unclosed_boundary_at_eof() {
    let source = include_str!("fixtures/incomplete-nested-case-recovery-diagnostics.orna");
    let parsed = parse_module(source);

    let expected = [
        ("ORNA-PARSE-001", "expected `:` after case pattern"),
        ("ORNA-PARSE-003", "unterminated case arms"),
        ("ORNA-PARSE-003", "unterminated case arms"),
        ("ORNA-PARSE-003", "unterminated block"),
    ];
    assert_eq!(
        parsed.diagnostics.len(),
        expected.len(),
        "{:?}",
        parsed.diagnostics
    );
    for (index, (diagnostic, (code, message))) in
        parsed.diagnostics.iter().zip(expected).enumerate()
    {
        assert_eq!(diagnostic.code, code, "diagnostic {index}: {diagnostic:?}");
        assert_eq!(
            diagnostic.message, message,
            "diagnostic {index}: {diagnostic:?}"
        );
        let (start, end) = if index == 0 {
            let start = source.find("true 0").expect("malformed inner arm") + "true ".len();
            (start, start + 1)
        } else {
            (source.len(), source.len())
        };
        assert_eq!(
            diagnostic.span.start, start,
            "diagnostic {index}: {diagnostic:?}"
        );
        assert_eq!(
            diagnostic.span.end, end,
            "diagnostic {index}: {diagnostic:?}"
        );
    }
    assert!(parsed.is_malformed());
    assert!(!parsed.is_incomplete());

    let Declaration::Function { body, .. } = &parsed.value.items[0].declaration else {
        panic!("expected the incomplete function");
    };
    let Expr::Block {
        statements,
        tail: None,
        ..
    } = body
    else {
        panic!("outer block lost its recovered statements: {body:?}");
    };
    let [
        Statement::Control {
            value: Expr::Control {
                arms: outer_arms, ..
            },
            ..
        },
    ] = statements.as_slice()
    else {
        panic!("outer case was lost: {statements:?}");
    };
    assert_eq!(outer_arms.len(), 1, "{outer_arms:?}");
    let Expr::Control {
        arms: inner_arms, ..
    } = &outer_arms[0].body
    else {
        panic!("inner case was lost: {:?}", outer_arms[0].body);
    };
    assert_eq!(inner_arms.len(), 1, "{inner_arms:?}");
    assert!(matches!(&inner_arms[0].body, Expr::Literal { text, .. } if text == "1"));

    // A definite syntax error keeps the aggregate result malformed even when
    // nested constructs also report their EOF boundaries.
    // The reference specifies case-arm syntax, not recovery diagnostics for
    // multiple open cases at EOF. Keep one diagnostic per open boundary.
}

#[test]
fn nested_guard_recovery_does_not_cascade_to_the_enclosing_case_arm() {
    let source = include_str!("fixtures/nested-case-recovery-in-guard.orna");
    let parsed = parse_module(source);

    assert_eq!(parsed.diagnostics.len(), 1, "{:?}", parsed.diagnostics);
    let diagnostic = &parsed.diagnostics[0];
    let start = source.find("true 0").expect("malformed nested guard arm") + "true ".len();
    assert_eq!(diagnostic.code, "ORNA-PARSE-001");
    assert_eq!(diagnostic.message, "expected `:` after case pattern");
    assert_eq!(diagnostic.span.start, start, "{diagnostic:?}");
    assert_eq!(diagnostic.span.end, start + 1, "{diagnostic:?}");
    assert!(parsed.is_malformed());
    assert!(!parsed.is_incomplete());

    let Declaration::Function { body, .. } = &parsed.value.items[0].declaration else {
        panic!("expected the recovered guard function");
    };
    let Expr::Block {
        tail: Some(root_tail),
        ..
    } = body
    else {
        panic!("root case was lost: {body:?}");
    };
    let Expr::Control {
        arms: root_arms, ..
    } = root_tail.as_ref()
    else {
        panic!("root tail is not a case: {root_tail:?}");
    };
    assert_eq!(root_arms.len(), 2, "{root_arms:?}");
    let Some(Expr::Control {
        arms: guard_arms, ..
    }) = root_arms[0].guard.as_ref()
    else {
        panic!("nested case guard was lost: {:?}", root_arms[0].guard);
    };
    assert_eq!(guard_arms.len(), 1, "{guard_arms:?}");
    assert!(matches!(&root_arms[0].body, Expr::Literal { text, .. } if text == "1"));
    assert!(matches!(&root_arms[1].body, Expr::Literal { text, .. } if text == "2"));

    // The reference specifies guard syntax, but not recovery inside a nested
    // guard expression. Keep that local error from fabricating an outer one.
}
