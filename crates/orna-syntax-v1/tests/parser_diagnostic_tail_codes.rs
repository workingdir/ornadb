use orna_syntax_v1::{
    CaseArm, Declaration, Expr, Pattern, Statement, StringSegment, SyntaxDiagnostic,
    parse_expression, parse_module, parse_repl, parse_row,
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
fn case_recovery_ignores_nested_case_commas_at_max_interpolation_depth() {
    let source = include_str!("fixtures/malformed-case-arm-max-depth-nested-case.orna");
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
fn case_recovery_restores_case_brace_depth_after_nested_interpolation_at_limit() {
    // The nested case-body string counts toward the lexer's total nesting cap.
    let source = include_str!("fixtures/malformed-case-arm-max-depth-case-interpolation.orna");
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
fn case_recovery_keeps_nested_case_scrutinee_and_pattern_interpolations_local_at_limit() {
    let source = include_str!("fixtures/malformed-case-arm-max-depth-case-scrutinee-pattern-interpolation.orna");
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
fn recovered_case_keeps_nested_interpolation_cases_as_the_final_tail() {
    let source = include_str!("fixtures/malformed-case-arm-max-depth-interpolation-final-tail.orna");
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
    let Some(Expr::Control {
        arms: outer_arms, ..
    }) = tail.as_deref()
    else {
        panic!("expected a case expression tail, got {tail:?}");
    };
    assert_eq!(outer_arms.len(), 1, "{outer_arms:?}");

    let Expr::Control {
        condition: Some(scrutinee),
        arms: nested_arms,
        ..
    } = &outer_arms[0].body
    else {
        panic!("expected the recovered arm body to be a nested case");
    };
    assert_eq!(nested_arms.len(), 2, "{nested_arms:?}");
    assert!(matches!(
        &nested_arms[1].body,
        Expr::Literal { text, .. } if text == "8"
    ));

    let Expr::InterpolatedString { segments, .. } = scrutinee.as_ref() else {
        panic!("expected an interpolated case scrutinee");
    };
    assert!(matches!(
        segments.as_slice(),
        [
            StringSegment::Text { text, .. },
            StringSegment::Expression {
                value: Expr::Control { arms, .. },
                ..
            }
        ] if text == "subject "
            && arms.len() == 2
            && matches!(&arms[1].body, Expr::Literal { text, .. } if text == "0")
    ));
}

#[test]
fn recovered_case_keeps_interpolated_block_case_as_the_final_tail() {
    let source = include_str!("fixtures/malformed-case-arm-max-depth-interpolation-block-tail.orna");
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
    let Some(Expr::Control {
        arms: outer_arms, ..
    }) = tail.as_deref()
    else {
        panic!("expected a case expression tail, got {tail:?}");
    };
    assert_eq!(outer_arms.len(), 1, "{outer_arms:?}");

    let Expr::Control {
        condition: Some(scrutinee),
        arms: nested_arms,
        ..
    } = &outer_arms[0].body
    else {
        panic!("expected the recovered arm body to be a nested case");
    };
    assert_eq!(nested_arms.len(), 2, "{nested_arms:?}");
    assert!(matches!(
        &nested_arms[1].body,
        Expr::Literal { text, .. } if text == "8"
    ));

    let Expr::InterpolatedString { segments, .. } = scrutinee.as_ref() else {
        panic!("expected an interpolated case scrutinee");
    };
    let [StringSegment::Text { text, .. }, StringSegment::Expression { value, .. }] =
        segments.as_slice()
    else {
        panic!("expected text and one block interpolation, got {segments:?}");
    };
    assert_eq!(text, "subject ");

    let Expr::Control {
        body: Some(interpolation_body),
        ..
    } = value
    else {
        panic!("expected an interpolated if control");
    };
    let Expr::Block {
        statements,
        tail: Some(block_tail),
        ..
    } = interpolation_body.as_ref()
    else {
        panic!("expected the interpolation control body to be a block with a tail");
    };
    assert!(statements.is_empty(), "{statements:?}");
    let Expr::Control {
        arms: interpolation_arms,
        ..
    } = block_tail.as_ref()
    else {
        panic!("expected the interpolation block tail to be a case expression");
    };
    assert_eq!(interpolation_arms.len(), 2, "{interpolation_arms:?}");
    assert!(matches!(
        &interpolation_arms[1].body,
        Expr::Literal { text, .. } if text == "0"
    ));
}

#[test]
fn recovered_interpolated_control_block_keeps_statement_and_final_tail_distinct() {
    let source = include_str!("fixtures/malformed-case-arm-max-depth-interpolated-control-statement-tail.orna");
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
    let Some(Expr::Control {
        arms: outer_arms, ..
    }) = tail.as_deref()
    else {
        panic!("expected the outer case to remain the function tail");
    };
    assert_eq!(outer_arms.len(), 1, "{outer_arms:?}");

    let Expr::Control {
        condition: Some(scrutinee),
        arms: recovered_arms,
        ..
    } = &outer_arms[0].body
    else {
        panic!("expected a nested case in the recovered arm");
    };
    assert_eq!(recovered_arms.len(), 2, "{recovered_arms:?}");
    assert!(matches!(
        &recovered_arms[1].body,
        Expr::Literal { text, .. } if text == "8"
    ));

    let Expr::InterpolatedString { segments, .. } = scrutinee.as_ref() else {
        panic!("expected an interpolated nested-case scrutinee");
    };
    let [StringSegment::Text { text, .. }, StringSegment::Expression { value, .. }] =
        segments.as_slice()
    else {
        panic!("expected one interpolation after the string prefix: {segments:?}");
    };
    assert_eq!(text, "subject ");

    let Expr::Control {
        body: Some(if_body), ..
    } = value
    else {
        panic!("expected the interpolation expression to be an if control");
    };
    let Expr::Block {
        statements: if_statements,
        tail: Some(if_tail),
        ..
    } = if_body.as_ref()
    else {
        panic!("expected the if control body block to retain its tail");
    };
    // The semicolon keeps the case as a statement, leaving `7` as the block tail.
    assert!(matches!(
        if_statements.as_slice(),
        [Statement::Control {
            value: Expr::Control { arms, .. },
            ..
        }] if arms.len() == 2
    ));
    assert!(matches!(if_tail.as_ref(), Expr::Literal { text, .. } if text == "7"));
}

#[test]
fn recovered_interpolated_if_keeps_each_branch_control_statement_and_tail() {
    let source = include_str!("fixtures/malformed-case-arm-max-depth-interpolated-if-branch-tails.orna");
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
    let Some(Expr::Control {
        arms: outer_arms, ..
    }) = tail.as_deref()
    else {
        panic!("expected the outer case to remain the function tail");
    };
    assert_eq!(outer_arms.len(), 1, "{outer_arms:?}");

    let Expr::Control {
        condition: Some(scrutinee),
        arms: recovered_arms,
        ..
    } = &outer_arms[0].body
    else {
        panic!("expected a nested case in the recovered arm");
    };
    assert_eq!(recovered_arms.len(), 2, "{recovered_arms:?}");

    let Expr::InterpolatedString { segments, .. } = scrutinee.as_ref() else {
        panic!("expected an interpolated nested-case scrutinee");
    };
    let [StringSegment::Text { text, .. }, StringSegment::Expression { value, .. }] =
        segments.as_slice()
    else {
        panic!("expected one interpolation after the string prefix: {segments:?}");
    };
    assert_eq!(text, "branches ");

    let Expr::Control {
        body: Some(then_body),
        alternate: Some(else_body),
        ..
    } = value
    else {
        panic!("expected an interpolated if with both branches");
    };
    // Each semicolon-terminated case stays a statement, distinct from its branch tail.
    for (branch, expected_tail) in [(then_body.as_ref(), "7"), (else_body.as_ref(), "8")] {
        let Expr::Block {
            statements: branch_statements,
            tail: Some(branch_tail),
            ..
        } = branch
        else {
            panic!("expected an if branch block with a final tail");
        };
        assert!(matches!(
            branch_statements.as_slice(),
            [Statement::Control {
                value: Expr::Control { arms, .. },
                ..
            }] if arms.len() == 2
        ));
        assert!(matches!(
            branch_tail.as_ref(),
            Expr::Literal { text, .. } if text == expected_tail
        ));
    }
}

#[test]
fn recovered_interpolated_else_if_chain_keeps_branch_local_tails() {
    let source = include_str!("fixtures/malformed-case-arm-max-depth-interpolated-else-if-tails.orna");
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
    let Some(Expr::Control {
        arms: outer_arms, ..
    }) = tail.as_deref()
    else {
        panic!("expected the outer case to remain the function tail");
    };
    assert_eq!(outer_arms.len(), 1, "{outer_arms:?}");

    let Expr::Control {
        condition: Some(scrutinee),
        arms: recovered_arms,
        ..
    } = &outer_arms[0].body
    else {
        panic!("expected a nested case in the recovered arm");
    };
    assert_eq!(recovered_arms.len(), 2, "{recovered_arms:?}");

    let Expr::InterpolatedString { segments, .. } = scrutinee.as_ref() else {
        panic!("expected an interpolated nested-case scrutinee");
    };
    let [StringSegment::Text { text, .. }, StringSegment::Expression { value, .. }] =
        segments.as_slice()
    else {
        panic!("expected one interpolation after the string prefix: {segments:?}");
    };
    assert_eq!(text, "branches ");

    let Expr::Control {
        body: Some(then_body),
        alternate: Some(else_if),
        ..
    } = value
    else {
        panic!("expected an if with an else-if branch");
    };
    let Expr::Control {
        body: Some(else_if_body),
        alternate: Some(else_body),
        ..
    } = else_if.as_ref()
    else {
        panic!("expected the else-if to retain its final else branch");
    };

    let assert_branch_tail = |branch: &Expr, expected: &str| {
        let Expr::Block {
            statements,
            tail: Some(tail),
            ..
        } = branch
        else {
            panic!("expected an if branch block with a final tail");
        };
        assert!(matches!(
            statements.as_slice(),
            [Statement::Control {
                value: Expr::Control { arms, .. },
                ..
            }] if arms.len() == 2
        ));
        assert!(matches!(
            tail.as_ref(),
            Expr::Literal { text, .. } if text == expected
        ));
    };
    assert_branch_tail(then_body.as_ref(), "7");
    assert_branch_tail(else_if_body.as_ref(), "8");
    assert_branch_tail(else_body.as_ref(), "9");
}

#[test]
fn recovered_interpolated_else_if_chain_keeps_deep_branch_tails() {
    let source = include_str!("fixtures/malformed-case-arm-max-depth-interpolated-deep-else-if-tails.orna");
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
    let Some(Expr::Control {
        arms: outer_arms, ..
    }) = tail.as_deref()
    else {
        panic!("expected the outer case to remain the function tail");
    };
    assert_eq!(outer_arms.len(), 1, "{outer_arms:?}");

    let Expr::Control {
        condition: Some(scrutinee),
        arms: recovered_arms,
        ..
    } = &outer_arms[0].body
    else {
        panic!("expected a nested case in the recovered arm");
    };
    assert_eq!(recovered_arms.len(), 2, "{recovered_arms:?}");

    let Expr::InterpolatedString { segments, .. } = scrutinee.as_ref() else {
        panic!("expected an interpolated nested-case scrutinee");
    };
    let [StringSegment::Text { text, .. }, StringSegment::Expression { value, .. }] =
        segments.as_slice()
    else {
        panic!("expected one interpolation after the string prefix: {segments:?}");
    };
    assert_eq!(text, "branches ");

    let Expr::Control {
        body: Some(first_body),
        alternate: Some(second_if),
        ..
    } = value
    else {
        panic!("expected a multi-branch interpolated if");
    };
    let Expr::Control {
        condition: Some(second_condition),
        body: Some(second_body),
        alternate: Some(third_if),
        ..
    } = second_if.as_ref()
    else {
        panic!("expected the first else-if branch");
    };
    assert!(matches!(
        second_condition.as_ref(),
        Expr::InterpolatedString { segments, .. }
            if matches!(segments.as_slice(), [
                StringSegment::Text { text, .. },
                StringSegment::Expression {
                    value: Expr::Literal { text: value, .. },
                    ..
                }
            ] if text == "gate " && value == "true")
    ));
    let Expr::Control {
        body: Some(third_body),
        alternate: Some(else_body),
        ..
    } = third_if.as_ref()
    else {
        panic!("expected the second else-if and final else branches");
    };

    let assert_branch_tail = |branch: &Expr, expected: &str| {
        let Expr::Block {
            statements,
            tail: Some(tail),
            ..
        } = branch
        else {
            panic!("expected an if branch block with a final tail");
        };
        assert!(matches!(
            statements.as_slice(),
            [Statement::Control {
                value: Expr::Control { arms, .. },
                ..
            }] if arms.len() == 2
        ));
        assert!(matches!(
            tail.as_ref(),
            Expr::Literal { text, .. } if text == expected
        ));
    };
    assert_branch_tail(first_body.as_ref(), "7");
    assert_branch_tail(second_body.as_ref(), "8");
    assert_branch_tail(third_body.as_ref(), "9");
    assert_branch_tail(else_body.as_ref(), "10");
}

#[test]
fn recovered_deep_else_if_chain_keeps_interpolated_condition_and_all_tails() {
    let source = include_str!("fixtures/malformed-case-arm-max-depth-interpolated-deepest-else-if.orna");
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
    let Some(Expr::Control {
        arms: outer_arms, ..
    }) = tail.as_deref()
    else {
        panic!("expected the outer case to remain the function tail");
    };
    assert_eq!(outer_arms.len(), 1, "{outer_arms:?}");

    let Expr::Control {
        condition: Some(scrutinee),
        arms: recovered_arms,
        ..
    } = &outer_arms[0].body
    else {
        panic!("expected a nested case in the recovered arm");
    };
    assert_eq!(recovered_arms.len(), 2, "{recovered_arms:?}");
    let Expr::InterpolatedString { segments, .. } = scrutinee.as_ref() else {
        panic!("expected an interpolated nested-case scrutinee");
    };
    let [StringSegment::Text { text, .. }, StringSegment::Expression { value, .. }] =
        segments.as_slice()
    else {
        panic!("expected one interpolation after the string prefix: {segments:?}");
    };
    assert_eq!(text, "branches ");

    let assert_branch_tail = |branch: &Expr, expected: &str| {
        let Expr::Block {
            statements,
            tail: Some(tail),
            ..
        } = branch
        else {
            panic!("expected an if branch block with a final tail");
        };
        assert!(matches!(
            statements.as_slice(),
            [Statement::Control {
                value: Expr::Control { arms, .. },
                ..
            }] if arms.len() == 2
        ));
        assert!(matches!(
            tail.as_ref(),
            Expr::Literal { text, .. } if text == expected
        ));
    };

    let mut branch_control = value;
    for (index, expected_tail) in ["7", "8", "9", "10"].iter().enumerate() {
        let Expr::Control {
            condition,
            body: Some(branch_body),
            alternate: Some(alternate),
            ..
        } = branch_control
        else {
            panic!("expected else-if level {index} in the interpolation chain");
        };
        assert_branch_tail(branch_body.as_ref(), expected_tail);
        if index == 2 {
            assert!(matches!(
                condition.as_deref(),
                Some(Expr::InterpolatedString { segments, .. })
                    if matches!(segments.as_slice(), [
                        StringSegment::Text { text, .. },
                        StringSegment::Expression {
                            value: Expr::Control { arms, .. },
                            ..
                        }
                    ] if text == "gate " && arms.len() == 2)
            ));
        }
        branch_control = alternate.as_ref();
    }
    assert_branch_tail(branch_control, "11");
}

#[test]
fn recovered_long_else_if_chain_keeps_every_branch_tail() {
    let source = include_str!("fixtures/malformed-case-arm-max-depth-interpolated-long-else-if-tails.orna");
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
    let Some(Expr::Control {
        arms: outer_arms, ..
    }) = tail.as_deref()
    else {
        panic!("expected the outer case to remain the function tail");
    };
    assert_eq!(outer_arms.len(), 1, "{outer_arms:?}");

    let Expr::Control {
        condition: Some(scrutinee),
        ..
    } = &outer_arms[0].body
    else {
        panic!("expected a nested case in the recovered arm");
    };
    let Expr::InterpolatedString { segments, .. } = scrutinee.as_ref() else {
        panic!("expected an interpolated nested-case scrutinee");
    };
    let [StringSegment::Text { text, .. }, StringSegment::Expression { value, .. }] =
        segments.as_slice()
    else {
        panic!("expected one interpolation after the string prefix: {segments:?}");
    };
    assert_eq!(text, "branches ");

    let assert_branch_tail = |branch: &Expr, expected: &str| {
        let Expr::Block {
            statements,
            tail: Some(tail),
            ..
        } = branch
        else {
            panic!("expected an if branch block with a final tail");
        };
        assert!(matches!(
            statements.as_slice(),
            [Statement::Control {
                value: Expr::Control { arms, .. },
                ..
            }] if arms.len() == 2
        ));
        assert!(matches!(
            tail.as_ref(),
            Expr::Literal { text, .. } if text == expected
        ));
    };

    let mut branch_control = value;
    // The surrounding function, case, and interpolation leave room for 27 controls.
    for index in 0..27 {
        let Expr::Control {
            condition,
            body: Some(branch_body),
            alternate: Some(alternate),
            ..
        } = branch_control
        else {
            panic!("else-if chain ended before branch {index}");
        };
        assert_branch_tail(branch_body.as_ref(), &(100 + index).to_string());
        if index == 13 {
            assert!(matches!(
                condition.as_deref(),
                Some(Expr::InterpolatedString { segments, .. })
                    if matches!(segments.as_slice(), [
                        StringSegment::Text { text, .. },
                        StringSegment::Expression {
                            value: Expr::Control { arms, .. },
                            ..
                        }
                    ] if text == "gate " && arms.len() == 2)
            ));
        }
        branch_control = alternate.as_ref();
    }
    assert_branch_tail(branch_control, "127");
}

#[test]
fn recovered_deep_interpolated_else_if_without_else_keeps_final_branch_tail() {
    let source = include_str!("fixtures/malformed-case-arm-max-depth-interpolated-final-else-if.orna");
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
    let Some(Expr::Control {
        arms: outer_arms, ..
    }) = tail.as_deref()
    else {
        panic!("expected the outer case to remain the function tail");
    };
    assert_eq!(outer_arms.len(), 1, "{outer_arms:?}");

    let Expr::Control {
        condition: Some(scrutinee),
        ..
    } = &outer_arms[0].body
    else {
        panic!("expected a nested case in the recovered arm");
    };
    let Expr::InterpolatedString { segments, .. } = scrutinee.as_ref() else {
        panic!("expected an interpolated nested-case scrutinee");
    };
    let [StringSegment::Text { text, .. }, StringSegment::Expression { value, .. }] =
        segments.as_slice()
    else {
        panic!("expected one interpolation after the string prefix: {segments:?}");
    };
    assert_eq!(text, "branches ");

    let assert_branch_tail = |branch: &Expr, expected: &str| {
        let Expr::Block {
            statements,
            tail: Some(tail),
            ..
        } = branch
        else {
            panic!("expected an if branch block with a final tail");
        };
        assert!(matches!(
            statements.as_slice(),
            [Statement::Control {
                value: Expr::Control { arms, .. },
                ..
            }] if arms.len() == 2
        ));
        assert!(matches!(
            tail.as_ref(),
            Expr::Literal { text, .. } if text == expected
        ));
    };

    let mut branch_control = value;
    // The final condition is interpolated too; its absent alternate must not
    // discard that deepest branch's block tail during recovery.
    for index in 0..27 {
        let Expr::Control {
            condition,
            body: Some(branch_body),
            alternate,
            ..
        } = branch_control
        else {
            panic!("else-if chain ended before branch {index}");
        };
        assert_branch_tail(branch_body.as_ref(), &(100 + index).to_string());
        if index == 26 {
            assert!(matches!(
                condition.as_deref(),
                Some(Expr::InterpolatedString { segments, .. })
                    if matches!(segments.as_slice(), [
                        StringSegment::Text { text, .. },
                        StringSegment::Expression { value: Expr::Literal { text: value, .. }, .. }
                    ] if text == "finish " && value == "true")
            ));
            assert!(alternate.is_none(), "deepest else-if unexpectedly has an else");
        } else {
            branch_control = alternate
                .as_deref()
                .expect("every earlier else-if has another branch");
        }
    }
}

#[test]
fn recovered_deep_final_else_if_keeps_interpolated_condition_and_tail_separate() {
    let source = include_str!("fixtures/malformed-case-arm-max-depth-interpolated-final-else-if-tail-interplay.orna");
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
    let Some(Expr::Control {
        arms: outer_arms, ..
    }) = tail.as_deref()
    else {
        panic!("expected the outer case to remain the function tail");
    };
    assert_eq!(outer_arms.len(), 1, "{outer_arms:?}");

    let Expr::Control {
        condition: Some(scrutinee),
        ..
    } = &outer_arms[0].body
    else {
        panic!("expected a nested case in the recovered arm");
    };
    let Expr::InterpolatedString { segments, .. } = scrutinee.as_ref() else {
        panic!("expected an interpolated nested-case scrutinee");
    };
    let [StringSegment::Text { text, .. }, StringSegment::Expression { value, .. }] =
        segments.as_slice()
    else {
        panic!("expected one interpolation after the string prefix: {segments:?}");
    };
    assert_eq!(text, "branches ");

    let assert_branch_case_statement = |branch: &Expr| {
        let Expr::Block { statements, .. } = branch else {
            panic!("expected an if branch block");
        };
        assert!(matches!(
            statements.as_slice(),
            [Statement::Control {
                value: Expr::Control { arms, .. },
                ..
            }] if arms.len() == 2
        ));
    };

    let mut branch_control = value;
    for index in 0..27 {
        let Expr::Control {
            condition,
            body: Some(branch_body),
            alternate,
            ..
        } = branch_control
        else {
            panic!("else-if chain ended before branch {index}");
        };
        assert_branch_case_statement(branch_body.as_ref());
        let Expr::Block {
            tail: Some(branch_tail),
            ..
        } = branch_body.as_ref()
        else {
            panic!("branch {index} lost its final tail");
        };
        if index == 26 {
            // For this malformed recovery edge, keep adjacent interpolations
            // in the final else-if condition and value tail independent.
            assert!(matches!(
                condition.as_deref(),
                Some(Expr::InterpolatedString { segments, .. })
                    if matches!(segments.as_slice(), [
                        StringSegment::Text { text, .. },
                        StringSegment::Expression { value: Expr::Literal { text: value, .. }, .. }
                    ] if text == "finish " && value == "true")
            ));
            assert!(matches!(
                branch_tail.as_ref(),
                Expr::InterpolatedString { segments, .. }
                    if matches!(segments.as_slice(), [
                        StringSegment::Text { text, .. },
                        StringSegment::Expression { value: Expr::Literal { text: value, .. }, .. }
                    ] if text == "tail " && value == "true")
            ));
            assert!(alternate.is_none(), "deepest else-if unexpectedly has an else");
        } else {
            assert!(matches!(
                branch_tail.as_ref(),
                Expr::Literal { text, .. } if text == &(100 + index).to_string()
            ));
            branch_control = alternate
                .as_deref()
                .expect("every earlier else-if has another branch");
        }
    }
}

#[test]
fn recovered_deep_final_else_if_keeps_each_interpolated_tail_segment() {
    let source = include_str!("fixtures/malformed-case-arm-max-depth-interpolated-final-else-if-multi-tail.orna");
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
    let Some(Expr::Control {
        arms: outer_arms, ..
    }) = tail.as_deref()
    else {
        panic!("expected the outer case to remain the function tail");
    };
    assert_eq!(outer_arms.len(), 1, "{outer_arms:?}");

    let Expr::Control {
        condition: Some(scrutinee),
        ..
    } = &outer_arms[0].body
    else {
        panic!("expected a nested case in the recovered arm");
    };
    let Expr::InterpolatedString { segments, .. } = scrutinee.as_ref() else {
        panic!("expected an interpolated nested-case scrutinee");
    };
    let [StringSegment::Text { text, .. }, StringSegment::Expression { value, .. }] =
        segments.as_slice()
    else {
        panic!("expected one interpolation after the string prefix: {segments:?}");
    };
    assert_eq!(text, "branches ");

    let assert_branch_case_statement = |branch: &Expr| {
        let Expr::Block { statements, .. } = branch else {
            panic!("expected an if branch block");
        };
        assert!(matches!(
            statements.as_slice(),
            [Statement::Control {
                value: Expr::Control { arms, .. },
                ..
            }] if arms.len() == 2
        ));
    };

    let mut branch_control = value;
    for index in 0..27 {
        let Expr::Control {
            condition,
            body: Some(branch_body),
            alternate,
            ..
        } = branch_control
        else {
            panic!("else-if chain ended before branch {index}");
        };
        assert_branch_case_statement(branch_body.as_ref());
        let Expr::Block {
            tail: Some(branch_tail),
            ..
        } = branch_body.as_ref()
        else {
            panic!("branch {index} lost its final tail");
        };
        if index == 26 {
            assert!(matches!(
                condition.as_deref(),
                Some(Expr::InterpolatedString { segments, .. })
                    if matches!(segments.as_slice(), [
                        StringSegment::Text { text, .. },
                        StringSegment::Expression { value: Expr::Literal { text: value, .. }, .. }
                    ] if text == "finish " && value == "true")
            ));
            // Keep ordered tail segments local to this recovered branch.
            assert!(matches!(
                branch_tail.as_ref(),
                Expr::InterpolatedString { segments, .. }
                    if matches!(segments.as_slice(), [
                        StringSegment::Text { text: first, .. },
                        StringSegment::Expression { value: Expr::Literal { text: first_value, .. }, .. },
                        StringSegment::Text { text: between, .. },
                        StringSegment::Expression { value: Expr::Literal { text: second_value, .. }, .. },
                        StringSegment::Text { text: last, .. }
                    ] if first == "tail "
                        && first_value == "true"
                        && between == ", then "
                        && second_value == "false"
                        && last == " end")
            ));
            assert!(alternate.is_none(), "deepest else-if unexpectedly has an else");
        } else {
            assert!(matches!(
                branch_tail.as_ref(),
                Expr::Literal { text, .. } if text == &(100 + index).to_string()
            ));
            branch_control = alternate
                .as_deref()
                .expect("every earlier else-if has another branch");
        }
    }
}

#[test]
fn recovered_deep_final_else_if_keeps_nested_interpolation_inside_tail() {
    let source = include_str!("fixtures/malformed-case-arm-max-depth-interpolated-final-else-if-nested-tail.orna");
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
    let Some(Expr::Control {
        arms: outer_arms, ..
    }) = tail.as_deref()
    else {
        panic!("expected the outer case to remain the function tail");
    };
    assert_eq!(outer_arms.len(), 1, "{outer_arms:?}");

    let Expr::Control {
        condition: Some(scrutinee),
        ..
    } = &outer_arms[0].body
    else {
        panic!("expected a nested case in the recovered arm");
    };
    let Expr::InterpolatedString { segments, .. } = scrutinee.as_ref() else {
        panic!("expected an interpolated nested-case scrutinee");
    };
    let [StringSegment::Text { text, .. }, StringSegment::Expression { value, .. }] =
        segments.as_slice()
    else {
        panic!("expected one interpolation after the string prefix: {segments:?}");
    };
    assert_eq!(text, "branches ");

    let assert_branch_case_statement = |branch: &Expr| {
        let Expr::Block { statements, .. } = branch else {
            panic!("expected an if branch block");
        };
        assert!(matches!(
            statements.as_slice(),
            [Statement::Control {
                value: Expr::Control { arms, .. },
                ..
            }] if arms.len() == 2
        ));
    };

    let mut branch_control = value;
    for index in 0..27 {
        let Expr::Control {
            condition,
            body: Some(branch_body),
            alternate,
            ..
        } = branch_control
        else {
            panic!("else-if chain ended before branch {index}");
        };
        assert_branch_case_statement(branch_body.as_ref());
        let Expr::Block {
            tail: Some(branch_tail),
            ..
        } = branch_body.as_ref()
        else {
            panic!("branch {index} lost its final tail");
        };
        if index == 26 {
            assert!(matches!(
                condition.as_deref(),
                Some(Expr::InterpolatedString { segments, .. })
                    if matches!(segments.as_slice(), [
                        StringSegment::Text { text, .. },
                        StringSegment::Expression { value: Expr::Literal { text: value, .. }, .. }
                    ] if text == "finish " && value == "true")
            ));
            // Preserve the nested string expression independently from the
            // enclosing tail's trailing text and closing branch delimiter.
            assert!(matches!(
                branch_tail.as_ref(),
                Expr::InterpolatedString { segments, .. }
                    if matches!(segments.as_slice(), [
                        StringSegment::Text { text: prefix, .. },
                        StringSegment::Expression {
                            value: Expr::InterpolatedString { segments: nested, .. },
                            ..
                        },
                        StringSegment::Text { text: suffix, .. }
                    ] if prefix == "tail "
                        && suffix == " end"
                        && matches!(nested.as_slice(), [
                            StringSegment::Text { text, .. },
                            StringSegment::Expression { value: Expr::Literal { text: value, .. }, .. }
                        ] if text == "inner " && value == "true"))
            ));
            assert!(alternate.is_none(), "deepest else-if unexpectedly has an else");
        } else {
            assert!(matches!(
                branch_tail.as_ref(),
                Expr::Literal { text, .. } if text == &(100 + index).to_string()
            ));
            branch_control = alternate
                .as_deref()
                .expect("every earlier else-if has another branch");
        }
    }
}

#[test]
fn recovered_deep_final_else_if_keeps_case_inside_interpolated_tail() {
    let source = include_str!("fixtures/malformed-case-arm-max-depth-interpolated-final-else-if-case-tail.orna");
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
    let Some(Expr::Control {
        arms: outer_arms, ..
    }) = tail.as_deref()
    else {
        panic!("expected the outer case to remain the function tail");
    };
    assert_eq!(outer_arms.len(), 1, "{outer_arms:?}");

    let Expr::Control {
        condition: Some(scrutinee),
        ..
    } = &outer_arms[0].body
    else {
        panic!("expected a nested case in the recovered arm");
    };
    let Expr::InterpolatedString { segments, .. } = scrutinee.as_ref() else {
        panic!("expected an interpolated nested-case scrutinee");
    };
    let [StringSegment::Text { text, .. }, StringSegment::Expression { value, .. }] =
        segments.as_slice()
    else {
        panic!("expected one interpolation after the string prefix: {segments:?}");
    };
    assert_eq!(text, "branches ");

    let assert_branch_case_statement = |branch: &Expr| {
        let Expr::Block { statements, .. } = branch else {
            panic!("expected an if branch block");
        };
        assert!(matches!(
            statements.as_slice(),
            [Statement::Control {
                value: Expr::Control { arms, .. },
                ..
            }] if arms.len() == 2
        ));
    };

    let mut branch_control = value;
    // The embedded case uses one additional parser frame, so this is the
    // deepest else-if chain that keeps the complete tail within the limit.
    for index in 0..26 {
        let Expr::Control {
            condition,
            body: Some(branch_body),
            alternate,
            ..
        } = branch_control
        else {
            panic!("else-if chain ended before branch {index}");
        };
        assert_branch_case_statement(branch_body.as_ref());
        let Expr::Block {
            tail: Some(branch_tail),
            ..
        } = branch_body.as_ref()
        else {
            panic!("branch {index} lost its final tail");
        };
        if index == 25 {
            assert!(matches!(
                condition.as_deref(),
                Some(Expr::InterpolatedString { segments, .. })
                    if matches!(segments.as_slice(), [
                        StringSegment::Text { text, .. },
                        StringSegment::Expression { value: Expr::Literal { text: value, .. }, .. }
                    ] if text == "finish " && value == "true")
            ));
            let Expr::InterpolatedString { segments, .. } = branch_tail.as_ref() else {
                panic!("expected an interpolated tail around the final case");
            };
            let [
                StringSegment::Text { text: prefix, .. },
                StringSegment::Expression { value: tail_case, .. },
                StringSegment::Text { text: suffix, .. },
            ] = segments.as_slice()
            else {
                panic!("expected prefix, case, and suffix in tail: {segments:?}");
            };
            assert_eq!(prefix, "tail ");
            assert_eq!(suffix, " end");

            // Treat the embedded case as the interpolation value; its arm
            // braces must stay separate from the outer string and branch.
            let Expr::Control { arms, .. } = tail_case else {
                panic!("expected the tail interpolation to contain a case");
            };
            assert_eq!(arms.len(), 2, "{arms:?}");
            assert!(matches!(
                &arms[0].body,
                Expr::Literal { text, .. } if text == "1"
            ));
            assert!(matches!(
                &arms[1].body,
                Expr::Literal { text, .. } if text == "0"
            ));
            assert!(alternate.is_none(), "deepest else-if unexpectedly has an else");
        } else {
            assert!(matches!(
                branch_tail.as_ref(),
                Expr::Literal { text, .. } if text == &(100 + index).to_string()
            ));
            branch_control = alternate
                .as_deref()
                .expect("every earlier else-if has another branch");
        }
    }
}

#[test]
fn recovered_deep_final_else_if_keeps_case_arm_tails_inside_interpolation() {
    let source = include_str!("fixtures/malformed-case-arm-max-depth-interpolated-final-else-if-case-arm-tails.orna");
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
    let Some(Expr::Control {
        arms: outer_arms, ..
    }) = tail.as_deref()
    else {
        panic!("expected the outer case to remain the function tail");
    };
    assert_eq!(outer_arms.len(), 1, "{outer_arms:?}");

    let Expr::Control {
        condition: Some(scrutinee),
        ..
    } = &outer_arms[0].body
    else {
        panic!("expected a nested case in the recovered arm");
    };
    let Expr::InterpolatedString { segments, .. } = scrutinee.as_ref() else {
        panic!("expected an interpolated nested-case scrutinee");
    };
    let [StringSegment::Text { text, .. }, StringSegment::Expression { value, .. }] =
        segments.as_slice()
    else {
        panic!("expected one interpolation after the string prefix: {segments:?}");
    };
    assert_eq!(text, "branches ");

    let assert_branch_case_statement = |branch: &Expr| {
        let Expr::Block { statements, .. } = branch else {
            panic!("expected an if branch block");
        };
        assert!(matches!(
            statements.as_slice(),
            [Statement::Control {
                value: Expr::Control { arms, .. },
                ..
            }] if arms.len() == 2
        ));
    };

    let assert_case_arm_tail = |body: &Expr, expected: &str| {
        let Expr::Block {
            statements,
            tail: Some(tail),
            ..
        } = body
        else {
            panic!("expected a case arm block with a tail");
        };
        assert_eq!(statements.len(), 1, "{statements:?}");
        assert!(matches!(
            tail.as_ref(),
            Expr::Literal { text, .. } if text == expected
        ));
    };

    let mut branch_control = value;
    // Case arm blocks add nesting frames; keep the chain at the deepest
    // accepted length while checking both arm tails inside the string.
    for index in 0..25 {
        let Expr::Control {
            condition,
            body: Some(branch_body),
            alternate,
            ..
        } = branch_control
        else {
            panic!("else-if chain ended before branch {index}");
        };
        assert_branch_case_statement(branch_body.as_ref());
        let Expr::Block {
            tail: Some(branch_tail),
            ..
        } = branch_body.as_ref()
        else {
            panic!("branch {index} lost its final tail");
        };
        if index == 24 {
            assert!(matches!(
                condition.as_deref(),
                Some(Expr::InterpolatedString { segments, .. })
                    if matches!(segments.as_slice(), [
                        StringSegment::Text { text, .. },
                        StringSegment::Expression { value: Expr::Literal { text: value, .. }, .. }
                    ] if text == "finish " && value == "true")
            ));
            let Expr::InterpolatedString { segments, .. } = branch_tail.as_ref() else {
                panic!("expected an interpolated tail around the final case");
            };
            let [
                StringSegment::Text { text: prefix, .. },
                StringSegment::Expression { value: tail_case, .. },
                StringSegment::Text { text: suffix, .. },
            ] = segments.as_slice()
            else {
                panic!("expected prefix, case, and suffix in tail: {segments:?}");
            };
            assert_eq!(prefix, "tail ");
            assert_eq!(suffix, " end");
            let Expr::Control { arms, .. } = tail_case else {
                panic!("expected the tail interpolation to contain a case");
            };
            assert_eq!(arms.len(), 2, "{arms:?}");
            assert_case_arm_tail(&arms[0].body, "2");
            assert_case_arm_tail(&arms[1].body, "3");
            assert!(alternate.is_none(), "deepest else-if unexpectedly has an else");
        } else {
            assert!(matches!(
                branch_tail.as_ref(),
                Expr::Literal { text, .. } if text == &(100 + index).to_string()
            ));
            branch_control = alternate
                .as_deref()
                .expect("every earlier else-if has another branch");
        }
    }
}

#[test]
fn recovered_deep_final_else_if_keeps_interpolated_case_arm_tails() {
    let source = include_str!("fixtures/malformed-case-arm-max-depth-interpolated-final-else-if-interpolated-arm-tails.orna");
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
    let Some(Expr::Control {
        arms: outer_arms, ..
    }) = tail.as_deref()
    else {
        panic!("expected the outer case to remain the function tail");
    };
    assert_eq!(outer_arms.len(), 1, "{outer_arms:?}");

    let Expr::Control {
        condition: Some(scrutinee),
        ..
    } = &outer_arms[0].body
    else {
        panic!("expected a nested case in the recovered arm");
    };
    let Expr::InterpolatedString { segments, .. } = scrutinee.as_ref() else {
        panic!("expected an interpolated nested-case scrutinee");
    };
    let [StringSegment::Text { text, .. }, StringSegment::Expression { value, .. }] =
        segments.as_slice()
    else {
        panic!("expected one interpolation after the string prefix: {segments:?}");
    };
    assert_eq!(text, "branches ");

    let assert_branch_case_statement = |branch: &Expr| {
        let Expr::Block { statements, .. } = branch else {
            panic!("expected an if branch block");
        };
        assert!(matches!(
            statements.as_slice(),
            [Statement::Control {
                value: Expr::Control { arms, .. },
                ..
            }] if arms.len() == 2
        ));
    };
    let assert_interpolated_case_arm_tail = |body: &Expr, prefix: &str, value: &str| {
        let Expr::Block {
            statements,
            tail: Some(tail),
            ..
        } = body
        else {
            panic!("expected a case arm block with a final tail");
        };
        assert_eq!(statements.len(), 1, "{statements:?}");
        assert!(matches!(
            tail.as_ref(),
            Expr::InterpolatedString { segments, .. }
                if matches!(segments.as_slice(), [
                    StringSegment::Text { text, .. },
                    StringSegment::Expression { value: Expr::Literal { text: expression, .. }, .. }
                ] if text == prefix && expression == value)
        ));
    };

    let mut branch_control = value;
    for index in 0..25 {
        let Expr::Control {
            condition,
            body: Some(branch_body),
            alternate,
            ..
        } = branch_control
        else {
            panic!("else-if chain ended before branch {index}");
        };
        assert_branch_case_statement(branch_body.as_ref());
        let Expr::Block {
            tail: Some(branch_tail),
            ..
        } = branch_body.as_ref()
        else {
            panic!("branch {index} lost its final tail");
        };
        if index == 24 {
            assert!(matches!(
                condition.as_deref(),
                Some(Expr::InterpolatedString { segments, .. })
                    if matches!(segments.as_slice(), [
                        StringSegment::Text { text, .. },
                        StringSegment::Expression { value: Expr::Literal { text: value, .. }, .. }
                    ] if text == "finish " && value == "true")
            ));
            let Expr::InterpolatedString { segments, .. } = branch_tail.as_ref() else {
                panic!("expected an interpolated tail around the final case");
            };
            let [
                StringSegment::Text { text: prefix, .. },
                StringSegment::Expression { value: tail_case, .. },
                StringSegment::Text { text: suffix, .. },
            ] = segments.as_slice()
            else {
                panic!("expected prefix, case, and suffix in tail: {segments:?}");
            };
            assert_eq!(prefix, "tail ");
            assert_eq!(suffix, " end");
            let Expr::Control { arms, .. } = tail_case else {
                panic!("expected the tail interpolation to contain a case");
            };
            assert_eq!(arms.len(), 2, "{arms:?}");
            // Preserve each arm's block value independently from the
            // enclosing string and recovered else-if branch delimiters.
            assert_interpolated_case_arm_tail(&arms[0].body, "then ", "true");
            assert_interpolated_case_arm_tail(&arms[1].body, "otherwise ", "false");
            assert!(alternate.is_none(), "deepest else-if unexpectedly has an else");
        } else {
            assert!(matches!(
                branch_tail.as_ref(),
                Expr::Literal { text, .. } if text == &(100 + index).to_string()
            ));
            branch_control = alternate
                .as_deref()
                .expect("every earlier else-if has another branch");
        }
    }
}

#[test]
fn recovered_deep_final_else_if_keeps_case_arm_interpolation_suffixes() {
    let source = include_str!("fixtures/malformed-case-arm-max-depth-interpolated-final-else-if-arm-tail-suffixes.orna");
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
    let Some(Expr::Control {
        arms: outer_arms, ..
    }) = tail.as_deref()
    else {
        panic!("expected the outer case to remain the function tail");
    };
    assert_eq!(outer_arms.len(), 1, "{outer_arms:?}");

    let Expr::Control {
        condition: Some(scrutinee),
        ..
    } = &outer_arms[0].body
    else {
        panic!("expected a nested case in the recovered arm");
    };
    let Expr::InterpolatedString { segments, .. } = scrutinee.as_ref() else {
        panic!("expected an interpolated nested-case scrutinee");
    };
    let [StringSegment::Text { text, .. }, StringSegment::Expression { value, .. }] =
        segments.as_slice()
    else {
        panic!("expected one interpolation after the string prefix: {segments:?}");
    };
    assert_eq!(text, "branches ");

    let assert_branch_case_statement = |branch: &Expr| {
        let Expr::Block { statements, .. } = branch else {
            panic!("expected an if branch block");
        };
        assert!(matches!(
            statements.as_slice(),
            [Statement::Control {
                value: Expr::Control { arms, .. },
                ..
            }] if arms.len() == 2
        ));
    };
    let assert_arm_tail = |body: &Expr, prefix: &str, value: &str, suffix: &str| {
        let Expr::Block {
            statements,
            tail: Some(tail),
            ..
        } = body
        else {
            panic!("expected a case arm block with a final tail");
        };
        assert_eq!(statements.len(), 1, "{statements:?}");
        assert!(matches!(
            tail.as_ref(),
            Expr::InterpolatedString { segments, .. }
                if matches!(segments.as_slice(), [
                    StringSegment::Text { text: leading, .. },
                    StringSegment::Expression { value: Expr::Literal { text: expression, .. }, .. },
                    StringSegment::Text { text: trailing, .. }
                ] if leading == prefix && expression == value && trailing == suffix)
        ));
    };

    let mut branch_control = value;
    for index in 0..25 {
        let Expr::Control {
            condition,
            body: Some(branch_body),
            alternate,
            ..
        } = branch_control
        else {
            panic!("else-if chain ended before branch {index}");
        };
        assert_branch_case_statement(branch_body.as_ref());
        let Expr::Block {
            tail: Some(branch_tail),
            ..
        } = branch_body.as_ref()
        else {
            panic!("branch {index} lost its final tail");
        };
        if index == 24 {
            assert!(matches!(
                condition.as_deref(),
                Some(Expr::InterpolatedString { segments, .. })
                    if matches!(segments.as_slice(), [
                        StringSegment::Text { text, .. },
                        StringSegment::Expression { value: Expr::Literal { text: value, .. }, .. }
                    ] if text == "finish " && value == "true")
            ));
            let Expr::InterpolatedString { segments, .. } = branch_tail.as_ref() else {
                panic!("expected an interpolated tail around the final case");
            };
            let [
                StringSegment::Text { text: prefix, .. },
                StringSegment::Expression { value: tail_case, .. },
                StringSegment::Text { text: suffix, .. },
            ] = segments.as_slice()
            else {
                panic!("expected prefix, case, and suffix in tail: {segments:?}");
            };
            assert_eq!(prefix, "tail ");
            assert_eq!(suffix, " end");
            let Expr::Control { arms, .. } = tail_case else {
                panic!("expected the tail interpolation to contain a case");
            };
            assert_eq!(arms.len(), 2, "{arms:?}");
            // Keep trailing text on each arm-local tail inside that arm.
            assert_arm_tail(&arms[0].body, "then ", "true", " done");
            assert_arm_tail(&arms[1].body, "otherwise ", "false", " done");
            assert!(alternate.is_none(), "deepest else-if unexpectedly has an else");
        } else {
            assert!(matches!(
                branch_tail.as_ref(),
                Expr::Literal { text, .. } if text == &(100 + index).to_string()
            ));
            branch_control = alternate
                .as_deref()
                .expect("every earlier else-if has another branch");
        }
    }
}

#[test]
fn recovered_deep_final_else_if_keeps_suffix_interpolations_in_case_arm_tails() {
    let source = include_str!("fixtures/malformed-case-arm-max-depth-interpolated-final-else-if-arm-tail-suffix-interpolations.orna");
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
    let Some(Expr::Control {
        arms: outer_arms, ..
    }) = tail.as_deref()
    else {
        panic!("expected the outer case to remain the function tail");
    };
    assert_eq!(outer_arms.len(), 1, "{outer_arms:?}");

    let Expr::Control {
        condition: Some(scrutinee),
        ..
    } = &outer_arms[0].body
    else {
        panic!("expected a nested case in the recovered arm");
    };
    let Expr::InterpolatedString { segments, .. } = scrutinee.as_ref() else {
        panic!("expected an interpolated nested-case scrutinee");
    };
    let [StringSegment::Text { text, .. }, StringSegment::Expression { value, .. }] =
        segments.as_slice()
    else {
        panic!("expected one interpolation after the string prefix: {segments:?}");
    };
    assert_eq!(text, "branches ");

    let assert_branch_case_statement = |branch: &Expr| {
        let Expr::Block { statements, .. } = branch else {
            panic!("expected an if branch block");
        };
        assert!(matches!(
            statements.as_slice(),
            [Statement::Control {
                value: Expr::Control { arms, .. },
                ..
            }] if arms.len() == 2
        ));
    };
    let assert_arm_tail = |body: &Expr,
                           leading: &str,
                           first: &str,
                           middle: &str,
                           second: &str,
                           repeated: &str,
                           third: &str,
                           trailing: &str| {
        let Expr::Block {
            statements,
            tail: Some(tail),
            ..
        } = body
        else {
            panic!("expected a case arm block with a final tail");
        };
        assert_eq!(statements.len(), 1, "{statements:?}");
        assert!(matches!(
            tail.as_ref(),
            Expr::InterpolatedString { segments, .. }
                if matches!(segments.as_slice(), [
                    StringSegment::Text { text: first_text, .. },
                    StringSegment::Expression { value: Expr::Literal { text: first_value, .. }, .. },
                    StringSegment::Text { text: middle_text, .. },
                    StringSegment::Expression { value: Expr::Literal { text: second_value, .. }, .. },
                    StringSegment::Text { text: repeated_text, .. },
                    StringSegment::Expression { value: Expr::Literal { text: third_value, .. }, .. },
                    StringSegment::Text { text: last_text, .. }
                ] if first_text == leading
                    && first_value == first
                    && middle_text == middle
                    && second_value == second
                    && repeated_text == repeated
                    && third_value == third
                    && last_text == trailing)
        ));
    };

    let mut branch_control = value;
    for index in 0..25 {
        let Expr::Control {
            condition,
            body: Some(branch_body),
            alternate,
            ..
        } = branch_control
        else {
            panic!("else-if chain ended before branch {index}");
        };
        assert_branch_case_statement(branch_body.as_ref());
        let Expr::Block {
            tail: Some(branch_tail),
            ..
        } = branch_body.as_ref()
        else {
            panic!("branch {index} lost its final tail");
        };
        if index == 24 {
            assert!(matches!(
                condition.as_deref(),
                Some(Expr::InterpolatedString { segments, .. })
                    if matches!(segments.as_slice(), [
                        StringSegment::Text { text, .. },
                        StringSegment::Expression { value: Expr::Literal { text: value, .. }, .. }
                    ] if text == "finish " && value == "true")
            ));
            let Expr::InterpolatedString { segments, .. } = branch_tail.as_ref() else {
                panic!("expected an interpolated tail around the final case");
            };
            let [
                StringSegment::Text { text: prefix, .. },
                StringSegment::Expression { value: tail_case, .. },
                StringSegment::Text { text: suffix, .. },
            ] = segments.as_slice()
            else {
                panic!("expected prefix, case, and suffix in tail: {segments:?}");
            };
            assert_eq!(prefix, "tail ");
            assert_eq!(suffix, " end");
            let Expr::Control { arms, .. } = tail_case else {
                panic!("expected the tail interpolation to contain a case");
            };
            assert_eq!(arms.len(), 2, "{arms:?}");
            // Reference 1.0 defines case-arm and interpolation syntax, but not
            // recovery at maximum parser depth; preserve repeated suffixes in
            // source order within each owning case-arm tail.
            assert_arm_tail(
                &arms[0].body,
                "then ",
                "true",
                " and ",
                "false",
                " and ",
                "true",
                " done",
            );
            assert_arm_tail(
                &arms[1].body,
                "otherwise ",
                "false",
                " then ",
                "true",
                " then ",
                "false",
                " done",
            );
            assert!(alternate.is_none(), "deepest else-if unexpectedly has an else");
        } else {
            assert!(matches!(
                branch_tail.as_ref(),
                Expr::Literal { text, .. } if text == &(100 + index).to_string()
            ));
            branch_control = alternate
                .as_deref()
                .expect("every earlier else-if has another branch");
        }
    }
}

#[test]
fn recovered_deep_final_case_arm_suffixes_remain_in_the_final_branch_tail() {
    let source = include_str!("fixtures/malformed-case-arm-max-depth-interpolated-final-else-if-case-arm-final-tail.orna");
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
    let Expr::Block { tail, .. } = body else {
        panic!("expected a function block");
    };
    let Some(Expr::Control { arms, .. }) = tail.as_deref() else {
        panic!("expected the outer case to remain the function tail");
    };
    assert_eq!(arms.len(), 1, "{arms:?}");
    let Expr::Control {
        condition: Some(scrutinee),
        ..
    } = &arms[0].body
    else {
        panic!("expected the recovered outer arm to contain a case");
    };
    let Expr::InterpolatedString { segments, .. } = scrutinee.as_ref() else {
        panic!("expected an interpolated nested-case scrutinee");
    };
    let [StringSegment::Text { text, .. }, StringSegment::Expression { value, .. }] =
        segments.as_slice()
    else {
        panic!("expected one interpolation after the string prefix: {segments:?}");
    };
    assert_eq!(text, "branches ");

    let mut branch_control = value;
    for index in 0..24 {
        let Expr::Control {
            alternate: Some(alternate),
            ..
        } = branch_control
        else {
            panic!("else-if chain ended before final branch {index}");
        };
        branch_control = alternate.as_ref();
    }

    let Expr::Control {
        condition,
        body: Some(branch_body),
        alternate,
        ..
    } = branch_control
    else {
        panic!("expected the deepest else-if branch");
    };
    assert!(alternate.is_none(), "deepest else-if unexpectedly has an else");
    assert!(matches!(
        condition.as_deref(),
        Some(Expr::InterpolatedString { segments, .. })
            if matches!(segments.as_slice(), [
                StringSegment::Text { text, .. },
                StringSegment::Expression { value: Expr::Literal { text: value, .. }, .. }
            ] if text == "finish " && value == "true")
    ));
    let Expr::Block {
        statements,
        tail: Some(branch_tail),
        ..
    } = branch_body.as_ref()
    else {
        panic!("deepest branch lost its final case tail");
    };
    assert!(matches!(
        statements.as_slice(),
        [Statement::Control {
            value: Expr::Control { arms, .. },
            ..
        }] if arms.len() == 2
    ));

    let Expr::Control { arms, .. } = branch_tail.as_ref() else {
        panic!("expected the final case expression to remain the branch tail");
    };
    assert_eq!(arms.len(), 2, "{arms:?}");
    let assert_arm_tail = |body: &Expr,
                           leading: &str,
                           first: &str,
                           middle: &str,
                           second: &str,
                           repeated: &str,
                           third: &str,
                           trailing: &str| {
        let Expr::Block {
            statements,
            tail: Some(tail),
            ..
        } = body
        else {
            panic!("expected a case-arm block with a final interpolated tail");
        };
        assert_eq!(statements.len(), 1, "{statements:?}");
        assert!(matches!(
            tail.as_ref(),
            Expr::InterpolatedString { segments, .. }
                if matches!(segments.as_slice(), [
                    StringSegment::Text { text: first_text, .. },
                    StringSegment::Expression { value: Expr::Literal { text: first_value, .. }, .. },
                    StringSegment::Text { text: middle_text, .. },
                    StringSegment::Expression { value: Expr::Literal { text: second_value, .. }, .. },
                    StringSegment::Text { text: repeated_text, .. },
                    StringSegment::Expression { value: Expr::Literal { text: third_value, .. }, .. },
                    StringSegment::Text { text: last_text, .. }
                ] if first_text == leading
                    && first_value == first
                    && middle_text == middle
                    && second_value == second
                    && repeated_text == repeated
                    && third_value == third
                    && last_text == trailing)
        ));
    };

    // The reference specifies the final case expression and string grammar,
    // but not recovery at maximum nesting; keep each arm suffix on the direct
    // final branch tail in source order.
    assert_arm_tail(
        &arms[0].body,
        "then ",
        "true",
        " and ",
        "false",
        " and ",
        "true",
        " done",
    );
    assert_arm_tail(
        &arms[1].body,
        "otherwise ",
        "false",
        " then ",
        "true",
        " then ",
        "false",
        " done",
    );
}

#[test]
fn recovered_deep_final_closure_tail_keeps_case_arm_suffixes() {
    let source = include_str!("fixtures/malformed-case-arm-max-depth-interpolated-final-else-if-closure-tail.orna");
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
    let Expr::Block { tail, .. } = body else {
        panic!("expected a function block");
    };
    let Some(Expr::Control { arms, .. }) = tail.as_deref() else {
        panic!("expected the outer case to remain the function tail");
    };
    assert_eq!(arms.len(), 1, "{arms:?}");
    let Expr::Control {
        condition: Some(scrutinee),
        ..
    } = &arms[0].body
    else {
        panic!("expected the recovered outer arm to contain a case");
    };
    let Expr::InterpolatedString { segments, .. } = scrutinee.as_ref() else {
        panic!("expected an interpolated nested-case scrutinee");
    };
    let [StringSegment::Text { text, .. }, StringSegment::Expression { value, .. }] =
        segments.as_slice()
    else {
        panic!("expected one interpolation after the string prefix: {segments:?}");
    };
    assert_eq!(text, "branches ");

    let mut branch_control = value;
    for index in 0..24 {
        let Expr::Control {
            alternate: Some(alternate),
            ..
        } = branch_control
        else {
            panic!("else-if chain ended before final branch {index}");
        };
        branch_control = alternate.as_ref();
    }

    let Expr::Control {
        condition,
        body: Some(branch_body),
        alternate,
        ..
    } = branch_control
    else {
        panic!("expected the deepest else-if branch");
    };
    assert!(alternate.is_none(), "deepest else-if unexpectedly has an else");
    assert!(matches!(
        condition.as_deref(),
        Some(Expr::InterpolatedString { segments, .. })
            if matches!(segments.as_slice(), [
                StringSegment::Text { text, .. },
                StringSegment::Expression { value: Expr::Literal { text: value, .. }, .. }
            ] if text == "finish " && value == "true")
    ));
    let Expr::Block {
        statements,
        tail: Some(branch_tail),
        ..
    } = branch_body.as_ref()
    else {
        panic!("deepest branch lost its closure tail");
    };
    assert!(matches!(
        statements.as_slice(),
        [Statement::Control {
            value: Expr::Control { arms, .. },
            ..
        }] if arms.len() == 2
    ));

    let Expr::Lambda { parameters, body, .. } = branch_tail.as_ref() else {
        panic!("expected the final branch tail to remain a lambda");
    };
    assert_eq!(parameters.len(), 1, "{parameters:?}");
    let Expr::Block {
        statements,
        tail: Some(closure_tail),
        ..
    } = body.as_ref()
    else {
        panic!("expected the lambda body block");
    };
    assert!(statements.is_empty(), "{statements:?}");
    let Expr::Control {
        condition: Some(captured),
        arms,
        ..
    } = closure_tail.as_ref()
    else {
        panic!("expected the lambda's final case expression");
    };
    assert!(matches!(captured.as_ref(), Expr::Name { text, .. } if text == "captured"));
    assert_eq!(arms.len(), 2, "{arms:?}");

    let assert_arm_tail = |body: &Expr,
                           leading: &str,
                           first: &str,
                           middle: &str,
                           second: &str,
                           repeated: &str,
                           third: &str,
                           trailing: &str| {
        let Expr::Block {
            statements,
            tail: Some(tail),
            ..
        } = body
        else {
            panic!("expected a case-arm block with a final interpolated tail");
        };
        assert_eq!(statements.len(), 1, "{statements:?}");
        assert!(matches!(
            tail.as_ref(),
            Expr::InterpolatedString { segments, .. }
                if matches!(segments.as_slice(), [
                    StringSegment::Text { text: first_text, .. },
                    StringSegment::Expression { value: Expr::Literal { text: first_value, .. }, .. },
                    StringSegment::Text { text: middle_text, .. },
                    StringSegment::Expression { value: Expr::Literal { text: second_value, .. }, .. },
                    StringSegment::Text { text: repeated_text, .. },
                    StringSegment::Expression { value: Expr::Literal { text: third_value, .. }, .. },
                    StringSegment::Text { text: last_text, .. }
                ] if first_text == leading
                    && first_value == first
                    && middle_text == middle
                    && second_value == second
                    && repeated_text == repeated
                    && third_value == third
                    && last_text == trailing)
        ));
    };

    // The reference defines lambda, case, and interpolation syntax, but not
    // recovery at maximum nesting; preserve arm suffixes across both tails.
    assert_arm_tail(
        &arms[0].body,
        "then ",
        "true",
        " and ",
        "false",
        " and ",
        "true",
        " done",
    );
    assert_arm_tail(
        &arms[1].body,
        "otherwise ",
        "false",
        " then ",
        "true",
        " then ",
        "false",
        " done",
    );
}

#[test]
fn recovered_case_closure_arm_edges_keep_each_suffix() {
    let source = include_str!("fixtures/malformed-case-arm-closure-arm-edges.orna");
    let parsed = parse_module(source);

    assert_eq!(parsed.diagnostics.len(), 1, "{:?}", parsed.diagnostics);
    let diagnostic = &parsed.diagnostics[0];
    assert_eq!(diagnostic.code, "ORNA-PARSE-001");
    assert_eq!(diagnostic.message, "expected `:` after case pattern");
    let malformed_separator = source.find("{ value").expect("fixture has the malformed arm");
    assert_eq!(diagnostic.span.start, malformed_separator);
    assert_eq!(diagnostic.span.end, malformed_separator + 1);

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
        panic!("expected the case expression to remain the function tail");
    };
    assert_eq!(arms.len(), 3, "{arms:?}");
    assert!(matches!(
        &arms[0].pattern,
        Pattern::Literal { text, .. } if text == "1"
    ));
    assert!(matches!(&arms[0].body, Expr::Literal { text, .. } if text == "2"));

    fn suffix_parts(expression: &Expr) -> Vec<String> {
        let Expr::InterpolatedString { segments, .. } = expression else {
            panic!("expected an interpolated case-arm tail");
        };
        segments
            .iter()
            .map(|segment| match segment {
                StringSegment::Text { text, .. } => format!("text:{text}"),
                StringSegment::Expression {
                    value: Expr::Literal { text, .. },
                    ..
                } => format!("expression:{text}"),
                other => panic!("unexpected interpolation suffix segment: {other:?}"),
            })
            .collect()
    }

    let expected_suffixes: [&[&str]; 4] = [
        &[
            "text:first ",
            "expression:true",
            "text: and ",
            "expression:false",
            "text: and ",
            "expression:true",
            "text: done",
        ],
        &[
            "text:second ",
            "expression:false",
            "text: then ",
            "expression:true",
            "text: done",
        ],
        &[
            "text:third ",
            "expression:true",
            "text: then ",
            "expression:false",
            "text: done",
        ],
        &[
            "text:fourth ",
            "expression:false",
            "text: and ",
            "expression:true",
            "text: done",
        ],
    ];

    for (closure_index, (closure_arm, expected_condition)) in
        arms.iter().skip(1).zip(["captured", "false"]).enumerate()
    {
        let Expr::Lambda { body, .. } = &closure_arm.body else {
            panic!("case arm {closure_index} lost its closure: {closure_arm:?}");
        };
        let Expr::Block {
            statements,
            tail: Some(case_tail),
            ..
        } = body.as_ref()
        else {
            panic!("case arm {closure_index} closure lost its final case");
        };
        assert!(statements.is_empty(), "{statements:?}");
        let Expr::Control {
            condition: Some(condition),
            arms: nested_arms,
            ..
        } = case_tail.as_ref()
        else {
            panic!("case arm {closure_index} closure tail is not a case");
        };
        assert!(matches!(
            condition.as_ref(),
            Expr::Name { text, .. } if text == expected_condition
        ) || matches!(
            condition.as_ref(),
            Expr::Literal { text, .. } if text == expected_condition
        ));
        assert_eq!(nested_arms.len(), 2, "{nested_arms:?}");

        for (nested_index, nested_arm) in nested_arms.iter().enumerate() {
            let Expr::Block {
                statements,
                tail: Some(tail),
                ..
            } = &nested_arm.body
            else {
                panic!("nested arm {nested_index} lost its block tail");
            };
            assert_eq!(statements.len(), 1, "{statements:?}");
            let actual = suffix_parts(tail.as_ref());
            let expected = expected_suffixes[closure_index * 2 + nested_index]
                .iter()
                .map(|part| (*part).to_owned())
                .collect::<Vec<_>>();
            assert_eq!(actual, expected, "closure {closure_index}, arm {nested_index}");
        }
    }

    // The reference permits lambda block bodies and optional case-arm commas,
    // but leaves recovery after malformed arm content unspecified. Preserve the
    // following closures and their arm suffixes as the pragmatic recovery.
}

#[test]
fn recovered_final_case_arm_closure_tails_keep_suffix_edges() {
    let source = include_str!("fixtures/malformed-case-arm-final-closure-tail-edges.orna");
    let parsed = parse_module(source);

    assert_eq!(parsed.diagnostics.len(), 1, "{:?}", parsed.diagnostics);
    let diagnostic = &parsed.diagnostics[0];
    assert_eq!(diagnostic.code, "ORNA-PARSE-001");
    assert_eq!(diagnostic.message, "expected `:` after case pattern");
    let malformed_separator = source.find("{ value").expect("fixture has the malformed arm");
    assert_eq!(diagnostic.span.start, malformed_separator);
    assert_eq!(diagnostic.span.end, malformed_separator + 1);

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
        panic!("expected the case expression to remain the function tail");
    };
    assert_eq!(arms.len(), 3, "{arms:?}");
    assert!(matches!(
        &arms[0].pattern,
        Pattern::Literal { text, .. } if text == "1"
    ));
    assert!(matches!(&arms[0].body, Expr::Literal { text, .. } if text == "2"));

    fn suffix_parts(expression: &Expr) -> Vec<String> {
        let Expr::InterpolatedString { segments, .. } = expression else {
            panic!("expected an interpolated final-arm tail");
        };
        segments
            .iter()
            .map(|segment| match segment {
                StringSegment::Text { text, .. } => format!("text:{text}"),
                StringSegment::Expression {
                    value: Expr::Literal { text, .. },
                    ..
                } => format!("expression:{text}"),
                other => panic!("unexpected interpolation suffix segment: {other:?}"),
            })
            .collect()
    }

    let expected_suffixes: [[&[&str]; 2]; 2] = [
        [
            &[
                "text:first ",
                "expression:true",
                "text: and ",
                "expression:false",
                "text: then ",
                "expression:true",
                "text: done",
            ],
            &[
                "text:second ",
                "expression:false",
                "text: then ",
                "expression:true",
                "text: done",
            ],
        ],
        [
            &[
                "text:third ",
                "expression:true",
                "text: then ",
                "expression:false",
                "text: done",
            ],
            &[
                "text:final ",
                "expression:false",
                "text: and ",
                "expression:true",
                "text: done",
            ],
        ],
    ];

    for (closure_index, outer_arm) in arms.iter().skip(1).enumerate() {
        let (outer_pattern, condition_text) =
            [("true", "captured"), ("false", "false")][closure_index];
        assert!(matches!(
            &outer_arm.pattern,
            Pattern::Literal { text, .. } if text == outer_pattern
        ));
        let Expr::Block {
            statements,
            tail: Some(closure_tail),
            ..
        } = &outer_arm.body
        else {
            panic!("outer case arm {closure_index} lost its block tail");
        };
        assert_eq!(statements.len(), 1, "{statements:?}");
        let Expr::Lambda { body: lambda_body, .. } = closure_tail.as_ref() else {
            panic!("outer case arm {closure_index} lost its closure tail");
        };
        let Expr::Block {
            statements,
            tail: Some(case_tail),
            ..
        } = lambda_body.as_ref()
        else {
            panic!("closure {closure_index} lost its final case");
        };
        assert!(statements.is_empty(), "{statements:?}");
        let Expr::Control {
            condition: Some(condition),
            arms: nested_arms,
            ..
        } = case_tail.as_ref()
        else {
            panic!("closure {closure_index} tail is not a case");
        };
        assert!(matches!(
            condition.as_ref(),
            Expr::Name { text, .. } if text == condition_text
        ) || matches!(
            condition.as_ref(),
            Expr::Literal { text, .. } if text == condition_text
        ));
        assert_eq!(nested_arms.len(), 2, "{nested_arms:?}");

        for (nested_index, nested_arm) in nested_arms.iter().enumerate() {
            let Expr::Block {
                statements,
                tail: Some(tail),
                ..
            } = &nested_arm.body
            else {
                panic!("nested case arm {nested_index} lost its block tail");
            };
            assert_eq!(statements.len(), 1, "{statements:?}");
            let actual = suffix_parts(tail.as_ref());
            let expected = expected_suffixes[closure_index][nested_index]
                .iter()
                .map(|part| (*part).to_owned())
                .collect::<Vec<_>>();
            assert_eq!(actual, expected, "closure {closure_index}, arm {nested_index}");
        }
    }

    // The reference defines block-bodied closures, case arms, and optional
    // commas; malformed-arm recovery is unspecified. Preserve closure tails at
    // outer and inner final-arm boundaries as the pragmatic recovery.
}

#[test]
fn recovered_direct_final_closure_keeps_case_suffix_remainder() {
    let source = include_str!("fixtures/malformed-case-arm-direct-final-closure-remainder.orna");
    let parsed = parse_module(source);

    assert_eq!(parsed.diagnostics.len(), 1, "{:?}", parsed.diagnostics);
    let diagnostic = &parsed.diagnostics[0];
    assert_eq!(diagnostic.code, "ORNA-PARSE-001");
    assert_eq!(diagnostic.message, "expected `:` after case pattern");
    let malformed_separator = source.find("{ value").expect("fixture has malformed arm");
    assert_eq!(diagnostic.span.start, malformed_separator);
    assert_eq!(diagnostic.span.end, malformed_separator + 1);

    let Declaration::Function { body, .. } = &parsed.value.items[0].declaration else {
        panic!("expected a function declaration");
    };
    let Expr::Block { tail: Some(tail), .. } = body else {
        panic!("expected the function body tail");
    };
    let Expr::Control { arms, .. } = tail.as_ref() else {
        panic!("expected the outer case to remain the function tail");
    };
    assert_eq!(arms.len(), 3, "{arms:?}");
    assert!(matches!(&arms[1].body, Expr::Literal { text, .. } if text == "5"));

    let Expr::Lambda {
        parameters,
        body: closure_body,
        ..
    } = &arms[2].body
    else {
        panic!("final outer arm lost its direct closure: {:?}", arms[2].body);
    };
    assert_eq!(parameters.len(), 1);
    let Expr::Block {
        statements,
        tail: Some(closure_tail),
        ..
    } = closure_body.as_ref()
    else {
        panic!("direct closure lost its final case expression");
    };
    assert!(statements.is_empty(), "{statements:?}");
    let Expr::Control { arms: closure_arms, .. } = closure_tail.as_ref() else {
        panic!("closure tail is not a case expression");
    };
    assert_eq!(closure_arms.len(), 2, "{closure_arms:?}");

    fn interpolation_parts(expression: &Expr) -> Vec<String> {
        let Expr::InterpolatedString { segments, .. } = expression else {
            panic!("expected a direct interpolated arm body: {expression:?}");
        };
        segments
            .iter()
            .map(|segment| match segment {
                StringSegment::Text { text, .. } => format!("text:{text}"),
                StringSegment::Expression {
                    value: Expr::Literal { text, .. },
                    ..
                } => format!("expression:{text}"),
                other => panic!("unexpected interpolation segment: {other:?}"),
            })
            .collect()
    }

    let expected_suffixes: [&[&str]; 2] = [
        &[
            "text:first ",
            "expression:true",
            "text: and ",
            "expression:false",
            "text: then ",
            "expression:true",
            "text: remains",
        ],
        &[
            "text:final ",
            "expression:false",
            "text: and ",
            "expression:true",
            "text: stays",
        ],
    ];
    for (index, arm) in closure_arms.iter().enumerate() {
        let expected = expected_suffixes[index]
            .iter()
            .map(|part| (*part).to_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            interpolation_parts(&arm.body),
            expected,
            "nested closure arm {index}"
        );
    }

    // The grammar permits direct lambda case bodies and optional arm commas,
    // but does not specify recovery after the malformed first arm. Preserve
    // the final closure and the complete interpolation suffixes pragmatically.
}

#[test]
fn recovered_final_multi_closure_keeps_nested_case_suffixes() {
    let source = include_str!("fixtures/malformed-case-arm-direct-final-multi-closure-suffixes.orna");
    let parsed = parse_module(source);

    assert_eq!(parsed.diagnostics.len(), 1, "{:?}", parsed.diagnostics);
    let diagnostic = &parsed.diagnostics[0];
    assert_eq!(diagnostic.code, "ORNA-PARSE-001");
    assert_eq!(diagnostic.message, "expected `:` after case pattern");
    let malformed_separator = source.find("{ value").expect("fixture has malformed arm");
    assert_eq!(diagnostic.span.start, malformed_separator);
    assert_eq!(diagnostic.span.end, malformed_separator + 1);

    let Declaration::Function { body, .. } = &parsed.value.items[0].declaration else {
        panic!("expected a function declaration");
    };
    let Expr::Block { tail: Some(tail), .. } = body else {
        panic!("expected the function body tail");
    };
    let Expr::Control { arms, .. } = tail.as_ref() else {
        panic!("expected the outer case to remain the function tail");
    };
    assert_eq!(arms.len(), 3, "{arms:?}");
    assert!(matches!(&arms[1].body, Expr::Literal { text, .. } if text == "5"));

    let Expr::Lambda {
        parameters,
        body: closure_body,
        ..
    } = &arms[2].body
    else {
        panic!("final outer arm lost its direct closure: {:?}", arms[2].body);
    };
    assert_eq!(parameters.len(), 2);
    let Expr::Block {
        statements,
        tail: Some(closure_tail),
        ..
    } = closure_body.as_ref()
    else {
        panic!("direct closure lost its final case expression");
    };
    assert!(statements.is_empty(), "{statements:?}");
    let Expr::Control {
        condition: Some(condition),
        arms: closure_arms,
        ..
    } = closure_tail.as_ref()
    else {
        panic!("direct closure tail is not a case expression");
    };
    assert!(matches!(condition.as_ref(), Expr::Name { text, .. } if text == "selected"));
    assert_eq!(closure_arms.len(), 2, "{closure_arms:?}");

    fn interpolation_parts(expression: &Expr) -> Vec<String> {
        let Expr::InterpolatedString { segments, .. } = expression else {
            panic!("expected a direct interpolated closure body: {expression:?}");
        };
        segments
            .iter()
            .map(|segment| match segment {
                StringSegment::Text { text, .. } => format!("text:{text}"),
                StringSegment::Expression {
                    value: Expr::Name { text, .. },
                    ..
                } => format!("expression:{text}"),
                other => panic!("unexpected interpolation segment: {other:?}"),
            })
            .collect()
    }

    let expected_suffixes: [&[&str]; 2] = [
        &[
            "text:first ",
            "expression:result",
            "text: retains ",
            "expression:spare",
            "text: after",
        ],
        &[
            "text:final ",
            "expression:result",
            "text: retains ",
            "expression:selected",
            "text: through",
        ],
    ];
    for (index, arm) in closure_arms.iter().enumerate() {
        let Expr::Lambda {
            parameters,
            body: nested_body,
            ..
        } = &arm.body
        else {
            panic!("nested case arm {index} lost its direct closure");
        };
        assert_eq!(parameters.len(), 1);
        let expected = expected_suffixes[index]
            .iter()
            .map(|part| (*part).to_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            interpolation_parts(nested_body.as_ref()),
            expected,
            "nested closure arm {index}"
        );
    }

    // The grammar permits parenthesized lambda parameters, direct lambda case
    // bodies, and optional commas. Recovery from the preceding malformed arm
    // is unspecified; keep both closure tails and their suffix text pragmatically.
}

#[test]
fn recovered_nested_final_closure_keeps_deep_case_suffixes() {
    let source = include_str!("fixtures/malformed-case-arm-nested-final-closure-suffixes.orna");
    let parsed = parse_module(source);

    assert_eq!(parsed.diagnostics.len(), 1, "{:?}", parsed.diagnostics);
    let diagnostic = &parsed.diagnostics[0];
    assert_eq!(diagnostic.code, "ORNA-PARSE-001");
    assert_eq!(diagnostic.message, "expected `:` after case pattern");
    let malformed_separator = source.find("{ value").expect("fixture has malformed arm");
    assert_eq!(diagnostic.span.start, malformed_separator);
    assert_eq!(diagnostic.span.end, malformed_separator + 1);

    let Declaration::Function { body, .. } = &parsed.value.items[0].declaration else {
        panic!("expected a function declaration");
    };
    let Expr::Block { tail: Some(tail), .. } = body else {
        panic!("expected the function body tail");
    };
    let Expr::Control { arms, .. } = tail.as_ref() else {
        panic!("expected the outer case to remain the function tail");
    };
    assert_eq!(arms.len(), 3, "{arms:?}");

    let Expr::Lambda {
        parameters,
        body: outer_body,
        ..
    } = &arms[2].body
    else {
        panic!("final outer arm lost its direct closure: {:?}", arms[2].body);
    };
    assert_eq!(parameters.len(), 1);
    let Expr::Block {
        statements,
        tail: Some(outer_tail),
        ..
    } = outer_body.as_ref()
    else {
        panic!("outer closure lost its final case expression");
    };
    assert!(statements.is_empty(), "{statements:?}");
    let Expr::Control {
        arms: outer_arms, ..
    } = outer_tail.as_ref()
    else {
        panic!("outer closure tail is not a case expression");
    };
    assert_eq!(outer_arms.len(), 2, "{outer_arms:?}");

    fn interpolation_parts(expression: &Expr) -> Vec<String> {
        let Expr::InterpolatedString { segments, .. } = expression else {
            panic!("expected an interpolated case-arm tail: {expression:?}");
        };
        segments
            .iter()
            .map(|segment| match segment {
                StringSegment::Text { text, .. } => format!("text:{text}"),
                StringSegment::Expression {
                    value: Expr::Name { text, .. },
                    ..
                } => format!("expression:{text}"),
                other => panic!("unexpected interpolation segment: {other:?}"),
            })
            .collect()
    }

    assert_eq!(
        interpolation_parts(&outer_arms[0].body),
        [
            "text:prefix ",
            "expression:outer",
            "text: direct branch remains"
        ]
        .map(str::to_owned)
    );

    let Expr::Lambda {
        parameters,
        body: inner_body,
        ..
    } = &outer_arms[1].body
    else {
        panic!("final nested case arm lost its closure");
    };
    assert_eq!(parameters.len(), 1);
    let Expr::Block {
        statements,
        tail: Some(inner_tail),
        ..
    } = inner_body.as_ref()
    else {
        panic!("nested closure lost its final case expression");
    };
    assert!(statements.is_empty(), "{statements:?}");
    let Expr::Control {
        condition: Some(condition),
        arms: inner_arms,
        ..
    } = inner_tail.as_ref()
    else {
        panic!("nested closure tail is not a case expression");
    };
    assert!(matches!(condition.as_ref(), Expr::Name { text, .. } if text == "flag"));
    assert_eq!(inner_arms.len(), 2, "{inner_arms:?}");
    assert_eq!(
        interpolation_parts(&inner_arms[0].body),
        [
            "text:first ",
            "expression:inner",
            "text: keeps ",
            "expression:outer",
            "text: before close"
        ]
        .map(str::to_owned)
    );
    assert_eq!(
        interpolation_parts(&inner_arms[1].body),
        [
            "text:final ",
            "expression:inner",
            "text: keeps ",
            "expression:flag",
            "text: remainder"
        ]
        .map(str::to_owned)
    );

    // Lambda block bodies and case arms are specified, but recovery following
    // malformed arm content is not. Keep the nested final closure and suffixes
    // at each case boundary as the pragmatic recovery behavior.
}

#[test]
fn recovered_final_case_closure_block_keeps_suffix_remainder() {
    let source = include_str!("fixtures/malformed-case-arm-nested-closure-final-tail.orna");
    let parsed = parse_module(source);

    assert_eq!(parsed.diagnostics.len(), 1, "{:?}", parsed.diagnostics);
    let diagnostic = &parsed.diagnostics[0];
    assert_eq!(diagnostic.code, "ORNA-PARSE-001");
    assert_eq!(diagnostic.message, "expected `:` after case pattern");
    let malformed_separator = source.find("{ value").expect("fixture has malformed arm");
    assert_eq!(diagnostic.span.start, malformed_separator);
    assert_eq!(diagnostic.span.end, malformed_separator + 1);

    let Declaration::Function { body, .. } = &parsed.value.items[0].declaration else {
        panic!("expected a function declaration");
    };
    let Expr::Block { tail: Some(tail), .. } = body else {
        panic!("expected the function body tail");
    };
    let Expr::Control { arms, .. } = tail.as_ref() else {
        panic!("expected the outer case to remain the function tail");
    };
    assert_eq!(arms.len(), 3, "{arms:?}");

    let Expr::Lambda {
        body: outer_body, ..
    } = &arms[2].body
    else {
        panic!("final outer arm lost its closure");
    };
    let Expr::Block {
        tail: Some(outer_tail), ..
    } = outer_body.as_ref()
    else {
        panic!("outer closure lost its case tail");
    };
    let Expr::Control {
        arms: outer_arms, ..
    } = outer_tail.as_ref()
    else {
        panic!("outer closure tail is not a case expression");
    };
    assert_eq!(outer_arms.len(), 2, "{outer_arms:?}");

    fn interpolation_parts(expression: &Expr) -> Vec<String> {
        let Expr::InterpolatedString { segments, .. } = expression else {
            panic!("expected an interpolated case-arm tail: {expression:?}");
        };
        segments
            .iter()
            .map(|segment| match segment {
                StringSegment::Text { text, .. } => format!("text:{text}"),
                StringSegment::Expression {
                    value: Expr::Name { text, .. },
                    ..
                } => format!("expression:{text}"),
                other => panic!("unexpected interpolation segment: {other:?}"),
            })
            .collect()
    }

    assert_eq!(
        interpolation_parts(&outer_arms[0].body),
        [
            "text:prefix ",
            "expression:outer",
            "text: direct branch remains"
        ]
        .map(str::to_owned)
    );

    let Expr::Lambda {
        body: inner_body, ..
    } = &outer_arms[1].body
    else {
        panic!("final outer case arm lost its nested closure");
    };
    let Expr::Block {
        tail: Some(inner_tail), ..
    } = inner_body.as_ref()
    else {
        panic!("nested closure lost its case tail");
    };
    let Expr::Control {
        arms: inner_arms, ..
    } = inner_tail.as_ref()
    else {
        panic!("nested closure tail is not a case expression");
    };
    assert_eq!(inner_arms.len(), 2, "{inner_arms:?}");
    assert_eq!(
        interpolation_parts(&inner_arms[0].body),
        [
            "text:first ",
            "expression:inner",
            "text: keeps ",
            "expression:outer",
            "text: before close"
        ]
        .map(str::to_owned)
    );

    let Expr::Lambda {
        parameters,
        body: final_body,
        ..
    } = &inner_arms[1].body
    else {
        panic!("last nested case arm lost its final closure");
    };
    assert_eq!(parameters.len(), 1);
    let Expr::Block {
        statements,
        tail: Some(final_tail),
        ..
    } = final_body.as_ref()
    else {
        panic!("final closure block lost its suffix tail");
    };
    assert!(statements.is_empty(), "{statements:?}");
    assert_eq!(
        interpolation_parts(final_tail.as_ref()),
        [
            "text:final ",
            "expression:last_value",
            "text: keeps ",
            "expression:flag",
            "text: remainder"
        ]
        .map(str::to_owned)
    );

    // The reference permits lambda block bodies and optional case-arm commas,
    // but malformed-arm recovery is unspecified. Keep the final nested closure
    // block and its interpolation remainder attached as the pragmatic choice.
}

#[test]
fn recovered_deepest_final_closure_case_keeps_suffixes() {
    let source = include_str!("fixtures/malformed-case-arm-nested-final-closure-case-tail.orna");
    let parsed = parse_module(source);

    assert_eq!(parsed.diagnostics.len(), 1, "{:?}", parsed.diagnostics);
    let diagnostic = &parsed.diagnostics[0];
    assert_eq!(diagnostic.code, "ORNA-PARSE-001");
    assert_eq!(diagnostic.message, "expected `:` after case pattern");
    let malformed_separator = source.find("{ value").expect("fixture has malformed arm");
    assert_eq!(diagnostic.span.start, malformed_separator);
    assert_eq!(diagnostic.span.end, malformed_separator + 1);

    let Declaration::Function { body, .. } = &parsed.value.items[0].declaration else {
        panic!("expected a function declaration");
    };
    let Expr::Block { tail: Some(tail), .. } = body else {
        panic!("expected the function body tail");
    };
    let Expr::Control { arms, .. } = tail.as_ref() else {
        panic!("expected the outer case to remain the function tail");
    };
    assert_eq!(arms.len(), 3, "{arms:?}");

    let Expr::Lambda {
        body: outer_body, ..
    } = &arms[2].body
    else {
        panic!("final outer arm lost its closure");
    };
    let Expr::Block {
        tail: Some(outer_tail), ..
    } = outer_body.as_ref()
    else {
        panic!("outer closure lost its case tail");
    };
    let Expr::Control {
        arms: outer_arms, ..
    } = outer_tail.as_ref()
    else {
        panic!("outer closure tail is not a case expression");
    };
    assert_eq!(outer_arms.len(), 2, "{outer_arms:?}");

    let Expr::Lambda {
        body: inner_body, ..
    } = &outer_arms[1].body
    else {
        panic!("final outer case arm lost its closure");
    };
    let Expr::Block {
        tail: Some(inner_tail), ..
    } = inner_body.as_ref()
    else {
        panic!("inner closure lost its case tail");
    };
    let Expr::Control {
        arms: inner_arms, ..
    } = inner_tail.as_ref()
    else {
        panic!("inner closure tail is not a case expression");
    };
    assert_eq!(inner_arms.len(), 2, "{inner_arms:?}");

    let Expr::Lambda {
        body: final_body, ..
    } = &inner_arms[1].body
    else {
        panic!("final inner case arm lost its closure");
    };
    let Expr::Block {
        statements,
        tail: Some(final_tail),
        ..
    } = final_body.as_ref()
    else {
        panic!("deep closure lost its final case tail");
    };
    assert!(statements.is_empty(), "{statements:?}");
    let Expr::Control {
        condition: Some(condition),
        arms: final_arms,
        ..
    } = final_tail.as_ref()
    else {
        panic!("deep closure tail is not its final case");
    };
    assert!(matches!(condition.as_ref(), Expr::Name { text, .. } if text == "last_value"));
    assert_eq!(final_arms.len(), 2, "{final_arms:?}");

    fn interpolation_parts(expression: &Expr) -> Vec<String> {
        let Expr::InterpolatedString { segments, .. } = expression else {
            panic!("expected a final interpolated arm tail: {expression:?}");
        };
        segments
            .iter()
            .map(|segment| match segment {
                StringSegment::Text { text, .. } => format!("text:{text}"),
                StringSegment::Expression {
                    value: Expr::Name { text, .. },
                    ..
                } => format!("expression:{text}"),
                other => panic!("unexpected interpolation segment: {other:?}"),
            })
            .collect()
    }

    let expected_suffixes: [&[&str]; 2] = [
        &[
            "text:deep ",
            "expression:last_value",
            "text: retains ",
            "expression:outer",
            "text: then",
        ],
        &[
            "text:final ",
            "expression:last_value",
            "text: retains ",
            "expression:flag",
            "text: remainder",
        ],
    ];
    for (index, arm) in final_arms.iter().enumerate() {
        let expected = expected_suffixes[index]
            .iter()
            .map(|part| (*part).to_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            interpolation_parts(&arm.body),
            expected,
            "deepest final case arm {index}"
        );
    }

    // The grammar permits lambda block bodies and nested cases, but recovery
    // after the malformed leading arm is unspecified. Preserve each final case
    // arm's interpolation suffix as the pragmatic recovery behavior.
}

#[test]
fn recovered_deep_interpolated_closure_keeps_final_suffix_text() {
    let source = include_str!("fixtures/malformed-case-arm-deep-interpolated-closure-case-tail.orna");
    let parsed = parse_module(source);

    assert_eq!(parsed.diagnostics.len(), 1, "{:?}", parsed.diagnostics);
    let diagnostic = &parsed.diagnostics[0];
    assert_eq!(diagnostic.code, "ORNA-PARSE-001");
    assert_eq!(diagnostic.message, "expected `:` after case pattern");
    let malformed_separator = source.find("{ value").expect("fixture has malformed arm");
    assert_eq!(diagnostic.span.start, malformed_separator);
    assert_eq!(diagnostic.span.end, malformed_separator + 1);

    let Declaration::Function { body, .. } = &parsed.value.items[0].declaration else {
        panic!("expected a function declaration");
    };
    let Expr::Block { tail: Some(tail), .. } = body else {
        panic!("expected the function body tail");
    };
    let Expr::Control { arms, .. } = tail.as_ref() else {
        panic!("expected the outer case to remain the function tail");
    };
    assert_eq!(arms.len(), 3, "{arms:?}");

    let Expr::Lambda {
        body: outer_body, ..
    } = &arms[2].body
    else {
        panic!("final outer arm lost its closure");
    };
    let Expr::Block {
        tail: Some(outer_tail), ..
    } = outer_body.as_ref()
    else {
        panic!("outer closure lost its case tail");
    };
    let Expr::Control {
        arms: outer_arms, ..
    } = outer_tail.as_ref()
    else {
        panic!("outer closure tail is not a case expression");
    };
    let Expr::Lambda {
        body: inner_body, ..
    } = &outer_arms[1].body
    else {
        panic!("final outer case arm lost its nested closure");
    };
    let Expr::Block {
        tail: Some(inner_tail), ..
    } = inner_body.as_ref()
    else {
        panic!("nested closure lost its case tail");
    };
    let Expr::Control {
        arms: inner_arms, ..
    } = inner_tail.as_ref()
    else {
        panic!("nested closure tail is not a case expression");
    };
    let Expr::Lambda {
        body: final_body, ..
    } = &inner_arms[1].body
    else {
        panic!("final nested case arm lost its closure");
    };
    let Expr::Block {
        tail: Some(final_tail), ..
    } = final_body.as_ref()
    else {
        panic!("deep closure lost its final case tail");
    };
    let Expr::Control {
        arms: final_arms, ..
    } = final_tail.as_ref()
    else {
        panic!("deep closure tail is not a case expression");
    };
    let Expr::InterpolatedString { segments, .. } = &final_arms[1].body else {
        panic!("deepest final case arm lost its interpolation");
    };
    assert_eq!(segments.len(), 3, "{segments:?}");
    assert!(matches!(
        &segments[0],
        StringSegment::Text { text, .. } if text == "final "
    ));
    assert!(matches!(
        &segments[2],
        StringSegment::Text { text, .. } if text == " remainder"
    ));

    let StringSegment::Expression { value, .. } = &segments[1] else {
        panic!("expected the interpolation to contain a lambda expression");
    };
    let Expr::Lambda {
        parameters,
        body: interpolated_lambda_body,
        ..
    } = value
    else {
        panic!("interpolated expression lost its lambda: {value:?}");
    };
    assert_eq!(parameters.len(), 1);
    let Expr::Block {
        statements,
        tail: Some(interpolated_lambda_tail),
        ..
    } = interpolated_lambda_body.as_ref()
    else {
        panic!("interpolated lambda lost its case tail");
    };
    assert!(statements.is_empty(), "{statements:?}");
    let Expr::Control {
        condition: Some(condition),
        arms: interpolation_arms,
        ..
    } = interpolated_lambda_tail.as_ref()
    else {
        panic!("interpolated lambda tail is not a case expression");
    };
    assert!(matches!(condition.as_ref(), Expr::Name { text, .. } if text == "probe"));
    assert_eq!(interpolation_arms.len(), 2, "{interpolation_arms:?}");
    assert!(matches!(
        &interpolation_arms[0].body,
        Expr::Name { text, .. } if text == "outer"
    ));
    assert!(matches!(
        &interpolation_arms[1].body,
        Expr::Name { text, .. } if text == "flag"
    ));

    // Interpolation expressions use normal nested-delimiter parsing, and the
    // grammar permits lambda and case expressions there. Recovery after the
    // malformed leading arm is unspecified; retain the trailing string text.
}

#[test]
fn recovered_repeated_interpolated_closures_keep_suffix_tails() {
    let source = include_str!("fixtures/malformed-case-arm-repeated-interpolated-closure-tails.orna");
    let parsed = parse_module(source);

    assert_eq!(parsed.diagnostics.len(), 1, "{:?}", parsed.diagnostics);
    let diagnostic = &parsed.diagnostics[0];
    assert_eq!(diagnostic.code, "ORNA-PARSE-001");
    assert_eq!(diagnostic.message, "expected `:` after case pattern");
    let malformed_separator = source.find("{ value").expect("fixture has malformed arm");
    assert_eq!(diagnostic.span.start, malformed_separator);
    assert_eq!(diagnostic.span.end, malformed_separator + 1);

    let Declaration::Function { body, .. } = &parsed.value.items[0].declaration else {
        panic!("expected a function declaration");
    };
    let Expr::Block { tail: Some(tail), .. } = body else {
        panic!("expected the function body tail");
    };
    let Expr::Control { arms, .. } = tail.as_ref() else {
        panic!("expected the outer case to remain the function tail");
    };
    let Expr::Lambda {
        body: outer_body, ..
    } = &arms[2].body
    else {
        panic!("final outer arm lost its closure");
    };
    let Expr::Block {
        tail: Some(outer_tail), ..
    } = outer_body.as_ref()
    else {
        panic!("outer closure lost its case tail");
    };
    let Expr::Control {
        arms: outer_arms, ..
    } = outer_tail.as_ref()
    else {
        panic!("outer closure tail is not a case expression");
    };
    let Expr::Lambda {
        body: inner_body, ..
    } = &outer_arms[1].body
    else {
        panic!("final outer case arm lost its nested closure");
    };
    let Expr::Block {
        tail: Some(inner_tail), ..
    } = inner_body.as_ref()
    else {
        panic!("nested closure lost its case tail");
    };
    let Expr::Control {
        arms: inner_arms, ..
    } = inner_tail.as_ref()
    else {
        panic!("nested closure tail is not a case expression");
    };
    let Expr::Lambda {
        body: final_body, ..
    } = &inner_arms[1].body
    else {
        panic!("deep final case arm lost its closure");
    };
    let Expr::Block {
        tail: Some(final_tail), ..
    } = final_body.as_ref()
    else {
        panic!("deep closure lost its case tail");
    };
    let Expr::Control {
        arms: final_arms, ..
    } = final_tail.as_ref()
    else {
        panic!("deep closure tail is not a case expression");
    };
    let Expr::InterpolatedString { segments, .. } = &final_arms[1].body else {
        panic!("deepest final arm lost its interpolated string");
    };
    assert_eq!(segments.len(), 5, "{segments:?}");
    assert!(matches!(
        &segments[0],
        StringSegment::Text { text, .. } if text == "first "
    ));
    assert!(matches!(
        &segments[2],
        StringSegment::Text { text, .. } if text == " around "
    ));
    assert!(matches!(
        &segments[4],
        StringSegment::Text { text, .. } if text == " remainder"
    ));

    fn assert_interpolated_closure_case(segment: &StringSegment, name: &str, values: [&str; 2]) {
        let StringSegment::Expression { value, .. } = segment else {
            panic!("expected an interpolated closure case: {segment:?}");
        };
        let Expr::Lambda {
            parameters,
            body,
            ..
        } = value
        else {
            panic!("interpolation lost its closure: {value:?}");
        };
        assert_eq!(parameters.len(), 1);
        let Expr::Block {
            statements,
            tail: Some(tail),
            ..
        } = body.as_ref()
        else {
            panic!("interpolated closure lost its case tail");
        };
        assert!(statements.is_empty(), "{statements:?}");
        let Expr::Control {
            condition: Some(condition),
            arms,
            ..
        } = tail.as_ref()
        else {
            panic!("interpolated closure tail is not a case");
        };
        assert!(matches!(condition.as_ref(), Expr::Name { text, .. } if text == name));
        assert_eq!(arms.len(), 2, "{arms:?}");
        for (arm, expected) in arms.iter().zip(values) {
            assert!(matches!(
                &arm.body,
                Expr::Name { text, .. } if text == expected
            ));
        }
    }

    assert_interpolated_closure_case(&segments[1], "probe", ["outer", "flag"]);
    assert_interpolated_closure_case(&segments[3], "other", ["flag", "outer"]);

    // Interpolation expressions use the ordinary nested-delimiter grammar.
    // Recovery after the malformed leading arm is unspecified; preserve both
    // closure/case expressions and the suffix text after each interpolation.
}

#[test]
fn recovered_repeated_closures_keep_nested_string_suffixes() {
    let source = include_str!("fixtures/malformed-case-arm-repeated-nested-string-closures.orna");
    let parsed = parse_module(source);

    assert_eq!(parsed.diagnostics.len(), 1, "{:?}", parsed.diagnostics);
    let diagnostic = &parsed.diagnostics[0];
    assert_eq!(diagnostic.code, "ORNA-PARSE-001");
    assert_eq!(diagnostic.message, "expected `:` after case pattern");
    let malformed_separator = source.find("{ value").expect("fixture has malformed arm");
    assert_eq!(diagnostic.span.start, malformed_separator);
    assert_eq!(diagnostic.span.end, malformed_separator + 1);

    let Declaration::Function { body, .. } = &parsed.value.items[0].declaration else {
        panic!("expected a function declaration");
    };
    let Expr::Block { tail: Some(tail), .. } = body else {
        panic!("expected the function body tail");
    };
    let Expr::Control { arms, .. } = tail.as_ref() else {
        panic!("expected the outer case to remain the function tail");
    };
    let Expr::Lambda {
        body: outer_body, ..
    } = &arms[2].body
    else {
        panic!("final outer arm lost its closure");
    };
    let Expr::Block {
        tail: Some(outer_tail), ..
    } = outer_body.as_ref()
    else {
        panic!("outer closure lost its case tail");
    };
    let Expr::Control {
        arms: outer_arms, ..
    } = outer_tail.as_ref()
    else {
        panic!("outer closure tail is not a case expression");
    };
    let Expr::Lambda {
        body: inner_body, ..
    } = &outer_arms[1].body
    else {
        panic!("final outer case arm lost its nested closure");
    };
    let Expr::Block {
        tail: Some(inner_tail), ..
    } = inner_body.as_ref()
    else {
        panic!("nested closure lost its case tail");
    };
    let Expr::Control {
        arms: inner_arms, ..
    } = inner_tail.as_ref()
    else {
        panic!("nested closure tail is not a case expression");
    };
    let Expr::Lambda {
        body: final_body, ..
    } = &inner_arms[1].body
    else {
        panic!("deep final case arm lost its closure");
    };
    let Expr::Block {
        tail: Some(final_tail), ..
    } = final_body.as_ref()
    else {
        panic!("deep closure lost its case tail");
    };
    let Expr::Control {
        arms: final_arms, ..
    } = final_tail.as_ref()
    else {
        panic!("deep closure tail is not a case expression");
    };
    let Expr::InterpolatedString { segments, .. } = &final_arms[1].body else {
        panic!("deepest final arm lost its interpolated string");
    };
    assert_eq!(segments.len(), 5, "{segments:?}");
    assert!(matches!(
        &segments[0],
        StringSegment::Text { text, .. } if text == "first "
    ));
    assert!(matches!(
        &segments[2],
        StringSegment::Text { text, .. } if text == " between "
    ));
    assert!(matches!(
        &segments[4],
        StringSegment::Text { text, .. } if text == " remainder"
    ));

    fn assert_nested_closure_strings(
        segment: &StringSegment,
        condition_name: &str,
        expected_strings: [[&str; 3]; 2],
    ) {
        let StringSegment::Expression { value, .. } = segment else {
            panic!("expected an interpolated closure: {segment:?}");
        };
        let Expr::Lambda { body, .. } = value else {
            panic!("interpolation lost its closure: {value:?}");
        };
        let Expr::Block {
            statements,
            tail: Some(tail),
            ..
        } = body.as_ref()
        else {
            panic!("nested closure lost its case tail");
        };
        assert!(statements.is_empty(), "{statements:?}");
        let Expr::Control {
            condition: Some(condition),
            arms,
            ..
        } = tail.as_ref()
        else {
            panic!("nested closure tail is not a case expression");
        };
        assert!(matches!(
            condition.as_ref(),
            Expr::Name { text, .. } if text == condition_name
        ));
        assert_eq!(arms.len(), 2, "{arms:?}");
        for (arm, expected) in arms.iter().zip(expected_strings) {
            let Expr::InterpolatedString { segments, .. } = &arm.body else {
                panic!("nested case arm lost its interpolated string: {arm:?}");
            };
            assert!(matches!(
                segments.as_slice(),
                [
                    StringSegment::Text { text: prefix, .. },
                    StringSegment::Expression {
                        value: Expr::Name { text: name, .. },
                        ..
                    },
                    StringSegment::Text { text: suffix, .. }
                ] if prefix == expected[0] && name == expected[1] && suffix == expected[2]
            ));
        }
    }

    assert_nested_closure_strings(
        &segments[1],
        "probe",
        [
            ["inner ", "outer", " first end"],
            ["inner ", "flag", " first final"],
        ],
    );
    assert_nested_closure_strings(
        &segments[3],
        "other",
        [
            ["inner ", "flag", " second end"],
            ["inner ", "outer", " second final"],
        ],
    );

    // The grammar allows nested strings and closure/case expressions inside
    // interpolation. Recovery after the malformed leading arm is unspecified;
    // retain both nested suffixes and the outer separator/remainder text.
}

#[test]
fn recovered_nested_interpolated_closures_keep_suffix_tails() {
    let source = include_str!("fixtures/malformed-case-arm-nested-interpolated-closure-tail.orna");
    let parsed = parse_module(source);

    assert_eq!(parsed.diagnostics.len(), 1, "{:?}", parsed.diagnostics);
    let diagnostic = &parsed.diagnostics[0];
    assert_eq!(diagnostic.code, "ORNA-PARSE-001");
    assert_eq!(diagnostic.message, "expected `:` after case pattern");
    let malformed_separator = source.find("{ value").expect("fixture has malformed arm");
    assert_eq!(diagnostic.span.start, malformed_separator);
    assert_eq!(diagnostic.span.end, malformed_separator + 1);

    let Declaration::Function { body, .. } = &parsed.value.items[0].declaration else {
        panic!("expected a function declaration");
    };
    let Expr::Block {
        tail: Some(tail), ..
    } = body
    else {
        panic!("expected the function body tail");
    };
    let Expr::Control { arms, .. } = tail.as_ref() else {
        panic!("expected the outer case to remain the function tail");
    };
    let Expr::Lambda {
        body: outer_body, ..
    } = &arms[2].body
    else {
        panic!("final outer arm lost its closure");
    };
    let Expr::Block {
        tail: Some(outer_tail),
        ..
    } = outer_body.as_ref()
    else {
        panic!("outer closure lost its case tail");
    };
    let Expr::Control {
        arms: outer_arms, ..
    } = outer_tail.as_ref()
    else {
        panic!("outer closure tail is not a case expression");
    };
    let Expr::Lambda {
        body: inner_body, ..
    } = &outer_arms[1].body
    else {
        panic!("final outer case arm lost its nested closure");
    };
    let Expr::Block {
        tail: Some(inner_tail),
        ..
    } = inner_body.as_ref()
    else {
        panic!("nested closure lost its case tail");
    };
    let Expr::Control {
        arms: inner_arms, ..
    } = inner_tail.as_ref()
    else {
        panic!("nested closure tail is not a case expression");
    };
    let Expr::Lambda {
        body: final_body, ..
    } = &inner_arms[1].body
    else {
        panic!("deep final case arm lost its closure");
    };
    let Expr::Block {
        tail: Some(final_tail),
        ..
    } = final_body.as_ref()
    else {
        panic!("deep closure lost its case tail");
    };
    let Expr::Control {
        arms: final_arms, ..
    } = final_tail.as_ref()
    else {
        panic!("deep closure tail is not a case expression");
    };
    let Expr::InterpolatedString { segments, .. } = &final_arms[1].body else {
        panic!("deepest final arm lost its interpolated string");
    };
    assert!(matches!(segments.as_slice(), [
        StringSegment::Text { text: first, .. },
        _,
        StringSegment::Text { text: between, .. },
        _,
        StringSegment::Text { text: remainder, .. }
    ] if first == "first " && between == " between " && remainder == " remainder"));

    fn assert_interpolated_closure_case(
        segment: &StringSegment,
        condition_name: &str,
        nested_cases: [(&str, &str, &str, &str, &str); 2],
    ) {
        let StringSegment::Expression { value, .. } = segment else {
            panic!("expected an interpolated closure: {segment:?}");
        };
        let Expr::Lambda { body, .. } = value else {
            panic!("interpolation lost its closure: {value:?}");
        };
        let Expr::Block {
            statements,
            tail: Some(tail),
            ..
        } = body.as_ref()
        else {
            panic!("interpolated closure lost its case tail");
        };
        assert!(statements.is_empty(), "{statements:?}");
        let Expr::Control {
            condition: Some(condition),
            arms,
            ..
        } = tail.as_ref()
        else {
            panic!("interpolated closure tail is not a case");
        };
        assert!(matches!(condition.as_ref(), Expr::Name { text, .. } if text == condition_name));
        assert_eq!(arms.len(), 2, "{arms:?}");

        for (arm, (prefix, nested_condition, first, second, expected_suffix)) in
            arms.iter().zip(nested_cases)
        {
            let Expr::InterpolatedString { segments, .. } = &arm.body else {
                panic!("case arm lost its interpolated suffix string: {arm:?}");
            };
            let [
                StringSegment::Text {
                    text: actual_prefix,
                    ..
                },
                StringSegment::Expression { value: nested, .. },
                StringSegment::Text { text: suffix, .. },
            ] = segments.as_slice()
            else {
                panic!("nested closure string lost a suffix boundary: {segments:?}");
            };
            assert_eq!(actual_prefix, prefix);
            assert_eq!(suffix, expected_suffix);

            let Expr::Lambda {
                body: nested_body, ..
            } = nested
            else {
                panic!("nested interpolation lost its closure: {nested:?}");
            };
            let Expr::Block {
                statements,
                tail: Some(nested_tail),
                ..
            } = nested_body.as_ref()
            else {
                panic!("nested interpolated closure lost its case tail");
            };
            assert!(statements.is_empty(), "{statements:?}");
            let Expr::Control {
                condition: Some(nested_condition_expr),
                arms: nested_arms,
                ..
            } = nested_tail.as_ref()
            else {
                panic!("nested closure tail is not a case");
            };
            assert!(
                matches!(nested_condition_expr.as_ref(), Expr::Name { text, .. } if text == nested_condition)
            );
            assert_eq!(nested_arms.len(), 2, "{nested_arms:?}");
            assert!(matches!(
                (&nested_arms[0].body, &nested_arms[1].body),
                (Expr::Name { text: actual_first, .. }, Expr::Name { text: actual_second, .. })
                    if actual_first == first && actual_second == second
            ));
        }
    }

    assert_interpolated_closure_case(
        &segments[1],
        "probe",
        [
            ("inner ", "left", "flag", "outer", " first end"),
            ("inner ", "right", "outer", "flag", " first final"),
        ],
    );
    assert_interpolated_closure_case(
        &segments[3],
        "other",
        [
            ("inner ", "left", "flag", "outer", " second end"),
            ("inner ", "right", "outer", "flag", " second final"),
        ],
    );

    // Nested strings and lambda/case expressions are part of the ordinary
    // grammar. Malformed-arm recovery is unspecified; retain each nested
    // closure, its arm suffixes, and the enclosing separator/remainder text.
}

#[test]
fn recovered_closure_final_tail_keeps_nested_interpolation_closures() {
    let source = include_str!("fixtures/malformed-case-arm-closure-final-interpolated-tail.orna");
    let parsed = parse_module(source);

    assert_eq!(parsed.diagnostics.len(), 1, "{:?}", parsed.diagnostics);
    let diagnostic = &parsed.diagnostics[0];
    assert_eq!(diagnostic.code, "ORNA-PARSE-001");
    assert_eq!(diagnostic.message, "expected `:` after case pattern");
    let malformed_separator = source.find("{ value").expect("fixture has malformed arm");
    assert_eq!(diagnostic.span.start, malformed_separator);
    assert_eq!(diagnostic.span.end, malformed_separator + 1);

    let Declaration::Function { body, .. } = &parsed.value.items[0].declaration else {
        panic!("expected a function declaration");
    };
    let Expr::Block { tail: Some(tail), .. } = body else {
        panic!("expected the function body tail");
    };
    let Expr::Control { arms, .. } = tail.as_ref() else {
        panic!("expected the outer case to remain the function tail");
    };
    let Expr::Lambda { body: outer_body, .. } = &arms[2].body else {
        panic!("final outer arm lost its closure");
    };
    let Expr::Block { tail: Some(outer_tail), .. } = outer_body.as_ref() else {
        panic!("outer closure lost its case tail");
    };
    let Expr::Control { arms: outer_arms, .. } = outer_tail.as_ref() else {
        panic!("outer closure tail is not a case expression");
    };
    let Expr::Lambda { body: inner_body, .. } = &outer_arms[1].body else {
        panic!("final outer case arm lost its nested closure");
    };
    let Expr::Block { tail: Some(inner_tail), .. } = inner_body.as_ref() else {
        panic!("nested closure lost its case tail");
    };
    let Expr::Control { arms: inner_arms, .. } = inner_tail.as_ref() else {
        panic!("nested closure tail is not a case expression");
    };
    let Expr::InterpolatedString { segments, .. } = &inner_arms[1].body else {
        panic!("final nested case arm lost its interpolated string");
    };
    let [
        StringSegment::Text { text: prefix, .. },
        StringSegment::Expression { value: probe, .. },
    ] = segments.as_slice()
    else {
        panic!("final case string lost its closure at the tail: {segments:?}");
    };
    assert_eq!(prefix, "closing ");

    let Expr::Lambda { body: probe_body, .. } = probe else {
        panic!("final string interpolation lost its closure: {probe:?}");
    };
    let Expr::Block { tail: Some(probe_tail), .. } = probe_body.as_ref() else {
        panic!("interpolated closure lost its final case");
    };
    let Expr::Control { arms: probe_arms, .. } = probe_tail.as_ref() else {
        panic!("interpolated closure tail is not a case");
    };
    assert_eq!(probe_arms.len(), 2, "{probe_arms:?}");
    assert!(matches!(
        &probe_arms[0].body,
        Expr::InterpolatedString { segments, .. }
            if matches!(segments.as_slice(), [
                StringSegment::Text { text, .. },
                StringSegment::Expression { value: Expr::Name { text: name, .. }, .. }
            ] if text == "case " && name == "flag")
    ));

    let Expr::InterpolatedString { segments: final_segments, .. } = &probe_arms[1].body else {
        panic!("final interpolation case arm lost its string");
    };
    let [
        StringSegment::Text { text: final_prefix, .. },
        StringSegment::Expression { value: nested, .. },
    ] = final_segments.as_slice()
    else {
        panic!("final interpolation string lost its nested closure tail: {final_segments:?}");
    };
    assert_eq!(final_prefix, "final ");

    let Expr::Lambda { body: nested_body, .. } = nested else {
        panic!("nested final interpolation lost its closure: {nested:?}");
    };
    let Expr::Block { tail: Some(nested_tail), .. } = nested_body.as_ref() else {
        panic!("nested interpolated closure lost its case tail");
    };
    let Expr::Control { arms: nested_arms, .. } = nested_tail.as_ref() else {
        panic!("nested interpolated closure tail is not a case");
    };
    assert_eq!(nested_arms.len(), 2, "{nested_arms:?}");
    for (arm, name) in nested_arms.iter().zip(["outer", "flag"]) {
        assert!(matches!(
            &arm.body,
            Expr::InterpolatedString { segments, .. }
                if matches!(segments.as_slice(), [
                    StringSegment::Text { text, .. },
                    StringSegment::Expression { value: Expr::Name { text: actual, .. }, .. }
                ] if text == "nested " && actual == name)
        ));
    }

    // The reference specifies case/lambda/string expressions but not recovery
    // after this malformed arm. Keep the final closure and its nested strings
    // intact through the consecutive closing delimiters.
}

#[test]
fn recovered_nested_final_case_closures_keep_closure_tails() {
    let source = include_str!("fixtures/malformed-case-arm-nested-closure-final-case-tail.orna");
    let parsed = parse_module(source);

    assert_eq!(parsed.diagnostics.len(), 1, "{:?}", parsed.diagnostics);
    let diagnostic = &parsed.diagnostics[0];
    assert_eq!(diagnostic.code, "ORNA-PARSE-001");
    assert_eq!(diagnostic.message, "expected `:` after case pattern");
    let malformed_separator = source.find("{ value").expect("fixture has malformed arm");
    assert_eq!(diagnostic.span.start, malformed_separator);
    assert_eq!(diagnostic.span.end, malformed_separator + 1);

    let Declaration::Function { body, .. } = &parsed.value.items[0].declaration else {
        panic!("expected a function declaration");
    };
    let Expr::Block { tail: Some(tail), .. } = body else {
        panic!("expected the function body tail");
    };
    let Expr::Control { arms, .. } = tail.as_ref() else {
        panic!("expected the outer case to remain the function tail");
    };
    let Expr::Lambda { body: outer_body, .. } = &arms[2].body else {
        panic!("final outer arm lost its closure");
    };
    let Expr::Block { tail: Some(outer_tail), .. } = outer_body.as_ref() else {
        panic!("outer closure lost its case tail");
    };
    let Expr::Control { arms: outer_arms, .. } = outer_tail.as_ref() else {
        panic!("outer closure tail is not a case expression");
    };
    let Expr::Lambda { body: inner_body, .. } = &outer_arms[1].body else {
        panic!("final outer case arm lost its nested closure");
    };
    let Expr::Block { tail: Some(inner_tail), .. } = inner_body.as_ref() else {
        panic!("nested closure lost its case tail");
    };
    let Expr::Control { arms: inner_arms, .. } = inner_tail.as_ref() else {
        panic!("nested closure tail is not a case expression");
    };
    let Expr::InterpolatedString { segments, .. } = &inner_arms[1].body else {
        panic!("final nested case arm lost its interpolated string");
    };
    let [
        StringSegment::Text { text: prefix, .. },
        StringSegment::Expression { value: probe, .. },
    ] = segments.as_slice()
    else {
        panic!("final case string lost its closure at the tail: {segments:?}");
    };
    assert_eq!(prefix, "closing ");

    let Expr::Lambda { body: probe_body, .. } = probe else {
        panic!("final string interpolation lost its closure: {probe:?}");
    };
    let Expr::Block { tail: Some(probe_tail), .. } = probe_body.as_ref() else {
        panic!("interpolated closure lost its final case");
    };
    let Expr::Control {
        condition: Some(probe_condition),
        arms: probe_arms,
        ..
    } = probe_tail.as_ref()
    else {
        panic!("interpolated closure tail is not a case");
    };
    assert!(matches!(probe_condition.as_ref(), Expr::Name { text, .. } if text == "probe"));
    assert_eq!(probe_arms.len(), 2, "{probe_arms:?}");
    assert!(matches!(
        &probe_arms[0].body,
        Expr::InterpolatedString { segments, .. }
            if matches!(segments.as_slice(), [
                StringSegment::Text { text, .. },
                StringSegment::Expression { value: Expr::Name { text: name, .. }, .. }
            ] if text == "probe " && name == "flag")
    ));

    let Expr::Lambda { body: nested_body, .. } = &probe_arms[1].body else {
        panic!("final interpolated case arm lost its nested closure");
    };
    let Expr::Block { tail: Some(nested_tail), .. } = nested_body.as_ref() else {
        panic!("nested closure lost its final case");
    };
    let Expr::Control {
        condition: Some(nested_condition),
        arms: nested_arms,
        ..
    } = nested_tail.as_ref()
    else {
        panic!("nested closure tail is not a case");
    };
    assert!(matches!(nested_condition.as_ref(), Expr::Name { text, .. } if text == "nested"));
    assert_eq!(nested_arms.len(), 2, "{nested_arms:?}");
    assert!(matches!(
        &nested_arms[0].body,
        Expr::InterpolatedString { segments, .. }
            if matches!(segments.as_slice(), [
                StringSegment::Text { text, .. },
                StringSegment::Expression { value: Expr::Name { text: name, .. }, .. }
            ] if text == "nested " && name == "outer")
    ));

    let Expr::Lambda { body: final_body, .. } = &nested_arms[1].body else {
        panic!("deep final case arm lost its closure tail");
    };
    let Expr::Block { tail: Some(final_tail), .. } = final_body.as_ref() else {
        panic!("deep closure lost its final case");
    };
    let Expr::Control {
        condition: Some(final_condition),
        arms: final_arms,
        ..
    } = final_tail.as_ref()
    else {
        panic!("deep closure tail is not a case");
    };
    assert!(matches!(final_condition.as_ref(), Expr::Name { text, .. } if text == "final"));
    assert_eq!(final_arms.len(), 2, "{final_arms:?}");
    for (arm, (prefix, name, suffix)) in final_arms.iter().zip([
        ("leaf ", "flag", " tail"),
        ("last ", "outer", " end"),
    ]) {
        assert!(matches!(
            &arm.body,
            Expr::InterpolatedString { segments, .. }
                if matches!(segments.as_slice(), [
                    StringSegment::Text { text: actual_prefix, .. },
                    StringSegment::Expression { value: Expr::Name { text: actual_name, .. }, .. },
                    StringSegment::Text { text: actual_suffix, .. }
                ] if actual_prefix == prefix && actual_name == name && actual_suffix == suffix)
        ));
    }

    // The reference grammar allows case and lambda expressions inside string
    // interpolation; malformed-arm recovery is unspecified. Preserve the
    // final nested closure tails and the final case-arm string suffixes.
}

#[test]
fn recovered_final_closure_tail_chain_keeps_each_case_arm() {
    let source = include_str!("fixtures/malformed-case-arm-final-closure-tail-chain.orna");
    let parsed = parse_module(source);

    assert_eq!(parsed.diagnostics.len(), 1, "{:?}", parsed.diagnostics);
    let diagnostic = &parsed.diagnostics[0];
    assert_eq!(diagnostic.code, "ORNA-PARSE-001");
    assert_eq!(diagnostic.message, "expected `:` after case pattern");
    let malformed_separator = source.find("{ value").expect("fixture has malformed arm");
    assert_eq!(diagnostic.span.start, malformed_separator);
    assert_eq!(diagnostic.span.end, malformed_separator + 1);

    fn final_arm_body<'a>(
        expression: &'a Expr,
        condition_name: &str,
        first_arm_name: &str,
    ) -> &'a Expr {
        let Expr::Lambda { body, .. } = expression else {
            panic!("expected a closure tail, got {expression:?}");
        };
        let Expr::Block { tail: Some(tail), .. } = body.as_ref() else {
            panic!("closure lost its case tail");
        };
        let Expr::Control {
            condition: Some(condition),
            arms,
            ..
        } = tail.as_ref()
        else {
            panic!("closure tail is not a case expression");
        };
        assert!(matches!(condition.as_ref(), Expr::Name { text, .. } if text == condition_name));
        assert_eq!(arms.len(), 2, "{arms:?}");
        assert!(matches!(
            &arms[0].body,
            Expr::Name { text, .. } if text == first_arm_name
        ));
        &arms[1].body
    }

    let Declaration::Function { body, .. } = &parsed.value.items[0].declaration else {
        panic!("expected a function declaration");
    };
    let Expr::Block { tail: Some(tail), .. } = body else {
        panic!("expected the function body tail");
    };
    let Expr::Control { arms, .. } = tail.as_ref() else {
        panic!("expected the outer case to remain the function tail");
    };
    let probe_arm = final_arm_body(&arms[2].body, "outer", "flag");
    let inner_tail = final_arm_body(probe_arm, "inner", "outer");
    let Expr::InterpolatedString { segments, .. } = inner_tail else {
        panic!("nested final closure lost its interpolated tail");
    };
    let [
        StringSegment::Text { text: prefix, .. },
        StringSegment::Expression { value: probe, .. },
    ] = segments.as_slice()
    else {
        panic!("final nested string lost its closure at the tail: {segments:?}");
    };
    assert_eq!(prefix, "tail ");

    let nested_tail = final_arm_body(probe, "probe", "flag");
    let deeper_tail = final_arm_body(nested_tail, "nested", "outer");
    let last_tail = final_arm_body(deeper_tail, "deeper", "flag");
    let deepest_tail = final_arm_body(last_tail, "last", "outer");
    let Expr::InterpolatedString {
        segments: final_segments,
        ..
    } = deepest_tail
    else {
        panic!("deepest final closure lost its string tail");
    };
    assert!(matches!(
        final_segments.as_slice(),
        [
            StringSegment::Text { text: final_prefix, .. },
            StringSegment::Expression { value: Expr::Name { text: name, .. }, .. },
            StringSegment::Text { text: suffix, .. }
        ] if final_prefix == "final " && name == "flag" && suffix == " done"
    ));

    // The reference permits closures and case expressions in interpolation,
    // but does not specify malformed-arm recovery. Retain each final closure
    // tail, the final interpolation boundary, and the deepest string suffix.
}

#[test]
fn recovered_closure_tail_chain_keeps_preceding_block_statements() {
    let source = include_str!("fixtures/malformed-case-arm-closure-tail-chain-block-statements.orna");
    let parsed = parse_module(source);

    assert_eq!(parsed.diagnostics.len(), 1, "{:?}", parsed.diagnostics);
    let diagnostic = &parsed.diagnostics[0];
    assert_eq!(diagnostic.code, "ORNA-PARSE-001");
    assert_eq!(diagnostic.message, "expected `:` after case pattern");
    let malformed_separator = source.find("{ value").expect("fixture has malformed arm");
    assert_eq!(diagnostic.span.start, malformed_separator);
    assert_eq!(diagnostic.span.end, malformed_separator + 1);

    fn final_arm_body<'a>(
        expression: &'a Expr,
        condition_name: &str,
        first_arm_name: &str,
    ) -> &'a Expr {
        let Expr::Lambda { body, .. } = expression else {
            panic!("expected a closure tail, got {expression:?}");
        };
        let Expr::Block {
            statements,
            tail: Some(tail),
            ..
        } = body.as_ref()
        else {
            panic!("closure lost its block tail");
        };
        assert_eq!(statements.len(), 1, "expected the leading let statement");
        let Expr::Control {
            condition: Some(condition),
            arms,
            ..
        } = tail.as_ref()
        else {
            panic!("closure block tail is not a case expression");
        };
        assert!(matches!(condition.as_ref(), Expr::Name { text, .. } if text == condition_name));
        assert_eq!(arms.len(), 2, "{arms:?}");
        assert!(matches!(
            &arms[0].body,
            Expr::Name { text, .. } if text == first_arm_name
        ));
        &arms[1].body
    }

    let Declaration::Function { body, .. } = &parsed.value.items[0].declaration else {
        panic!("expected a function declaration");
    };
    let Expr::Block { tail: Some(tail), .. } = body else {
        panic!("expected the function body tail");
    };
    let Expr::Control { arms, .. } = tail.as_ref() else {
        panic!("expected the outer case to remain the function tail");
    };
    let inner_tail = final_arm_body(&arms[2].body, "outer", "outer_value");
    let string_tail = final_arm_body(inner_tail, "inner", "inner_value");
    let Expr::InterpolatedString { segments, .. } = string_tail else {
        panic!("nested final closure lost its interpolated tail");
    };
    let [
        StringSegment::Text { text: prefix, .. },
        StringSegment::Expression { value: probe, .. },
        StringSegment::Text { text: suffix, .. },
    ] = segments.as_slice()
    else {
        panic!("final nested string lost its suffix boundaries: {segments:?}");
    };
    assert_eq!(prefix, "tail ");
    assert_eq!(suffix, " after");

    let nested_tail = final_arm_body(probe, "probe", "probe_value");
    let deepest_tail = final_arm_body(nested_tail, "nested", "nested_value");
    assert!(matches!(
        deepest_tail,
        Expr::InterpolatedString { segments, .. }
            if matches!(segments.as_slice(), [
                StringSegment::Text { text: prefix, .. },
                StringSegment::Expression { value: Expr::Name { text: name, .. }, .. },
                StringSegment::Text { text: suffix, .. }
            ] if prefix == "end " && name == "flag" && suffix == " done")
    ));

    // The grammar permits let statements before a block's final case tail and
    // nested closures inside interpolation. Recovery after the malformed arm
    // is unspecified; retain each tail and both enclosing string suffixes.
}

#[test]
fn recovered_nested_closure_tail_chain_keeps_following_arms() {
    let source = include_str!("fixtures/malformed-case-arm-nested-closure-tail-chain-recovery.orna");
    let parsed = parse_module(source);

    let malformed_separators = source
        .match_indices("{ value")
        .map(|(offset, _)| offset)
        .collect::<Vec<_>>();
    assert_eq!(malformed_separators.len(), 2);
    assert_eq!(parsed.diagnostics.len(), 2, "{:?}", parsed.diagnostics);
    for (diagnostic, start) in parsed.diagnostics.iter().zip(malformed_separators) {
        assert_eq!(diagnostic.code, "ORNA-PARSE-001");
        assert_eq!(diagnostic.message, "expected `:` after case pattern");
        assert_eq!(diagnostic.span.start, start);
        assert_eq!(diagnostic.span.end, start + 1);
    }

    fn case_arms<'a>(expression: &'a Expr, condition_name: &str) -> (&'a Expr, &'a Expr) {
        let Expr::Lambda { body, .. } = expression else {
            panic!("expected a closure, got {expression:?}");
        };
        let Expr::Block { tail: Some(tail), .. } = body.as_ref() else {
            panic!("closure lost its case tail");
        };
        let Expr::Control {
            condition: Some(condition),
            arms,
            ..
        } = tail.as_ref()
        else {
            panic!("closure tail is not a case expression");
        };
        assert!(matches!(condition.as_ref(), Expr::Name { text, .. } if text == condition_name));
        assert_eq!(arms.len(), 2, "{arms:?}");
        (&arms[0].body, &arms[1].body)
    }

    let Declaration::Function { body, .. } = &parsed.value.items[0].declaration else {
        panic!("expected a function declaration");
    };
    let Expr::Block { tail: Some(tail), .. } = body else {
        panic!("expected the function's outer case tail");
    };
    let Expr::Control { arms, .. } = tail.as_ref() else {
        panic!("expected the outer case expression");
    };
    assert_eq!(arms.len(), 3, "outer case lost a following arm: {arms:?}");
    let Expr::InterpolatedString { segments, .. } = &arms[1].body else {
        panic!("outer recovered arm lost its interpolated string");
    };
    let [
        StringSegment::Text { text: prefix, .. },
        StringSegment::Expression { value: closure, .. },
        StringSegment::Text { text: suffix, .. },
    ] = segments.as_slice()
    else {
        panic!("outer string lost the closure interpolation boundary: {segments:?}");
    };
    assert_eq!(prefix, "outer ");
    assert_eq!(suffix, " after");

    let Expr::Lambda { body: closure_body, .. } = closure else {
        panic!("outer interpolation lost its closure");
    };
    let Expr::Block { tail: Some(closure_tail), .. } = closure_body.as_ref() else {
        panic!("outer closure lost its case tail");
    };
    let Expr::Control {
        arms: closure_arms,
        ..
    } = closure_tail.as_ref()
    else {
        panic!("outer closure tail is not a case expression");
    };
    assert_eq!(closure_arms.len(), 3, "nested recovery lost arms: {closure_arms:?}");
    assert!(matches!(
        &closure_arms[1].body,
        Expr::Name { text, .. } if text == "outer"
    ));
    let nested_closure = &closure_arms[2].body;
    let (leaf, final_leaf) = case_arms(nested_closure, "nested");
    assert!(matches!(
        leaf,
        Expr::InterpolatedString { segments, .. }
            if matches!(segments.as_slice(), [
                StringSegment::Text { text: prefix, .. },
                StringSegment::Expression { value: Expr::Name { text: name, .. }, .. },
                StringSegment::Text { text: suffix, .. }
            ] if prefix == "leaf " && name == "flag" && suffix == " suffix")
    ));
    assert!(matches!(
        final_leaf,
        Expr::InterpolatedString { segments, .. }
            if matches!(segments.as_slice(), [
                StringSegment::Text { text: prefix, .. },
                StringSegment::Expression { value: Expr::Name { text: name, .. }, .. },
                StringSegment::Text { text: suffix, .. }
            ] if prefix == "last " && name == "outer" && suffix == " end")
    ));
    assert!(matches!(
        &arms[2].body,
        Expr::InterpolatedString { segments, .. }
            if matches!(segments.as_slice(), [
                StringSegment::Text { text: prefix, .. },
                StringSegment::Expression { value: Expr::Name { text: name, .. }, .. },
                StringSegment::Text { text: suffix, .. }
            ] if prefix == "fallback " && name == "flag" && suffix == " done")
    ));

    // The reference permits closures and cases inside interpolation but leaves
    // malformed-arm recovery unspecified. Preserve both recovery points, the
    // closure tail chain, suffix boundaries, and following sibling arms.
}

#[test]
fn recovered_deep_closure_tail_chain_keeps_terminal_suffix_arms() {
    let source = include_str!("fixtures/malformed-case-arm-deep-closure-tail-chain-recovery.orna");
    let parsed = parse_module(source);

    let malformed_separators = source
        .match_indices("{ value")
        .map(|(offset, _)| offset)
        .collect::<Vec<_>>();
    assert_eq!(malformed_separators.len(), 3);
    assert_eq!(parsed.diagnostics.len(), 3, "{:?}", parsed.diagnostics);
    for (diagnostic, start) in parsed.diagnostics.iter().zip(malformed_separators) {
        assert_eq!(diagnostic.code, "ORNA-PARSE-001");
        assert_eq!(diagnostic.message, "expected `:` after case pattern");
        assert_eq!(diagnostic.span.start, start);
        assert_eq!(diagnostic.span.end, start + 1);
    }

    fn case_bodies<'a>(
        expression: &'a Expr,
        condition_name: &str,
    ) -> (&'a Expr, &'a Expr, &'a Expr) {
        let Expr::Lambda { body, .. } = expression else {
            panic!("expected a closure, got {expression:?}");
        };
        let Expr::Block { tail: Some(tail), .. } = body.as_ref() else {
            panic!("closure lost its case tail");
        };
        let Expr::Control {
            condition: Some(condition),
            arms,
            ..
        } = tail.as_ref()
        else {
            panic!("closure tail is not a case expression");
        };
        assert!(matches!(condition.as_ref(), Expr::Name { text, .. } if text == condition_name));
        assert_eq!(arms.len(), 3, "recovery lost a case arm: {arms:?}");
        (&arms[0].body, &arms[1].body, &arms[2].body)
    }

    fn interpolation_closure<'a>(expression: &'a Expr, prefix: &str, suffix: &str) -> &'a Expr {
        let Expr::InterpolatedString { segments, .. } = expression else {
            panic!("expected a closure interpolation, got {expression:?}");
        };
        let [
            StringSegment::Text { text: actual_prefix, .. },
            StringSegment::Expression { value, .. },
            StringSegment::Text { text: actual_suffix, .. },
        ] = segments.as_slice()
        else {
            panic!("interpolation lost a surrounding text segment: {segments:?}");
        };
        assert_eq!(actual_prefix, prefix);
        assert_eq!(actual_suffix, suffix);
        assert!(matches!(value, Expr::Lambda { .. }), "{value:?}");
        value
    }

    let Declaration::Function { body, .. } = &parsed.value.items[0].declaration else {
        panic!("expected a function declaration");
    };
    let Expr::Block { tail: Some(tail), .. } = body else {
        panic!("expected the outer case tail");
    };
    let Expr::Control { arms, .. } = tail.as_ref() else {
        panic!("expected the outer case expression");
    };
    assert_eq!(arms.len(), 3, "outer recovery lost a following arm: {arms:?}");
    let outer_closure = interpolation_closure(&arms[1].body, "outer ", " after");
    let (_, middle_arm, terminal_string) = case_bodies(outer_closure, "outer");
    assert!(matches!(middle_arm, Expr::Name { text, .. } if text == "outer"));

    let nested_closure = interpolation_closure(terminal_string, "inner ", " terminal");
    let (_, leaf, final_leaf) = case_bodies(nested_closure, "nested");
    assert!(matches!(
        leaf,
        Expr::InterpolatedString { segments, .. }
            if matches!(segments.as_slice(), [
                StringSegment::Text { text: prefix, .. },
                StringSegment::Expression { value: Expr::Name { text: name, .. }, .. },
                StringSegment::Text { text: suffix, .. }
            ] if prefix == "leaf " && name == "flag" && suffix == " suffix")
    ));
    assert!(matches!(
        final_leaf,
        Expr::InterpolatedString { segments, .. }
            if matches!(segments.as_slice(), [
                StringSegment::Text { text: prefix, .. },
                StringSegment::Expression { value: Expr::Name { text: name, .. }, .. },
                StringSegment::Text { text: suffix, .. }
            ] if prefix == "last " && name == "outer" && suffix == " end")
    ));
    assert!(matches!(
        &arms[2].body,
        Expr::InterpolatedString { segments, .. }
            if matches!(segments.as_slice(), [
                StringSegment::Text { text: prefix, .. },
                StringSegment::Expression { value: Expr::Name { text: name, .. }, .. },
                StringSegment::Text { text: suffix, .. }
            ] if prefix == "fallback " && name == "flag" && suffix == " done")
    ));

    // Malformed-arm recovery is unspecified by the reference. Keep the three
    // nested recovery points, terminal case arms, and suffixes after closures.
}

#[test]
fn recovered_deep_terminal_arm_keeps_enclosing_closure_suffixes() {
    let source = include_str!("fixtures/malformed-case-arm-terminal-closure-tail-missing-separator.orna");
    let parsed = parse_module(source);

    assert_eq!(parsed.diagnostics.len(), 1, "{:?}", parsed.diagnostics);
    let diagnostic = &parsed.diagnostics[0];
    assert_eq!(diagnostic.code, "ORNA-PARSE-001");
    assert_eq!(diagnostic.message, "expected `:` after case pattern");
    let malformed_separator = source.find("false 0").expect("fixture has missing separator") + 6;
    assert_eq!(diagnostic.span.start, malformed_separator);
    assert_eq!(diagnostic.span.end, malformed_separator + 1);

    fn closure_case<'a>(expression: &'a Expr, condition_name: &str) -> &'a [CaseArm] {
        let Expr::Lambda { body, .. } = expression else {
            panic!("expected a closure, got {expression:?}");
        };
        let Expr::Block { tail: Some(tail), .. } = body.as_ref() else {
            panic!("closure lost its case tail");
        };
        let Expr::Control {
            condition: Some(condition),
            arms,
            ..
        } = tail.as_ref()
        else {
            panic!("closure tail is not a case expression");
        };
        assert!(matches!(condition.as_ref(), Expr::Name { text, .. } if text == condition_name));
        arms
    }

    fn interpolation_closure<'a>(expression: &'a Expr, prefix: &str, suffix: &str) -> &'a Expr {
        let Expr::InterpolatedString { segments, .. } = expression else {
            panic!("expected a closure interpolation, got {expression:?}");
        };
        let [
            StringSegment::Text { text: actual_prefix, .. },
            StringSegment::Expression { value, .. },
            StringSegment::Text { text: actual_suffix, .. },
        ] = segments.as_slice()
        else {
            panic!("interpolation lost its surrounding suffix segment: {segments:?}");
        };
        assert_eq!(actual_prefix, prefix);
        assert_eq!(actual_suffix, suffix);
        assert!(matches!(value, Expr::Lambda { .. }), "{value:?}");
        value
    }

    let Declaration::Function { body, .. } = &parsed.value.items[0].declaration else {
        panic!("expected a function declaration");
    };
    let Expr::Block { tail: Some(tail), .. } = body else {
        panic!("expected the root case tail");
    };
    let Expr::Control { arms, .. } = tail.as_ref() else {
        panic!("expected the root case expression");
    };
    assert_eq!(arms.len(), 2, "root recovery lost its following arm: {arms:?}");
    let outer_closure = interpolation_closure(&arms[0].body, "outer ", " outer-after");
    let outer_arms = closure_case(outer_closure, "outer");
    assert_eq!(outer_arms.len(), 2, "outer case lost its arms: {outer_arms:?}");
    assert!(matches!(
        &outer_arms[0].body,
        Expr::Name { text, .. } if text == "outer"
    ));

    let nested_closure = interpolation_closure(&outer_arms[1].body, "inner ", " inner-after");
    let nested_arms = closure_case(nested_closure, "nested");
    assert!(!nested_arms.is_empty(), "deep recovery discarded all arms");
    assert!(matches!(
        &nested_arms[0].body,
        Expr::InterpolatedString { segments, .. }
            if matches!(segments.as_slice(), [
                StringSegment::Text { text: prefix, .. },
                StringSegment::Expression { value: Expr::Name { text: name, .. }, .. },
                StringSegment::Text { text: suffix, .. }
            ] if prefix == "leaf " && name == "flag" && suffix == " suffix")
    ));
    assert!(matches!(
        &arms[1].body,
        Expr::InterpolatedString { segments, .. }
            if matches!(segments.as_slice(), [
                StringSegment::Text { text: prefix, .. },
                StringSegment::Expression { value: Expr::Name { text: name, .. }, .. },
                StringSegment::Text { text: suffix, .. }
            ] if prefix == "fallback " && name == "flag" && suffix == " done")
    ));

    // The reference leaves missing-separator recovery unspecified. Retain the
    // deepest valid tail and the suffix boundaries of both enclosing strings.
}

#[test]
fn recovered_deep_middle_arm_keeps_following_closure_tail() {
    let source = include_str!("fixtures/malformed-case-arm-deep-recovery-before-closure-tail.orna");
    let parsed = parse_module(source);

    assert_eq!(parsed.diagnostics.len(), 1, "{:?}", parsed.diagnostics);
    let diagnostic = &parsed.diagnostics[0];
    assert_eq!(diagnostic.code, "ORNA-PARSE-001");
    assert_eq!(diagnostic.message, "expected `:` after case pattern");
    let malformed_separator = source.find("false 0").expect("fixture has malformed arm") + 6;
    assert_eq!(diagnostic.span.start, malformed_separator);
    assert_eq!(diagnostic.span.end, malformed_separator + 1);

    fn closure_case<'a>(expression: &'a Expr, condition_name: &str) -> &'a [CaseArm] {
        let Expr::Lambda { body, .. } = expression else {
            panic!("expected a closure, got {expression:?}");
        };
        let Expr::Block { tail: Some(tail), .. } = body.as_ref() else {
            panic!("closure lost its case tail");
        };
        let Expr::Control {
            condition: Some(condition),
            arms,
            ..
        } = tail.as_ref()
        else {
            panic!("closure tail is not a case expression");
        };
        assert!(matches!(condition.as_ref(), Expr::Name { text, .. } if text == condition_name));
        arms
    }

    fn interpolation_closure<'a>(expression: &'a Expr, prefix: &str, suffix: &str) -> &'a Expr {
        let Expr::InterpolatedString { segments, .. } = expression else {
            panic!("expected a closure interpolation, got {expression:?}");
        };
        let [
            StringSegment::Text { text: actual_prefix, .. },
            StringSegment::Expression { value, .. },
            StringSegment::Text { text: actual_suffix, .. },
        ] = segments.as_slice()
        else {
            panic!("interpolation lost surrounding text: {segments:?}");
        };
        assert_eq!(actual_prefix, prefix);
        assert_eq!(actual_suffix, suffix);
        assert!(matches!(value, Expr::Lambda { .. }), "{value:?}");
        value
    }

    let Declaration::Function { body, .. } = &parsed.value.items[0].declaration else {
        panic!("expected a function declaration");
    };
    let Expr::Block { tail: Some(tail), .. } = body else {
        panic!("expected the root case tail");
    };
    let Expr::Control { arms, .. } = tail.as_ref() else {
        panic!("expected the root case expression");
    };
    assert_eq!(arms.len(), 2, "root case lost its sibling: {arms:?}");

    let outer_closure = interpolation_closure(&arms[0].body, "outer ", " outer-after");
    let outer_arms = closure_case(outer_closure, "outer");
    assert_eq!(outer_arms.len(), 2);
    let nested_closure = interpolation_closure(&outer_arms[1].body, "inner ", " inner-after");
    let nested_arms = closure_case(nested_closure, "nested");
    assert!(nested_arms.len() >= 2, "deep recovery lost later arms: {nested_arms:?}");
    assert!(matches!(
        &nested_arms[0].body,
        Expr::InterpolatedString { segments, .. }
            if matches!(segments.as_slice(), [
                StringSegment::Text { text: prefix, .. },
                StringSegment::Expression { value: Expr::Name { text: name, .. }, .. },
                StringSegment::Text { text: suffix, .. }
            ] if prefix == "leaf " && name == "flag" && suffix == " suffix")
    ));
    let Expr::Lambda { .. } = &nested_arms[nested_arms.len() - 1].body else {
        panic!("deep recovery discarded the following closure tail: {nested_arms:?}");
    };
    let final_closure = &nested_arms[nested_arms.len() - 1].body;
    let final_arms = closure_case(final_closure, "final");
    assert_eq!(final_arms.len(), 2);
    assert!(matches!(
        &final_arms[0].body,
        Expr::InterpolatedString { segments, .. }
            if matches!(segments.as_slice(), [
                StringSegment::Text { text: prefix, .. },
                StringSegment::Expression { value: Expr::Name { text: name, .. }, .. },
                StringSegment::Text { text: suffix, .. }
            ] if prefix == "terminal " && name == "flag" && suffix == " end")
    ));
    assert!(matches!(
        &arms[1].body,
        Expr::InterpolatedString { segments, .. }
            if matches!(segments.as_slice(), [
                StringSegment::Text { text: prefix, .. },
                StringSegment::Expression { value: Expr::Name { text: name, .. }, .. },
                StringSegment::Text { text: suffix, .. }
            ] if prefix == "fallback " && name == "flag" && suffix == " done")
    ));

    // Missing-separator recovery is unspecified by the reference. Preserve the
    // closure arm after the error and text suffixes as parsing unwinds outward.
}

#[test]
fn recovered_deep_block_closure_keeps_final_case_tail() {
    let source = include_str!("fixtures/malformed-case-arm-deep-block-closure-tail-recovery.orna");
    let parsed = parse_module(source);

    assert_eq!(parsed.diagnostics.len(), 1, "{:?}", parsed.diagnostics);
    let diagnostic = &parsed.diagnostics[0];
    assert_eq!(diagnostic.code, "ORNA-PARSE-001");
    assert_eq!(diagnostic.message, "expected `:` after case pattern");
    let malformed_separator = source.find("false 0").expect("fixture has malformed arm") + 6;
    assert_eq!(diagnostic.span.start, malformed_separator);
    assert_eq!(diagnostic.span.end, malformed_separator + 1);

    fn closure_case<'a>(
        expression: &'a Expr,
        condition_name: &str,
    ) -> (&'a [Statement], &'a [CaseArm]) {
        let Expr::Lambda { body, .. } = expression else {
            panic!("expected a closure, got {expression:?}");
        };
        let Expr::Block {
            statements,
            tail: Some(tail),
            ..
        } = body.as_ref()
        else {
            panic!("closure lost its case tail");
        };
        let Expr::Control {
            condition: Some(condition),
            arms,
            ..
        } = tail.as_ref()
        else {
            panic!("closure tail is not a case expression");
        };
        assert!(matches!(condition.as_ref(), Expr::Name { text, .. } if text == condition_name));
        (statements, arms)
    }

    fn assert_leading_let(statements: &[Statement], binding: &str, value: &str) {
        assert!(matches!(
            statements,
            [Statement::Let {
                pattern: Pattern::Name(name, _),
                value: Expr::Name { text, .. },
                ..
            }] if name == binding && text == value
        ));
    }

    let Declaration::Function { body, .. } = &parsed.value.items[0].declaration else {
        panic!("expected a function declaration");
    };
    let Expr::Block { tail: Some(tail), .. } = body else {
        panic!("expected the root case tail");
    };
    let Expr::Control { arms, .. } = tail.as_ref() else {
        panic!("expected the root case expression");
    };
    assert_eq!(arms.len(), 2, "root case lost its fallback arm: {arms:?}");

    let (outer_statements, outer_arms) = closure_case(&arms[0].body, "outer");
    assert_leading_let(outer_statements, "outer_value", "outer");
    assert_eq!(outer_arms.len(), 2);
    let Expr::InterpolatedString { segments, .. } = &outer_arms[1].body else {
        panic!("outer case lost its wrapped closure string");
    };
    let [
        StringSegment::Text { text: prefix, .. },
        StringSegment::Expression { value: inner_closure, .. },
        StringSegment::Text { text: suffix, .. },
    ] = segments.as_slice()
    else {
        panic!("outer string lost its closure suffix boundary: {segments:?}");
    };
    assert_eq!(prefix, "wrap ");
    assert_eq!(suffix, " end");

    let (inner_statements, inner_arms) = closure_case(inner_closure, "inner");
    assert_leading_let(inner_statements, "inner_value", "inner");
    assert_eq!(inner_arms.len(), 2, "recovery lost a valid arm: {inner_arms:?}");
    assert!(matches!(
        &inner_arms[0].body,
        Expr::Name { text, .. } if text == "inner_value"
    ));
    let Expr::Lambda { .. } = &inner_arms[1].body else {
        panic!("recovery lost the following block closure tail: {inner_arms:?}");
    };
    let (final_statements, final_arms) = closure_case(&inner_arms[1].body, "final");
    assert_leading_let(final_statements, "final_value", "final");
    assert_eq!(final_arms.len(), 2);
    assert!(matches!(
        &final_arms[0].body,
        Expr::Name { text, .. } if text == "final_value"
    ));
    assert!(matches!(
        &final_arms[1].body,
        Expr::InterpolatedString { segments, .. }
            if matches!(segments.as_slice(), [
                StringSegment::Text { text: prefix, .. },
                StringSegment::Expression { value: Expr::Name { text: name, .. }, .. },
                StringSegment::Text { text: suffix, .. }
            ] if prefix == "done " && name == "flag" && suffix == " tail")
    ));
    assert!(matches!(
        &arms[1].body,
        Expr::InterpolatedString { segments, .. }
            if matches!(segments.as_slice(), [
                StringSegment::Text { text: prefix, .. },
                StringSegment::Expression { value: Expr::Name { text: name, .. }, .. },
                StringSegment::Text { text: suffix, .. }
            ] if prefix == "fallback " && name == "flag" && suffix == " done")
    ));

    // The reference permits block closures and case tails but does not define
    // missing-separator recovery. Keep the following closure tail and its lets.
}

#[test]
fn recovered_block_control_keeps_following_closure_tail() {
    let source = include_str!("fixtures/malformed-case-arm-block-control-before-closure-tail.orna");
    let parsed = parse_module(source);

    assert_eq!(parsed.diagnostics.len(), 1, "{:?}", parsed.diagnostics);
    let diagnostic = &parsed.diagnostics[0];
    assert_eq!(diagnostic.code, "ORNA-PARSE-001");
    assert_eq!(diagnostic.message, "expected `:` after case pattern");
    let malformed_separator = source.find("false 0").expect("fixture has malformed arm") + 6;
    assert_eq!(diagnostic.span.start, malformed_separator);
    assert_eq!(diagnostic.span.end, malformed_separator + 1);

    let Declaration::Function { body, .. } = &parsed.value.items[0].declaration else {
        panic!("expected a function declaration");
    };
    let Expr::Block { tail: Some(tail), .. } = body else {
        panic!("expected the root case tail");
    };
    let Expr::Control { arms, .. } = tail.as_ref() else {
        panic!("expected the root case expression");
    };
    assert_eq!(arms.len(), 2, "root case lost its fallback arm: {arms:?}");

    let Expr::InterpolatedString { segments, .. } = &arms[0].body else {
        panic!("root arm lost its closure interpolation");
    };
    let [
        StringSegment::Text { text: prefix, .. },
        StringSegment::Expression { value: closure, .. },
        StringSegment::Text { text: suffix, .. },
    ] = segments.as_slice()
    else {
        panic!("interpolated closure lost its outer suffix: {segments:?}");
    };
    assert_eq!(prefix, "before ");
    assert_eq!(suffix, " after");

    let Expr::Lambda { body: closure_body, .. } = closure else {
        panic!("string interpolation lost its closure");
    };
    let Expr::Block {
        statements,
        tail: Some(final_closure),
        ..
    } = closure_body.as_ref()
    else {
        panic!("closure block lost its final tail");
    };
    let [
        Statement::Let {
            pattern: Pattern::Name(outer_binding, _),
            ..
        },
        Statement::Control {
            value: Expr::Control { arms: recovered_arms, .. },
            ..
        },
    ] = statements.as_slice()
    else {
        panic!("malformed case was not retained as a control statement: {statements:?}");
    };
    assert_eq!(outer_binding, "outer_saved");
    assert_eq!(recovered_arms.len(), 1, "{recovered_arms:?}");
    assert!(matches!(
        &recovered_arms[0].body,
        Expr::Name { text, .. } if text == "outer_saved"
    ));

    let Expr::Lambda { body: final_body, .. } = final_closure.as_ref() else {
        panic!("recovery lost the closure tail after the semicolon");
    };
    let Expr::Block {
        statements: final_statements,
        tail: Some(final_tail),
        ..
    } = final_body.as_ref()
    else {
        panic!("final closure lost its block tail");
    };
    assert!(matches!(
        final_statements.as_slice(),
        [Statement::Let {
            pattern: Pattern::Name(name, _),
            ..
        }] if name == "final_saved"
    ));
    let Expr::Control {
        condition: Some(condition),
        arms: final_arms,
        ..
    } = final_tail.as_ref()
    else {
        panic!("final closure tail is not a case expression");
    };
    assert!(matches!(condition.as_ref(), Expr::Name { text, .. } if text == "final"));
    assert_eq!(final_arms.len(), 2);
    assert!(matches!(
        &final_arms[0].body,
        Expr::Name { text, .. } if text == "final_saved"
    ));
    assert!(matches!(
        &final_arms[1].body,
        Expr::InterpolatedString { segments, .. }
            if matches!(segments.as_slice(), [
                StringSegment::Text { text: prefix, .. },
                StringSegment::Expression { value: Expr::Name { text: name, .. }, .. },
                StringSegment::Text { text: suffix, .. }
            ] if prefix == "done " && name == "flag" && suffix == " tail")
    ));
    assert!(matches!(
        &arms[1].body,
        Expr::InterpolatedString { segments, .. }
            if matches!(segments.as_slice(), [
                StringSegment::Text { text: prefix, .. },
                StringSegment::Expression { value: Expr::Name { text: name, .. }, .. },
                StringSegment::Text { text: suffix, .. }
            ] if prefix == "fallback " && name == "flag" && suffix == " done")
    ));

    // The reference permits control statements and closure block tails but
    // does not define malformed-arm recovery. Preserve the semicolon boundary.
}

#[test]
fn recovered_block_control_trailing_comma_keeps_closure_tail() {
    let source = include_str!("fixtures/malformed-case-arm-block-control-trailing-comma-before-tail.orna");
    let parsed = parse_module(source);

    assert_eq!(parsed.diagnostics.len(), 1, "{:?}", parsed.diagnostics);
    let diagnostic = &parsed.diagnostics[0];
    assert_eq!(diagnostic.code, "ORNA-PARSE-001");
    assert_eq!(diagnostic.message, "expected `:` after case pattern");
    let malformed_separator = source.find("false 0").expect("fixture has malformed arm") + 6;
    assert_eq!(diagnostic.span.start, malformed_separator);
    assert_eq!(diagnostic.span.end, malformed_separator + 1);

    let Declaration::Function { body, .. } = &parsed.value.items[0].declaration else {
        panic!("expected a function declaration");
    };
    let Expr::Block { tail: Some(root_tail), .. } = body else {
        panic!("expected the root case tail");
    };
    let Expr::Control { arms: root_arms, .. } = root_tail.as_ref() else {
        panic!("expected the root case expression");
    };
    assert_eq!(root_arms.len(), 2, "{root_arms:?}");
    let Expr::InterpolatedString { segments, .. } = &root_arms[0].body else {
        panic!("root arm lost its closure string");
    };
    let [
        StringSegment::Text { text: prefix, .. },
        StringSegment::Expression { value: closure, .. },
        StringSegment::Text { text: suffix, .. },
    ] = segments.as_slice()
    else {
        panic!("outer interpolation lost its suffix boundary: {segments:?}");
    };
    assert_eq!(prefix, "before ");
    assert_eq!(suffix, " after");

    let Expr::Lambda { body: closure_body, .. } = closure else {
        panic!("outer interpolation lost its closure");
    };
    let Expr::Block {
        statements,
        tail: Some(final_closure),
        ..
    } = closure_body.as_ref()
    else {
        panic!("case semicolon swallowed the closure block tail");
    };
    let [
        Statement::Let { .. },
        Statement::Control {
            value: Expr::Control { arms, .. },
            ..
        },
    ] = statements.as_slice()
    else {
        panic!("case was not retained as a block control statement: {statements:?}");
    };
    assert_eq!(arms.len(), 1, "{arms:?}");
    assert!(matches!(final_closure.as_ref(), Expr::Lambda { .. }));
    let Expr::Lambda { body: final_body, .. } = final_closure.as_ref() else {
        unreachable!();
    };
    let Expr::Block {
        tail: Some(final_tail),
        ..
    } = final_body.as_ref()
    else {
        panic!("following closure lost its case tail");
    };
    let Expr::Control {
        arms: final_arms, ..
    } = final_tail.as_ref()
    else {
        panic!("following closure tail is not a case expression");
    };
    assert_eq!(final_arms.len(), 2, "{final_arms:?}");
    assert!(matches!(
        &root_arms[1].body,
        Expr::InterpolatedString { segments, .. }
            if matches!(segments.as_slice(), [
                StringSegment::Text { text: prefix, .. },
                StringSegment::Expression { value: Expr::Name { text: name, .. }, .. },
                StringSegment::Text { text: suffix, .. }
            ] if prefix == "fallback " && name == "flag" && suffix == " done")
    ));

    // A trailing arm comma before the block statement's semicolon must not
    // blur the statement boundary or consume the closure's final case tail.
}

#[test]
fn recovered_trailing_comma_keeps_following_arm_and_block_tail() {
    let source = include_str!("fixtures/malformed-case-arm-trailing-comma-before-following-block-tail.orna");
    let parsed = parse_module(source);

    assert_eq!(parsed.diagnostics.len(), 1, "{:?}", parsed.diagnostics);
    let diagnostic = &parsed.diagnostics[0];
    assert_eq!(diagnostic.code, "ORNA-PARSE-001");
    assert_eq!(diagnostic.message, "expected `:` after case pattern");
    let malformed_separator = source.find("false 0").expect("fixture has malformed arm") + 6;
    assert_eq!(diagnostic.span.start, malformed_separator);
    assert_eq!(diagnostic.span.end, malformed_separator + 1);

    fn closure_case<'a>(expression: &'a Expr, condition_name: &str) -> &'a [CaseArm] {
        let Expr::Lambda { body, .. } = expression else {
            panic!("expected a closure, got {expression:?}");
        };
        let Expr::Block { tail: Some(tail), .. } = body.as_ref() else {
            panic!("closure lost its case tail");
        };
        let Expr::Control {
            condition: Some(condition),
            arms,
            ..
        } = tail.as_ref()
        else {
            panic!("closure tail is not a case expression");
        };
        assert!(matches!(condition.as_ref(), Expr::Name { text, .. } if text == condition_name));
        arms
    }

    let Declaration::Function { body, .. } = &parsed.value.items[0].declaration else {
        panic!("expected a function declaration");
    };
    let Expr::Block { tail: Some(root_tail), .. } = body else {
        panic!("expected the root case tail");
    };
    let Expr::Control { arms: root_arms, .. } = root_tail.as_ref() else {
        panic!("expected the root case expression");
    };
    assert_eq!(root_arms.len(), 2, "root case lost its fallback: {root_arms:?}");
    let Expr::InterpolatedString { segments, .. } = &root_arms[0].body else {
        panic!("outer arm lost its closure string");
    };
    let [
        StringSegment::Text { text: prefix, .. },
        StringSegment::Expression { value: outer_closure, .. },
        StringSegment::Text { text: suffix, .. },
    ] = segments.as_slice()
    else {
        panic!("outer string lost its suffix boundaries: {segments:?}");
    };
    assert_eq!(prefix, "start ");
    assert_eq!(suffix, " after");

    let Expr::Lambda { body: outer_body, .. } = outer_closure else {
        panic!("outer interpolation lost its closure");
    };
    let Expr::Block {
        statements,
        tail: Some(finish_closure),
        ..
    } = outer_body.as_ref()
    else {
        panic!("case statement consumed the following block tail");
    };
    let [
        Statement::Let { .. },
        Statement::Control {
            value: Expr::Control { arms: recovered_arms, .. },
            ..
        },
    ] = statements.as_slice()
    else {
        panic!("recovered case lost its statement boundary: {statements:?}");
    };
    assert_eq!(recovered_arms.len(), 2, "{recovered_arms:?}");
    assert!(matches!(
        &recovered_arms[0].body,
        Expr::Name { text, .. } if text == "saved"
    ));
    let Expr::Lambda { .. } = &recovered_arms[1].body else {
        panic!("trailing-comma recovery lost the following closure arm: {recovered_arms:?}");
    };
    let branch_arms = closure_case(&recovered_arms[1].body, "branch");
    assert_eq!(branch_arms.len(), 2);
    assert!(matches!(
        &branch_arms[1].body,
        Expr::InterpolatedString { segments, .. }
            if matches!(segments.as_slice(), [
                StringSegment::Text { text: prefix, .. },
                StringSegment::Expression { value: Expr::Name { text: name, .. }, .. },
                StringSegment::Text { text: suffix, .. }
            ] if prefix == "last " && name == "branch" && suffix == " end")
    ));

    let finish_arms = closure_case(finish_closure, "finish");
    assert_eq!(finish_arms.len(), 2);
    assert!(matches!(
        &finish_arms[1].body,
        Expr::InterpolatedString { segments, .. }
            if matches!(segments.as_slice(), [
                StringSegment::Text { text: prefix, .. },
                StringSegment::Expression { value: Expr::Name { text: name, .. }, .. },
                StringSegment::Text { text: suffix, .. }
            ] if prefix == "finish " && name == "flag" && suffix == " done")
    ));
    assert!(matches!(
        &root_arms[1].body,
        Expr::InterpolatedString { segments, .. }
            if matches!(segments.as_slice(), [
                StringSegment::Text { text: prefix, .. },
                StringSegment::Expression { value: Expr::Name { text: name, .. }, .. },
                StringSegment::Text { text: suffix, .. }
            ] if prefix == "fallback " && name == "flag" && suffix == " done")
    ));

    // Missing-separator recovery is unspecified. Retain the following comma-
    // separated closure arm and the block tail after the control semicolon.
}

#[test]
fn recovered_nested_trailing_commas_keep_block_tail() {
    let source = include_str!("fixtures/malformed-case-arm-nested-trailing-comma-before-block-tail.orna");
    let parsed = parse_module(source);

    let malformed_patterns = source
        .match_indices("false 0")
        .map(|(offset, _)| offset + 6)
        .collect::<Vec<_>>();
    assert_eq!(malformed_patterns.len(), 2);
    assert_eq!(parsed.diagnostics.len(), 2, "{:?}", parsed.diagnostics);
    for (diagnostic, start) in parsed.diagnostics.iter().zip(malformed_patterns) {
        assert_eq!(diagnostic.code, "ORNA-PARSE-001");
        assert_eq!(diagnostic.message, "expected `:` after case pattern");
        assert_eq!(diagnostic.span.start, start);
        assert_eq!(diagnostic.span.end, start + 1);
    }

    fn closure_case<'a>(expression: &'a Expr, condition_name: &str) -> &'a [CaseArm] {
        let Expr::Lambda { body, .. } = expression else {
            panic!("expected a closure, got {expression:?}");
        };
        let Expr::Block { tail: Some(tail), .. } = body.as_ref() else {
            panic!("closure lost its case tail");
        };
        let Expr::Control {
            condition: Some(condition),
            arms,
            ..
        } = tail.as_ref()
        else {
            panic!("closure tail is not a case expression");
        };
        assert!(matches!(condition.as_ref(), Expr::Name { text, .. } if text == condition_name));
        arms
    }

    let Declaration::Function { body, .. } = &parsed.value.items[0].declaration else {
        panic!("expected a function declaration");
    };
    let Expr::Block { tail: Some(root_tail), .. } = body else {
        panic!("expected the root case tail");
    };
    let Expr::Control { arms: root_arms, .. } = root_tail.as_ref() else {
        panic!("expected the root case expression");
    };
    assert_eq!(root_arms.len(), 2, "{root_arms:?}");
    let Expr::InterpolatedString { segments, .. } = &root_arms[0].body else {
        panic!("outer arm lost its closure string");
    };
    let [
        StringSegment::Text { text: prefix, .. },
        StringSegment::Expression { value: outer_closure, .. },
        StringSegment::Text { text: suffix, .. },
    ] = segments.as_slice()
    else {
        panic!("outer string lost suffix boundaries: {segments:?}");
    };
    assert_eq!(prefix, "start ");
    assert_eq!(suffix, " after");

    let Expr::Lambda { body: outer_body, .. } = outer_closure else {
        panic!("outer interpolation lost its closure");
    };
    let Expr::Block {
        statements,
        tail: Some(finish_closure),
        ..
    } = outer_body.as_ref()
    else {
        panic!("recovered case statement consumed the block tail");
    };
    let [
        Statement::Let { .. },
        Statement::Control {
            value: Expr::Control { arms: outer_arms, .. },
            ..
        },
    ] = statements.as_slice()
    else {
        panic!("outer case did not remain a control statement: {statements:?}");
    };
    assert_eq!(outer_arms.len(), 2, "{outer_arms:?}");
    assert!(matches!(
        &outer_arms[0].body,
        Expr::Name { text, .. } if text == "saved"
    ));
    let Expr::Lambda { .. } = &outer_arms[1].body else {
        panic!("outer recovery lost the following branch closure: {outer_arms:?}");
    };
    let branch_arms = closure_case(&outer_arms[1].body, "branch");
    assert_eq!(branch_arms.len(), 2, "{branch_arms:?}");
    assert!(matches!(
        &branch_arms[0].body,
        Expr::InterpolatedString { segments, .. }
            if matches!(segments.as_slice(), [
                StringSegment::Text { text: prefix, .. },
                StringSegment::Expression { value: Expr::Name { text: name, .. }, .. },
                StringSegment::Text { text: suffix, .. }
            ] if prefix == "branch " && name == "flag" && suffix == " tail")
    ));
    assert!(matches!(
        &branch_arms[1].body,
        Expr::InterpolatedString { segments, .. }
            if matches!(segments.as_slice(), [
                StringSegment::Text { text: prefix, .. },
                StringSegment::Expression { value: Expr::Name { text: name, .. }, .. },
                StringSegment::Text { text: suffix, .. }
            ] if prefix == "last " && name == "branch" && suffix == " end")
    ));

    let finish_arms = closure_case(finish_closure, "finish");
    assert_eq!(finish_arms.len(), 2);
    assert!(matches!(
        &finish_arms[1].body,
        Expr::InterpolatedString { segments, .. }
            if matches!(segments.as_slice(), [
                StringSegment::Text { text: prefix, .. },
                StringSegment::Expression { value: Expr::Name { text: name, .. }, .. },
                StringSegment::Text { text: suffix, .. }
            ] if prefix == "finish " && name == "flag" && suffix == " done")
    ));
    assert!(matches!(
        &root_arms[1].body,
        Expr::InterpolatedString { segments, .. }
            if matches!(segments.as_slice(), [
                StringSegment::Text { text: prefix, .. },
                StringSegment::Expression { value: Expr::Name { text: name, .. }, .. },
                StringSegment::Text { text: suffix, .. }
            ] if prefix == "fallback " && name == "flag" && suffix == " done")
    ));

    // The reference leaves nested malformed-arm recovery unspecified. Keep
    // both following arms, the semicolon boundary, and the block-tail closure.
}

#[test]
fn recovered_repeated_trailing_comma_arms_keep_block_tail() {
    let source = include_str!("fixtures/malformed-case-arm-repeated-trailing-comma-before-block-tail.orna");
    let parsed = parse_module(source);

    let malformed_patterns = [
        source.find("false 0").expect("first malformed arm") + 6,
        source.find("false 1").expect("second malformed arm") + 6,
    ];
    assert_eq!(parsed.diagnostics.len(), 2, "{:?}", parsed.diagnostics);
    for (diagnostic, start) in parsed.diagnostics.iter().zip(malformed_patterns) {
        assert_eq!(diagnostic.code, "ORNA-PARSE-001");
        assert_eq!(diagnostic.message, "expected `:` after case pattern");
        assert_eq!(diagnostic.span.start, start);
        assert_eq!(diagnostic.span.end, start + 1);
    }

    let Declaration::Function { body, .. } = &parsed.value.items[0].declaration else {
        panic!("expected a function declaration");
    };
    let Expr::Block { tail: Some(root_tail), .. } = body else {
        panic!("expected the root case tail");
    };
    let Expr::Control { arms: root_arms, .. } = root_tail.as_ref() else {
        panic!("expected the root case expression");
    };
    assert_eq!(root_arms.len(), 2, "{root_arms:?}");
    let Expr::InterpolatedString { segments, .. } = &root_arms[0].body else {
        panic!("outer arm lost its closure string");
    };
    let [
        StringSegment::Text { text: prefix, .. },
        StringSegment::Expression { value: outer_closure, .. },
        StringSegment::Text { text: suffix, .. },
    ] = segments.as_slice()
    else {
        panic!("outer string lost its closure suffix boundary: {segments:?}");
    };
    assert_eq!(prefix, "start ");
    assert_eq!(suffix, " after");

    let Expr::Lambda { body: outer_body, .. } = outer_closure else {
        panic!("outer interpolation lost its closure");
    };
    let Expr::Block {
        statements,
        tail: Some(finish_closure),
        ..
    } = outer_body.as_ref()
    else {
        panic!("repeated recovery consumed the closure block tail");
    };
    let [
        Statement::Let { .. },
        Statement::Control {
            value: Expr::Control { arms: recovered_arms, .. },
            ..
        },
    ] = statements.as_slice()
    else {
        panic!("recovered case lost its statement boundary: {statements:?}");
    };
    assert_eq!(recovered_arms.len(), 2, "{recovered_arms:?}");
    assert!(matches!(
        &recovered_arms[0].body,
        Expr::Name { text, .. } if text == "saved"
    ));
    let Expr::Lambda { .. } = &recovered_arms[1].body else {
        panic!("recovery lost the valid arm after both malformed arms: {recovered_arms:?}");
    };
    let Expr::Lambda { body: branch_body, .. } = &recovered_arms[1].body else {
        unreachable!();
    };
    let Expr::Block { tail: Some(branch_tail), .. } = branch_body.as_ref() else {
        panic!("following branch closure lost its case tail");
    };
    let Expr::Control { arms: branch_arms, .. } = branch_tail.as_ref() else {
        panic!("following branch tail is not a case expression");
    };
    assert_eq!(branch_arms.len(), 2, "{branch_arms:?}");
    assert!(matches!(
        &branch_arms[1].body,
        Expr::InterpolatedString { segments, .. }
            if matches!(segments.as_slice(), [
                StringSegment::Text { text: prefix, .. },
                StringSegment::Expression { value: Expr::Name { text: name, .. }, .. },
                StringSegment::Text { text: suffix, .. }
            ] if prefix == "last " && name == "branch" && suffix == " end")
    ));

    let Expr::Lambda { body: finish_body, .. } = finish_closure.as_ref() else {
        panic!("following block-tail closure was lost");
    };
    let Expr::Block { tail: Some(finish_tail), .. } = finish_body.as_ref() else {
        panic!("following closure lost its block tail");
    };
    assert!(matches!(finish_tail.as_ref(), Expr::Control { .. }));
    assert!(matches!(
        &root_arms[1].body,
        Expr::InterpolatedString { segments, .. }
            if matches!(segments.as_slice(), [
                StringSegment::Text { text: prefix, .. },
                StringSegment::Expression { value: Expr::Name { text: name, .. }, .. },
                StringSegment::Text { text: suffix, .. }
            ] if prefix == "fallback " && name == "flag" && suffix == " done")
    ));

    // Repeated malformed-arm recovery is unspecified. Keep the comma-delimited
    // valid arm, semicolon statement boundary, and subsequent closure tail.
}

#[test]
fn three_trailing_comma_recoveries_preserve_following_closure_tails() {
    let source = include_str!("fixtures/malformed-case-arm-three-trailing-comma-before-block-tail.orna");
    let parsed = parse_module(source);

    let malformed_patterns = [
        source.find("false 0").expect("first malformed arm") + 6,
        source.find("false 1").expect("second malformed arm") + 6,
        source.find("false 2").expect("third malformed arm") + 6,
    ];
    assert_eq!(parsed.diagnostics.len(), 3, "{:?}", parsed.diagnostics);
    for (diagnostic, start) in parsed.diagnostics.iter().zip(malformed_patterns) {
        assert_eq!(diagnostic.code, "ORNA-PARSE-001");
        assert_eq!(diagnostic.message, "expected `:` after case pattern");
        assert_eq!(diagnostic.span.start, start);
        assert_eq!(diagnostic.span.end, start + 1);
    }

    let Declaration::Function { body, .. } = &parsed.value.items[0].declaration else {
        panic!("expected a function declaration");
    };
    let Expr::Block { tail: Some(root_tail), .. } = body else {
        panic!("expected the root case tail");
    };
    let Expr::Control { arms: root_arms, .. } = root_tail.as_ref() else {
        panic!("expected the root case expression");
    };
    assert_eq!(root_arms.len(), 2, "{root_arms:?}");
    let Expr::InterpolatedString { segments, .. } = &root_arms[0].body else {
        panic!("outer arm lost its closure string");
    };
    let [
        StringSegment::Text { text: prefix, .. },
        StringSegment::Expression { value: outer_closure, .. },
        StringSegment::Text { text: suffix, .. },
    ] = segments.as_slice()
    else {
        panic!("outer string lost its closure suffix boundary: {segments:?}");
    };
    assert_eq!(prefix, "start ");
    assert_eq!(suffix, " after");

    let Expr::Lambda { body: outer_body, .. } = outer_closure else {
        panic!("outer interpolation lost its closure");
    };
    let Expr::Block {
        statements,
        tail: Some(finish_closure),
        ..
    } = outer_body.as_ref()
    else {
        panic!("repeated recovery consumed the closure block tail");
    };
    let [
        Statement::Let { .. },
        Statement::Control {
            value: Expr::Control { arms: recovered_arms, .. },
            ..
        },
    ] = statements.as_slice()
    else {
        panic!("recovered case lost its statement boundary: {statements:?}");
    };
    assert_eq!(recovered_arms.len(), 2, "{recovered_arms:?}");
    assert!(matches!(
        &recovered_arms[0].body,
        Expr::Name { text, .. } if text == "saved"
    ));
    let Expr::Lambda { body: branch_body, .. } = &recovered_arms[1].body else {
        panic!("recovery lost the valid arm after all malformed arms");
    };
    let Expr::Block { tail: Some(branch_tail), .. } = branch_body.as_ref() else {
        panic!("following branch closure lost its case tail");
    };
    let Expr::Control { arms: branch_arms, .. } = branch_tail.as_ref() else {
        panic!("following branch tail is not a case expression");
    };
    assert_eq!(branch_arms.len(), 2, "{branch_arms:?}");

    let Expr::Lambda { body: finish_body, .. } = finish_closure.as_ref() else {
        panic!("following block-tail closure was lost");
    };
    let Expr::Block { tail: Some(finish_tail), .. } = finish_body.as_ref() else {
        panic!("following closure lost its block tail");
    };
    assert!(matches!(finish_tail.as_ref(), Expr::Control { .. }));

    // Three repeated malformed arms are unspecified by the reference. Preserve
    // the next comma-delimited closure arm and both enclosing closure tails.
}

#[test]
fn four_trailing_comma_recoveries_preserve_following_closure_tails() {
    let source = include_str!("fixtures/malformed-case-arm-four-trailing-comma-before-block-tail.orna");
    let parsed = parse_module(source);

    let malformed_patterns = [
        source.find("false 0").expect("first malformed arm") + 6,
        source.find("false 1").expect("second malformed arm") + 6,
        source.find("false 2").expect("third malformed arm") + 6,
        source.find("false 3").expect("fourth malformed arm") + 6,
    ];
    assert_eq!(parsed.diagnostics.len(), 4, "{:?}", parsed.diagnostics);
    for (diagnostic, start) in parsed.diagnostics.iter().zip(malformed_patterns) {
        assert_eq!(diagnostic.code, "ORNA-PARSE-001");
        assert_eq!(diagnostic.message, "expected `:` after case pattern");
        assert_eq!(diagnostic.span.start, start);
        assert_eq!(diagnostic.span.end, start + 1);
    }

    let Declaration::Function { body, .. } = &parsed.value.items[0].declaration else {
        panic!("expected a function declaration");
    };
    let Expr::Block { tail: Some(root_tail), .. } = body else {
        panic!("expected the root case tail");
    };
    let Expr::Control { arms: root_arms, .. } = root_tail.as_ref() else {
        panic!("expected the root case expression");
    };
    assert_eq!(root_arms.len(), 2, "{root_arms:?}");
    let Expr::InterpolatedString { segments, .. } = &root_arms[0].body else {
        panic!("outer arm lost its closure string");
    };
    let [
        StringSegment::Text { text: prefix, .. },
        StringSegment::Expression {
            value: Expr::Lambda { body: outer_body, .. },
            ..
        },
        StringSegment::Text { text: suffix, .. },
    ] = segments.as_slice()
    else {
        panic!("outer string lost its closure suffix boundary: {segments:?}");
    };
    assert_eq!(prefix, "start ");
    assert_eq!(suffix, " after");
    let Expr::Block {
        statements,
        tail: Some(finish_closure),
        ..
    } = outer_body.as_ref()
    else {
        panic!("repeated recovery consumed the closure block tail");
    };
    let [
        Statement::Let { .. },
        Statement::Control {
            value: Expr::Control { arms: recovered_arms, .. },
            ..
        },
    ] = statements.as_slice()
    else {
        panic!("recovered case lost its statement boundary: {statements:?}");
    };
    assert_eq!(recovered_arms.len(), 2, "{recovered_arms:?}");
    assert!(matches!(
        &recovered_arms[0].body,
        Expr::Name { text, .. } if text == "saved"
    ));
    let Expr::Lambda { body: branch_body, .. } = &recovered_arms[1].body else {
        panic!("recovery lost the valid arm after all malformed arms");
    };
    let Expr::Block { tail: Some(branch_tail), .. } = branch_body.as_ref() else {
        panic!("following branch closure lost its case tail");
    };
    assert!(matches!(branch_tail.as_ref(), Expr::Control { .. }));

    let Expr::Lambda { body: finish_body, .. } = finish_closure.as_ref() else {
        panic!("following block-tail closure was lost");
    };
    let Expr::Block { tail: Some(finish_tail), .. } = finish_body.as_ref() else {
        panic!("following closure lost its block tail");
    };
    assert!(matches!(finish_tail.as_ref(), Expr::Control { .. }));

    // Four repeated malformed arms are unspecified by the reference. Keep the
    // valid comma-delimited arm plus nested and enclosing closure tails intact.
}

#[test]
fn four_recoveries_keep_comma_on_following_closure_arm() {
    let source = include_str!("fixtures/malformed-case-arm-four-recoveries-before-comma-closure-tail.orna");
    let parsed = parse_module(source);

    let malformed_patterns = [
        source.find("false 0").expect("first malformed arm") + 6,
        source.find("false 1").expect("second malformed arm") + 6,
        source.find("false 2").expect("third malformed arm") + 6,
        source.find("false 3").expect("fourth malformed arm") + 6,
    ];
    assert_eq!(parsed.diagnostics.len(), 4, "{:?}", parsed.diagnostics);
    for (diagnostic, start) in parsed.diagnostics.iter().zip(malformed_patterns) {
        assert_eq!(diagnostic.code, "ORNA-PARSE-001");
        assert_eq!(diagnostic.message, "expected `:` after case pattern");
        assert_eq!(diagnostic.span.start, start);
        assert_eq!(diagnostic.span.end, start + 1);
    }

    let Declaration::Function { body, .. } = &parsed.value.items[0].declaration else {
        panic!("expected a function declaration");
    };
    let Expr::Block { tail: Some(root_tail), .. } = body else {
        panic!("expected the root case tail");
    };
    let Expr::Control { arms: root_arms, .. } = root_tail.as_ref() else {
        panic!("expected the root case expression");
    };
    assert_eq!(root_arms.len(), 2, "{root_arms:?}");
    let Expr::InterpolatedString { segments, .. } = &root_arms[0].body else {
        panic!("outer arm lost its closure string");
    };
    let [
        StringSegment::Text { text: prefix, .. },
        StringSegment::Expression {
            value: Expr::Lambda { body: outer_body, .. },
            ..
        },
        StringSegment::Text { text: suffix, .. },
    ] = segments.as_slice()
    else {
        panic!("outer string lost its closure suffix boundary: {segments:?}");
    };
    assert_eq!(prefix, "start ");
    assert_eq!(suffix, " after");

    let Expr::Block {
        statements,
        tail: Some(finish_closure),
        ..
    } = outer_body.as_ref()
    else {
        panic!("four recovery diagnostics consumed the enclosing block tail");
    };
    let [
        Statement::Let { .. },
        Statement::Control {
            value: Expr::Control { arms: recovered_arms, .. },
            ..
        },
    ] = statements.as_slice()
    else {
        panic!("recovered case lost its statement boundary: {statements:?}");
    };
    assert_eq!(recovered_arms.len(), 2, "{recovered_arms:?}");
    assert!(matches!(
        &recovered_arms[0].body,
        Expr::Name { text, .. } if text == "saved"
    ));
    let Expr::Lambda { body: branch_body, .. } = &recovered_arms[1].body else {
        panic!("trailing comma lost the valid closure arm");
    };
    let Expr::Block { tail: Some(branch_tail), .. } = branch_body.as_ref() else {
        panic!("following closure arm lost its nested case tail");
    };
    assert!(matches!(branch_tail.as_ref(), Expr::Control { .. }));

    let Expr::Lambda { body: finish_body, .. } = finish_closure.as_ref() else {
        panic!("following block-tail closure was lost");
    };
    let Expr::Block { tail: Some(finish_tail), .. } = finish_body.as_ref() else {
        panic!("following closure lost its block tail");
    };
    assert!(matches!(finish_tail.as_ref(), Expr::Control { .. }));

    // The reference does not specify recovery across this valid comma after
    // four malformed arms; preserve both nested and enclosing closure tails.
}

#[test]
fn four_recoveries_preserve_immediate_following_closure_tail() {
    let source = include_str!("fixtures/malformed-case-arm-four-recoveries-direct-closure-tail.orna");
    let parsed = parse_module(source);

    let malformed_patterns = [
        source.find("false 0").expect("first malformed arm") + 6,
        source.find("false 1").expect("second malformed arm") + 6,
        source.find("false 2").expect("third malformed arm") + 6,
        source.find("false 3").expect("fourth malformed arm") + 6,
    ];
    assert_eq!(parsed.diagnostics.len(), 4, "{:?}", parsed.diagnostics);
    for (diagnostic, start) in parsed.diagnostics.iter().zip(malformed_patterns) {
        assert_eq!(diagnostic.code, "ORNA-PARSE-001");
        assert_eq!(diagnostic.message, "expected `:` after case pattern");
        assert_eq!(diagnostic.span.start, start);
        assert_eq!(diagnostic.span.end, start + 1);
    }

    let Declaration::Function { body, .. } = &parsed.value.items[0].declaration else {
        panic!("expected a function declaration");
    };
    let Expr::Block { tail: Some(root_tail), .. } = body else {
        panic!("expected the root case tail");
    };
    let Expr::Control { arms: root_arms, .. } = root_tail.as_ref() else {
        panic!("expected the root case expression");
    };
    assert_eq!(root_arms.len(), 2, "{root_arms:?}");
    let Expr::InterpolatedString { segments, .. } = &root_arms[0].body else {
        panic!("outer arm lost its closure string");
    };
    let [
        StringSegment::Text { text: prefix, .. },
        StringSegment::Expression {
            value: Expr::Lambda { body: outer_body, .. },
            ..
        },
        StringSegment::Text { text: suffix, .. },
    ] = segments.as_slice()
    else {
        panic!("outer string lost its closure suffix boundary: {segments:?}");
    };
    assert_eq!(prefix, "start ");
    assert_eq!(suffix, " after");

    let Expr::Block {
        statements,
        tail: Some(finish_closure),
        ..
    } = outer_body.as_ref()
    else {
        panic!("four recovery diagnostics consumed the immediate closure tail");
    };
    let [
        Statement::Let { .. },
        Statement::Control {
            value: Expr::Control { arms: recovered_arms, .. },
            ..
        },
    ] = statements.as_slice()
    else {
        panic!("recovered case lost its statement boundary: {statements:?}");
    };
    assert_eq!(recovered_arms.len(), 1, "{recovered_arms:?}");
    assert!(matches!(
        &recovered_arms[0].body,
        Expr::Name { text, .. } if text == "saved"
    ));

    let Expr::Lambda { body: finish_body, .. } = finish_closure.as_ref() else {
        panic!("closure immediately following recovery was lost");
    };
    let Expr::Block { tail: Some(finish_tail), .. } = finish_body.as_ref() else {
        panic!("following closure lost its block tail");
    };
    assert!(matches!(finish_tail.as_ref(), Expr::Control { .. }));

    // The reference is silent on repeated malformed arms ending at a comma.
    // Use the case boundary to recover, leaving the next block-tail closure intact.
}

#[test]
fn four_recoveries_without_final_comma_keep_closure_tail() {
    let source = include_str!("fixtures/malformed-case-arm-four-recoveries-closure-tail-without-final-comma.orna");
    let parsed = parse_module(source);

    let malformed_patterns = [
        source.find("false 0").expect("first malformed arm") + 6,
        source.find("false 1").expect("second malformed arm") + 6,
        source.find("false 2").expect("third malformed arm") + 6,
        source.find("false 3").expect("fourth malformed arm") + 6,
    ];
    assert_eq!(parsed.diagnostics.len(), 4, "{:?}", parsed.diagnostics);
    for (diagnostic, start) in parsed.diagnostics.iter().zip(malformed_patterns) {
        assert_eq!(diagnostic.code, "ORNA-PARSE-001");
        assert_eq!(diagnostic.message, "expected `:` after case pattern");
        assert_eq!(diagnostic.span.start, start);
        assert_eq!(diagnostic.span.end, start + 1);
    }

    let Declaration::Function { body, .. } = &parsed.value.items[0].declaration else {
        panic!("expected a function declaration");
    };
    let Expr::Block { tail: Some(root_tail), .. } = body else {
        panic!("expected the root case tail");
    };
    let Expr::Control { arms: root_arms, .. } = root_tail.as_ref() else {
        panic!("expected the root case expression");
    };
    assert_eq!(root_arms.len(), 2, "{root_arms:?}");
    let Expr::InterpolatedString { segments, .. } = &root_arms[0].body else {
        panic!("outer arm lost its closure string");
    };
    let [
        StringSegment::Text { text: prefix, .. },
        StringSegment::Expression {
            value: Expr::Lambda { body: outer_body, .. },
            ..
        },
        StringSegment::Text { text: suffix, .. },
    ] = segments.as_slice()
    else {
        panic!("outer string lost its closure suffix boundary: {segments:?}");
    };
    assert_eq!(prefix, "start ");
    assert_eq!(suffix, " after");

    let Expr::Block {
        statements,
        tail: Some(finish_closure),
        ..
    } = outer_body.as_ref()
    else {
        panic!("four recovery diagnostics consumed the immediate closure tail");
    };
    let [
        Statement::Let { .. },
        Statement::Control {
            value: Expr::Control { arms: recovered_arms, .. },
            ..
        },
    ] = statements.as_slice()
    else {
        panic!("recovered case lost its statement boundary: {statements:?}");
    };
    assert_eq!(recovered_arms.len(), 1, "{recovered_arms:?}");
    assert!(matches!(
        &recovered_arms[0].body,
        Expr::Name { text, .. } if text == "saved"
    ));

    let Expr::Lambda { body: finish_body, .. } = finish_closure.as_ref() else {
        panic!("closure immediately following recovery was lost");
    };
    let Expr::Block { tail: Some(finish_tail), .. } = finish_body.as_ref() else {
        panic!("following closure lost its block tail");
    };
    assert!(matches!(finish_tail.as_ref(), Expr::Control { .. }));

    // The reference does not specify four malformed arms ending without a
    // comma; keep the case boundary and subsequent closure tail intact.
}

#[test]
fn all_four_invalid_arms_keep_following_closure_tail() {
    let source = include_str!("fixtures/malformed-case-arm-four-invalid-arms-before-closure-tail.orna");
    let parsed = parse_module(source);

    let malformed_patterns = [
        source.find("false 0").expect("first malformed arm") + 6,
        source.find("false 1").expect("second malformed arm") + 6,
        source.find("false 2").expect("third malformed arm") + 6,
        source.find("false 3").expect("fourth malformed arm") + 6,
    ];
    assert_eq!(parsed.diagnostics.len(), 4, "{:?}", parsed.diagnostics);
    for (diagnostic, start) in parsed.diagnostics.iter().zip(malformed_patterns) {
        assert_eq!(diagnostic.code, "ORNA-PARSE-001");
        assert_eq!(diagnostic.message, "expected `:` after case pattern");
        assert_eq!(diagnostic.span.start, start);
        assert_eq!(diagnostic.span.end, start + 1);
    }

    let Declaration::Function { body, .. } = &parsed.value.items[0].declaration else {
        panic!("expected a function declaration");
    };
    let Expr::Block { tail: Some(root_tail), .. } = body else {
        panic!("expected the root case tail");
    };
    let Expr::Control { arms: root_arms, .. } = root_tail.as_ref() else {
        panic!("expected the root case expression");
    };
    assert_eq!(root_arms.len(), 2, "{root_arms:?}");
    let Expr::InterpolatedString { segments, .. } = &root_arms[0].body else {
        panic!("outer arm lost its closure string");
    };
    let [
        StringSegment::Text { text: prefix, .. },
        StringSegment::Expression {
            value: Expr::Lambda { body: outer_body, .. },
            ..
        },
        StringSegment::Text { text: suffix, .. },
    ] = segments.as_slice()
    else {
        panic!("outer string lost its closure suffix boundary: {segments:?}");
    };
    assert_eq!(prefix, "start ");
    assert_eq!(suffix, " after");

    let Expr::Block {
        statements,
        tail: Some(finish_closure),
        ..
    } = outer_body.as_ref()
    else {
        panic!("four invalid arms consumed the immediate closure tail");
    };
    let [
        Statement::Let { .. },
        Statement::Control {
            value: Expr::Control { arms: recovered_arms, .. },
            ..
        },
    ] = statements.as_slice()
    else {
        panic!("recovered case lost its statement boundary: {statements:?}");
    };
    assert!(recovered_arms.is_empty(), "{recovered_arms:?}");

    let Expr::Lambda { body: finish_body, .. } = finish_closure.as_ref() else {
        panic!("closure following the all-invalid case was lost");
    };
    let Expr::Block { tail: Some(finish_tail), .. } = finish_body.as_ref() else {
        panic!("following closure lost its block tail");
    };
    assert!(matches!(finish_tail.as_ref(), Expr::Control { .. }));

    // The reference does not specify recovery for an all-invalid case with a
    // trailing comma; retain its boundary and the following closure tail.
}

#[test]
fn invalid_nested_closure_recovery_keeps_following_closure_tail() {
    let source = include_str!("fixtures/malformed-case-arm-invalid-closure-body-before-tail.orna");
    let parsed = parse_module(source);

    let malformed_patterns = [
        source.find("false 0").expect("first malformed arm") + 6,
        source.find("false 1").expect("second malformed arm") + 6,
        source.find("false 2").expect("third malformed arm") + 6,
        source.find("false 3").expect("fourth malformed arm") + 6,
    ];
    assert_eq!(parsed.diagnostics.len(), 4, "{:?}", parsed.diagnostics);
    for (diagnostic, start) in parsed.diagnostics.iter().zip(malformed_patterns) {
        assert_eq!(diagnostic.code, "ORNA-PARSE-001");
        assert_eq!(diagnostic.message, "expected `:` after case pattern");
        assert_eq!(diagnostic.span.start, start);
        assert_eq!(diagnostic.span.end, start + 1);
    }

    let Declaration::Function { body, .. } = &parsed.value.items[0].declaration else {
        panic!("expected a function declaration");
    };
    let Expr::Block { tail: Some(root_tail), .. } = body else {
        panic!("expected the root case tail");
    };
    let Expr::Control { arms: root_arms, .. } = root_tail.as_ref() else {
        panic!("expected the root case expression");
    };
    assert_eq!(root_arms.len(), 2, "{root_arms:?}");
    let Expr::InterpolatedString { segments, .. } = &root_arms[0].body else {
        panic!("outer arm lost its closure string");
    };
    let [
        StringSegment::Text { text: prefix, .. },
        StringSegment::Expression {
            value: Expr::Lambda { body: outer_body, .. },
            ..
        },
        StringSegment::Text { text: suffix, .. },
    ] = segments.as_slice()
    else {
        panic!("outer string lost its closure suffix boundary: {segments:?}");
    };
    assert_eq!(prefix, "start ");
    assert_eq!(suffix, " after");

    let Expr::Block {
        statements,
        tail: Some(finish_closure),
        ..
    } = outer_body.as_ref()
    else {
        panic!("invalid nested closure recovery consumed the following tail");
    };
    let [
        Statement::Let { .. },
        Statement::Control {
            value: Expr::Control { arms: recovered_arms, .. },
            ..
        },
    ] = statements.as_slice()
    else {
        panic!("recovered case lost its statement boundary: {statements:?}");
    };
    assert!(recovered_arms.is_empty(), "{recovered_arms:?}");

    let Expr::Lambda { body: finish_body, .. } = finish_closure.as_ref() else {
        panic!("closure after the invalid nested closure arm was lost");
    };
    let Expr::Block { tail: Some(finish_tail), .. } = finish_body.as_ref() else {
        panic!("following closure lost its block tail");
    };
    assert!(matches!(finish_tail.as_ref(), Expr::Control { .. }));

    // The reference is silent on recovery through an invalid arm containing a
    // nested closure; retain its comma boundary and the following closure tail.
}

#[test]
fn invalid_nested_closure_without_final_comma_keeps_tail() {
    let source = include_str!("fixtures/malformed-case-arm-invalid-closure-body-without-final-comma.orna");
    let parsed = parse_module(source);

    let malformed_patterns = [
        source.find("false 0").expect("first malformed arm") + 6,
        source.find("false 1").expect("second malformed arm") + 6,
        source.find("false 2").expect("third malformed arm") + 6,
        source.find("false 3").expect("fourth malformed arm") + 6,
    ];
    assert_eq!(parsed.diagnostics.len(), 4, "{:?}", parsed.diagnostics);
    for (diagnostic, start) in parsed.diagnostics.iter().zip(malformed_patterns) {
        assert_eq!(diagnostic.code, "ORNA-PARSE-001");
        assert_eq!(diagnostic.message, "expected `:` after case pattern");
        assert_eq!(diagnostic.span.start, start);
        assert_eq!(diagnostic.span.end, start + 1);
    }

    let Declaration::Function { body, .. } = &parsed.value.items[0].declaration else {
        panic!("expected a function declaration");
    };
    let Expr::Block { tail: Some(root_tail), .. } = body else {
        panic!("expected the root case tail");
    };
    let Expr::Control { arms: root_arms, .. } = root_tail.as_ref() else {
        panic!("expected the root case expression");
    };
    assert_eq!(root_arms.len(), 2, "{root_arms:?}");
    let Expr::InterpolatedString { segments, .. } = &root_arms[0].body else {
        panic!("outer arm lost its closure string");
    };
    let [
        StringSegment::Text { text: prefix, .. },
        StringSegment::Expression {
            value: Expr::Lambda { body: outer_body, .. },
            ..
        },
        StringSegment::Text { text: suffix, .. },
    ] = segments.as_slice()
    else {
        panic!("outer string lost its closure suffix boundary: {segments:?}");
    };
    assert_eq!(prefix, "start ");
    assert_eq!(suffix, " after");

    let Expr::Block {
        statements,
        tail: Some(finish_closure),
        ..
    } = outer_body.as_ref()
    else {
        panic!("nested invalid closure recovery consumed the following tail");
    };
    let [
        Statement::Let { .. },
        Statement::Control {
            value: Expr::Control { arms: recovered_arms, .. },
            ..
        },
    ] = statements.as_slice()
    else {
        panic!("recovered case lost its statement boundary: {statements:?}");
    };
    assert!(recovered_arms.is_empty(), "{recovered_arms:?}");

    let Expr::Lambda { body: finish_body, .. } = finish_closure.as_ref() else {
        panic!("closure after the invalid nested closure case was lost");
    };
    let Expr::Block { tail: Some(finish_tail), .. } = finish_body.as_ref() else {
        panic!("following closure lost its block tail");
    };
    assert!(matches!(finish_tail.as_ref(), Expr::Control { .. }));

    // The reference does not specify an all-invalid case containing a nested
    // closure and ending without a comma; preserve its boundary and tail.
}

#[test]
fn early_invalid_closure_recovery_keeps_following_closure_tail() {
    let source = include_str!("fixtures/malformed-case-arm-early-closure-body-before-tail.orna");
    let parsed = parse_module(source);

    let malformed_patterns = [
        source.find("false 0").expect("first malformed arm") + 6,
        source.find("false 1").expect("second malformed arm") + 6,
        source.find("false 2").expect("third malformed arm") + 6,
        source.find("false 3").expect("fourth malformed arm") + 6,
    ];
    assert_eq!(parsed.diagnostics.len(), 4, "{:?}", parsed.diagnostics);
    for (diagnostic, start) in parsed.diagnostics.iter().zip(malformed_patterns) {
        assert_eq!(diagnostic.code, "ORNA-PARSE-001");
        assert_eq!(diagnostic.message, "expected `:` after case pattern");
        assert_eq!(diagnostic.span.start, start);
        assert_eq!(diagnostic.span.end, start + 1);
    }

    let Declaration::Function { body, .. } = &parsed.value.items[0].declaration else {
        panic!("expected a function declaration");
    };
    let Expr::Block { tail: Some(root_tail), .. } = body else {
        panic!("expected the root case tail");
    };
    let Expr::Control { arms: root_arms, .. } = root_tail.as_ref() else {
        panic!("expected the root case expression");
    };
    assert_eq!(root_arms.len(), 2, "{root_arms:?}");
    let Expr::InterpolatedString { segments, .. } = &root_arms[0].body else {
        panic!("outer arm lost its closure string");
    };
    let [
        StringSegment::Text { text: prefix, .. },
        StringSegment::Expression {
            value: Expr::Lambda { body: outer_body, .. },
            ..
        },
        StringSegment::Text { text: suffix, .. },
    ] = segments.as_slice()
    else {
        panic!("outer string lost its closure suffix boundary: {segments:?}");
    };
    assert_eq!(prefix, "start ");
    assert_eq!(suffix, " after");

    let Expr::Block {
        statements,
        tail: Some(finish_closure),
        ..
    } = outer_body.as_ref()
    else {
        panic!("early nested closure recovery consumed the following tail");
    };
    let [
        Statement::Let { .. },
        Statement::Control {
            value: Expr::Control { arms: recovered_arms, .. },
            ..
        },
    ] = statements.as_slice()
    else {
        panic!("recovered case lost its statement boundary: {statements:?}");
    };
    assert!(recovered_arms.is_empty(), "{recovered_arms:?}");

    let Expr::Lambda { body: finish_body, .. } = finish_closure.as_ref() else {
        panic!("closure after the invalid case was lost");
    };
    let Expr::Block { tail: Some(finish_tail), .. } = finish_body.as_ref() else {
        panic!("following closure lost its block tail");
    };
    assert!(matches!(finish_tail.as_ref(), Expr::Control { .. }));

    // The reference is silent on a nested closure in an early invalid arm;
    // keep later recovery boundaries and the enclosing closure tail intact.
}

#[test]
fn early_invalid_closure_without_final_comma_keeps_tail() {
    let source = include_str!("fixtures/malformed-case-arm-early-closure-without-final-comma.orna");
    let parsed = parse_module(source);

    let malformed_patterns = [
        source.find("false 0").expect("first malformed arm") + 6,
        source.find("false 1").expect("second malformed arm") + 6,
        source.find("false 2").expect("third malformed arm") + 6,
        source.find("false 3").expect("fourth malformed arm") + 6,
    ];
    assert_eq!(parsed.diagnostics.len(), 4, "{:?}", parsed.diagnostics);
    for (diagnostic, start) in parsed.diagnostics.iter().zip(malformed_patterns) {
        assert_eq!(diagnostic.code, "ORNA-PARSE-001");
        assert_eq!(diagnostic.message, "expected `:` after case pattern");
        assert_eq!(diagnostic.span.start, start);
        assert_eq!(diagnostic.span.end, start + 1);
    }

    let Declaration::Function { body, .. } = &parsed.value.items[0].declaration else {
        panic!("expected a function declaration");
    };
    let Expr::Block { tail: Some(root_tail), .. } = body else {
        panic!("expected the root case tail");
    };
    let Expr::Control { arms: root_arms, .. } = root_tail.as_ref() else {
        panic!("expected the root case expression");
    };
    assert_eq!(root_arms.len(), 2, "{root_arms:?}");
    let Expr::InterpolatedString { segments, .. } = &root_arms[0].body else {
        panic!("outer arm lost its closure string");
    };
    let [
        StringSegment::Text { text: prefix, .. },
        StringSegment::Expression {
            value: Expr::Lambda { body: outer_body, .. },
            ..
        },
        StringSegment::Text { text: suffix, .. },
    ] = segments.as_slice()
    else {
        panic!("outer string lost its closure suffix boundary: {segments:?}");
    };
    assert_eq!(prefix, "start ");
    assert_eq!(suffix, " after");

    let Expr::Block {
        statements,
        tail: Some(finish_closure),
        ..
    } = outer_body.as_ref()
    else {
        panic!("early invalid closure recovery consumed the following tail");
    };
    let [
        Statement::Let { .. },
        Statement::Control {
            value: Expr::Control { arms: recovered_arms, .. },
            ..
        },
    ] = statements.as_slice()
    else {
        panic!("recovered case lost its statement boundary: {statements:?}");
    };
    assert!(recovered_arms.is_empty(), "{recovered_arms:?}");

    let Expr::Lambda { body: finish_body, .. } = finish_closure.as_ref() else {
        panic!("closure following early invalid recovery was lost");
    };
    let Expr::Block { tail: Some(finish_tail), .. } = finish_body.as_ref() else {
        panic!("following closure lost its block tail");
    };
    assert!(matches!(finish_tail.as_ref(), Expr::Control { .. }));

    // The reference is silent on a nested closure in an early invalid arm
    // followed by a final invalid arm without a comma; preserve the closure tail.
}

#[test]
fn leading_valid_arm_survives_early_closure_recovery() {
    let source = include_str!("fixtures/malformed-case-arm-leading-valid-before-early-closure.orna");
    let parsed = parse_module(source);

    let malformed_patterns = [
        source.find("false 0").expect("first malformed arm") + 6,
        source.find("false 1").expect("second malformed arm") + 6,
        source.find("false 2").expect("third malformed arm") + 6,
        source.find("false 3").expect("fourth malformed arm") + 6,
    ];
    assert_eq!(parsed.diagnostics.len(), 4, "{:?}", parsed.diagnostics);
    for (diagnostic, start) in parsed.diagnostics.iter().zip(malformed_patterns) {
        assert_eq!(diagnostic.code, "ORNA-PARSE-001");
        assert_eq!(diagnostic.message, "expected `:` after case pattern");
        assert_eq!(diagnostic.span.start, start);
        assert_eq!(diagnostic.span.end, start + 1);
    }

    let Declaration::Function { body, .. } = &parsed.value.items[0].declaration else {
        panic!("expected a function declaration");
    };
    let Expr::Block { tail: Some(root_tail), .. } = body else {
        panic!("expected the root case tail");
    };
    let Expr::Control { arms: root_arms, .. } = root_tail.as_ref() else {
        panic!("expected the root case expression");
    };
    assert_eq!(root_arms.len(), 2, "{root_arms:?}");
    let Expr::InterpolatedString { segments, .. } = &root_arms[0].body else {
        panic!("outer arm lost its closure string");
    };
    let [
        StringSegment::Text { text: prefix, .. },
        StringSegment::Expression {
            value: Expr::Lambda { body: outer_body, .. },
            ..
        },
        StringSegment::Text { text: suffix, .. },
    ] = segments.as_slice()
    else {
        panic!("outer string lost its closure suffix boundary: {segments:?}");
    };
    assert_eq!(prefix, "start ");
    assert_eq!(suffix, " after");

    let Expr::Block {
        statements,
        tail: Some(finish_closure),
        ..
    } = outer_body.as_ref()
    else {
        panic!("early closure recovery consumed the following block tail");
    };
    let [
        Statement::Let { .. },
        Statement::Control {
            value: Expr::Control { arms: recovered_arms, .. },
            ..
        },
    ] = statements.as_slice()
    else {
        panic!("recovered case lost its statement boundary: {statements:?}");
    };
    assert_eq!(recovered_arms.len(), 1, "{recovered_arms:?}");
    assert!(matches!(
        &recovered_arms[0].body,
        Expr::Name { text, .. } if text == "saved"
    ));

    let Expr::Lambda { body: finish_body, .. } = finish_closure.as_ref() else {
        panic!("following closure tail was lost");
    };
    let Expr::Block { tail: Some(finish_tail), .. } = finish_body.as_ref() else {
        panic!("following closure lost its block tail");
    };
    assert!(matches!(finish_tail.as_ref(), Expr::Control { .. }));

    // The reference is silent on a valid prefix before an early nested closure
    // in a malformed run; preserve that prefix and the later closure tail.
}

#[test]
fn valid_closure_prefix_survives_following_invalid_recovery() {
    let source = include_str!("fixtures/malformed-case-arm-valid-closure-prefix-before-invalid-run.orna");
    let parsed = parse_module(source);

    let malformed_patterns = [
        source.find("false 0").expect("first malformed arm") + 6,
        source.find("false 1").expect("second malformed arm") + 6,
        source.find("false 2").expect("third malformed arm") + 6,
        source.find("false 3").expect("fourth malformed arm") + 6,
    ];
    assert_eq!(parsed.diagnostics.len(), 4, "{:?}", parsed.diagnostics);
    for (diagnostic, start) in parsed.diagnostics.iter().zip(malformed_patterns) {
        assert_eq!(diagnostic.code, "ORNA-PARSE-001");
        assert_eq!(diagnostic.message, "expected `:` after case pattern");
        assert_eq!(diagnostic.span.start, start);
        assert_eq!(diagnostic.span.end, start + 1);
    }

    let Declaration::Function { body, .. } = &parsed.value.items[0].declaration else {
        panic!("expected a function declaration");
    };
    let Expr::Block { tail: Some(root_tail), .. } = body else {
        panic!("expected the root case tail");
    };
    let Expr::Control { arms: root_arms, .. } = root_tail.as_ref() else {
        panic!("expected the root case expression");
    };
    assert_eq!(root_arms.len(), 2, "{root_arms:?}");
    let Expr::InterpolatedString { segments, .. } = &root_arms[0].body else {
        panic!("outer arm lost its closure string");
    };
    let [
        StringSegment::Text { text: prefix, .. },
        StringSegment::Expression {
            value: Expr::Lambda { body: outer_body, .. },
            ..
        },
        StringSegment::Text { text: suffix, .. },
    ] = segments.as_slice()
    else {
        panic!("outer string lost its closure suffix boundary: {segments:?}");
    };
    assert_eq!(prefix, "start ");
    assert_eq!(suffix, " after");

    let Expr::Block {
        statements,
        tail: Some(finish_closure),
        ..
    } = outer_body.as_ref()
    else {
        panic!("invalid recovery consumed the enclosing closure tail");
    };
    let [
        Statement::Let { .. },
        Statement::Control {
            value: Expr::Control { arms: recovered_arms, .. },
            ..
        },
    ] = statements.as_slice()
    else {
        panic!("recovered case lost its statement boundary: {statements:?}");
    };
    assert_eq!(recovered_arms.len(), 2, "{recovered_arms:?}");
    assert!(matches!(
        &recovered_arms[0].body,
        Expr::Name { text, .. } if text == "saved"
    ));
    let Expr::Lambda { body: prefix_closure_body, .. } = &recovered_arms[1].body else {
        panic!("valid closure prefix was lost");
    };
    let Expr::Block { tail: Some(prefix_tail), .. } = prefix_closure_body.as_ref() else {
        panic!("valid closure prefix lost its own tail");
    };
    assert!(matches!(prefix_tail.as_ref(), Expr::Control { .. }));

    let Expr::Lambda { body: finish_body, .. } = finish_closure.as_ref() else {
        panic!("following enclosing closure was lost");
    };
    let Expr::Block { tail: Some(finish_tail), .. } = finish_body.as_ref() else {
        panic!("following closure lost its block tail");
    };
    assert!(matches!(finish_tail.as_ref(), Expr::Control { .. }));

    // The reference is silent on a valid closure arm immediately before an
    // invalid run; preserve its nested tail and the enclosing closure tail.
}

#[test]
fn interpolated_closure_prefix_survives_invalid_recovery() {
    let source = include_str!("fixtures/malformed-case-arm-interpolated-closure-prefix-before-invalid-run.orna");
    let parsed = parse_module(source);

    let malformed_patterns = [
        source.find("false 0").expect("first malformed arm") + 6,
        source.find("false 1").expect("second malformed arm") + 6,
        source.find("false 2").expect("third malformed arm") + 6,
        source.find("false 3").expect("fourth malformed arm") + 6,
    ];
    assert_eq!(parsed.diagnostics.len(), 4, "{:?}", parsed.diagnostics);
    for (diagnostic, start) in parsed.diagnostics.iter().zip(malformed_patterns) {
        assert_eq!(diagnostic.code, "ORNA-PARSE-001");
        assert_eq!(diagnostic.message, "expected `:` after case pattern");
        assert_eq!(diagnostic.span.start, start);
        assert_eq!(diagnostic.span.end, start + 1);
    }

    let Declaration::Function { body, .. } = &parsed.value.items[0].declaration else {
        panic!("expected a function declaration");
    };
    let Expr::Block { tail: Some(root_tail), .. } = body else {
        panic!("expected the root case tail");
    };
    let Expr::Control { arms: root_arms, .. } = root_tail.as_ref() else {
        panic!("expected the root case expression");
    };
    assert_eq!(root_arms.len(), 2, "{root_arms:?}");
    let Expr::InterpolatedString { segments, .. } = &root_arms[0].body else {
        panic!("outer arm lost its closure string");
    };
    let StringSegment::Expression {
        value: Expr::Lambda { body: outer_body, .. },
        ..
    } = &segments[1]
    else {
        panic!("outer interpolation lost its closure");
    };

    let Expr::Block {
        statements,
        tail: Some(finish_closure),
        ..
    } = outer_body.as_ref()
    else {
        panic!("invalid recovery consumed the enclosing closure tail");
    };
    let [
        Statement::Let { .. },
        Statement::Control {
            value: Expr::Control { arms: recovered_arms, .. },
            ..
        },
    ] = statements.as_slice()
    else {
        panic!("recovered case lost its statement boundary: {statements:?}");
    };
    assert_eq!(recovered_arms.len(), 2, "{recovered_arms:?}");
    assert!(matches!(
        &recovered_arms[0].body,
        Expr::Name { text, .. } if text == "saved"
    ));
    let Expr::InterpolatedString { segments, .. } = &recovered_arms[1].body else {
        panic!("valid interpolated closure prefix was lost");
    };
    let [
        StringSegment::Text { text: prefix, .. },
        StringSegment::Expression {
            value: Expr::Lambda { body: prefix_body, .. },
            ..
        },
        StringSegment::Text { text: suffix, .. },
    ] = segments.as_slice()
    else {
        panic!("closure interpolation prefix lost its suffix boundary: {segments:?}");
    };
    assert_eq!(prefix, "prefix ");
    assert_eq!(suffix, " suffix");
    let Expr::Block { tail: Some(prefix_tail), .. } = prefix_body.as_ref() else {
        panic!("valid closure prefix lost its own block tail");
    };
    assert!(matches!(prefix_tail.as_ref(), Expr::Control { .. }));

    let Expr::Lambda { body: finish_body, .. } = finish_closure.as_ref() else {
        panic!("following enclosing closure was lost");
    };
    let Expr::Block { tail: Some(finish_tail), .. } = finish_body.as_ref() else {
        panic!("following closure lost its block tail");
    };
    assert!(matches!(finish_tail.as_ref(), Expr::Control { .. }));

    // The reference is silent on a valid interpolated closure prefix before a
    // malformed run; preserve both its own tail and the enclosing closure tail.
}

#[test]
fn two_closure_prefixes_survive_invalid_recovery() {
    let source = include_str!("fixtures/malformed-case-arm-two-closure-prefixes-before-invalid-run.orna");
    let parsed = parse_module(source);

    let malformed_patterns = [
        source.find("false 0").expect("first malformed arm") + 6,
        source.find("true 1").expect("second malformed arm") + 5,
        source.find("false 2").expect("third malformed arm") + 6,
        source.find("true 3").expect("fourth malformed arm") + 5,
    ];
    assert_eq!(parsed.diagnostics.len(), 4, "{:?}", parsed.diagnostics);
    for (diagnostic, start) in parsed.diagnostics.iter().zip(malformed_patterns) {
        assert_eq!(diagnostic.code, "ORNA-PARSE-001");
        assert_eq!(diagnostic.message, "expected `:` after case pattern");
        assert_eq!(diagnostic.span.start, start);
        assert_eq!(diagnostic.span.end, start + 1);
    }

    let Declaration::Function { body, .. } = &parsed.value.items[0].declaration else {
        panic!("expected a function declaration");
    };
    let Expr::Block { tail: Some(root_tail), .. } = body else {
        panic!("expected the root case tail");
    };
    let Expr::Control { arms: root_arms, .. } = root_tail.as_ref() else {
        panic!("expected the root case expression");
    };
    assert_eq!(root_arms.len(), 2, "{root_arms:?}");
    let Expr::InterpolatedString { segments, .. } = &root_arms[0].body else {
        panic!("outer arm lost its closure string");
    };
    let StringSegment::Expression {
        value: Expr::Lambda { body: outer_body, .. },
        ..
    } = &segments[1]
    else {
        panic!("outer interpolation lost its closure");
    };

    let Expr::Block {
        statements,
        tail: Some(finish_closure),
        ..
    } = outer_body.as_ref()
    else {
        panic!("invalid recovery consumed the enclosing closure tail");
    };
    let [
        Statement::Let { .. },
        Statement::Control {
            value: Expr::Control { arms: recovered_arms, .. },
            ..
        },
    ] = statements.as_slice()
    else {
        panic!("recovered case lost its statement boundary: {statements:?}");
    };
    assert_eq!(recovered_arms.len(), 2, "{recovered_arms:?}");

    let Expr::Lambda { body: direct_body, .. } = &recovered_arms[0].body else {
        panic!("first valid closure prefix was lost");
    };
    let Expr::Block {
        tail: Some(direct_tail),
        ..
    } = direct_body.as_ref()
    else {
        panic!("first closure prefix lost its block tail");
    };
    let Expr::Control { arms: direct_arms, .. } = direct_tail.as_ref() else {
        panic!("first closure prefix lost its case tail");
    };
    assert_eq!(direct_arms.len(), 2, "{direct_arms:?}");

    let Expr::InterpolatedString { segments, .. } = &recovered_arms[1].body else {
        panic!("second interpolated closure prefix was lost");
    };
    let [
        StringSegment::Text { text: prefix, .. },
        StringSegment::Expression {
            value: Expr::Lambda { body: nested_body, .. },
            ..
        },
        StringSegment::Text { text: suffix, .. },
    ] = segments.as_slice()
    else {
        panic!("second closure prefix lost its interpolation boundaries: {segments:?}");
    };
    assert_eq!(prefix, "middle ");
    assert_eq!(suffix, " suffix");
    let Expr::Block { tail: Some(nested_tail), .. } = nested_body.as_ref() else {
        panic!("second closure prefix lost its block tail");
    };
    let Expr::Control { arms: nested_arms, .. } = nested_tail.as_ref() else {
        panic!("second closure prefix lost its case tail");
    };
    assert_eq!(nested_arms.len(), 2, "{nested_arms:?}");

    let Expr::Lambda { body: finish_body, .. } = finish_closure.as_ref() else {
        panic!("following enclosing closure was lost");
    };
    let Expr::Block { tail: Some(finish_tail), .. } = finish_body.as_ref() else {
        panic!("following closure lost its block tail");
    };
    assert!(matches!(finish_tail.as_ref(), Expr::Control { .. }));

    // The reference is silent on consecutive valid closure prefixes before a
    // malformed run; preserve both nested case tails and the enclosing tail.
}

#[test]
fn three_closure_prefixes_survive_invalid_recovery() {
    let source = include_str!("fixtures/malformed-case-arm-three-closure-prefixes-before-invalid-run.orna");
    let parsed = parse_module(source);

    let malformed_patterns = [
        source.find("false 0").expect("first malformed arm") + 6,
        source.find("true 1").expect("second malformed arm") + 5,
        source.find("false 2").expect("third malformed arm") + 6,
        source.find("true 3").expect("fourth malformed arm") + 5,
    ];
    assert_eq!(parsed.diagnostics.len(), 4, "{:?}", parsed.diagnostics);
    for (diagnostic, start) in parsed.diagnostics.iter().zip(malformed_patterns) {
        assert_eq!(diagnostic.code, "ORNA-PARSE-001");
        assert_eq!(diagnostic.message, "expected `:` after case pattern");
        assert_eq!(diagnostic.span.start, start);
        assert_eq!(diagnostic.span.end, start + 1);
    }

    let Declaration::Function { body, .. } = &parsed.value.items[0].declaration else {
        panic!("expected a function declaration");
    };
    let Expr::Block { tail: Some(root_tail), .. } = body else {
        panic!("expected the root case tail");
    };
    let Expr::Control { arms: root_arms, .. } = root_tail.as_ref() else {
        panic!("expected the root case expression");
    };
    assert_eq!(root_arms.len(), 2, "{root_arms:?}");
    let Expr::InterpolatedString { segments, .. } = &root_arms[0].body else {
        panic!("outer arm lost its closure string");
    };
    let StringSegment::Expression {
        value: Expr::Lambda { body: outer_body, .. },
        ..
    } = &segments[1]
    else {
        panic!("outer interpolation lost its closure");
    };

    let Expr::Block {
        statements,
        tail: Some(finish_closure),
        ..
    } = outer_body.as_ref()
    else {
        panic!("invalid recovery consumed the enclosing closure tail");
    };
    let [
        Statement::Let { .. },
        Statement::Control {
            value: Expr::Control { arms: recovered_arms, .. },
            ..
        },
    ] = statements.as_slice()
    else {
        panic!("recovered case lost its statement boundary: {statements:?}");
    };
    assert_eq!(recovered_arms.len(), 3, "{recovered_arms:?}");

    let Expr::Lambda { body: first_body, .. } = &recovered_arms[0].body else {
        panic!("first closure prefix was lost");
    };
    let Expr::Block { tail: Some(first_tail), .. } = first_body.as_ref() else {
        panic!("first closure prefix lost its block tail");
    };
    let Expr::Control { arms: first_arms, .. } = first_tail.as_ref() else {
        panic!("first closure prefix lost its case tail");
    };
    assert_eq!(first_arms.len(), 2, "{first_arms:?}");

    let Expr::InterpolatedString { segments, .. } = &recovered_arms[1].body else {
        panic!("interpolated closure prefix was lost");
    };
    let [
        StringSegment::Text { text: prefix, .. },
        StringSegment::Expression {
            value: Expr::Lambda { body: second_body, .. },
            ..
        },
        StringSegment::Text { text: suffix, .. },
    ] = segments.as_slice()
    else {
        panic!("second closure prefix lost its interpolation boundaries: {segments:?}");
    };
    assert_eq!(prefix, "interpolated ");
    assert_eq!(suffix, " suffix");
    let Expr::Block { tail: Some(second_tail), .. } = second_body.as_ref() else {
        panic!("second closure prefix lost its block tail");
    };
    let Expr::Control { arms: second_arms, .. } = second_tail.as_ref() else {
        panic!("second closure prefix lost its case tail");
    };
    assert_eq!(second_arms.len(), 2, "{second_arms:?}");

    let Expr::Lambda { body: third_body, .. } = &recovered_arms[2].body else {
        panic!("third closure prefix was lost");
    };
    let Expr::Block { tail: Some(third_tail), .. } = third_body.as_ref() else {
        panic!("third closure prefix lost its block tail");
    };
    let Expr::Control { arms: third_arms, .. } = third_tail.as_ref() else {
        panic!("third closure prefix lost its case tail");
    };
    assert_eq!(third_arms.len(), 2, "{third_arms:?}");

    let Expr::Lambda { body: finish_body, .. } = finish_closure.as_ref() else {
        panic!("following enclosing closure was lost");
    };
    let Expr::Block { tail: Some(finish_tail), .. } = finish_body.as_ref() else {
        panic!("following closure lost its block tail");
    };
    assert!(matches!(finish_tail.as_ref(), Expr::Control { .. }));

    // The reference is silent on three consecutive closure prefixes before a
    // malformed run; preserve their case tails and the enclosing closure tail.
}

#[test]
fn four_closure_prefixes_preserve_nested_tail_during_recovery() {
    let source = include_str!("fixtures/malformed-case-arm-four-closure-prefixes-with-nested-tail.orna");
    let parsed = parse_module(source);

    let malformed_patterns = [
        source.find("false 0").expect("first malformed arm") + 6,
        source.find("true 1").expect("second malformed arm") + 5,
        source.find("false 2").expect("third malformed arm") + 6,
        source.find("true 3").expect("fourth malformed arm") + 5,
    ];
    assert_eq!(parsed.diagnostics.len(), 4, "{:?}", parsed.diagnostics);
    for (diagnostic, start) in parsed.diagnostics.iter().zip(malformed_patterns) {
        assert_eq!(diagnostic.code, "ORNA-PARSE-001");
        assert_eq!(diagnostic.message, "expected `:` after case pattern");
        assert_eq!(diagnostic.span.start, start);
        assert_eq!(diagnostic.span.end, start + 1);
    }

    let Declaration::Function { body, .. } = &parsed.value.items[0].declaration else {
        panic!("expected a function declaration");
    };
    let Expr::Block { tail: Some(root_tail), .. } = body else {
        panic!("expected the root case tail");
    };
    let Expr::Control { arms: root_arms, .. } = root_tail.as_ref() else {
        panic!("expected the root case expression");
    };
    assert_eq!(root_arms.len(), 2, "{root_arms:?}");
    let Expr::InterpolatedString { segments, .. } = &root_arms[0].body else {
        panic!("outer arm lost its closure string");
    };
    let StringSegment::Expression {
        value: Expr::Lambda { body: outer_body, .. },
        ..
    } = &segments[1]
    else {
        panic!("outer interpolation lost its closure");
    };

    let Expr::Block {
        statements,
        tail: Some(finish_closure),
        ..
    } = outer_body.as_ref()
    else {
        panic!("invalid recovery consumed the enclosing closure tail");
    };
    let [
        Statement::Let { .. },
        Statement::Control {
            value: Expr::Control { arms: recovered_arms, .. },
            ..
        },
    ] = statements.as_slice()
    else {
        panic!("recovered case lost its statement boundary: {statements:?}");
    };
    assert_eq!(recovered_arms.len(), 4, "{recovered_arms:?}");

    let Expr::Lambda { body: first_body, .. } = &recovered_arms[0].body else {
        panic!("first closure prefix was lost");
    };
    let Expr::Block { tail: Some(first_tail), .. } = first_body.as_ref() else {
        panic!("first closure prefix lost its block tail");
    };
    assert!(matches!(first_tail.as_ref(), Expr::Control { .. }));

    let Expr::InterpolatedString { segments, .. } = &recovered_arms[1].body else {
        panic!("second interpolated closure prefix was lost");
    };
    let [
        StringSegment::Text { text: middle_prefix, .. },
        StringSegment::Expression {
            value: Expr::Lambda { body: second_body, .. },
            ..
        },
        StringSegment::Text { text: middle_suffix, .. },
    ] = segments.as_slice()
    else {
        panic!("second closure prefix lost its boundaries: {segments:?}");
    };
    assert_eq!(middle_prefix, "middle ");
    assert_eq!(middle_suffix, " suffix");
    let Expr::Block { tail: Some(second_tail), .. } = second_body.as_ref() else {
        panic!("second closure prefix lost its block tail");
    };
    assert!(matches!(second_tail.as_ref(), Expr::Control { .. }));

    let Expr::Lambda { body: third_body, .. } = &recovered_arms[2].body else {
        panic!("third closure prefix was lost");
    };
    let Expr::Block { tail: Some(third_tail), .. } = third_body.as_ref() else {
        panic!("third closure prefix lost its block tail");
    };
    assert!(matches!(third_tail.as_ref(), Expr::Control { .. }));

    let Expr::InterpolatedString { segments, .. } = &recovered_arms[3].body else {
        panic!("fourth interpolated closure prefix was lost");
    };
    let [
        StringSegment::Text { text: deep_prefix, .. },
        StringSegment::Expression {
            value: Expr::Lambda { body: fourth_body, .. },
            ..
        },
        StringSegment::Text { text: deep_suffix, .. },
    ] = segments.as_slice()
    else {
        panic!("fourth closure prefix lost its boundaries: {segments:?}");
    };
    assert_eq!(deep_prefix, "deep ");
    assert_eq!(deep_suffix, " end");
    let Expr::Block {
        statements: fourth_statements,
        tail: Some(fourth_tail),
        ..
    } = fourth_body.as_ref()
    else {
        panic!("fourth closure prefix lost its nested block tail");
    };
    let [Statement::Let {
        value: Expr::Lambda { body: nested_body, .. },
        ..
    }] = fourth_statements.as_slice()
    else {
        panic!("fourth closure lost its nested closure prefix: {fourth_statements:?}");
    };
    let Expr::Block { tail: Some(nested_tail), .. } = nested_body.as_ref() else {
        panic!("nested closure prefix lost its block tail");
    };
    assert!(matches!(nested_tail.as_ref(), Expr::Control { .. }));
    assert!(matches!(fourth_tail.as_ref(), Expr::Control { .. }));

    let Expr::Lambda { body: finish_body, .. } = finish_closure.as_ref() else {
        panic!("following enclosing closure was lost");
    };
    let Expr::Block { tail: Some(finish_tail), .. } = finish_body.as_ref() else {
        panic!("following closure lost its block tail");
    };
    assert!(matches!(finish_tail.as_ref(), Expr::Control { .. }));

    // The reference is silent on four consecutive closure prefixes with a
    // nested closure tail; preserve each prefix and the following closure tail.
}

#[test]
fn nested_repeated_closure_prefixes_survive_invalid_recovery() {
    let source = include_str!("fixtures/malformed-nested-case-four-closure-prefixes-before-invalid-run.orna");
    let parsed = parse_module(source);

    let malformed_patterns = [
        source.find("false 0").expect("first malformed arm") + 6,
        source.find("true 1").expect("second malformed arm") + 5,
        source.find("false 2").expect("third malformed arm") + 6,
        source.find("true 3").expect("fourth malformed arm") + 5,
    ];
    assert_eq!(parsed.diagnostics.len(), 4, "{:?}", parsed.diagnostics);
    for (diagnostic, start) in parsed.diagnostics.iter().zip(malformed_patterns) {
        assert_eq!(diagnostic.code, "ORNA-PARSE-001");
        assert_eq!(diagnostic.message, "expected `:` after case pattern");
        assert_eq!(diagnostic.span.start, start);
        assert_eq!(diagnostic.span.end, start + 1);
    }

    let Declaration::Function { body, .. } = &parsed.value.items[0].declaration else {
        panic!("expected a function declaration");
    };
    let Expr::Block { tail: Some(root_tail), .. } = body else {
        panic!("expected the root case tail");
    };
    let Expr::Control { arms: root_arms, .. } = root_tail.as_ref() else {
        panic!("expected the root case expression");
    };
    assert_eq!(root_arms.len(), 2, "{root_arms:?}");
    let Expr::InterpolatedString { segments, .. } = &root_arms[0].body else {
        panic!("outer arm lost its closure string");
    };
    let StringSegment::Expression {
        value: Expr::Lambda { body: root_closure_body, .. },
        ..
    } = &segments[1]
    else {
        panic!("outer interpolation lost its closure");
    };

    let Expr::Block {
        statements: root_statements,
        tail: Some(finish_closure),
        ..
    } = root_closure_body.as_ref()
    else {
        panic!("nested recovery consumed the root closure tail");
    };
    let [
        Statement::Let { .. },
        Statement::Control {
            value: Expr::Control { arms: outer_arms, .. },
            ..
        },
    ] = root_statements.as_slice()
    else {
        panic!("outer case lost its statement boundary: {root_statements:?}");
    };
    assert_eq!(outer_arms.len(), 2, "{outer_arms:?}");

    let Expr::Lambda { body: inner_body, .. } = &outer_arms[0].body else {
        panic!("nested closure prefix was lost");
    };
    let Expr::Block {
        statements: inner_statements,
        tail: Some(after_closure),
        ..
    } = inner_body.as_ref()
    else {
        panic!("inner recovery consumed the following closure tail");
    };
    let [
        Statement::Let { .. },
        Statement::Control {
            value: Expr::Control { arms: recovered_arms, .. },
            ..
        },
    ] = inner_statements.as_slice()
    else {
        panic!("inner case lost its statement boundary: {inner_statements:?}");
    };
    assert_eq!(recovered_arms.len(), 4, "{recovered_arms:?}");

    let Expr::Lambda { body: first_body, .. } = &recovered_arms[0].body else {
        panic!("first nested closure prefix was lost");
    };
    let Expr::Block { tail: Some(first_tail), .. } = first_body.as_ref() else {
        panic!("first nested closure prefix lost its tail");
    };
    assert!(matches!(first_tail.as_ref(), Expr::Control { .. }));

    let Expr::InterpolatedString { segments, .. } = &recovered_arms[1].body else {
        panic!("second nested interpolated closure prefix was lost");
    };
    let [
        StringSegment::Text { text: prefix, .. },
        StringSegment::Expression {
            value: Expr::Lambda { body: second_body, .. },
            ..
        },
        StringSegment::Text { text: suffix, .. },
    ] = segments.as_slice()
    else {
        panic!("second nested closure prefix lost its boundaries: {segments:?}");
    };
    assert_eq!(prefix, "prefix ");
    assert_eq!(suffix, " suffix");
    let Expr::Block { tail: Some(second_tail), .. } = second_body.as_ref() else {
        panic!("second nested closure prefix lost its tail");
    };
    assert!(matches!(second_tail.as_ref(), Expr::Control { .. }));

    let Expr::Lambda { body: third_body, .. } = &recovered_arms[2].body else {
        panic!("third nested closure prefix was lost");
    };
    let Expr::Block { tail: Some(third_tail), .. } = third_body.as_ref() else {
        panic!("third nested closure prefix lost its tail");
    };
    assert!(matches!(third_tail.as_ref(), Expr::Control { .. }));

    let Expr::InterpolatedString { segments, .. } = &recovered_arms[3].body else {
        panic!("fourth nested interpolated closure prefix was lost");
    };
    let [
        StringSegment::Text { text: deep_prefix, .. },
        StringSegment::Expression {
            value: Expr::Lambda { body: fourth_body, .. },
            ..
        },
        StringSegment::Text { text: deep_suffix, .. },
    ] = segments.as_slice()
    else {
        panic!("fourth nested closure prefix lost its boundaries: {segments:?}");
    };
    assert_eq!(deep_prefix, "deep ");
    assert_eq!(deep_suffix, " end");
    let Expr::Block {
        statements: fourth_statements,
        tail: Some(fourth_tail),
        ..
    } = fourth_body.as_ref()
    else {
        panic!("fourth nested closure prefix lost its block tail");
    };
    let [Statement::Let {
        value: Expr::Lambda { body: leaf_body, .. },
        ..
    }] = fourth_statements.as_slice()
    else {
        panic!("fourth prefix lost its nested closure: {fourth_statements:?}");
    };
    let Expr::Block { tail: Some(leaf_tail), .. } = leaf_body.as_ref() else {
        panic!("leaf closure lost its nested case tail");
    };
    assert!(matches!(leaf_tail.as_ref(), Expr::Control { .. }));
    assert!(matches!(fourth_tail.as_ref(), Expr::Control { .. }));

    let Expr::Lambda { body: after_body, .. } = after_closure.as_ref() else {
        panic!("closure following the inner case was lost");
    };
    let Expr::Block { tail: Some(after_tail), .. } = after_body.as_ref() else {
        panic!("closure following the inner case lost its tail");
    };
    assert!(matches!(after_tail.as_ref(), Expr::Control { .. }));

    let Expr::Lambda { body: outer_finish_body, .. } = &outer_arms[1].body else {
        panic!("following outer case closure was lost");
    };
    let Expr::Block { tail: Some(outer_finish_tail), .. } = outer_finish_body.as_ref() else {
        panic!("following outer case closure lost its tail");
    };
    assert!(matches!(outer_finish_tail.as_ref(), Expr::Control { .. }));

    let Expr::Lambda { body: finish_body, .. } = finish_closure.as_ref() else {
        panic!("following root closure was lost");
    };
    let Expr::Block { tail: Some(finish_tail), .. } = finish_body.as_ref() else {
        panic!("following root closure lost its tail");
    };
    assert!(matches!(finish_tail.as_ref(), Expr::Control { .. }));

    // The reference is silent on repeated closure prefixes inside a nested
    // recovering case; preserve nested prefixes and every following tail.
}

#[test]
fn repeated_nested_closure_prefix_recoveries_preserve_following_tails() {
    let source = include_str!("fixtures/malformed-case-two-nested-repeated-closure-prefix-recoveries.orna");
    let parsed = parse_module(source);

    let malformed_patterns = [
        ("false 0", 6),
        ("true 1", 5),
        ("false 2", 6),
        ("true 3", 5),
        ("false 4", 6),
        ("true 5", 5),
        ("false 6", 6),
        ("true 7", 5),
    ];
    assert_eq!(parsed.diagnostics.len(), malformed_patterns.len(), "{:?}", parsed.diagnostics);
    for (diagnostic, (pattern, offset)) in parsed.diagnostics.iter().zip(malformed_patterns) {
        let start = source.find(pattern).expect("malformed arm") + offset;
        assert_eq!(diagnostic.code, "ORNA-PARSE-001");
        assert_eq!(diagnostic.message, "expected `:` after case pattern");
        assert_eq!(diagnostic.span.start, start);
        assert_eq!(diagnostic.span.end, start + 1);
    }

    let Declaration::Function { body, .. } = &parsed.value.items[0].declaration else {
        panic!("expected a function declaration");
    };
    let Expr::Block { tail: Some(root_tail), .. } = body else {
        panic!("expected the root case tail");
    };
    let Expr::Control { arms: root_arms, .. } = root_tail.as_ref() else {
        panic!("expected the root case expression");
    };
    assert_eq!(root_arms.len(), 2, "{root_arms:?}");
    let Expr::InterpolatedString { segments, .. } = &root_arms[0].body else {
        panic!("outer arm lost its closure string");
    };
    let StringSegment::Expression {
        value: Expr::Lambda { body: root_body, .. },
        ..
    } = &segments[1]
    else {
        panic!("outer interpolation lost its closure");
    };

    let Expr::Block {
        statements: root_statements,
        tail: Some(finish_closure),
        ..
    } = root_body.as_ref()
    else {
        panic!("nested recoveries consumed the root closure tail");
    };
    let [
        Statement::Let { .. },
        Statement::Control {
            value: Expr::Control { arms: outer_arms, .. },
            ..
        },
    ] = root_statements.as_slice()
    else {
        panic!("outer case lost its statement boundary: {root_statements:?}");
    };
    assert_eq!(outer_arms.len(), 2, "{outer_arms:?}");
    let Expr::Lambda { body: nested_body, .. } = &outer_arms[0].body else {
        panic!("nested recovery closure prefix was lost");
    };

    let Expr::Block {
        statements: nested_statements,
        tail: Some(after_closure),
        ..
    } = nested_body.as_ref()
    else {
        panic!("repeated recoveries consumed the following nested closure tail");
    };
    let [
        Statement::Control {
            value: Expr::Control { arms: left_arms, .. },
            ..
        },
        Statement::Control {
            value: Expr::Control { arms: right_arms, .. },
            ..
        },
    ] = nested_statements.as_slice()
    else {
        panic!("nested recovery cases were not both preserved: {nested_statements:?}");
    };
    assert_eq!(left_arms.len(), 2, "{left_arms:?}");
    assert_eq!(right_arms.len(), 3, "{right_arms:?}");
    assert!(matches!(&left_arms[0].body, Expr::Lambda { .. }));
    assert!(matches!(&left_arms[1].body, Expr::InterpolatedString { .. }));
    assert!(matches!(&right_arms[0].body, Expr::Lambda { .. }));
    assert!(matches!(&right_arms[1].body, Expr::InterpolatedString { .. }));
    assert!(matches!(&right_arms[2].body, Expr::Lambda { .. }));

    let Expr::Lambda { body: after_body, .. } = after_closure.as_ref() else {
        panic!("closure after both recovering cases was lost");
    };
    let Expr::Block { tail: Some(after_tail), .. } = after_body.as_ref() else {
        panic!("closure after both recovering cases lost its tail");
    };
    assert!(matches!(after_tail.as_ref(), Expr::Control { .. }));

    let Expr::Lambda { body: outer_finish_body, .. } = &outer_arms[1].body else {
        panic!("following outer case closure was lost");
    };
    let Expr::Block { tail: Some(outer_finish_tail), .. } = outer_finish_body.as_ref() else {
        panic!("following outer case closure lost its tail");
    };
    assert!(matches!(outer_finish_tail.as_ref(), Expr::Control { .. }));

    let Expr::Lambda { body: finish_body, .. } = finish_closure.as_ref() else {
        panic!("following root closure was lost");
    };
    let Expr::Block { tail: Some(finish_tail), .. } = finish_body.as_ref() else {
        panic!("following root closure lost its tail");
    };
    assert!(matches!(finish_tail.as_ref(), Expr::Control { .. }));

    // The reference is silent on repeated nested recoveries in one closure;
    // preserve both closure-prefix lists and all subsequent closure tails.
}

#[test]
fn nested_closure_prefix_recovery_preserves_valid_closure_suffix() {
    let source = include_str!("fixtures/malformed-case-closure-prefix-recovery-before-valid-closure-suffix.orna");
    let parsed = parse_module(source);

    let malformed_patterns = [
        ("false 0", 6),
        ("true 1", 5),
        ("false 2", 6),
        ("true 3", 5),
    ];
    assert_eq!(parsed.diagnostics.len(), malformed_patterns.len(), "{:?}", parsed.diagnostics);
    for (diagnostic, (pattern, offset)) in parsed.diagnostics.iter().zip(malformed_patterns) {
        let start = source.find(pattern).expect("malformed arm") + offset;
        assert_eq!(diagnostic.code, "ORNA-PARSE-001");
        assert_eq!(diagnostic.message, "expected `:` after case pattern");
        assert_eq!(diagnostic.span.start, start);
        assert_eq!(diagnostic.span.end, start + 1);
    }

    let Declaration::Function { body, .. } = &parsed.value.items[0].declaration else {
        panic!("expected a function declaration");
    };
    let Expr::Block { tail: Some(root_tail), .. } = body else {
        panic!("expected the root case tail");
    };
    let Expr::Control { arms: root_arms, .. } = root_tail.as_ref() else {
        panic!("expected the root case expression");
    };
    assert_eq!(root_arms.len(), 2, "{root_arms:?}");
    let Expr::InterpolatedString { segments, .. } = &root_arms[0].body else {
        panic!("outer arm lost its closure string");
    };
    let StringSegment::Expression {
        value: Expr::Lambda { body: root_body, .. },
        ..
    } = &segments[1]
    else {
        panic!("outer interpolation lost its closure");
    };

    let Expr::Block {
        statements: root_statements,
        tail: Some(finish_closure),
        ..
    } = root_body.as_ref()
    else {
        panic!("nested recovery consumed the root closure tail");
    };
    let [
        Statement::Let { .. },
        Statement::Control {
            value: Expr::Control { arms: outer_arms, .. },
            ..
        },
    ] = root_statements.as_slice()
    else {
        panic!("outer case lost its statement boundary: {root_statements:?}");
    };
    assert_eq!(outer_arms.len(), 2, "{outer_arms:?}");
    let Expr::Lambda { body: nested_body, .. } = &outer_arms[0].body else {
        panic!("nested closure prefix was lost");
    };

    let Expr::Block {
        statements: nested_statements,
        tail: Some(after_closure),
        ..
    } = nested_body.as_ref()
    else {
        panic!("recovery consumed the closure tail following its case");
    };
    let [Statement::Control {
        value: Expr::Control { arms: recovered_arms, .. },
        ..
    }] = nested_statements.as_slice()
    else {
        panic!("nested recovery case was lost: {nested_statements:?}");
    };
    assert_eq!(recovered_arms.len(), 3, "{recovered_arms:?}");
    assert!(matches!(&recovered_arms[0].body, Expr::Lambda { .. }));
    assert!(matches!(&recovered_arms[1].body, Expr::InterpolatedString { .. }));
    assert!(matches!(&recovered_arms[2].body, Expr::Lambda { .. }));

    let Expr::Lambda { body: recovered_body, .. } = &recovered_arms[2].body else {
        unreachable!();
    };
    let Expr::Block { tail: Some(recovered_tail), .. } = recovered_body.as_ref() else {
        panic!("valid closure arm after recovery lost its block tail");
    };
    assert!(matches!(recovered_tail.as_ref(), Expr::Control { .. }));

    let Expr::Lambda { body: after_body, .. } = after_closure.as_ref() else {
        panic!("closure after the nested case was lost");
    };
    let Expr::Block { tail: Some(after_tail), .. } = after_body.as_ref() else {
        panic!("closure after the nested case lost its tail");
    };
    assert!(matches!(after_tail.as_ref(), Expr::Control { .. }));

    let Expr::Lambda { body: outer_finish_body, .. } = &outer_arms[1].body else {
        panic!("following outer case closure was lost");
    };
    let Expr::Block { tail: Some(outer_finish_tail), .. } = outer_finish_body.as_ref() else {
        panic!("following outer case closure lost its tail");
    };
    assert!(matches!(outer_finish_tail.as_ref(), Expr::Control { .. }));

    let Expr::Lambda { body: finish_body, .. } = finish_closure.as_ref() else {
        panic!("following root closure was lost");
    };
    let Expr::Block { tail: Some(finish_tail), .. } = finish_body.as_ref() else {
        panic!("following root closure lost its tail");
    };
    assert!(matches!(finish_tail.as_ref(), Expr::Control { .. }));

    // The reference is silent on a valid closure arm after a malformed run;
    // keep that suffix arm as well as all enclosing closure tails.
}

#[test]
fn nested_closure_recovery_preserves_interpolated_closure_suffix() {
    let source = include_str!("fixtures/malformed-nested-closure-recovery-before-interpolated-closure-suffix.orna");
    let parsed = parse_module(source);

    let malformed_patterns = [
        ("false 0", 6),
        ("true 1", 5),
        ("false 2", 6),
        ("true 3", 5),
    ];
    assert_eq!(parsed.diagnostics.len(), malformed_patterns.len(), "{:?}", parsed.diagnostics);
    for (diagnostic, (pattern, offset)) in parsed.diagnostics.iter().zip(malformed_patterns) {
        let start = source.find(pattern).expect("malformed arm") + offset;
        assert_eq!(diagnostic.code, "ORNA-PARSE-001");
        assert_eq!(diagnostic.message, "expected `:` after case pattern");
        assert_eq!(diagnostic.span.start, start);
        assert_eq!(diagnostic.span.end, start + 1);
    }

    let Declaration::Function { body, .. } = &parsed.value.items[0].declaration else {
        panic!("expected a function declaration");
    };
    let Expr::Block { tail: Some(root_tail), .. } = body else {
        panic!("expected the root case tail");
    };
    let Expr::Control { arms: root_arms, .. } = root_tail.as_ref() else {
        panic!("expected the root case expression");
    };
    assert_eq!(root_arms.len(), 2, "{root_arms:?}");
    let Expr::InterpolatedString { segments, .. } = &root_arms[0].body else {
        panic!("outer arm lost its interpolated string");
    };
    let [
        StringSegment::Text { text: prefix, .. },
        StringSegment::Expression {
            value: Expr::Lambda { body: outer_body, .. },
            ..
        },
        StringSegment::Text {
            text: between_prefixes,
            ..
        },
        StringSegment::Expression {
            value: Expr::Lambda { body: suffix_body, .. },
            ..
        },
        StringSegment::Text { text: suffix, .. },
    ] = segments.as_slice()
    else {
        panic!("nested recovery lost an interpolation segment: {segments:?}");
    };
    assert_eq!(prefix, "prefix ");
    assert_eq!(between_prefixes, " middle ");
    assert_eq!(suffix, " end");

    let Expr::Block {
        statements: outer_statements,
        tail: Some(after_closure),
        ..
    } = outer_body.as_ref()
    else {
        panic!("nested recovery consumed the enclosing block tail");
    };
    let [
        Statement::Let { .. },
        Statement::Control {
            value: Expr::Control { arms: recovered_arms, .. },
            ..
        },
    ] = outer_statements.as_slice()
    else {
        panic!("recovered case lost its statement boundary: {outer_statements:?}");
    };
    assert_eq!(recovered_arms.len(), 1, "{recovered_arms:?}");
    let Expr::Lambda { body: first_body, .. } = &recovered_arms[0].body else {
        panic!("valid closure prefix before recovery was lost");
    };
    let Expr::Block { tail: Some(first_tail), .. } = first_body.as_ref() else {
        panic!("valid closure prefix lost its block tail");
    };
    assert!(matches!(first_tail.as_ref(), Expr::Control { .. }));

    let Expr::Lambda { body: after_body, .. } = after_closure.as_ref() else {
        panic!("closure tail after the nested case was lost");
    };
    let Expr::Block { tail: Some(after_tail), .. } = after_body.as_ref() else {
        panic!("closure tail after the nested case lost its block tail");
    };
    assert!(matches!(after_tail.as_ref(), Expr::Control { .. }));

    let Expr::Block { tail: Some(suffix_tail), .. } = suffix_body.as_ref() else {
        panic!("closure interpolation after recovery lost its block tail");
    };
    assert!(matches!(suffix_tail.as_ref(), Expr::Control { .. }));

    // The reference is silent on a sibling closure interpolation after nested
    // recovery; preserve the suffix segment and both closure tails.
}

#[test]
fn interpolated_closure_suffix_preserves_its_nested_recovery_tail() {
    let source = include_str!("fixtures/malformed-interpolated-closure-suffix-with-nested-recovery.orna");
    let parsed = parse_module(source);

    let malformed_patterns = [
        ("false 0", 6),
        ("true 1", 5),
        ("false 2", 6),
        ("true 3", 5),
        ("false 4", 6),
        ("true 5", 5),
        ("false 6", 6),
        ("true 7", 5),
    ];
    assert_eq!(parsed.diagnostics.len(), malformed_patterns.len(), "{:?}", parsed.diagnostics);
    for (diagnostic, (pattern, offset)) in parsed.diagnostics.iter().zip(malformed_patterns) {
        let start = source.find(pattern).expect("malformed arm") + offset;
        assert_eq!(diagnostic.code, "ORNA-PARSE-001");
        assert_eq!(diagnostic.message, "expected `:` after case pattern");
        assert_eq!(diagnostic.span.start, start);
        assert_eq!(diagnostic.span.end, start + 1);
    }

    let Declaration::Function { body, .. } = &parsed.value.items[0].declaration else {
        panic!("expected a function declaration");
    };
    let Expr::Block { tail: Some(root_tail), .. } = body else {
        panic!("expected the root case tail");
    };
    let Expr::Control { arms: root_arms, .. } = root_tail.as_ref() else {
        panic!("expected the root case expression");
    };
    assert_eq!(root_arms.len(), 2, "{root_arms:?}");
    let Expr::InterpolatedString { segments, .. } = &root_arms[0].body else {
        panic!("root arm lost its interpolated string");
    };
    let [
        StringSegment::Text { text: prefix, .. },
        StringSegment::Expression {
            value: Expr::Lambda { body: outer_body, .. },
            ..
        },
        StringSegment::Text { text: between_closures, .. },
        StringSegment::Expression {
            value: Expr::Lambda { body: suffix_body, .. },
            ..
        },
        StringSegment::Text { text: suffix, .. },
    ] = segments.as_slice()
    else {
        panic!("recovery lost a string segment: {segments:?}");
    };
    assert_eq!(prefix, "prefix ");
    assert_eq!(between_closures, " middle ");
    assert_eq!(suffix, " end");

    let Expr::Block {
        statements: outer_statements,
        tail: Some(after_closure),
        ..
    } = outer_body.as_ref()
    else {
        panic!("first recovery consumed its enclosing closure tail");
    };
    let [
        Statement::Let { .. },
        Statement::Control {
            value: Expr::Control { arms: outer_recovered, .. },
            ..
        },
    ] = outer_statements.as_slice()
    else {
        panic!("first recovered case lost its boundary: {outer_statements:?}");
    };
    assert_eq!(outer_recovered.len(), 1, "{outer_recovered:?}");

    let Expr::Block {
        statements: suffix_statements,
        tail: Some(finish_closure),
        ..
    } = suffix_body.as_ref()
    else {
        panic!("suffix interpolation lost its closure block tail");
    };
    let [
        Statement::Let { .. },
        Statement::Control {
            value: Expr::Control { arms: suffix_recovered, .. },
            ..
        },
    ] = suffix_statements.as_slice()
    else {
        panic!("suffix recovery lost its case boundary: {suffix_statements:?}");
    };
    assert_eq!(suffix_recovered.len(), 2, "{suffix_recovered:?}");
    assert!(matches!(&suffix_recovered[0].body, Expr::Lambda { .. }));
    let Expr::InterpolatedString { segments, .. } = &suffix_recovered[1].body else {
        panic!("suffix recovery lost its nested interpolated closure prefix");
    };
    let [
        StringSegment::Text { text: inner_prefix, .. },
        StringSegment::Expression {
            value: Expr::Lambda { body: inner_closure_body, .. },
            ..
        },
        StringSegment::Text { text: inner_suffix, .. },
    ] = segments.as_slice()
    else {
        panic!("nested suffix closure lost its boundaries: {segments:?}");
    };
    assert_eq!(inner_prefix, "inner ");
    assert_eq!(inner_suffix, " tail");
    let Expr::Block { tail: Some(inner_tail), .. } = inner_closure_body.as_ref() else {
        panic!("nested suffix closure lost its case tail");
    };
    assert!(matches!(inner_tail.as_ref(), Expr::Control { .. }));

    let Expr::Lambda { body: after_body, .. } = after_closure.as_ref() else {
        panic!("closure after the first recovery was lost");
    };
    let Expr::Block { tail: Some(after_tail), .. } = after_body.as_ref() else {
        panic!("closure after the first recovery lost its tail");
    };
    assert!(matches!(after_tail.as_ref(), Expr::Control { .. }));

    let Expr::Lambda { body: finish_body, .. } = finish_closure.as_ref() else {
        panic!("closure after suffix recovery was lost");
    };
    let Expr::Block { tail: Some(finish_tail), .. } = finish_body.as_ref() else {
        panic!("closure after suffix recovery lost its tail");
    };
    assert!(matches!(finish_tail.as_ref(), Expr::Control { .. }));

    // The reference is silent on a suffix interpolation with its own recovery;
    // retain both interpolations and each closure tail after recovery.
}

#[test]
fn nested_recovery_across_closure_tails_preserves_each_boundary() {
    let source = include_str!("fixtures/malformed-nested-recovery-across-closure-tails.orna");
    let parsed = parse_module(source);

    let malformed_patterns = [
        ("false 0", 6),
        ("true 1", 5),
        ("false 2", 6),
        ("true 3", 5),
        ("false 4", 6),
        ("true 5", 5),
        ("false 6", 6),
        ("true 7", 5),
    ];
    assert_eq!(parsed.diagnostics.len(), malformed_patterns.len(), "{:?}", parsed.diagnostics);
    for (diagnostic, (pattern, offset)) in parsed.diagnostics.iter().zip(malformed_patterns) {
        let start = source.find(pattern).expect("malformed arm") + offset;
        assert_eq!(diagnostic.code, "ORNA-PARSE-001");
        assert_eq!(diagnostic.message, "expected `:` after case pattern");
        assert_eq!(diagnostic.span.start, start);
        assert_eq!(diagnostic.span.end, start + 1);
    }

    let Declaration::Function { body, .. } = &parsed.value.items[0].declaration else {
        panic!("expected a function declaration");
    };
    let Expr::Block { tail: Some(root_tail), .. } = body else {
        panic!("expected the root case tail");
    };
    let Expr::Control { arms: root_arms, .. } = root_tail.as_ref() else {
        panic!("expected the root case expression");
    };
    assert_eq!(root_arms.len(), 2, "{root_arms:?}");
    let Expr::InterpolatedString { segments, .. } = &root_arms[0].body else {
        panic!("root arm lost its interpolated closure");
    };
    let [
        StringSegment::Text { text: prefix, .. },
        StringSegment::Expression {
            value: Expr::Lambda { body: outer_body, .. },
            ..
        },
        StringSegment::Text { text: suffix, .. },
    ] = segments.as_slice()
    else {
        panic!("root interpolation lost its closure boundaries: {segments:?}");
    };
    assert_eq!(prefix, "prefix ");
    assert_eq!(suffix, " suffix");

    let Expr::Block {
        statements: outer_statements,
        tail: Some(root_finish),
        ..
    } = outer_body.as_ref()
    else {
        panic!("first recovery consumed the root closure tail");
    };
    let [
        Statement::Let { .. },
        Statement::Control {
            value: Expr::Control { arms: outer_arms, .. },
            ..
        },
    ] = outer_statements.as_slice()
    else {
        panic!("outer case lost its statement boundary: {outer_statements:?}");
    };
    assert_eq!(outer_arms.len(), 2, "{outer_arms:?}");

    let Expr::Lambda { body: inner_body, .. } = &outer_arms[0].body else {
        panic!("inner closure prefix was lost");
    };
    let Expr::Block {
        statements: inner_statements,
        tail: Some(middle_closure),
        ..
    } = inner_body.as_ref()
    else {
        panic!("inner recovery consumed its following closure tail");
    };
    let [
        Statement::Let { .. },
        Statement::Control {
            value: Expr::Control { arms: inner_arms, .. },
            ..
        },
    ] = inner_statements.as_slice()
    else {
        panic!("inner recovered case lost its boundary: {inner_statements:?}");
    };
    assert_eq!(inner_arms.len(), 2, "{inner_arms:?}");
    assert!(matches!(&inner_arms[0].body, Expr::Lambda { .. }));
    assert!(matches!(&inner_arms[1].body, Expr::InterpolatedString { .. }));

    let Expr::Lambda { body: middle_body, .. } = middle_closure.as_ref() else {
        panic!("closure after the first recovery was lost");
    };
    let Expr::Block {
        statements: middle_statements,
        tail: Some(finish_closure),
        ..
    } = middle_body.as_ref()
    else {
        panic!("second recovery consumed its following closure tail");
    };
    let [
        Statement::Let { .. },
        Statement::Control {
            value: Expr::Control { arms: middle_arms, .. },
            ..
        },
    ] = middle_statements.as_slice()
    else {
        panic!("middle recovered case lost its boundary: {middle_statements:?}");
    };
    assert_eq!(middle_arms.len(), 1, "{middle_arms:?}");
    assert!(matches!(&middle_arms[0].body, Expr::Lambda { .. }));

    let Expr::Lambda { body: finish_body, .. } = finish_closure.as_ref() else {
        panic!("closure after the second recovery was lost");
    };
    let Expr::Block { tail: Some(finish_tail), .. } = finish_body.as_ref() else {
        panic!("closure after the second recovery lost its tail");
    };
    assert!(matches!(finish_tail.as_ref(), Expr::Control { .. }));

    let Expr::Lambda { body: outer_finish_body, .. } = &outer_arms[1].body else {
        panic!("following outer case closure was lost");
    };
    let Expr::Block { tail: Some(outer_finish_tail), .. } = outer_finish_body.as_ref() else {
        panic!("following outer case closure lost its tail");
    };
    assert!(matches!(outer_finish_tail.as_ref(), Expr::Control { .. }));

    let Expr::Lambda { body: root_finish_body, .. } = root_finish.as_ref() else {
        panic!("following root closure was lost");
    };
    let Expr::Block { tail: Some(root_finish_tail), .. } = root_finish_body.as_ref() else {
        panic!("following root closure lost its tail");
    };
    assert!(matches!(root_finish_tail.as_ref(), Expr::Control { .. }));

    // The reference is silent on recovery repeated in successively nested
    // closure tails; preserve both boundaries and each subsequent closure.
}

#[test]
fn nested_and_enclosing_recovery_preserve_closure_tails() {
    let source = include_str!("fixtures/malformed-inner-and-enclosing-case-recovery-with-closure-tails.orna");
    let parsed = parse_module(source);

    let malformed_patterns = [
        ("false 0", 6),
        ("true 1", 5),
        ("false 2", 6),
        ("true 3", 5),
        ("false 4", 6),
        ("true 5", 5),
        ("false 6", 6),
        ("true 7", 5),
    ];
    assert_eq!(parsed.diagnostics.len(), malformed_patterns.len(), "{:?}", parsed.diagnostics);
    for (diagnostic, (pattern, offset)) in parsed.diagnostics.iter().zip(malformed_patterns) {
        let start = source.find(pattern).expect("malformed arm") + offset;
        assert_eq!(diagnostic.code, "ORNA-PARSE-001");
        assert_eq!(diagnostic.message, "expected `:` after case pattern");
        assert_eq!(diagnostic.span.start, start);
        assert_eq!(diagnostic.span.end, start + 1);
    }

    let Declaration::Function { body, .. } = &parsed.value.items[0].declaration else {
        panic!("expected a function declaration");
    };
    let Expr::Block { tail: Some(root_tail), .. } = body else {
        panic!("expected the root case tail");
    };
    let Expr::Control { arms: root_arms, .. } = root_tail.as_ref() else {
        panic!("expected the root case expression");
    };
    assert_eq!(root_arms.len(), 2, "{root_arms:?}");
    let Expr::InterpolatedString { segments, .. } = &root_arms[0].body else {
        panic!("root arm lost its interpolated closure");
    };
    let [
        StringSegment::Text { text: prefix, .. },
        StringSegment::Expression {
            value: Expr::Lambda { body: root_body, .. },
            ..
        },
        StringSegment::Text { text: suffix, .. },
    ] = segments.as_slice()
    else {
        panic!("root interpolation lost its boundaries: {segments:?}");
    };
    assert_eq!(prefix, "prefix ");
    assert_eq!(suffix, " suffix");

    let Expr::Block {
        statements: root_statements,
        tail: Some(root_finish),
        ..
    } = root_body.as_ref()
    else {
        panic!("nested and outer recovery consumed the root closure tail");
    };
    let [
        Statement::Let { .. },
        Statement::Control {
            value: Expr::Control { arms: outer_arms, .. },
            ..
        },
    ] = root_statements.as_slice()
    else {
        panic!("outer case lost its statement boundary: {root_statements:?}");
    };
    assert_eq!(outer_arms.len(), 2, "{outer_arms:?}");

    let Expr::Lambda { body: inner_body, .. } = &outer_arms[0].body else {
        panic!("nested closure prefix was lost");
    };
    let Expr::Block {
        statements: inner_statements,
        tail: Some(inner_finish),
        ..
    } = inner_body.as_ref()
    else {
        panic!("inner recovery consumed its closure tail");
    };
    let [
        Statement::Let { .. },
        Statement::Control {
            value: Expr::Control { arms: inner_arms, .. },
            ..
        },
    ] = inner_statements.as_slice()
    else {
        panic!("inner case lost its statement boundary: {inner_statements:?}");
    };
    assert_eq!(inner_arms.len(), 1, "{inner_arms:?}");
    assert!(matches!(&inner_arms[0].body, Expr::Lambda { .. }));

    let Expr::Lambda { body: inner_finish_body, .. } = inner_finish.as_ref() else {
        panic!("closure after inner recovery was lost");
    };
    let Expr::Block { tail: Some(inner_finish_tail), .. } = inner_finish_body.as_ref() else {
        panic!("closure after inner recovery lost its tail");
    };
    assert!(matches!(inner_finish_tail.as_ref(), Expr::Control { .. }));

    let Expr::Lambda { body: outer_suffix_body, .. } = &outer_arms[1].body else {
        panic!("valid closure suffix after outer recovery was lost");
    };
    let Expr::Block { tail: Some(outer_suffix_tail), .. } = outer_suffix_body.as_ref() else {
        panic!("valid closure suffix after outer recovery lost its tail");
    };
    assert!(matches!(outer_suffix_tail.as_ref(), Expr::Control { .. }));

    let Expr::Lambda { body: root_finish_body, .. } = root_finish.as_ref() else {
        panic!("closure after outer case was lost");
    };
    let Expr::Block { tail: Some(root_finish_tail), .. } = root_finish_body.as_ref() else {
        panic!("closure after outer case lost its tail");
    };
    assert!(matches!(root_finish_tail.as_ref(), Expr::Control { .. }));

    // The reference is silent on an enclosing recovery after nested closure
    // recovery; retain both recovered suffixes and their subsequent tails.
}

#[test]
fn nested_recovery_preserves_interpolated_closure_tail() {
    let source = include_str!("fixtures/malformed-nested-recovery-before-interpolated-closure-tail.orna");
    let parsed = parse_module(source);

    let malformed_patterns = [
        ("false 0", 6),
        ("true 1", 5),
        ("false 2", 6),
        ("true 3", 5),
    ];
    assert_eq!(parsed.diagnostics.len(), malformed_patterns.len(), "{:?}", parsed.diagnostics);
    for (diagnostic, (pattern, offset)) in parsed.diagnostics.iter().zip(malformed_patterns) {
        let start = source.find(pattern).expect("malformed arm") + offset;
        assert_eq!(diagnostic.code, "ORNA-PARSE-001");
        assert_eq!(diagnostic.message, "expected `:` after case pattern");
        assert_eq!(diagnostic.span.start, start);
        assert_eq!(diagnostic.span.end, start + 1);
    }

    let Declaration::Function { body, .. } = &parsed.value.items[0].declaration else {
        panic!("expected a function declaration");
    };
    let Expr::Block { tail: Some(root_tail), .. } = body else {
        panic!("expected the root case tail");
    };
    let Expr::Control { arms: root_arms, .. } = root_tail.as_ref() else {
        panic!("expected the root case expression");
    };
    assert_eq!(root_arms.len(), 2, "{root_arms:?}");
    let Expr::InterpolatedString { segments, .. } = &root_arms[0].body else {
        panic!("root arm lost its interpolated string");
    };
    let [
        StringSegment::Text { text: prefix, .. },
        StringSegment::Expression {
            value: Expr::Lambda { body: root_body, .. },
            ..
        },
        StringSegment::Text { text: suffix, .. },
    ] = segments.as_slice()
    else {
        panic!("root interpolation lost its boundaries: {segments:?}");
    };
    assert_eq!(prefix, "outer ");
    assert_eq!(suffix, " end");

    let Expr::Block {
        statements: root_statements,
        tail: Some(after_closure),
        ..
    } = root_body.as_ref()
    else {
        panic!("nested recovery consumed the following closure tail");
    };
    let [
        Statement::Let { .. },
        Statement::Control {
            value: Expr::Control { arms: recovered_arms, .. },
            ..
        },
    ] = root_statements.as_slice()
    else {
        panic!("recovered case lost its statement boundary: {root_statements:?}");
    };
    assert_eq!(recovered_arms.len(), 2, "{recovered_arms:?}");
    assert!(matches!(&recovered_arms[0].body, Expr::Lambda { .. }));
    let Expr::InterpolatedString { segments, .. } = &recovered_arms[1].body else {
        panic!("interpolated closure prefix before recovery was lost");
    };
    let [
        StringSegment::Text { text: keep_prefix, .. },
        StringSegment::Expression {
            value: Expr::Lambda { body: prefix_body, .. },
            ..
        },
        StringSegment::Text { text: keep_suffix, .. },
    ] = segments.as_slice()
    else {
        panic!("valid interpolated prefix lost its boundaries: {segments:?}");
    };
    assert_eq!(keep_prefix, "keep ");
    assert_eq!(keep_suffix, " alive");
    let Expr::Block { tail: Some(prefix_tail), .. } = prefix_body.as_ref() else {
        panic!("interpolated closure prefix lost its case tail");
    };
    assert!(matches!(prefix_tail.as_ref(), Expr::Control { .. }));

    let Expr::Lambda { body: after_body, .. } = after_closure.as_ref() else {
        panic!("closure after nested recovery was lost");
    };
    let Expr::Block {
        statements: after_statements,
        tail: Some(after_tail),
        ..
    } = after_body.as_ref()
    else {
        panic!("closure after nested recovery lost its interpolated tail");
    };
    assert!(matches!(after_statements.as_slice(), [Statement::Let { .. }]));
    let Expr::InterpolatedString { segments, .. } = after_tail.as_ref() else {
        panic!("closure tail lost its interpolated string");
    };
    let [
        StringSegment::Text { text: tail_prefix, .. },
        StringSegment::Expression {
            value: Expr::Lambda { body: suffix_body, .. },
            ..
        },
        StringSegment::Text { text: tail_suffix, .. },
    ] = segments.as_slice()
    else {
        panic!("interpolated closure tail lost its boundaries: {segments:?}");
    };
    assert_eq!(tail_prefix, "middle ");
    assert_eq!(tail_suffix, " tail");
    let Expr::Block { tail: Some(suffix_tail), .. } = suffix_body.as_ref() else {
        panic!("suffix closure lost its nested case tail");
    };
    assert!(matches!(suffix_tail.as_ref(), Expr::Control { .. }));

    // The reference is silent on a post-recovery closure tail containing an
    // interpolated closure; preserve both string boundaries and nested tails.
}

#[test]
fn recovery_preserves_two_sibling_interpolated_closure_tails() {
    let source = include_str!("fixtures/malformed-interpolated-recovery-before-two-closure-tails.orna");
    let parsed = parse_module(source);

    let malformed_patterns = [
        ("false 0", 6),
        ("true 1", 5),
        ("false 2", 6),
        ("true 3", 5),
    ];
    assert_eq!(parsed.diagnostics.len(), malformed_patterns.len(), "{:?}", parsed.diagnostics);
    for (diagnostic, (pattern, offset)) in parsed.diagnostics.iter().zip(malformed_patterns) {
        let start = source.find(pattern).expect("malformed arm") + offset;
        assert_eq!(diagnostic.code, "ORNA-PARSE-001");
        assert_eq!(diagnostic.message, "expected `:` after case pattern");
        assert_eq!(diagnostic.span.start, start);
        assert_eq!(diagnostic.span.end, start + 1);
    }

    let Declaration::Function { body, .. } = &parsed.value.items[0].declaration else {
        panic!("expected a function declaration");
    };
    let Expr::Block { tail: Some(root_tail), .. } = body else {
        panic!("expected the root case tail");
    };
    let Expr::Control { arms: root_arms, .. } = root_tail.as_ref() else {
        panic!("expected the root case expression");
    };
    assert_eq!(root_arms.len(), 2, "{root_arms:?}");
    let Expr::InterpolatedString { segments, .. } = &root_arms[0].body else {
        panic!("root arm lost its interpolated string");
    };
    let [
        StringSegment::Text { text: prefix, .. },
        StringSegment::Expression {
            value: Expr::Lambda { body: outer_body, .. },
            ..
        },
        StringSegment::Text { text: middle, .. },
        StringSegment::Expression {
            value: Expr::Lambda { body: next_body, .. },
            ..
        },
        StringSegment::Text {
            text: conjunction,
            ..
        },
        StringSegment::Expression {
            value: Expr::Lambda { body: last_body, .. },
            ..
        },
        StringSegment::Text { text: suffix, .. },
    ] = segments.as_slice()
    else {
        panic!("recovery lost a sibling closure interpolation: {segments:?}");
    };
    assert_eq!(prefix, "prefix ");
    assert_eq!(middle, " middle ");
    assert_eq!(conjunction, " and ");
    assert_eq!(suffix, " end");

    let Expr::Block {
        statements: outer_statements,
        tail: Some(after_closure),
        ..
    } = outer_body.as_ref()
    else {
        panic!("nested recovery consumed the enclosing closure tail");
    };
    let [
        Statement::Let { .. },
        Statement::Control {
            value: Expr::Control { arms: recovered_arms, .. },
            ..
        },
    ] = outer_statements.as_slice()
    else {
        panic!("recovered case lost its statement boundary: {outer_statements:?}");
    };
    assert_eq!(recovered_arms.len(), 1, "{recovered_arms:?}");

    let Expr::Lambda { body: after_body, .. } = after_closure.as_ref() else {
        panic!("closure tail after recovery was lost");
    };
    let Expr::Block { tail: Some(after_tail), .. } = after_body.as_ref() else {
        panic!("closure tail after recovery lost its case tail");
    };
    assert!(matches!(after_tail.as_ref(), Expr::Control { .. }));
    for (label, closure_body) in [("next", next_body), ("last", last_body)] {
        let Expr::Block {
            tail: Some(closure_tail),
            ..
        } = closure_body.as_ref()
        else {
            panic!("{label} suffix closure lost its block tail");
        };
        assert!(matches!(closure_tail.as_ref(), Expr::Control { .. }));
    }

    // The reference is silent on repeated sibling closure tails after nested
    // recovery; preserve each closure interpolation and its case tail.
}

#[test]
fn single_recovery_preserves_sibling_closure_before_text_tail() {
    let source = include_str!("fixtures/malformed-first-sibling-closure-before-text-tail.orna");
    let parsed = parse_module(source);

    assert_eq!(parsed.diagnostics.len(), 1, "{:?}", parsed.diagnostics);
    let diagnostic = &parsed.diagnostics[0];
    let start = source.find("false 0").expect("malformed first arm") + "false ".len();
    assert_eq!(diagnostic.code, "ORNA-PARSE-001");
    assert_eq!(diagnostic.message, "expected `:` after case pattern");
    assert_eq!(diagnostic.span.start, start, "{diagnostic:?}");
    assert_eq!(diagnostic.span.end, start + 1, "{diagnostic:?}");

    let Declaration::Function { body, .. } = &parsed.value.items[0].declaration else {
        panic!("expected a function declaration");
    };
    let Expr::Block { tail: Some(root_tail), .. } = body else {
        panic!("expected the root case tail");
    };
    let Expr::Control { arms: root_arms, .. } = root_tail.as_ref() else {
        panic!("expected the root case expression");
    };
    let Expr::InterpolatedString { segments, .. } = &root_arms[0].body else {
        panic!("root arm lost its interpolated string");
    };
    let [
        StringSegment::Text { text: prefix, .. },
        StringSegment::Expression { value: Expr::Lambda { body: first, .. }, .. },
        StringSegment::Text { text: middle, .. },
        StringSegment::Expression { value: Expr::Lambda { body: second, .. }, .. },
        StringSegment::Text { text: suffix, .. },
    ] = segments.as_slice()
    else {
        panic!("a sibling closure or text tail was lost: {segments:?}");
    };
    assert_eq!(prefix, "left ");
    assert_eq!(middle, " right ");
    assert_eq!(suffix, " end");

    let Expr::Block { statements, tail: Some(first_tail), .. } = first.as_ref() else {
        panic!("first closure lost its recovered case or following tail");
    };
    let [Statement::Let { .. }, Statement::Control { value: Expr::Control { arms, .. }, .. }] =
        statements.as_slice()
    else {
        panic!("first closure lost its statement boundary: {statements:?}");
    };
    assert_eq!(arms.len(), 1, "{arms:?}");
    let Expr::Lambda { body: first_suffix_body, .. } = first_tail.as_ref() else {
        panic!("first closure lost its following closure tail");
    };
    let Expr::Block { tail: Some(first_suffix_tail), .. } = first_suffix_body.as_ref() else {
        panic!("following closure lost its case tail");
    };
    assert!(matches!(first_suffix_tail.as_ref(), Expr::Control { .. }));

    let Expr::Block { statements, tail: Some(second_tail), .. } = second.as_ref() else {
        panic!("second sibling closure lost its case or closure tail");
    };
    let [Statement::Let { .. }, Statement::Control { value: Expr::Control { arms, .. }, .. }] =
        statements.as_slice()
    else {
        panic!("second closure lost its statement boundary: {statements:?}");
    };
    assert_eq!(arms.len(), 2, "{arms:?}");
    assert!(matches!(second_tail.as_ref(), Expr::Lambda { .. }));

    // The reference is silent on text after a recovered sibling closure and its
    // following sibling; retain both case tails and the final string segment.
}

#[test]
fn second_sibling_recovery_preserves_closure_before_text_tail() {
    let source = include_str!("fixtures/malformed-second-sibling-closure-before-text-tail.orna");
    let parsed = parse_module(source);

    assert_eq!(parsed.diagnostics.len(), 1, "{:?}", parsed.diagnostics);
    let diagnostic = &parsed.diagnostics[0];
    let start = source.find("false 1").expect("malformed second arm") + "false ".len();
    assert_eq!(diagnostic.code, "ORNA-PARSE-001");
    assert_eq!(diagnostic.message, "expected `:` after case pattern");
    assert_eq!(diagnostic.span.start, start, "{diagnostic:?}");
    assert_eq!(diagnostic.span.end, start + 1, "{diagnostic:?}");

    let Declaration::Function { body, .. } = &parsed.value.items[0].declaration else {
        panic!("expected a function declaration");
    };
    let Expr::Block { tail: Some(root_tail), .. } = body else {
        panic!("expected the root case tail");
    };
    let Expr::Control { arms: root_arms, .. } = root_tail.as_ref() else {
        panic!("expected the root case expression");
    };
    let Expr::InterpolatedString { segments, .. } = &root_arms[0].body else {
        panic!("root arm lost its interpolated string");
    };
    let [
        StringSegment::Text { text: prefix, .. },
        StringSegment::Expression { value: Expr::Lambda { body: first, .. }, .. },
        StringSegment::Text { text: middle, .. },
        StringSegment::Expression { value: Expr::Lambda { body: second, .. }, .. },
        StringSegment::Text { text: suffix, .. },
    ] = segments.as_slice()
    else {
        panic!("a sibling closure or text tail was lost: {segments:?}");
    };
    assert_eq!(prefix, "left ");
    assert_eq!(middle, " right ");
    assert_eq!(suffix, " end");

    for (label, closure_body, expected_arms) in [("first", first, 2), ("second", second, 1)] {
        let Expr::Block { statements, tail: Some(closure_tail), .. } = closure_body.as_ref() else {
            panic!("{label} sibling closure lost its case or following tail");
        };
        let [Statement::Let { .. }, Statement::Control { value: Expr::Control { arms, .. }, .. }] =
            statements.as_slice()
        else {
            panic!("{label} closure lost its statement boundary: {statements:?}");
        };
        assert_eq!(arms.len(), expected_arms, "{label} case: {arms:?}");
        let Expr::Lambda { body: suffix_body, .. } = closure_tail.as_ref() else {
            panic!("{label} closure tail was lost");
        };
        let Expr::Block { tail: Some(suffix_tail), .. } = suffix_body.as_ref() else {
            panic!("{label} following closure lost its case tail");
        };
        assert!(matches!(suffix_tail.as_ref(), Expr::Control { .. }));
    }

    // The reference is silent on recovery in the final sibling before literal
    // text; preserve its following closure case and the complete string suffix.
}

#[test]
fn middle_sibling_recovery_preserves_closures_before_text_tail() {
    let source = include_str!("fixtures/malformed-middle-sibling-closure-before-text-tail.orna");
    let parsed = parse_module(source);

    assert_eq!(parsed.diagnostics.len(), 1, "{:?}", parsed.diagnostics);
    let diagnostic = &parsed.diagnostics[0];
    let start = source.find("false 2").expect("malformed middle arm") + "false ".len();
    assert_eq!(diagnostic.code, "ORNA-PARSE-001");
    assert_eq!(diagnostic.message, "expected `:` after case pattern");
    assert_eq!(diagnostic.span.start, start, "{diagnostic:?}");
    assert_eq!(diagnostic.span.end, start + 1, "{diagnostic:?}");

    let Declaration::Function { body, .. } = &parsed.value.items[0].declaration else {
        panic!("expected a function declaration");
    };
    let Expr::Block { tail: Some(root_tail), .. } = body else {
        panic!("expected the root case tail");
    };
    let Expr::Control { arms: root_arms, .. } = root_tail.as_ref() else {
        panic!("expected the root case expression");
    };
    assert_eq!(root_arms.len(), 2, "{root_arms:?}");
    let Expr::InterpolatedString { segments, .. } = &root_arms[0].body else {
        panic!("root arm lost its interpolated string");
    };
    let [
        StringSegment::Text { text: prefix, .. },
        StringSegment::Expression { value: Expr::Lambda { body: first, .. }, .. },
        StringSegment::Text { text: middle, .. },
        StringSegment::Expression { value: Expr::Lambda { body: second, .. }, .. },
        StringSegment::Text { text: conjunction, .. },
        StringSegment::Expression { value: Expr::Lambda { body: third, .. }, .. },
        StringSegment::Text { text: suffix, .. },
    ] = segments.as_slice()
    else {
        panic!("a sibling closure or trailing text was lost: {segments:?}");
    };
    assert_eq!(prefix, "left ");
    assert_eq!(middle, " middle ");
    assert_eq!(conjunction, " right ");
    assert_eq!(suffix, " end");

    for (label, closure_body, expected_arms) in
        [("first", first, 2), ("middle", second, 1), ("last", third, 2)]
    {
        let Expr::Block { statements, tail: Some(closure_tail), .. } = closure_body.as_ref() else {
            panic!("{label} sibling closure lost its case or following tail");
        };
        let [Statement::Let { .. }, Statement::Control { value: Expr::Control { arms, .. }, .. }] =
            statements.as_slice()
        else {
            panic!("{label} closure lost its statement boundary: {statements:?}");
        };
        assert_eq!(arms.len(), expected_arms, "{label} case: {arms:?}");
        let Expr::Lambda { body: suffix_body, .. } = closure_tail.as_ref() else {
            panic!("{label} closure tail was lost");
        };
        let Expr::Block { tail: Some(suffix_tail), .. } = suffix_body.as_ref() else {
            panic!("{label} following closure lost its case tail");
        };
        assert!(matches!(suffix_tail.as_ref(), Expr::Control { .. }));
    }

    // The reference is silent on recovery in a middle sibling before literal
    // text; preserve both neighboring closures and the final string segment.
}

#[test]
fn two_sibling_recoveries_preserve_closures_before_text_tail() {
    let source = include_str!("fixtures/malformed-two-sibling-closures-before-text-tail.orna");
    let parsed = parse_module(source);

    let malformed_patterns = [("false 0", 6), ("false 1", 6)];
    assert_eq!(parsed.diagnostics.len(), malformed_patterns.len(), "{:?}", parsed.diagnostics);
    for (diagnostic, (pattern, offset)) in parsed.diagnostics.iter().zip(malformed_patterns) {
        let start = source.find(pattern).expect("malformed sibling arm") + offset;
        assert_eq!(diagnostic.code, "ORNA-PARSE-001");
        assert_eq!(diagnostic.message, "expected `:` after case pattern");
        assert_eq!(diagnostic.span.start, start, "{diagnostic:?}");
        assert_eq!(diagnostic.span.end, start + 1, "{diagnostic:?}");
    }

    let Declaration::Function { body, .. } = &parsed.value.items[0].declaration else {
        panic!("expected a function declaration");
    };
    let Expr::Block { tail: Some(root_tail), .. } = body else {
        panic!("expected the root case tail");
    };
    let Expr::Control { arms: root_arms, .. } = root_tail.as_ref() else {
        panic!("expected the root case expression");
    };
    let Expr::InterpolatedString { segments, .. } = &root_arms[0].body else {
        panic!("root arm lost its interpolated string");
    };
    let [
        StringSegment::Text { text: prefix, .. },
        StringSegment::Expression { value: Expr::Lambda { body: first, .. }, .. },
        StringSegment::Text { text: middle, .. },
        StringSegment::Expression { value: Expr::Lambda { body: second, .. }, .. },
        StringSegment::Text { text: suffix, .. },
    ] = segments.as_slice()
    else {
        panic!("a recovered sibling closure or trailing text was lost: {segments:?}");
    };
    assert_eq!(prefix, "left ");
    assert_eq!(middle, " right ");
    assert_eq!(suffix, " end");

    for (label, closure_body) in [("first", first), ("second", second)] {
        let Expr::Block { statements, tail: Some(closure_tail), .. } = closure_body.as_ref() else {
            panic!("{label} closure lost its recovered case or following tail");
        };
        let [Statement::Let { .. }, Statement::Control { value: Expr::Control { arms, .. }, .. }] =
            statements.as_slice()
        else {
            panic!("{label} closure lost its statement boundary: {statements:?}");
        };
        assert_eq!(arms.len(), 1, "{label} recovered case: {arms:?}");
        let Expr::Lambda { body: suffix_body, .. } = closure_tail.as_ref() else {
            panic!("{label} closure tail was lost");
        };
        let Expr::Block { tail: Some(suffix_tail), .. } = suffix_body.as_ref() else {
            panic!("{label} following closure lost its case tail");
        };
        assert!(matches!(suffix_tail.as_ref(), Expr::Control { .. }));
    }

    // The reference is silent on adjacent sibling recoveries before literal
    // text; preserve both recovered closure tails and the complete string suffix.
}

#[test]
fn sibling_recoveries_at_both_ends_preserve_text_tail() {
    let source = include_str!("fixtures/malformed-first-and-last-of-three-sibling-closures-before-text-tail.orna");
    let parsed = parse_module(source);

    let malformed_patterns = [("false 0", 6), ("false 2", 6)];
    assert_eq!(parsed.diagnostics.len(), malformed_patterns.len(), "{:?}", parsed.diagnostics);
    for (diagnostic, (pattern, offset)) in parsed.diagnostics.iter().zip(malformed_patterns) {
        let start = source.find(pattern).expect("malformed sibling arm") + offset;
        assert_eq!(diagnostic.code, "ORNA-PARSE-001");
        assert_eq!(diagnostic.message, "expected `:` after case pattern");
        assert_eq!(diagnostic.span.start, start, "{diagnostic:?}");
        assert_eq!(diagnostic.span.end, start + 1, "{diagnostic:?}");
    }

    let Declaration::Function { body, .. } = &parsed.value.items[0].declaration else {
        panic!("expected a function declaration");
    };
    let Expr::Block { tail: Some(root_tail), .. } = body else {
        panic!("expected the root case tail");
    };
    let Expr::Control { arms: root_arms, .. } = root_tail.as_ref() else {
        panic!("expected the root case expression");
    };
    let Expr::InterpolatedString { segments, .. } = &root_arms[0].body else {
        panic!("root arm lost its interpolated string");
    };
    let [
        StringSegment::Text { text: prefix, .. },
        StringSegment::Expression { value: Expr::Lambda { body: first, .. }, .. },
        StringSegment::Text { text: middle, .. },
        StringSegment::Expression { value: Expr::Lambda { body: second, .. }, .. },
        StringSegment::Text { text: conjunction, .. },
        StringSegment::Expression { value: Expr::Lambda { body: third, .. }, .. },
        StringSegment::Text { text: suffix, .. },
    ] = segments.as_slice()
    else {
        panic!("a sibling closure or trailing text was lost: {segments:?}");
    };
    assert_eq!(prefix, "left ");
    assert_eq!(middle, " middle ");
    assert_eq!(conjunction, " right ");
    assert_eq!(suffix, " end");

    for (label, closure_body, expected_arms) in
        [("first", first, 1), ("middle", second, 2), ("last", third, 1)]
    {
        let Expr::Block { statements, tail: Some(closure_tail), .. } = closure_body.as_ref() else {
            panic!("{label} sibling closure lost its case or following tail");
        };
        let [Statement::Let { .. }, Statement::Control { value: Expr::Control { arms, .. }, .. }] =
            statements.as_slice()
        else {
            panic!("{label} closure lost its statement boundary: {statements:?}");
        };
        assert_eq!(arms.len(), expected_arms, "{label} case: {arms:?}");
        let Expr::Lambda { body: suffix_body, .. } = closure_tail.as_ref() else {
            panic!("{label} closure tail was lost");
        };
        let Expr::Block { tail: Some(suffix_tail), .. } = suffix_body.as_ref() else {
            panic!("{label} following closure lost its case tail");
        };
        assert!(matches!(suffix_tail.as_ref(), Expr::Control { .. }));
    }

    // The reference is silent on recoveries at both ends of sibling closures
    // before literal text; preserve the valid middle closure and string suffix.
}

#[test]
fn alternating_sibling_recoveries_preserve_closures_before_text_tail() {
    let source = include_str!("fixtures/malformed-second-and-fourth-sibling-closures-before-text-tail.orna");
    let parsed = parse_module(source);

    let malformed_patterns = [("false 1", 6), ("false 3", 6)];
    assert_eq!(parsed.diagnostics.len(), malformed_patterns.len(), "{:?}", parsed.diagnostics);
    for (diagnostic, (pattern, offset)) in parsed.diagnostics.iter().zip(malformed_patterns) {
        let start = source.find(pattern).expect("malformed sibling arm") + offset;
        assert_eq!(diagnostic.code, "ORNA-PARSE-001");
        assert_eq!(diagnostic.message, "expected `:` after case pattern");
        assert_eq!(diagnostic.span.start, start, "{diagnostic:?}");
        assert_eq!(diagnostic.span.end, start + 1, "{diagnostic:?}");
    }

    let Declaration::Function { body, .. } = &parsed.value.items[0].declaration else {
        panic!("expected a function declaration");
    };
    let Expr::Block { tail: Some(root_tail), .. } = body else {
        panic!("expected the root case tail");
    };
    let Expr::Control { arms: root_arms, .. } = root_tail.as_ref() else {
        panic!("expected the root case expression");
    };
    let Expr::InterpolatedString { segments, .. } = &root_arms[0].body else {
        panic!("root arm lost its interpolated string");
    };
    let [
        StringSegment::Text { text: one, .. },
        StringSegment::Expression { value: Expr::Lambda { body: first, .. }, .. },
        StringSegment::Text { text: two, .. },
        StringSegment::Expression { value: Expr::Lambda { body: second, .. }, .. },
        StringSegment::Text { text: three, .. },
        StringSegment::Expression { value: Expr::Lambda { body: third, .. }, .. },
        StringSegment::Text { text: four, .. },
        StringSegment::Expression { value: Expr::Lambda { body: fourth, .. }, .. },
        StringSegment::Text { text: suffix, .. },
    ] = segments.as_slice()
    else {
        panic!("a sibling closure or trailing text was lost: {segments:?}");
    };
    assert_eq!(one, "one ");
    assert_eq!(two, " two ");
    assert_eq!(three, " three ");
    assert_eq!(four, " four ");
    assert_eq!(suffix, " end");

    for (label, closure_body, expected_arms) in [
        ("first", first, 2),
        ("second", second, 1),
        ("third", third, 2),
        ("fourth", fourth, 1),
    ] {
        let Expr::Block { statements, tail: Some(closure_tail), .. } = closure_body.as_ref() else {
            panic!("{label} sibling closure lost its case or following tail");
        };
        let [Statement::Let { .. }, Statement::Control { value: Expr::Control { arms, .. }, .. }] =
            statements.as_slice()
        else {
            panic!("{label} closure lost its statement boundary: {statements:?}");
        };
        assert_eq!(arms.len(), expected_arms, "{label} case: {arms:?}");
        let Expr::Lambda { body: suffix_body, .. } = closure_tail.as_ref() else {
            panic!("{label} closure tail was lost");
        };
        let Expr::Block { tail: Some(suffix_tail), .. } = suffix_body.as_ref() else {
            panic!("{label} following closure lost its case tail");
        };
        assert!(matches!(suffix_tail.as_ref(), Expr::Control { .. }));
    }

    // The reference is silent on alternating sibling recoveries before text;
    // preserve both neighboring closures and the final string segment.
}

#[test]
fn odd_sibling_recoveries_preserve_five_closures_before_text_tail() {
    let source = include_str!("fixtures/malformed-odd-sibling-closures-before-text-tail.orna");
    let parsed = parse_module(source);

    let malformed_patterns = [("false 0", 6), ("false 2", 6), ("false 4", 6)];
    assert_eq!(parsed.diagnostics.len(), malformed_patterns.len(), "{:?}", parsed.diagnostics);
    for (diagnostic, (pattern, offset)) in parsed.diagnostics.iter().zip(malformed_patterns) {
        let start = source.find(pattern).expect("malformed sibling arm") + offset;
        assert_eq!(diagnostic.code, "ORNA-PARSE-001");
        assert_eq!(diagnostic.message, "expected `:` after case pattern");
        assert_eq!(diagnostic.span.start, start, "{diagnostic:?}");
        assert_eq!(diagnostic.span.end, start + 1, "{diagnostic:?}");
    }

    let Declaration::Function { body, .. } = &parsed.value.items[0].declaration else {
        panic!("expected a function declaration");
    };
    let Expr::Block { tail: Some(root_tail), .. } = body else {
        panic!("expected the root case tail");
    };
    let Expr::Control { arms: root_arms, .. } = root_tail.as_ref() else {
        panic!("expected the root case expression");
    };
    let Expr::InterpolatedString { segments, .. } = &root_arms[0].body else {
        panic!("root arm lost its interpolated string");
    };
    let [
        StringSegment::Text { text: one, .. },
        StringSegment::Expression { value: Expr::Lambda { body: first, .. }, .. },
        StringSegment::Text { text: two, .. },
        StringSegment::Expression { value: Expr::Lambda { body: second, .. }, .. },
        StringSegment::Text { text: three, .. },
        StringSegment::Expression { value: Expr::Lambda { body: third, .. }, .. },
        StringSegment::Text { text: four, .. },
        StringSegment::Expression { value: Expr::Lambda { body: fourth, .. }, .. },
        StringSegment::Text { text: five, .. },
        StringSegment::Expression { value: Expr::Lambda { body: fifth, .. }, .. },
        StringSegment::Text { text: suffix, .. },
    ] = segments.as_slice()
    else {
        panic!("a sibling closure or trailing text was lost: {segments:?}");
    };
    assert_eq!(one, "one ");
    assert_eq!(two, " two ");
    assert_eq!(three, " three ");
    assert_eq!(four, " four ");
    assert_eq!(five, " five ");
    assert_eq!(suffix, " end");

    for (label, closure_body, expected_arms) in [
        ("first", first, 1),
        ("second", second, 2),
        ("third", third, 1),
        ("fourth", fourth, 2),
        ("fifth", fifth, 1),
    ] {
        let Expr::Block { statements, tail: Some(closure_tail), .. } = closure_body.as_ref() else {
            panic!("{label} sibling closure lost its case or following tail");
        };
        let [Statement::Let { .. }, Statement::Control { value: Expr::Control { arms, .. }, .. }] =
            statements.as_slice()
        else {
            panic!("{label} closure lost its statement boundary: {statements:?}");
        };
        assert_eq!(arms.len(), expected_arms, "{label} case: {arms:?}");
        let Expr::Lambda { body: suffix_body, .. } = closure_tail.as_ref() else {
            panic!("{label} closure tail was lost");
        };
        let Expr::Block { tail: Some(suffix_tail), .. } = suffix_body.as_ref() else {
            panic!("{label} following closure lost its case tail");
        };
        assert!(matches!(suffix_tail.as_ref(), Expr::Control { .. }));
    }

    // The reference is silent on odd-position recoveries across five siblings
    // before literal text; preserve intervening closures and the final segment.
}

#[test]
fn even_sibling_recoveries_preserve_six_closures_before_text_tail() {
    let source = include_str!("fixtures/malformed-even-sibling-closures-before-text-tail.orna");
    let parsed = parse_module(source);

    let malformed_patterns = [("false 1", 6), ("false 3", 6), ("false 5", 6)];
    assert_eq!(parsed.diagnostics.len(), malformed_patterns.len(), "{:?}", parsed.diagnostics);
    for (diagnostic, (pattern, offset)) in parsed.diagnostics.iter().zip(malformed_patterns) {
        let start = source.find(pattern).expect("malformed sibling arm") + offset;
        assert_eq!(diagnostic.code, "ORNA-PARSE-001");
        assert_eq!(diagnostic.message, "expected `:` after case pattern");
        assert_eq!(diagnostic.span.start, start, "{diagnostic:?}");
        assert_eq!(diagnostic.span.end, start + 1, "{diagnostic:?}");
    }

    let Declaration::Function { body, .. } = &parsed.value.items[0].declaration else {
        panic!("expected a function declaration");
    };
    let Expr::Block { tail: Some(root_tail), .. } = body else {
        panic!("expected the root case tail");
    };
    let Expr::Control { arms: root_arms, .. } = root_tail.as_ref() else {
        panic!("expected the root case expression");
    };
    let Expr::InterpolatedString { segments, .. } = &root_arms[0].body else {
        panic!("root arm lost its interpolated string");
    };
    let [
        StringSegment::Text { text: one, .. },
        StringSegment::Expression { value: Expr::Lambda { body: first, .. }, .. },
        StringSegment::Text { text: two, .. },
        StringSegment::Expression { value: Expr::Lambda { body: second, .. }, .. },
        StringSegment::Text { text: three, .. },
        StringSegment::Expression { value: Expr::Lambda { body: third, .. }, .. },
        StringSegment::Text { text: four, .. },
        StringSegment::Expression { value: Expr::Lambda { body: fourth, .. }, .. },
        StringSegment::Text { text: five, .. },
        StringSegment::Expression { value: Expr::Lambda { body: fifth, .. }, .. },
        StringSegment::Text { text: six, .. },
        StringSegment::Expression { value: Expr::Lambda { body: sixth, .. }, .. },
        StringSegment::Text { text: suffix, .. },
    ] = segments.as_slice()
    else {
        panic!("a sibling closure or trailing text was lost: {segments:?}");
    };
    assert_eq!(one, "one ");
    assert_eq!(two, " two ");
    assert_eq!(three, " three ");
    assert_eq!(four, " four ");
    assert_eq!(five, " five ");
    assert_eq!(six, " six ");
    assert_eq!(suffix, " end");

    for (label, closure_body, expected_arms) in [
        ("first", first, 2),
        ("second", second, 1),
        ("third", third, 2),
        ("fourth", fourth, 1),
        ("fifth", fifth, 2),
        ("sixth", sixth, 1),
    ] {
        let Expr::Block { statements, tail: Some(closure_tail), .. } = closure_body.as_ref() else {
            panic!("{label} sibling closure lost its case or following tail");
        };
        let [Statement::Let { .. }, Statement::Control { value: Expr::Control { arms, .. }, .. }] =
            statements.as_slice()
        else {
            panic!("{label} closure lost its statement boundary: {statements:?}");
        };
        assert_eq!(arms.len(), expected_arms, "{label} case: {arms:?}");
        let Expr::Lambda { body: suffix_body, .. } = closure_tail.as_ref() else {
            panic!("{label} closure tail was lost");
        };
        let Expr::Block { tail: Some(suffix_tail), .. } = suffix_body.as_ref() else {
            panic!("{label} following closure lost its case tail");
        };
        assert!(matches!(suffix_tail.as_ref(), Expr::Control { .. }));
    }

    // The reference is silent on even-position recoveries across six siblings
    // before literal text; preserve intervening closures and the final segment.
}

#[test]
fn odd_sibling_recoveries_preserve_seven_closures_before_text_tail() {
    let source = include_str!("fixtures/malformed-odd-seven-sibling-closures-before-text-tail.orna");
    let parsed = parse_module(source);

    let malformed_patterns = [("false 0", 6), ("false 2", 6), ("false 4", 6), ("false 6", 6)];
    assert_eq!(parsed.diagnostics.len(), malformed_patterns.len(), "{:?}", parsed.diagnostics);
    for (diagnostic, (pattern, offset)) in parsed.diagnostics.iter().zip(malformed_patterns) {
        let start = source.find(pattern).expect("malformed sibling arm") + offset;
        assert_eq!(diagnostic.code, "ORNA-PARSE-001");
        assert_eq!(diagnostic.message, "expected `:` after case pattern");
        assert_eq!(diagnostic.span.start, start, "{diagnostic:?}");
        assert_eq!(diagnostic.span.end, start + 1, "{diagnostic:?}");
    }

    let Declaration::Function { body, .. } = &parsed.value.items[0].declaration else {
        panic!("expected a function declaration");
    };
    let Expr::Block { tail: Some(root_tail), .. } = body else {
        panic!("expected the root case tail");
    };
    let Expr::Control { arms: root_arms, .. } = root_tail.as_ref() else {
        panic!("expected the root case expression");
    };
    let Expr::InterpolatedString { segments, .. } = &root_arms[0].body else {
        panic!("root arm lost its interpolated string");
    };
    let [
        StringSegment::Text { text: one, .. },
        StringSegment::Expression { value: Expr::Lambda { body: first, .. }, .. },
        StringSegment::Text { text: two, .. },
        StringSegment::Expression { value: Expr::Lambda { body: second, .. }, .. },
        StringSegment::Text { text: three, .. },
        StringSegment::Expression { value: Expr::Lambda { body: third, .. }, .. },
        StringSegment::Text { text: four, .. },
        StringSegment::Expression { value: Expr::Lambda { body: fourth, .. }, .. },
        StringSegment::Text { text: five, .. },
        StringSegment::Expression { value: Expr::Lambda { body: fifth, .. }, .. },
        StringSegment::Text { text: six, .. },
        StringSegment::Expression { value: Expr::Lambda { body: sixth, .. }, .. },
        StringSegment::Text { text: seven, .. },
        StringSegment::Expression { value: Expr::Lambda { body: seventh, .. }, .. },
        StringSegment::Text { text: suffix, .. },
    ] = segments.as_slice()
    else {
        panic!("a sibling closure or trailing text was lost: {segments:?}");
    };
    assert_eq!(one, "one ");
    assert_eq!(two, " two ");
    assert_eq!(three, " three ");
    assert_eq!(four, " four ");
    assert_eq!(five, " five ");
    assert_eq!(six, " six ");
    assert_eq!(seven, " seven ");
    assert_eq!(suffix, " end");

    for (label, closure_body, expected_arms) in [
        ("first", first, 1),
        ("second", second, 2),
        ("third", third, 1),
        ("fourth", fourth, 2),
        ("fifth", fifth, 1),
        ("sixth", sixth, 2),
        ("seventh", seventh, 1),
    ] {
        let Expr::Block { statements, tail: Some(closure_tail), .. } = closure_body.as_ref() else {
            panic!("{label} sibling closure lost its case or following tail");
        };
        let [Statement::Let { .. }, Statement::Control { value: Expr::Control { arms, .. }, .. }] =
            statements.as_slice()
        else {
            panic!("{label} closure lost its statement boundary: {statements:?}");
        };
        assert_eq!(arms.len(), expected_arms, "{label} case: {arms:?}");
        let Expr::Lambda { body: suffix_body, .. } = closure_tail.as_ref() else {
            panic!("{label} closure tail was lost");
        };
        let Expr::Block { tail: Some(suffix_tail), .. } = suffix_body.as_ref() else {
            panic!("{label} following closure lost its case tail");
        };
        assert!(matches!(suffix_tail.as_ref(), Expr::Control { .. }));
    }

    // The reference is silent on odd-position recoveries across seven siblings
    // before literal text; preserve intervening closures and the final segment.
}

#[test]
fn even_sibling_recoveries_preserve_eight_closures_before_text_tail() {
    let source = include_str!("fixtures/malformed-even-eight-sibling-closures-before-text-tail.orna");
    let parsed = parse_module(source);

    let malformed_patterns = [("false 1", 6), ("false 3", 6), ("false 5", 6), ("false 7", 6)];
    assert_eq!(parsed.diagnostics.len(), malformed_patterns.len(), "{:?}", parsed.diagnostics);
    for (diagnostic, (pattern, offset)) in parsed.diagnostics.iter().zip(malformed_patterns) {
        let start = source.find(pattern).expect("malformed sibling arm") + offset;
        assert_eq!(diagnostic.code, "ORNA-PARSE-001");
        assert_eq!(diagnostic.message, "expected `:` after case pattern");
        assert_eq!(diagnostic.span.start, start, "{diagnostic:?}");
        assert_eq!(diagnostic.span.end, start + 1, "{diagnostic:?}");
    }

    let Declaration::Function { body, .. } = &parsed.value.items[0].declaration else {
        panic!("expected a function declaration");
    };
    let Expr::Block { tail: Some(root_tail), .. } = body else {
        panic!("expected the root case tail");
    };
    let Expr::Control { arms: root_arms, .. } = root_tail.as_ref() else {
        panic!("expected the root case expression");
    };
    let Expr::InterpolatedString { segments, .. } = &root_arms[0].body else {
        panic!("root arm lost its interpolated string");
    };
    let [
        StringSegment::Text { text: one, .. },
        StringSegment::Expression { value: Expr::Lambda { body: first, .. }, .. },
        StringSegment::Text { text: two, .. },
        StringSegment::Expression { value: Expr::Lambda { body: second, .. }, .. },
        StringSegment::Text { text: three, .. },
        StringSegment::Expression { value: Expr::Lambda { body: third, .. }, .. },
        StringSegment::Text { text: four, .. },
        StringSegment::Expression { value: Expr::Lambda { body: fourth, .. }, .. },
        StringSegment::Text { text: five, .. },
        StringSegment::Expression { value: Expr::Lambda { body: fifth, .. }, .. },
        StringSegment::Text { text: six, .. },
        StringSegment::Expression { value: Expr::Lambda { body: sixth, .. }, .. },
        StringSegment::Text { text: seven, .. },
        StringSegment::Expression { value: Expr::Lambda { body: seventh, .. }, .. },
        StringSegment::Text { text: eight, .. },
        StringSegment::Expression { value: Expr::Lambda { body: eighth, .. }, .. },
        StringSegment::Text { text: suffix, .. },
    ] = segments.as_slice()
    else {
        panic!("a sibling closure or trailing text was lost: {segments:?}");
    };
    assert_eq!(one, "one ");
    assert_eq!(two, " two ");
    assert_eq!(three, " three ");
    assert_eq!(four, " four ");
    assert_eq!(five, " five ");
    assert_eq!(six, " six ");
    assert_eq!(seven, " seven ");
    assert_eq!(eight, " eight ");
    assert_eq!(suffix, " end");

    for (label, closure_body, expected_arms) in [
        ("first", first, 2),
        ("second", second, 1),
        ("third", third, 2),
        ("fourth", fourth, 1),
        ("fifth", fifth, 2),
        ("sixth", sixth, 1),
        ("seventh", seventh, 2),
        ("eighth", eighth, 1),
    ] {
        let Expr::Block { statements, tail: Some(closure_tail), .. } = closure_body.as_ref() else {
            panic!("{label} sibling closure lost its case or following tail");
        };
        let [Statement::Let { .. }, Statement::Control { value: Expr::Control { arms, .. }, .. }] =
            statements.as_slice()
        else {
            panic!("{label} closure lost its statement boundary: {statements:?}");
        };
        assert_eq!(arms.len(), expected_arms, "{label} case: {arms:?}");
        let Expr::Lambda { body: suffix_body, .. } = closure_tail.as_ref() else {
            panic!("{label} closure tail was lost");
        };
        let Expr::Block { tail: Some(suffix_tail), .. } = suffix_body.as_ref() else {
            panic!("{label} following closure lost its case tail");
        };
        assert!(matches!(suffix_tail.as_ref(), Expr::Control { .. }));
    }

    // The reference is silent on even-position recoveries across eight siblings
    // before literal text; preserve intervening closures and the final segment.
}

#[test]
fn three_sibling_closure_tails_survive_independent_recoveries() {
    let source = include_str!("fixtures/malformed-recovery-with-three-sibling-closure-tails.orna");

    let malformed_patterns = [
        ("false 0", 6, 1),
        ("true 1", 5, 1),
        ("false 2", 6, 1),
        ("true 3", 5, 1),
        ("false 4", 6, 1),
        ("true 5", 5, 1),
        ("false 6", 6, 1),
        ("true 7", 5, 1),
        ("false 8", 6, 1),
        ("true 9", 5, 1),
        ("false 10", 6, 2),
        ("true 11", 5, 2),
    ];
    let parsed = parse_module(source);
    assert_eq!(parsed.diagnostics.len(), malformed_patterns.len(), "{:?}", parsed.diagnostics);
    for (diagnostic, (pattern, offset, width)) in parsed.diagnostics.iter().zip(malformed_patterns) {
        let start = source.find(pattern).expect("malformed arm") + offset;
        assert_eq!(diagnostic.code, "ORNA-PARSE-001");
        assert_eq!(diagnostic.message, "expected `:` after case pattern");
        assert_eq!(diagnostic.span.start, start, "{pattern}: {diagnostic:?}");
        assert_eq!(diagnostic.span.end, start + width, "{pattern}: {diagnostic:?}");
    }

    let Declaration::Function { body, .. } = &parsed.value.items[0].declaration else {
        panic!("expected a function declaration");
    };
    let Expr::Block { tail: Some(root_tail), .. } = body else {
        panic!("expected the root case tail");
    };
    let Expr::Control { arms: root_arms, .. } = root_tail.as_ref() else {
        panic!("expected the root case expression");
    };
    assert_eq!(root_arms.len(), 2, "{root_arms:?}");
    let Expr::InterpolatedString { segments, .. } = &root_arms[0].body else {
        panic!("root arm lost its interpolated string");
    };
    let [
        StringSegment::Text { text: prefix, .. },
        StringSegment::Expression {
            value: Expr::Lambda { body: outer_body, .. },
            ..
        },
        StringSegment::Text { text: middle, .. },
        StringSegment::Expression {
            value: Expr::Lambda { body: left_body, .. },
            ..
        },
        StringSegment::Text {
            text: conjunction,
            ..
        },
        StringSegment::Expression {
            value: Expr::Lambda { body: right_body, .. },
            ..
        },
        StringSegment::Text { text: suffix, .. },
    ] = segments.as_slice()
    else {
        panic!("a sibling closure interpolation was lost: {segments:?}");
    };
    assert_eq!(prefix, "prefix ");
    assert_eq!(middle, " middle ");
    assert_eq!(conjunction, " and ");
    assert_eq!(suffix, " end");

    for (label, closure_body) in [
        ("outer", outer_body),
        ("left", left_body),
        ("right", right_body),
    ] {
        let Expr::Block {
            statements,
            tail: Some(closure_tail),
            ..
        } = closure_body.as_ref()
        else {
            panic!("{label} closure lost its tail after recovery");
        };
        let [
            Statement::Let { .. },
            Statement::Control {
                value: Expr::Control { arms, .. },
                ..
            },
        ] = statements.as_slice()
        else {
            panic!("{label} recovered case lost its statement boundary: {statements:?}");
        };
        assert_eq!(arms.len(), 1, "{label} recovery lost its valid prefix: {arms:?}");
        assert!(matches!(&arms[0].body, Expr::Lambda { .. }));

        let Expr::Lambda { body: suffix_body, .. } = closure_tail.as_ref() else {
            panic!("{label} closure lost its following closure tail");
        };
        let Expr::Block { tail: Some(suffix_tail), .. } = suffix_body.as_ref() else {
            panic!("{label} following closure lost its case tail");
        };
        assert!(matches!(suffix_tail.as_ref(), Expr::Control { .. }));
    }

    // The reference is silent on three sibling closure recoveries in one
    // interpolated string; preserve every valid prefix and closure suffix.
}

#[test]
fn four_sibling_closure_tails_survive_recovery_across_the_string_tail() {
    let source = include_str!("fixtures/malformed-recovery-with-four-sibling-closure-tails.orna");
    let parsed = parse_module(source);

    let malformed_patterns = ["false 0", "false 1", "false 2", "false 3"];
    assert_eq!(parsed.diagnostics.len(), malformed_patterns.len(), "{:?}", parsed.diagnostics);
    for (diagnostic, pattern) in parsed.diagnostics.iter().zip(malformed_patterns) {
        let start = source.find(pattern).expect("malformed arm") + "false ".len();
        assert_eq!(diagnostic.code, "ORNA-PARSE-001");
        assert_eq!(diagnostic.message, "expected `:` after case pattern");
        assert_eq!(diagnostic.span.start, start, "{pattern}: {diagnostic:?}");
        assert_eq!(diagnostic.span.end, start + 1, "{pattern}: {diagnostic:?}");
    }

    let Declaration::Function { body, .. } = &parsed.value.items[0].declaration else {
        panic!("expected a function declaration");
    };
    let Expr::Block { tail: Some(root_tail), .. } = body else {
        panic!("expected the root case tail");
    };
    let Expr::Control { arms: root_arms, .. } = root_tail.as_ref() else {
        panic!("expected the root case expression");
    };
    assert_eq!(root_arms.len(), 2, "{root_arms:?}");
    let Expr::InterpolatedString { segments, .. } = &root_arms[0].body else {
        panic!("root arm lost its interpolated string");
    };
    let [
        StringSegment::Text { text: one, .. },
        StringSegment::Expression { value: Expr::Lambda { body: first, .. }, .. },
        StringSegment::Text { text: two, .. },
        StringSegment::Expression { value: Expr::Lambda { body: second, .. }, .. },
        StringSegment::Text { text: three, .. },
        StringSegment::Expression { value: Expr::Lambda { body: third, .. }, .. },
        StringSegment::Text { text: four, .. },
        StringSegment::Expression { value: Expr::Lambda { body: fourth, .. }, .. },
        StringSegment::Text { text: end, .. },
    ] = segments.as_slice()
    else {
        panic!("a sibling closure or string tail was lost: {segments:?}");
    };
    assert_eq!(one, "one ");
    assert_eq!(two, " two ");
    assert_eq!(three, " three ");
    assert_eq!(four, " four ");
    assert_eq!(end, " end");

    for (label, closure_body) in [
        ("first", first),
        ("second", second),
        ("third", third),
        ("fourth", fourth),
    ] {
        let Expr::Block { statements, tail: Some(closure_tail), .. } = closure_body.as_ref() else {
            panic!("{label} closure lost its tail after recovery");
        };
        let [
            Statement::Let { .. },
            Statement::Control { value: Expr::Control { arms, .. }, .. },
        ] = statements.as_slice()
        else {
            panic!("{label} recovered case lost its statement boundary: {statements:?}");
        };
        assert_eq!(arms.len(), 1, "{label} valid prefix was lost: {arms:?}");

        let Expr::Lambda { body: suffix_body, .. } = closure_tail.as_ref() else {
            panic!("{label} closure lost its following closure tail");
        };
        let Expr::Block { tail: Some(suffix_tail), .. } = suffix_body.as_ref() else {
            panic!("{label} following closure lost its case tail");
        };
        assert!(matches!(suffix_tail.as_ref(), Expr::Control { .. }));
    }

    // The reference is silent on four sibling recovery closures extending
    // through an interpolated string's final tail; retain each suffix.
}

#[test]
fn five_sibling_closure_tails_survive_recovery_across_the_string_tail() {
    let source = include_str!("fixtures/malformed-recovery-with-five-sibling-closure-tails.orna");
    let parsed = parse_module(source);

    let malformed_patterns = ["false 0", "false 1", "false 2", "false 3", "false 4"];
    assert_eq!(parsed.diagnostics.len(), malformed_patterns.len(), "{:?}", parsed.diagnostics);
    for (diagnostic, pattern) in parsed.diagnostics.iter().zip(malformed_patterns) {
        let start = source.find(pattern).expect("malformed arm") + "false ".len();
        assert_eq!(diagnostic.code, "ORNA-PARSE-001");
        assert_eq!(diagnostic.message, "expected `:` after case pattern");
        assert_eq!(diagnostic.span.start, start, "{pattern}: {diagnostic:?}");
        assert_eq!(diagnostic.span.end, start + 1, "{pattern}: {diagnostic:?}");
    }

    let Declaration::Function { body, .. } = &parsed.value.items[0].declaration else {
        panic!("expected a function declaration");
    };
    let Expr::Block { tail: Some(root_tail), .. } = body else {
        panic!("expected the root case tail");
    };
    let Expr::Control { arms: root_arms, .. } = root_tail.as_ref() else {
        panic!("expected the root case expression");
    };
    assert_eq!(root_arms.len(), 2, "{root_arms:?}");
    let Expr::InterpolatedString { segments, .. } = &root_arms[0].body else {
        panic!("root arm lost its interpolated string");
    };
    let [
        StringSegment::Text { text: one, .. },
        StringSegment::Expression { value: Expr::Lambda { body: first, .. }, .. },
        StringSegment::Text { text: two, .. },
        StringSegment::Expression { value: Expr::Lambda { body: second, .. }, .. },
        StringSegment::Text { text: three, .. },
        StringSegment::Expression { value: Expr::Lambda { body: third, .. }, .. },
        StringSegment::Text { text: four, .. },
        StringSegment::Expression { value: Expr::Lambda { body: fourth, .. }, .. },
        StringSegment::Text { text: five, .. },
        StringSegment::Expression { value: Expr::Lambda { body: fifth, .. }, .. },
        StringSegment::Text { text: end, .. },
    ] = segments.as_slice()
    else {
        panic!("a sibling closure or string tail was lost: {segments:?}");
    };
    assert_eq!(one, "one ");
    assert_eq!(two, " two ");
    assert_eq!(three, " three ");
    assert_eq!(four, " four ");
    assert_eq!(five, " five ");
    assert_eq!(end, " end");

    for (label, closure_body) in [
        ("first", first),
        ("second", second),
        ("third", third),
        ("fourth", fourth),
        ("fifth", fifth),
    ] {
        let Expr::Block { statements, tail: Some(closure_tail), .. } = closure_body.as_ref() else {
            panic!("{label} closure lost its tail after recovery");
        };
        let [
            Statement::Let { .. },
            Statement::Control { value: Expr::Control { arms, .. }, .. },
        ] = statements.as_slice()
        else {
            panic!("{label} recovered case lost its statement boundary: {statements:?}");
        };
        assert_eq!(arms.len(), 1, "{label} valid prefix was lost: {arms:?}");

        let Expr::Lambda { body: suffix_body, .. } = closure_tail.as_ref() else {
            panic!("{label} closure lost its following closure tail");
        };
        let Expr::Block { tail: Some(suffix_tail), .. } = suffix_body.as_ref() else {
            panic!("{label} following closure lost its case tail");
        };
        assert!(matches!(suffix_tail.as_ref(), Expr::Control { .. }));
    }

    // The reference is silent on five sibling recovery closures extending
    // through an interpolated string's final tail; retain each suffix.
}

#[test]
fn one_recovery_preserves_five_sibling_closure_tails_to_the_final_segment() {
    let source = include_str!("fixtures/malformed-first-of-five-sibling-closure-tails.orna");
    let parsed = parse_module(source);

    assert_eq!(parsed.diagnostics.len(), 1, "{:?}", parsed.diagnostics);
    let diagnostic = &parsed.diagnostics[0];
    let start = source.find("false 0").expect("malformed arm") + "false ".len();
    assert_eq!(diagnostic.code, "ORNA-PARSE-001");
    assert_eq!(diagnostic.message, "expected `:` after case pattern");
    assert_eq!(diagnostic.span.start, start, "{diagnostic:?}");
    assert_eq!(diagnostic.span.end, start + 1, "{diagnostic:?}");

    let Declaration::Function { body, .. } = &parsed.value.items[0].declaration else {
        panic!("expected a function declaration");
    };
    let Expr::Block { tail: Some(root_tail), .. } = body else {
        panic!("expected the root case tail");
    };
    let Expr::Control { arms: root_arms, .. } = root_tail.as_ref() else {
        panic!("expected the root case expression");
    };
    assert_eq!(root_arms.len(), 2, "{root_arms:?}");
    let Expr::InterpolatedString { segments, .. } = &root_arms[0].body else {
        panic!("root arm lost its interpolated string");
    };
    let [
        StringSegment::Text { text: one, .. },
        StringSegment::Expression { value: Expr::Lambda { body: first, .. }, .. },
        StringSegment::Text { text: two, .. },
        StringSegment::Expression { value: Expr::Lambda { body: second, .. }, .. },
        StringSegment::Text { text: three, .. },
        StringSegment::Expression { value: Expr::Lambda { body: third, .. }, .. },
        StringSegment::Text { text: four, .. },
        StringSegment::Expression { value: Expr::Lambda { body: fourth, .. }, .. },
        StringSegment::Text { text: five, .. },
        StringSegment::Expression { value: Expr::Lambda { body: fifth, .. }, .. },
        StringSegment::Text { text: end, .. },
    ] = segments.as_slice()
    else {
        panic!("a sibling closure or final string segment was lost: {segments:?}");
    };
    assert_eq!(one, "one ");
    assert_eq!(two, " two ");
    assert_eq!(three, " three ");
    assert_eq!(four, " four ");
    assert_eq!(five, " five ");
    assert_eq!(end, " end");

    for (label, expected_arms, closure_body) in [
        ("first", 1, first),
        ("second", 2, second),
        ("third", 2, third),
        ("fourth", 2, fourth),
        ("fifth", 2, fifth),
    ] {
        let Expr::Block { statements, tail: Some(closure_tail), .. } = closure_body.as_ref() else {
            panic!("{label} closure lost its tail");
        };
        let [
            Statement::Let { .. },
            Statement::Control { value: Expr::Control { arms, .. }, .. },
        ] = statements.as_slice()
        else {
            panic!("{label} case lost its statement boundary: {statements:?}");
        };
        assert_eq!(arms.len(), expected_arms, "{label} case: {arms:?}");

        let Expr::Lambda { body: suffix_body, .. } = closure_tail.as_ref() else {
            panic!("{label} closure lost its following closure tail");
        };
        let Expr::Block { tail: Some(suffix_tail), .. } = suffix_body.as_ref() else {
            panic!("{label} following closure lost its case tail");
        };
        assert!(matches!(suffix_tail.as_ref(), Expr::Control { .. }));
    }

    // The reference is silent on later sibling closure tails following one
    // recovered case; retain all five interpolation segments through the end.
}

#[test]
fn middle_recovery_preserves_five_sibling_closure_tails_in_order() {
    let source = include_str!("fixtures/malformed-middle-of-five-sibling-closure-tails.orna");
    let parsed = parse_module(source);

    assert_eq!(parsed.diagnostics.len(), 1, "{:?}", parsed.diagnostics);
    let diagnostic = &parsed.diagnostics[0];
    let start = source.find("false 2").expect("malformed middle arm") + "false ".len();
    assert_eq!(diagnostic.code, "ORNA-PARSE-001");
    assert_eq!(diagnostic.message, "expected `:` after case pattern");
    assert_eq!(diagnostic.span.start, start, "{diagnostic:?}");
    assert_eq!(diagnostic.span.end, start + 1, "{diagnostic:?}");

    let Declaration::Function { body, .. } = &parsed.value.items[0].declaration else {
        panic!("expected a function declaration");
    };
    let Expr::Block { tail: Some(root_tail), .. } = body else {
        panic!("expected the root case tail");
    };
    let Expr::Control { arms: root_arms, .. } = root_tail.as_ref() else {
        panic!("expected the root case expression");
    };
    assert_eq!(root_arms.len(), 2, "{root_arms:?}");
    let Expr::InterpolatedString { segments, .. } = &root_arms[0].body else {
        panic!("root arm lost its interpolated string");
    };
    let [
        StringSegment::Text { text: one, .. },
        StringSegment::Expression { value: Expr::Lambda { body: first, .. }, .. },
        StringSegment::Text { text: two, .. },
        StringSegment::Expression { value: Expr::Lambda { body: second, .. }, .. },
        StringSegment::Text { text: three, .. },
        StringSegment::Expression { value: Expr::Lambda { body: third, .. }, .. },
        StringSegment::Text { text: four, .. },
        StringSegment::Expression { value: Expr::Lambda { body: fourth, .. }, .. },
        StringSegment::Text { text: five, .. },
        StringSegment::Expression { value: Expr::Lambda { body: fifth, .. }, .. },
        StringSegment::Text { text: end, .. },
    ] = segments.as_slice()
    else {
        panic!("a sibling closure or final string segment was lost: {segments:?}");
    };
    assert_eq!(one, "one ");
    assert_eq!(two, " two ");
    assert_eq!(three, " three ");
    assert_eq!(four, " four ");
    assert_eq!(five, " five ");
    assert_eq!(end, " end");

    for (label, expected_arms, closure_body) in [
        ("first", 2, first),
        ("second", 2, second),
        ("third", 1, third),
        ("fourth", 2, fourth),
        ("fifth", 2, fifth),
    ] {
        let Expr::Block { statements, tail: Some(closure_tail), .. } = closure_body.as_ref() else {
            panic!("{label} closure lost its tail");
        };
        let [
            Statement::Let { .. },
            Statement::Control { value: Expr::Control { arms, .. }, .. },
        ] = statements.as_slice()
        else {
            panic!("{label} case lost its statement boundary: {statements:?}");
        };
        assert_eq!(arms.len(), expected_arms, "{label} case: {arms:?}");

        let Expr::Lambda { body: suffix_body, .. } = closure_tail.as_ref() else {
            panic!("{label} closure lost its following closure tail");
        };
        let Expr::Block { tail: Some(suffix_tail), .. } = suffix_body.as_ref() else {
            panic!("{label} following closure lost its case tail");
        };
        assert!(matches!(suffix_tail.as_ref(), Expr::Control { .. }));
    }

    // The reference is silent on recovery between sibling closure tails;
    // retain both preceding and following suffixes in their string positions.
}

#[test]
fn final_recovery_preserves_five_sibling_closure_tails_through_end() {
    let source = include_str!("fixtures/malformed-last-of-five-sibling-closure-tails.orna");
    let parsed = parse_module(source);

    assert_eq!(parsed.diagnostics.len(), 1, "{:?}", parsed.diagnostics);
    let diagnostic = &parsed.diagnostics[0];
    let start = source.find("false 4").expect("malformed final arm") + "false ".len();
    assert_eq!(diagnostic.code, "ORNA-PARSE-001");
    assert_eq!(diagnostic.message, "expected `:` after case pattern");
    assert_eq!(diagnostic.span.start, start, "{diagnostic:?}");
    assert_eq!(diagnostic.span.end, start + 1, "{diagnostic:?}");

    let Declaration::Function { body, .. } = &parsed.value.items[0].declaration else {
        panic!("expected a function declaration");
    };
    let Expr::Block { tail: Some(root_tail), .. } = body else {
        panic!("expected the root case tail");
    };
    let Expr::Control { arms: root_arms, .. } = root_tail.as_ref() else {
        panic!("expected the root case expression");
    };
    assert_eq!(root_arms.len(), 2, "{root_arms:?}");
    let Expr::InterpolatedString { segments, .. } = &root_arms[0].body else {
        panic!("root arm lost its interpolated string");
    };
    let [
        StringSegment::Text { text: one, .. },
        StringSegment::Expression { value: Expr::Lambda { body: first, .. }, .. },
        StringSegment::Text { text: two, .. },
        StringSegment::Expression { value: Expr::Lambda { body: second, .. }, .. },
        StringSegment::Text { text: three, .. },
        StringSegment::Expression { value: Expr::Lambda { body: third, .. }, .. },
        StringSegment::Text { text: four, .. },
        StringSegment::Expression { value: Expr::Lambda { body: fourth, .. }, .. },
        StringSegment::Text { text: five, .. },
        StringSegment::Expression { value: Expr::Lambda { body: fifth, .. }, .. },
        StringSegment::Text { text: end, .. },
    ] = segments.as_slice()
    else {
        panic!("a sibling closure or final string segment was lost: {segments:?}");
    };
    assert_eq!(one, "one ");
    assert_eq!(two, " two ");
    assert_eq!(three, " three ");
    assert_eq!(four, " four ");
    assert_eq!(five, " five ");
    assert_eq!(end, " end");

    for (label, expected_arms, closure_body) in [
        ("first", 2, first),
        ("second", 2, second),
        ("third", 2, third),
        ("fourth", 2, fourth),
        ("fifth", 1, fifth),
    ] {
        let Expr::Block { statements, tail: Some(closure_tail), .. } = closure_body.as_ref() else {
            panic!("{label} closure lost its tail");
        };
        let [
            Statement::Let { .. },
            Statement::Control { value: Expr::Control { arms, .. }, .. },
        ] = statements.as_slice()
        else {
            panic!("{label} case lost its statement boundary: {statements:?}");
        };
        assert_eq!(arms.len(), expected_arms, "{label} case: {arms:?}");

        let Expr::Lambda { body: suffix_body, .. } = closure_tail.as_ref() else {
            panic!("{label} closure lost its following closure tail");
        };
        let Expr::Block { tail: Some(suffix_tail), .. } = suffix_body.as_ref() else {
            panic!("{label} following closure lost its case tail");
        };
        assert!(matches!(suffix_tail.as_ref(), Expr::Control { .. }));
    }

    // The reference is silent on recovery in the final sibling closure;
    // retain all preceding tails and the final recovered closure suffix.
}

#[test]
fn penultimate_recovery_preserves_final_sibling_closure_tail() {
    let source = include_str!("fixtures/malformed-penultimate-of-five-sibling-closure-tails.orna");
    let parsed = parse_module(source);

    assert_eq!(parsed.diagnostics.len(), 1, "{:?}", parsed.diagnostics);
    let diagnostic = &parsed.diagnostics[0];
    let start = source.find("false 3").expect("malformed penultimate arm") + "false ".len();
    assert_eq!(diagnostic.code, "ORNA-PARSE-001");
    assert_eq!(diagnostic.message, "expected `:` after case pattern");
    assert_eq!(diagnostic.span.start, start, "{diagnostic:?}");
    assert_eq!(diagnostic.span.end, start + 1, "{diagnostic:?}");

    let Declaration::Function { body, .. } = &parsed.value.items[0].declaration else {
        panic!("expected a function declaration");
    };
    let Expr::Block { tail: Some(root_tail), .. } = body else {
        panic!("expected the root case tail");
    };
    let Expr::Control { arms: root_arms, .. } = root_tail.as_ref() else {
        panic!("expected the root case expression");
    };
    assert_eq!(root_arms.len(), 2, "{root_arms:?}");
    let Expr::InterpolatedString { segments, .. } = &root_arms[0].body else {
        panic!("root arm lost its interpolated string");
    };
    let [
        StringSegment::Text { text: one, .. },
        StringSegment::Expression { value: Expr::Lambda { body: first, .. }, .. },
        StringSegment::Text { text: two, .. },
        StringSegment::Expression { value: Expr::Lambda { body: second, .. }, .. },
        StringSegment::Text { text: three, .. },
        StringSegment::Expression { value: Expr::Lambda { body: third, .. }, .. },
        StringSegment::Text { text: four, .. },
        StringSegment::Expression { value: Expr::Lambda { body: fourth, .. }, .. },
        StringSegment::Text { text: five, .. },
        StringSegment::Expression { value: Expr::Lambda { body: fifth, .. }, .. },
        StringSegment::Text { text: end, .. },
    ] = segments.as_slice()
    else {
        panic!("a sibling closure or final string segment was lost: {segments:?}");
    };
    assert_eq!(one, "one ");
    assert_eq!(two, " two ");
    assert_eq!(three, " three ");
    assert_eq!(four, " four ");
    assert_eq!(five, " five ");
    assert_eq!(end, " end");

    for (label, expected_arms, closure_body) in [
        ("first", 2, first),
        ("second", 2, second),
        ("third", 2, third),
        ("fourth", 1, fourth),
        ("fifth", 2, fifth),
    ] {
        let Expr::Block { statements, tail: Some(closure_tail), .. } = closure_body.as_ref() else {
            panic!("{label} closure lost its tail");
        };
        let [
            Statement::Let { .. },
            Statement::Control { value: Expr::Control { arms, .. }, .. },
        ] = statements.as_slice()
        else {
            panic!("{label} case lost its statement boundary: {statements:?}");
        };
        assert_eq!(arms.len(), expected_arms, "{label} case: {arms:?}");

        let Expr::Lambda { body: suffix_body, .. } = closure_tail.as_ref() else {
            panic!("{label} closure lost its following closure tail");
        };
        let Expr::Block { tail: Some(suffix_tail), .. } = suffix_body.as_ref() else {
            panic!("{label} following closure lost its case tail");
        };
        assert!(matches!(suffix_tail.as_ref(), Expr::Control { .. }));
    }

    // The reference is silent on recovery in the penultimate sibling;
    // retain the final closure's tail after the earlier siblings.
}

#[test]
fn final_sibling_closure_tail_survives_at_string_end() {
    let source = include_str!("fixtures/malformed-final-sibling-closure-at-string-end.orna");
    let parsed = parse_module(source);

    assert_eq!(parsed.diagnostics.len(), 1, "{:?}", parsed.diagnostics);
    let diagnostic = &parsed.diagnostics[0];
    let start = source.find("false 4").expect("malformed final arm") + "false ".len();
    assert_eq!(diagnostic.code, "ORNA-PARSE-001");
    assert_eq!(diagnostic.message, "expected `:` after case pattern");
    assert_eq!(diagnostic.span.start, start, "{diagnostic:?}");
    assert_eq!(diagnostic.span.end, start + 1, "{diagnostic:?}");

    let Declaration::Function { body, .. } = &parsed.value.items[0].declaration else {
        panic!("expected a function declaration");
    };
    let Expr::Block { tail: Some(root_tail), .. } = body else {
        panic!("expected the root case tail");
    };
    let Expr::Control { arms: root_arms, .. } = root_tail.as_ref() else {
        panic!("expected the root case expression");
    };
    assert_eq!(root_arms.len(), 2, "{root_arms:?}");
    let Expr::InterpolatedString { segments, .. } = &root_arms[0].body else {
        panic!("root arm lost its interpolated string");
    };
    let [
        StringSegment::Text { text: one, .. },
        StringSegment::Expression { value: Expr::Lambda { body: first, .. }, .. },
        StringSegment::Text { text: two, .. },
        StringSegment::Expression { value: Expr::Lambda { body: second, .. }, .. },
        StringSegment::Text { text: three, .. },
        StringSegment::Expression { value: Expr::Lambda { body: third, .. }, .. },
        StringSegment::Text { text: four, .. },
        StringSegment::Expression { value: Expr::Lambda { body: fourth, .. }, .. },
        StringSegment::Text { text: five, .. },
        StringSegment::Expression { value: Expr::Lambda { body: fifth, .. }, .. },
    ] = segments.as_slice()
    else {
        panic!("a sibling closure or final interpolation was lost: {segments:?}");
    };
    assert_eq!(one, "one ");
    assert_eq!(two, " two ");
    assert_eq!(three, " three ");
    assert_eq!(four, " four ");
    assert_eq!(five, " five ");

    for (label, expected_arms, closure_body) in [
        ("first", 2, first),
        ("second", 2, second),
        ("third", 2, third),
        ("fourth", 2, fourth),
        ("fifth", 1, fifth),
    ] {
        let Expr::Block { statements, tail: Some(closure_tail), .. } = closure_body.as_ref() else {
            panic!("{label} closure lost its tail");
        };
        let [
            Statement::Let { .. },
            Statement::Control { value: Expr::Control { arms, .. }, .. },
        ] = statements.as_slice()
        else {
            panic!("{label} case lost its statement boundary: {statements:?}");
        };
        assert_eq!(arms.len(), expected_arms, "{label} case: {arms:?}");

        let Expr::Lambda { body: suffix_body, .. } = closure_tail.as_ref() else {
            panic!("{label} closure lost its following closure tail");
        };
        let Expr::Block { tail: Some(suffix_tail), .. } = suffix_body.as_ref() else {
            panic!("{label} following closure lost its case tail");
        };
        assert!(matches!(suffix_tail.as_ref(), Expr::Control { .. }));
    }

    // The reference is silent on a recovered closure interpolation at the
    // string boundary; preserve its nested tail as the final segment.
}

#[test]
fn final_sibling_closure_tail_preserves_one_character_string_suffix() {
    let source = include_str!("fixtures/malformed-final-sibling-closure-before-s-suffix.orna");
    let parsed = parse_module(source);

    assert_eq!(parsed.diagnostics.len(), 1, "{:?}", parsed.diagnostics);
    let diagnostic = &parsed.diagnostics[0];
    let start = source.find("false 4").expect("malformed final arm") + "false ".len();
    assert_eq!(diagnostic.code, "ORNA-PARSE-001");
    assert_eq!(diagnostic.message, "expected `:` after case pattern");
    assert_eq!(diagnostic.span.start, start, "{diagnostic:?}");
    assert_eq!(diagnostic.span.end, start + 1, "{diagnostic:?}");

    let Declaration::Function { body, .. } = &parsed.value.items[0].declaration else {
        panic!("expected a function declaration");
    };
    let Expr::Block { tail: Some(root_tail), .. } = body else {
        panic!("expected the root case tail");
    };
    let Expr::Control { arms: root_arms, .. } = root_tail.as_ref() else {
        panic!("expected the root case expression");
    };
    assert_eq!(root_arms.len(), 2, "{root_arms:?}");
    let Expr::InterpolatedString { segments, .. } = &root_arms[0].body else {
        panic!("root arm lost its interpolated string");
    };
    let [
        StringSegment::Text { text: one, .. },
        StringSegment::Expression { value: Expr::Lambda { body: first, .. }, .. },
        StringSegment::Text { text: two, .. },
        StringSegment::Expression { value: Expr::Lambda { body: second, .. }, .. },
        StringSegment::Text { text: three, .. },
        StringSegment::Expression { value: Expr::Lambda { body: third, .. }, .. },
        StringSegment::Text { text: four, .. },
        StringSegment::Expression { value: Expr::Lambda { body: fourth, .. }, .. },
        StringSegment::Text { text: five, .. },
        StringSegment::Expression { value: Expr::Lambda { body: fifth, .. }, .. },
        StringSegment::Text { text: suffix, .. },
    ] = segments.as_slice()
    else {
        panic!("a sibling closure or its string suffix was lost: {segments:?}");
    };
    assert_eq!(one, "one ");
    assert_eq!(two, " two ");
    assert_eq!(three, " three ");
    assert_eq!(four, " four ");
    assert_eq!(five, " five ");
    assert_eq!(suffix, "s");

    for (label, expected_arms, closure_body) in [
        ("first", 2, first),
        ("second", 2, second),
        ("third", 2, third),
        ("fourth", 2, fourth),
        ("fifth", 1, fifth),
    ] {
        let Expr::Block { statements, tail: Some(closure_tail), .. } = closure_body.as_ref() else {
            panic!("{label} closure lost its tail");
        };
        let [
            Statement::Let { .. },
            Statement::Control { value: Expr::Control { arms, .. }, .. },
        ] = statements.as_slice()
        else {
            panic!("{label} case lost its statement boundary: {statements:?}");
        };
        assert_eq!(arms.len(), expected_arms, "{label} case: {arms:?}");

        let Expr::Lambda { body: suffix_body, .. } = closure_tail.as_ref() else {
            panic!("{label} closure lost its following closure tail");
        };
        let Expr::Block { tail: Some(suffix_tail), .. } = suffix_body.as_ref() else {
            panic!("{label} following closure lost its case tail");
        };
        assert!(matches!(suffix_tail.as_ref(), Expr::Control { .. }));
    }

    // The reference is silent on a final recovered sibling before a one-byte
    // string suffix; keep both the closure tail and suffix segment.
}

#[test]
fn final_sibling_closure_tail_preserves_following_text_tail() {
    let source = include_str!("fixtures/malformed-final-sibling-closure-before-tail.orna");
    let parsed = parse_module(source);

    assert_eq!(parsed.diagnostics.len(), 1, "{:?}", parsed.diagnostics);
    let diagnostic = &parsed.diagnostics[0];
    let start = source.find("false 4").expect("malformed final arm") + "false ".len();
    assert_eq!(diagnostic.code, "ORNA-PARSE-001");
    assert_eq!(diagnostic.message, "expected `:` after case pattern");
    assert_eq!(diagnostic.span.start, start, "{diagnostic:?}");
    assert_eq!(diagnostic.span.end, start + 1, "{diagnostic:?}");

    let Declaration::Function { body, .. } = &parsed.value.items[0].declaration else {
        panic!("expected a function declaration");
    };
    let Expr::Block { tail: Some(root_tail), .. } = body else {
        panic!("expected the root case tail");
    };
    let Expr::Control { arms: root_arms, .. } = root_tail.as_ref() else {
        panic!("expected the root case expression");
    };
    assert_eq!(root_arms.len(), 2, "{root_arms:?}");
    let Expr::InterpolatedString { segments, .. } = &root_arms[0].body else {
        panic!("root arm lost its interpolated string");
    };
    let [
        StringSegment::Text { text: one, .. },
        StringSegment::Expression { value: Expr::Lambda { body: first, .. }, .. },
        StringSegment::Text { text: two, .. },
        StringSegment::Expression { value: Expr::Lambda { body: second, .. }, .. },
        StringSegment::Text { text: three, .. },
        StringSegment::Expression { value: Expr::Lambda { body: third, .. }, .. },
        StringSegment::Text { text: four, .. },
        StringSegment::Expression { value: Expr::Lambda { body: fourth, .. }, .. },
        StringSegment::Text { text: five, .. },
        StringSegment::Expression { value: Expr::Lambda { body: fifth, .. }, .. },
        StringSegment::Text { text: suffix, .. },
    ] = segments.as_slice()
    else {
        panic!("a sibling closure or trailing text was lost: {segments:?}");
    };
    assert_eq!(one, "one ");
    assert_eq!(two, " two ");
    assert_eq!(three, " three ");
    assert_eq!(four, " four ");
    assert_eq!(five, " five ");
    assert_eq!(suffix, " tail");

    for (label, expected_arms, closure_body) in [
        ("first", 2, first),
        ("second", 2, second),
        ("third", 2, third),
        ("fourth", 2, fourth),
        ("fifth", 1, fifth),
    ] {
        let Expr::Block { statements, tail: Some(closure_tail), .. } = closure_body.as_ref() else {
            panic!("{label} closure lost its tail");
        };
        let [
            Statement::Let { .. },
            Statement::Control { value: Expr::Control { arms, .. }, .. },
        ] = statements.as_slice()
        else {
            panic!("{label} case lost its statement boundary: {statements:?}");
        };
        assert_eq!(arms.len(), expected_arms, "{label} case: {arms:?}");

        let Expr::Lambda { body: suffix_body, .. } = closure_tail.as_ref() else {
            panic!("{label} closure lost its following closure tail");
        };
        let Expr::Block { tail: Some(suffix_tail), .. } = suffix_body.as_ref() else {
            panic!("{label} following closure lost its case tail");
        };
        assert!(matches!(suffix_tail.as_ref(), Expr::Control { .. }));
    }

    // The reference is silent on trailing literal text after final-sibling
    // recovery; retain the case tail before this string suffix.
}

#[test]
fn final_sibling_closure_tail_preserves_adjacent_interpolation_and_text() {
    let source = include_str!("fixtures/malformed-final-sibling-closure-before-interpolation-tail.orna");
    let parsed = parse_module(source);

    assert_eq!(parsed.diagnostics.len(), 1, "{:?}", parsed.diagnostics);
    let diagnostic = &parsed.diagnostics[0];
    let start = source.find("false 4").expect("malformed final arm") + "false ".len();
    assert_eq!(diagnostic.code, "ORNA-PARSE-001");
    assert_eq!(diagnostic.message, "expected `:` after case pattern");
    assert_eq!(diagnostic.span.start, start, "{diagnostic:?}");
    assert_eq!(diagnostic.span.end, start + 1, "{diagnostic:?}");

    let Declaration::Function { body, .. } = &parsed.value.items[0].declaration else {
        panic!("expected a function declaration");
    };
    let Expr::Block { tail: Some(root_tail), .. } = body else {
        panic!("expected the root case tail");
    };
    let Expr::Control { arms: root_arms, .. } = root_tail.as_ref() else {
        panic!("expected the root case expression");
    };
    assert_eq!(root_arms.len(), 2, "{root_arms:?}");
    let Expr::InterpolatedString { segments, .. } = &root_arms[0].body else {
        panic!("root arm lost its interpolated string");
    };
    let [
        StringSegment::Text { text: one, .. },
        StringSegment::Expression { value: Expr::Lambda { body: first, .. }, .. },
        StringSegment::Text { text: two, .. },
        StringSegment::Expression { value: Expr::Lambda { body: second, .. }, .. },
        StringSegment::Text { text: three, .. },
        StringSegment::Expression { value: Expr::Lambda { body: third, .. }, .. },
        StringSegment::Text { text: four, .. },
        StringSegment::Expression { value: Expr::Lambda { body: fourth, .. }, .. },
        StringSegment::Text { text: five, .. },
        StringSegment::Expression { value: Expr::Lambda { body: fifth, .. }, .. },
        StringSegment::Expression { value: Expr::Name { text: interpolation, .. }, .. },
        StringSegment::Text { text: tail, .. },
    ] = segments.as_slice()
    else {
        panic!("a sibling closure, adjacent interpolation, or tail was lost: {segments:?}");
    };
    assert_eq!(one, "one ");
    assert_eq!(two, " two ");
    assert_eq!(three, " three ");
    assert_eq!(four, " four ");
    assert_eq!(five, " five ");
    assert_eq!(interpolation, "flag");
    assert_eq!(tail, " tail");

    for (label, expected_arms, closure_body) in [
        ("first", 2, first),
        ("second", 2, second),
        ("third", 2, third),
        ("fourth", 2, fourth),
        ("fifth", 1, fifth),
    ] {
        let Expr::Block { statements, tail: Some(closure_tail), .. } = closure_body.as_ref() else {
            panic!("{label} closure lost its tail");
        };
        let [
            Statement::Let { .. },
            Statement::Control { value: Expr::Control { arms, .. }, .. },
        ] = statements.as_slice()
        else {
            panic!("{label} case lost its statement boundary: {statements:?}");
        };
        assert_eq!(arms.len(), expected_arms, "{label} case: {arms:?}");

        let Expr::Lambda { body: suffix_body, .. } = closure_tail.as_ref() else {
            panic!("{label} closure lost its following closure tail");
        };
        let Expr::Block { tail: Some(suffix_tail), .. } = suffix_body.as_ref() else {
            panic!("{label} following closure lost its case tail");
        };
        assert!(matches!(suffix_tail.as_ref(), Expr::Control { .. }));
    }

    // The reference is silent on an adjacent interpolation after final-sibling
    // recovery; keep its nested tail, the following expression, and text tail.
}

#[test]
fn final_sibling_closure_tail_preserves_adjacent_closure_and_text() {
    let source = include_str!("fixtures/malformed-final-sibling-closure-before-adjacent-closure-tail.orna");
    let parsed = parse_module(source);

    assert_eq!(parsed.diagnostics.len(), 1, "{:?}", parsed.diagnostics);
    let diagnostic = &parsed.diagnostics[0];
    let start = source.find("false 4").expect("malformed final arm") + "false ".len();
    assert_eq!(diagnostic.code, "ORNA-PARSE-001");
    assert_eq!(diagnostic.message, "expected `:` after case pattern");
    assert_eq!(diagnostic.span.start, start, "{diagnostic:?}");
    assert_eq!(diagnostic.span.end, start + 1, "{diagnostic:?}");

    let Declaration::Function { body, .. } = &parsed.value.items[0].declaration else {
        panic!("expected a function declaration");
    };
    let Expr::Block { tail: Some(root_tail), .. } = body else {
        panic!("expected the root case tail");
    };
    let Expr::Control { arms: root_arms, .. } = root_tail.as_ref() else {
        panic!("expected the root case expression");
    };
    assert_eq!(root_arms.len(), 2, "{root_arms:?}");
    let Expr::InterpolatedString { segments, .. } = &root_arms[0].body else {
        panic!("root arm lost its interpolated string");
    };
    assert_eq!(segments.len(), 12, "{segments:?}");
    assert!(matches!(&segments[0], StringSegment::Text { text, .. } if text == "one "));
    assert!(matches!(&segments[2], StringSegment::Text { text, .. } if text == " two "));
    assert!(matches!(&segments[4], StringSegment::Text { text, .. } if text == " three "));
    assert!(matches!(&segments[6], StringSegment::Text { text, .. } if text == " four "));
    assert!(matches!(&segments[8], StringSegment::Text { text, .. } if text == " five "));
    for (index, segment) in segments.iter().enumerate().skip(1).step_by(2).take(5) {
        let StringSegment::Expression { value: Expr::Lambda { body, .. }, .. } = segment else {
            panic!("closure interpolation {index} was lost: {segments:?}");
        };
        let Expr::Block { tail: Some(closure_tail), .. } = body.as_ref() else {
            panic!("closure interpolation {index} lost its tail");
        };
        assert!(matches!(closure_tail.as_ref(), Expr::Lambda { .. }));
    }
    let StringSegment::Expression { value: Expr::Lambda { body: adjacent_body, .. }, .. } = &segments[10] else {
        panic!("adjacent closure interpolation was lost: {segments:?}");
    };
    let Expr::Block { tail: Some(adjacent_tail), .. } = adjacent_body.as_ref() else {
        panic!("adjacent closure lost its case tail");
    };
    assert!(matches!(adjacent_tail.as_ref(), Expr::Control { .. }));
    assert!(matches!(&segments[11], StringSegment::Text { text, .. } if text == " tail"));

    // The reference is silent on an adjacent closure interpolation after final-sibling
    // recovery; preserve the recovered sibling and both following tail segments.
}

#[test]
fn final_sibling_closure_tail_preserves_text_before_interpolation() {
    let source = include_str!("fixtures/malformed-final-sibling-closure-before-text-interpolation-tail.orna");
    let parsed = parse_module(source);

    assert_eq!(parsed.diagnostics.len(), 1, "{:?}", parsed.diagnostics);
    let diagnostic = &parsed.diagnostics[0];
    let start = source.find("false 4").expect("malformed final arm") + "false ".len();
    assert_eq!(diagnostic.code, "ORNA-PARSE-001");
    assert_eq!(diagnostic.message, "expected `:` after case pattern");
    assert_eq!(diagnostic.span.start, start, "{diagnostic:?}");
    assert_eq!(diagnostic.span.end, start + 1, "{diagnostic:?}");

    let Declaration::Function { body, .. } = &parsed.value.items[0].declaration else {
        panic!("expected a function declaration");
    };
    let Expr::Block { tail: Some(root_tail), .. } = body else {
        panic!("expected the root case tail");
    };
    let Expr::Control { arms: root_arms, .. } = root_tail.as_ref() else {
        panic!("expected the root case expression");
    };
    let Expr::InterpolatedString { segments, .. } = &root_arms[0].body else {
        panic!("root arm lost its interpolated string");
    };
    assert_eq!(segments.len(), 13, "{segments:?}");
    for (index, expected) in ["one ", " two ", " three ", " four ", " five "]
        .into_iter()
        .enumerate()
    {
        assert!(matches!(&segments[index * 2], StringSegment::Text { text, .. } if text == expected));
        let closure_index = index * 2 + 1;
        let StringSegment::Expression { value: Expr::Lambda { body, .. }, .. } = &segments[closure_index] else {
            panic!("sibling closure {index} was lost: {segments:?}");
        };
        let Expr::Block { tail: Some(closure_tail), .. } = body.as_ref() else {
            panic!("sibling closure {index} lost its tail");
        };
        assert!(matches!(closure_tail.as_ref(), Expr::Lambda { .. }));
    }
    assert!(matches!(&segments[10], StringSegment::Text { text, .. } if text == " tail "));
    assert!(matches!(
        &segments[11],
        StringSegment::Expression { value: Expr::Name { text, .. }, .. } if text == "flag"
    ));
    assert!(matches!(&segments[12], StringSegment::Text { text, .. } if text == "!"));

    // The reference is silent on text before an interpolation after final-sibling
    // recovery; retain the recovered closure, text, expression, and final text.
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
