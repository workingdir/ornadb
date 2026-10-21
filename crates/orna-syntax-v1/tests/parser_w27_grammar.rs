//! Parser conformance for the W27 grammar audit against `grammar/orna.ebnf`.
//! Each test pins one audited divergence and states the production it follows.

use orna_syntax_v1::{Expr, Pattern, parse_expression, parse_module};

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

// W27-05 (grammar pattern: "(" pattern ")" is grouping; a tuple pattern needs
// a comma): a single parenthesised pattern is its inner pattern.
#[test]
fn parenthesised_single_pattern_is_grouping() {
    let parsed = parse_expression("case x { (a): 1 }");
    assert!(parsed.is_ok(), "{:?}", parsed.diagnostics);
    let Expr::Control { arms, .. } = parsed.value else {
        panic!("expected a case expression");
    };
    assert!(matches!(arms[0].pattern, Pattern::Name(..)), "{:?}", arms[0].pattern);
}

// W27-06 (grammar enum_record_pattern: qualified_variant requires a dot): a bare
// `Name { .. }` is not an enum record pattern.
#[test]
fn enum_record_pattern_requires_qualified_variant() {
    expression_is_accepted("case x { Shape.circle { r }: r }");
    expression_is_rejected("case x { Shape { r }: r }");
}

// W27-07 (grammar use_tail): alias and import-list names are identifiers (or `_`
// for an alias), never keywords.
#[test]
fn use_alias_and_import_names_are_identifiers() {
    for source in ["use a.b as c;", "use a.b as _;", "use a.{ b, c };", "use a.b.*;"] {
        let parsed = parse_module(source);
        assert!(parsed.is_ok(), "{source}: {:?}", parsed.diagnostics);
    }
    for source in ["use a.b as case;", "use a.{ case };"] {
        let parsed = parse_module(source);
        assert!(!parsed.is_ok(), "{source} must be rejected");
    }
}

// W27-08 (grammar lambda_parameters: identifier | "_" | parenthesised list): the
// keyword `self` is not a lambda parameter.
#[test]
fn self_is_not_a_bare_lambda_parameter() {
    expression_is_rejected("self => 1");
    expression_is_accepted("(a) => a");
}

// W27-09 (ORNA-PARSE-002, ORNA-CFLOW-010): a braced control-flow expression
// followed by another block item is a statement; the final unsemicolonated
// expression is the block tail. The implementation follows the normative
// prose, which overrides the grammar's looser statement production.
#[test]
fn braced_control_flow_statement_precedes_block_tail() {
    for source in [
        "fn f(): Int { if c { g() } 3 }",
        "fn f(): Int { case x { true: 1, false: 2 } 3 }",
    ] {
        let parsed = parse_module(source);
        assert!(parsed.is_ok(), "{source}: {:?}", parsed.diagnostics);
    }
}
