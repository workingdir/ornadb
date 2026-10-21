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
