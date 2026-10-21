//! Parser conformance for the W27 grammar audit against `grammar/orna.ebnf`.
//! Each test pins one audited divergence and states the production it follows.

use orna_syntax_v1::parse_expression;

fn expression_is_accepted(source: &str) {
    let parsed = parse_expression(source);
    assert!(parsed.is_ok(), "{source}: {:?}", parsed.diagnostics);
}

// W27-01 (grammar range_expression): the upper endpoint of a range is an
// additive_expression, so every primary expression, including the braced
// control forms, is a valid start.
#[test]
fn range_upper_endpoint_accepts_control_expressions() {
    for source in [
        "0..if c { 1 } else { 2 }",
        "0..=case c { true: 1, false: 2 }",
        "0..while c { }",
        "0..for i in xs { }",
        "0..loop { }",
    ] {
        expression_is_accepted(source);
    }
}

fn expression_is_rejected(source: &str) {
    let parsed = parse_expression(source);
    assert!(!parsed.is_ok(), "{source} should be rejected by the grammar");
}

// W27-02 (grammar pipeline_stage = postfix_expression): a stage cannot be
// followed by additive, multiplicative, power, or range operators, because
// those bind tighter than the pipeline operator and the grammar has no
// production that continues a stage with them.
#[test]
fn pipeline_stage_is_a_postfix_expression() {
    for source in ["1 | f + 1", "1 | f * 2", "1 | f .. 2", "1 | f ^ 2", "1 |? f - 1"] {
        expression_is_rejected(source);
    }
    for source in ["1 | f(2) | g.h", "a | f ?? b", "a | f == b", "1 | f || b"] {
        expression_is_accepted(source);
    }
}

// W27-03 (grammar: lambda is an expression-level production, not an operand):
// a lambda may only begin a complete expression, never the right operand of an
// infix operator.
#[test]
fn lambda_is_only_a_complete_expression() {
    for source in ["1 + x => x", "a == (b) => b", "1 + (a) => a", "f(1 + x => x)"] {
        expression_is_rejected(source);
    }
    for source in ["x => x + 1", "(a) => a", "f(x => x)"] {
        expression_is_accepted(source);
    }
}

// W27-04 (grammar continue_statement = "continue" ";"): `continue` takes no
// value, unlike `return` and `break`.
#[test]
fn continue_statement_takes_no_value() {
    expression_is_rejected("loop { continue 1; }");
    expression_is_accepted("loop { continue; }");
    expression_is_accepted("loop { break 1; }");
}
