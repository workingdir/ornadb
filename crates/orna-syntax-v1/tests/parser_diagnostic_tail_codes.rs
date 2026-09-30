use orna_syntax_v1::{
    Declaration, Expr, Pattern, Statement, StringSegment, SyntaxDiagnostic, parse_expression,
    parse_module, parse_repl, parse_row,
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
