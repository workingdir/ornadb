use orna_syntax_v1::{
    Declaration, Expr, Pattern, Statement, SyntaxDiagnostic, parse_expression, parse_module,
    parse_repl, parse_row,
};

fn assert_postfix_question(diagnostics: &[SyntaxDiagnostic], source: &str) {
    assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
    let diagnostic = &diagnostics[0];
    assert_eq!(diagnostic.code, "ORNA091-E-POSTFIX-QUESTION");
    let question = source.find('?').expect("fixture has a question mark");
    assert_eq!(diagnostic.span.start, question);
    assert_eq!(diagnostic.span.end, question + 1);
}

#[test]
fn postfix_question_code_and_span_are_stable_across_expression_entrypoints() {
    let expression = include_str!("fixtures/postfix-question-expression.orna");
    assert_postfix_question(&parse_expression(expression).diagnostics, expression);
    assert_postfix_question(&parse_repl(expression).diagnostics, expression);

    let row = include_str!("fixtures/postfix-question-row.orna");
    assert_postfix_question(&parse_row(row).diagnostics, row);
}

#[test]
fn missing_control_statement_terminator_uses_parse_002_at_the_closing_brace() {
    let source = include_str!("fixtures/missing-control-statement-terminator.orna");
    let parsed = parse_module(source);

    assert_eq!(parsed.diagnostics.len(), 1, "{:?}", parsed.diagnostics);
    let diagnostic = &parsed.diagnostics[0];
    assert_eq!(diagnostic.code, "ORNA-PARSE-002");
    assert_eq!(diagnostic.message, "expected `;` after control statement");
    let closing_brace = source.rfind('}').expect("fixture has a closing brace");
    assert_eq!(diagnostic.span.start, closing_brace);
    assert_eq!(diagnostic.span.end, closing_brace + 1);
    assert!(parsed.is_malformed());
}

#[test]
fn unterminated_block_uses_parse_003_and_remains_incomplete_at_eof() {
    let source = include_str!("fixtures/incomplete-block.orna");
    let parsed = parse_module(source);

    assert_eq!(parsed.diagnostics.len(), 1, "{:?}", parsed.diagnostics);
    let diagnostic = &parsed.diagnostics[0];
    assert_eq!(diagnostic.code, "ORNA-PARSE-003");
    assert_eq!(diagnostic.message, "unterminated block");
    assert_eq!(diagnostic.span.start, source.len());
    assert_eq!(diagnostic.span.end, source.len());
    assert!(parsed.is_incomplete());
    assert!(!parsed.is_malformed());
}

#[test]
fn final_unsemicolonated_control_is_the_block_value() {
    let parsed = parse_module(include_str!("fixtures/control-expression-block-tail.orna"));
    assert!(parsed.is_ok(), "{:?}", parsed.diagnostics);
    let Declaration::Function { body, .. } = &parsed.value.items[0].declaration else {
        panic!("expected a function declaration");
    };
    let Expr::Block { statements, tail, .. } = body else {
        panic!("expected a function block");
    };
    assert!(statements.is_empty(), "{statements:?}");
    assert!(matches!(tail.as_deref(), Some(Expr::Control { .. })), "{tail:?}");
}

#[test]
fn control_tail_keeps_postfix_field_continuation() {
    let parsed = parse_module(include_str!("fixtures/control-tail-postfix-field.orna"));
    assert!(parsed.is_ok(), "{:?}", parsed.diagnostics);
    let Declaration::Function { body, .. } = &parsed.value.items[0].declaration else {
        panic!("expected a function declaration");
    };
    let Expr::Block { tail, .. } = body else {
        panic!("expected a function block");
    };
    assert!(matches!(
        tail.as_deref(),
        Some(Expr::Field { base, name, .. })
            if name == "value" && matches!(base.as_ref(), Expr::Control { .. })
    ));
}

