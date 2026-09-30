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
