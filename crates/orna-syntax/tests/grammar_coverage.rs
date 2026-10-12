use orna_syntax_v1::{Expr, parse_expression, parse_repl, parse_row};

// ORNA-TEST-005 at source/32-conformance.md:25 calls for syntax-alternative
// and precedence coverage. Before this file, syntax-v1's only direct
// parse_repl assertion was conformance.rs:44-50 (expression input), and its
// direct arithmetic AST check at conformance.rs:345-356 asserted only `+`.
const IMPORTS: &str = include_str!(
    "fixtures/reference/examples/valid/imports.orna"
);
const CONTROL_FLOW: &str = include_str!(
    "fixtures/reference/examples/valid/control-flow.orna"
);
const FUNCTION_DECLARATION: &str = include_str!(
    "fixtures/reference/examples/valid/function-expression.orna"
);
const REPL_EXPRESSION: &str =
    include_str!("../../orna-syntax-v1/tests/fixtures/entry-repl.orna");
const ROW_WITH_TERMINATOR: &str =
    include_str!("../../orna-syntax-v1/tests/fixtures/entry-row.orna");
const ROW_WITHOUT_TERMINATOR: &str =
    include_str!("fixtures/reference/examples/valid/row-body.orna");
const PRECEDENCE: &str = include_str!("fixtures/grammar-precedence.orna");

fn binary<'a>(expr: &'a Expr, expected_op: &str) -> (&'a Expr, &'a Expr) {
    match expr {
        Expr::Binary { lhs, op, rhs, .. } if op == expected_op => (lhs, rhs),
        other => panic!("expected binary {expected_op:?}, got {other:?}"),
    }
}

fn name(expr: &Expr, expected: &str) {
    assert!(
        matches!(expr, Expr::Name { text, .. } if text == expected),
        "expected name {expected:?}, got {expr:?}"
    );
}

#[test]
fn repl_input_covers_each_grammar_alternative() {
    // The frozen imports fixture supplies empty, alias, wildcard and selected
    // import tails; each line is one complete use_declaration input.
    let use_lines: Vec<_> = IMPORTS.lines().filter(|line| !line.trim().is_empty()).collect();
    assert_eq!(use_lines.len(), 4);
    for line in use_lines {
        let parsed = parse_repl(line);
        assert!(parsed.is_ok(), "use_declaration {line:?}: {:?}", parsed.diagnostics);
    }

    let let_statement = CONTROL_FLOW
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with("let "))
        .expect("reference control-flow fixture contains a let statement");
    let parsed = parse_repl(let_statement);
    assert!(parsed.is_ok(), "let_statement: {:?}", parsed.diagnostics);

    let parsed = parse_repl(FUNCTION_DECLARATION);
    assert!(parsed.is_ok(), "function_declaration: {:?}", parsed.diagnostics);

    let parsed = parse_repl(REPL_EXPRESSION);
    assert!(parsed.is_ok(), "expression: {:?}", parsed.diagnostics);
}

#[test]
fn row_unit_covers_both_optional_terminator_forms() {
    assert!(
        parse_row(ROW_WITH_TERMINATOR).is_ok(),
        "row with semicolon"
    );
    assert!(
        parse_row(ROW_WITHOUT_TERMINATOR).is_ok(),
        "row without semicolon"
    );
}

#[test]
fn expression_ast_covers_each_precedence_boundary() {
    let parsed = parse_expression(PRECEDENCE);
    assert!(parsed.is_ok(), "{:?}", parsed.diagnostics);

    let Expr::Lambda { body, .. } = &parsed.value else {
        panic!("anonymous function is the lowest-precedence expression: {:?}", parsed.value);
    };
    let (or_left, and) = binary(body, "||");
    name(or_left, "b");
    let (and_left, comparison) = binary(and, "&&");
    name(and_left, "c");
    let (comparison_left, coalesce) = binary(comparison, "<");
    name(comparison_left, "d");
    let (coalesce_left, pipeline) = binary(coalesce, "??");
    name(coalesce_left, "e");
    let (range, pipeline_stage) = binary(pipeline, "|");
    name(pipeline_stage, "k");

    let Expr::Range {
        lower: Some(additive),
        operator,
        upper: Some(multiplication),
        ..
    } = range
    else {
        panic!("range binds outside its additive endpoints: {range:?}");
    };
    assert_eq!(operator, "..");
    let (lower, upper) = binary(additive, "+");
    name(lower, "f");
    name(upper, "g");
    let (mult_left, power) = binary(multiplication, "*");
    name(mult_left, "h");
    let (power_left, unary) = binary(power, "^");
    name(power_left, "i");
    let Expr::Unary { op, rhs, .. } = unary else {
        panic!("unary binds as the power operand: {unary:?}");
    };
    assert_eq!(op, "-");
    assert!(matches!(
        rhs.as_ref(),
        Expr::Field { base, name: field, .. }
            if field == "field" && matches!(base.as_ref(), Expr::Name { text, .. } if text == "j")
    ));
}