#[test]
fn malformed_control_continuation_keeps_following_tail_and_primary_diagnostics() {
    let source = include_str!("fixtures/control-continuation-missing-separator.orna");
    let parsed = parse_module(source);

    assert_eq!(parsed.diagnostics.len(), 2, "{:?}", parsed.diagnostics);
    assert_eq!(
        parsed
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.code)
            .collect::<Vec<_>>(),
        ["ORNA091-E-POSTFIX-QUESTION", "ORNA-PARSE-002"]
    );
    let question = source.find('?').expect("fixture has a postfix question");
    assert_eq!(parsed.diagnostics[0].span.start, question);
    let missing_separator = source.find(" 2;").expect("fixture has a following item") + 1;
    assert_eq!(parsed.diagnostics[1].span.start, missing_separator);
    assert_eq!(parsed.diagnostics[1].span.end, missing_separator + 1);
    assert!(parsed.is_malformed());

    let Declaration::Function { body, .. } = &parsed.value.items[0].declaration else {
        panic!("expected a function declaration");
    };
    let Expr::Block {
        statements, tail, ..
    } = body
    else {
        panic!("expected a function block");
    };
    assert_eq!(statements.len(), 2, "{statements:?}");
    assert!(matches!(
        statements.get(1),
        Some(Statement::Expression {
            value: Expr::Literal { text, .. },
            ..
        }) if text == "2"
    ));
    assert!(matches!(tail.as_deref(), Some(Expr::Control { .. })), "{tail:?}");
}

#[test]
fn malformed_case_arm_recovers_at_case_boundary_and_preserves_outer_tail() {
    let source = include_str!("fixtures/malformed-case-arm-outer-tail.orna");
    let parsed = parse_module(source);

    assert_eq!(parsed.diagnostics.len(), 1, "{:?}", parsed.diagnostics);
    let diagnostic = &parsed.diagnostics[0];
    assert_eq!(diagnostic.code, "ORNA-PARSE-001");
    assert_eq!(diagnostic.message, "expected `:` after case pattern");
    let bad_pattern_separator = source.find(" 2 if").expect("fixture has a malformed arm") + 1;
    assert_eq!(diagnostic.span.start, bad_pattern_separator);
    assert_eq!(diagnostic.span.end, bad_pattern_separator + 1);
    assert!(parsed.is_malformed());

    let Declaration::Function { body, .. } = &parsed.value.items[0].declaration else {
        panic!("expected a function declaration");
    };
    let Expr::Block {
        statements, tail, ..
    } = body
    else {
        panic!("expected a function block");
    };
    assert!(matches!(
        statements.as_slice(),
        [Statement::Control {
            value: Expr::Control { .. },
            ..
        }]
    ));
    assert!(matches!(
        tail.as_deref(),
        Some(Expr::Literal { text, .. }) if text == "4"
    ));
}

#[test]
fn malformed_case_arm_recovers_at_top_level_comma_and_keeps_later_arm() {
    let source = include_str!("fixtures/malformed-case-arm-resumes-after-comma.orna");
    let parsed = parse_module(source);

    assert_eq!(parsed.diagnostics.len(), 1, "{:?}", parsed.diagnostics);
    let diagnostic = &parsed.diagnostics[0];
    assert_eq!(diagnostic.code, "ORNA-PARSE-001");
    assert_eq!(diagnostic.message, "expected `:` after case pattern");
    let bad_pattern_separator = source
        .find("{ value")
        .expect("fixture has a malformed arm");
    assert_eq!(diagnostic.span.start, bad_pattern_separator);
    assert_eq!(diagnostic.span.end, bad_pattern_separator + 1);

    let Declaration::Function { body, .. } = &parsed.value.items[0].declaration else {
        panic!("expected a function declaration");
    };
    let Expr::Block {
        statements, tail, ..
    } = body
    else {
        panic!("expected a function block");
    };
    assert!(statements.is_empty(), "{statements:?}");
    let Some(Expr::Control { arms, .. }) = tail.as_deref() else {
        panic!("expected a case expression tail, got {tail:?}");
    };
    assert_eq!(arms.len(), 2, "{arms:?}");
    assert!(matches!(
        &arms[1].pattern,
        Pattern::Literal { text, .. } if text == "true"
    ));
    assert!(matches!(
        &arms[1].body,
        Expr::Literal { text, .. } if text == "5"
    ));
}

