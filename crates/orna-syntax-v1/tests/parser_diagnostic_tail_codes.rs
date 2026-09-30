use orna_syntax_v1::{
    Declaration, Expr, Statement, SyntaxDiagnostic, parse_expression, parse_module, parse_repl,
    parse_row,
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
