use orna_syntax_v1::parse_expression;

#[test]
fn pipeline_lambda_requires_parentheses() {
    let rejected = parse_expression(include_str!("fixtures/pipeline_lambda/unparenthesized.orna"));
    assert!(!rejected.is_ok(), "{:?}", rejected.diagnostics);
    assert_eq!(rejected.diagnostics.len(), 1);
    assert_eq!(rejected.diagnostics[0].code, "ORNA-PARSE-001");
    assert_eq!(
        rejected.diagnostics[0].message,
        "pipeline lambdas must be parenthesized"
    );

    let accepted = parse_expression(include_str!("fixtures/pipeline_lambda/parenthesized.orna"));
    assert!(accepted.is_ok(), "{:?}", accepted.diagnostics);
}