#[test]
fn case_recovery_ignores_interpolation_commas_before_the_arm_boundary() {
    let source = include_str!("fixtures/malformed-case-arm-interpolation-comma.orna");
    let parsed = parse_module(source);

    assert_eq!(parsed.diagnostics.len(), 1, "{:?}", parsed.diagnostics);
    let diagnostic = &parsed.diagnostics[0];
    assert_eq!(diagnostic.code, "ORNA-PARSE-001");
    assert_eq!(diagnostic.message, "expected pattern");
    let invalid_pattern = source.find("\"value").expect("fixture has a string pattern");
    assert_eq!(diagnostic.span.start, invalid_pattern);
    assert_eq!(diagnostic.span.end, invalid_pattern + 1);

    let Declaration::Function { body, .. } = &parsed.value.items[0].declaration else {
        panic!("expected a function declaration");
    };
    let Expr::Block { statements, tail, .. } = body else {
        panic!("expected a function block");
    };
    assert!(statements.is_empty(), "{statements:?}");
    let Some(Expr::Control { arms, .. }) = tail.as_deref() else {
        panic!("expected a case expression tail, got {tail:?}");
    };
    assert!(matches!(
        arms.as_slice(),
        [orna_syntax_v1::CaseArm {
            pattern: Pattern::Literal { text, .. },
            body: Expr::Literal { text: body, .. },
            ..
        }] if text == "true" && body == "4"
    ));
}

#[test]
fn case_recovery_restores_delimiter_depth_after_each_interpolation() {
    let source = include_str!("fixtures/malformed-case-arm-interpolation-delimiter-scope.orna");
    let parsed = parse_module(source);

    assert_eq!(parsed.diagnostics.len(), 1, "{:?}", parsed.diagnostics);
    let diagnostic = &parsed.diagnostics[0];
    assert_eq!(diagnostic.code, "ORNA-PARSE-001");
    assert_eq!(diagnostic.message, "expected pattern");
    let invalid_pattern = source.find("\"value").expect("fixture has a string pattern");
    assert_eq!(diagnostic.span.start, invalid_pattern);
    assert_eq!(diagnostic.span.end, invalid_pattern + 1);

    let Declaration::Function { body, .. } = &parsed.value.items[0].declaration else {
        panic!("expected a function declaration");
    };
    let Expr::Block { statements, tail, .. } = body else {
        panic!("expected a function block");
    };
    assert!(statements.is_empty(), "{statements:?}");
    let Some(Expr::Control { arms, .. }) = tail.as_deref() else {
        panic!("expected a case expression tail, got {tail:?}");
    };
    assert!(matches!(
        arms.as_slice(),
        [orna_syntax_v1::CaseArm {
            pattern: Pattern::Literal { text, .. },
            body: Expr::Literal { text: body, .. },
            ..
        }] if text == "true" && body == "5"
    ));
}

#[test]
fn case_recovery_isolates_nested_interpolation_delimiters_and_keeps_tail() {
    let source = include_str!("fixtures/malformed-case-arm-nested-interpolation-delimiter-scope.orna");
    let parsed = parse_module(source);

    assert_eq!(parsed.diagnostics.len(), 1, "{:?}", parsed.diagnostics);
    let diagnostic = &parsed.diagnostics[0];
    assert_eq!(diagnostic.code, "ORNA-PARSE-001");
    assert_eq!(diagnostic.message, "expected pattern");
    let invalid_pattern = source.find("\"value").expect("fixture has a string pattern");
    assert_eq!(diagnostic.span.start, invalid_pattern);
    assert_eq!(diagnostic.span.end, invalid_pattern + 1);

    let Declaration::Function { body, .. } = &parsed.value.items[0].declaration else {
        panic!("expected a function declaration");
    };
    let Expr::Block { statements, tail, .. } = body else {
        panic!("expected a function block");
    };
    assert!(statements.is_empty(), "{statements:?}");
    let Some(Expr::Control { arms, .. }) = tail.as_deref() else {
        panic!("expected a case expression tail, got {tail:?}");
    };
    assert!(matches!(
        arms.as_slice(),
        [orna_syntax_v1::CaseArm {
            pattern: Pattern::Literal { text, .. },
            body: Expr::Literal { text: body, .. },
            ..
        }] if text == "true" && body == "5"
    ));
}

#[test]
fn case_recovery_restores_each_nested_interpolation_delimiter_tail() {
    let source = include_str!("fixtures/malformed-case-arm-deep-interpolation-delimiter-scope.orna");
    let parsed = parse_module(source);

    assert_eq!(parsed.diagnostics.len(), 1, "{:?}", parsed.diagnostics);
    let diagnostic = &parsed.diagnostics[0];
    assert_eq!(diagnostic.code, "ORNA-PARSE-001");
    assert_eq!(diagnostic.message, "expected pattern");
    let invalid_pattern = source.find("\"value").expect("fixture has a string pattern");
    assert_eq!(diagnostic.span.start, invalid_pattern);
    assert_eq!(diagnostic.span.end, invalid_pattern + 1);

    let Declaration::Function { body, .. } = &parsed.value.items[0].declaration else {
        panic!("expected a function declaration");
    };
    let Expr::Block { statements, tail, .. } = body else {
        panic!("expected a function block");
    };
    assert!(statements.is_empty(), "{statements:?}");
    let Some(Expr::Control { arms, .. }) = tail.as_deref() else {
        panic!("expected a case expression tail, got {tail:?}");
    };
    assert!(matches!(
        arms.as_slice(),
        [orna_syntax_v1::CaseArm {
            pattern: Pattern::Literal { text, .. },
            body: Expr::Literal { text: body, .. },
            ..
        }] if text == "true" && body == "8"
    ));
}

#[test]
fn case_recovery_preserves_tail_at_maximum_nested_interpolation_depth() {
    // Exercise the lexer nesting cap while every enclosing expression carries
    // unmatched delimiters that recovery must discard at its boundary.
    let source = include_str!("fixtures/malformed-case-arm-max-interpolation-depth.orna");
    let parsed = parse_module(source);

    assert_eq!(parsed.diagnostics.len(), 1, "{:?}", parsed.diagnostics);
    let diagnostic = &parsed.diagnostics[0];
    assert_eq!(diagnostic.code, "ORNA-PARSE-001");
    assert_eq!(diagnostic.message, "expected pattern");
    let invalid_pattern = source.find("\"layer").expect("fixture has a string pattern");
    assert_eq!(diagnostic.span.start, invalid_pattern);
    assert_eq!(diagnostic.span.end, invalid_pattern + 1);

    let Declaration::Function { body, .. } = &parsed.value.items[0].declaration else {
        panic!("expected a function declaration");
    };
    let Expr::Block { statements, tail, .. } = body else {
        panic!("expected a function block");
    };
    assert!(statements.is_empty(), "{statements:?}");
    let Some(Expr::Control { arms, .. }) = tail.as_deref() else {
        panic!("expected a case expression tail, got {tail:?}");
    };
    assert!(matches!(
        arms.as_slice(),
        [orna_syntax_v1::CaseArm {
            pattern: Pattern::Literal { text, .. },
            body: Expr::Literal { text: body, .. },
            ..
        }] if text == "true" && body == "8"
    ));
}

#[test]
fn semicolon_keeps_control_expression_as_a_statement() {
    let parsed = parse_module(include_str!("fixtures/semicolon-control-block-item.orna"));
    assert!(parsed.is_ok(), "{:?}", parsed.diagnostics);
    let Declaration::Function { body, .. } = &parsed.value.items[0].declaration else {
        panic!("expected a function declaration");
    };
    let Expr::Block { statements, tail, .. } = body else {
        panic!("expected a function block");
    };
    assert!(matches!(
        statements.as_slice(),
        [Statement::Control {
            value: Expr::Control { .. },
            ..
        }]
    ));
    assert!(tail.is_none());
}
