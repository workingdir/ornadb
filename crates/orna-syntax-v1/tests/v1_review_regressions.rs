//! Parse-only audit regressions for ornadb-gov5.17.11 / GitHub #781.
//!
//! Requirements below refer to immutable reference/Orna-1.0.0. Ignored tests
//! assert normative behavior and are expected to FAIL when explicitly run
//! against the reviewed parser. An ignored result is not a conformance pass.
//! Inputs stay in memory; no filesystem fixtures or temporary files are used.

use orna_syntax_v1::{Declaration, Expr, LiteralKind, parse_module};

fn accepted_body(source: &str) -> Expr {
    let parsed = parse_module(source);
    assert!(
        parsed.diagnostics.is_empty(),
        "expected syntactic acceptance of {source:?}: {:?}",
        parsed.diagnostics
    );
    let mut bodies = parsed.value.items.into_iter().filter_map(|item| {
        if let Declaration::Function { body, .. } = item.declaration {
            Some(body)
        } else {
            None
        }
    });
    let body = bodies.next().expect("expected a function body");
    assert!(bodies.next().is_none(), "expected exactly one function");
    body
}

fn rejected(source: &str) {
    let parsed = parse_module(source);
    assert!(
        !parsed.diagnostics.is_empty(),
        "expected syntax rejection before semantic analysis: {source:?}; AST: {:?}",
        parsed.value
    );
}

fn nominal_body(source: &str, expected_path: &[&str], expected_fields: &[&str]) {
    let body = accepted_body(source);
    let Expr::Nominal { path, fields, .. } = body else {
        panic!("expected nominal construction, got {body:?}");
    };
    assert_eq!(
        path.iter()
            .map(|segment| segment.text.as_str())
            .collect::<Vec<_>>(),
        expected_path
    );
    assert_eq!(
        fields
            .iter()
            .map(|field| field.name.as_str())
            .collect::<Vec<_>>(),
        expected_fields
    );
}

fn date_range_body(source: &str, expected_operator: &str) {
    let body = accepted_body(source);
    let Expr::Range {
        lower,
        operator,
        upper,
        ..
    } = body
    else {
        panic!("expected a date range, got {body:?}");
    };
    assert_eq!(operator, expected_operator);
    for (endpoint, expected_text) in [
        (lower.as_deref(), "2026-09-01"),
        (upper.as_deref(), "2026-09-02"),
    ] {
        assert!(
            matches!(endpoint, Some(Expr::Literal { kind: LiteralKind::Date, text, .. })
                if text == expected_text),
            "expected Date endpoint {expected_text}, got {endpoint:?}"
        );
    }
}

// grammar/orna.ebnf: logical_or_expression; source/06-expressions.md:
// ORNA-LAMBDA-004 and ORNA-OP-001 admit infix OR, but not a pipe closure.
#[test]
#[ignore = "Known Orna 1.0.0 gap: #781; run explicitly for review"]
fn logical_or_is_accepted() {
    let body = accepted_body("fn f() = true || false;");
    assert!(matches!(body, Expr::Binary { op, .. } if op == "||"));
}

#[test]
fn logical_and_is_accepted() {
    let body = accepted_body("fn f() = true && false;");
    assert!(matches!(body, Expr::Binary { op, .. } if op == "&&"));
}

#[test]
fn legacy_empty_pipe_closure_is_rejected() {
    rejected("fn f() = || true;");
}

// grammar/orna.ebnf: qualified_name and nominal_constructor (optional fields);
// source/04-lexical.md: Contextual names and construction;
// source/06-expressions.md: ORNA-ENUM-003 requires explicit payload fields.
#[test]
#[ignore = "Known Orna 1.0.0 gap: #781; run explicitly for review"]
fn qualified_nominal_construction_is_accepted() {
    nominal_body(
        "enum E { v { x: Int } } fn f() = E.v { x: 1 };",
        &["E", "v"],
        &["x"],
    );
}

#[test]
#[ignore = "Known Orna 1.0.0 gap: #781; run explicitly for review"]
fn empty_nominal_construction_is_accepted() {
    nominal_body("type T {} fn f() = T {};", &["T"], &[]);
}

#[test]
fn nonempty_unqualified_nominal_construction_is_accepted() {
    nominal_body("type T { x: Int, } fn f() = T { x: 1 };", &["T"], &["x"]);
}

// source/04-lexical.md: ORNA-RECORD-001 and ORNA-PATTERN-002.
#[test]
fn nominal_construction_punning_is_rejected() {
    rejected("type T { x: Int, } fn f() = T { x };");
}

// grammar/orna.ebnf: comparison_expression admits only one comparison_operator
// (including `in`). source/06-expressions.md: ORNA-COMPARE-001 and ORNA-OP-001.
// Grouping an operand does not group the first comparison; grouping a whole
// comparison explicitly does permit its use as the next comparison's operand.
#[test]
#[ignore = "Known Orna 1.0.0 gap: #781; run explicitly for review"]
fn comparison_chain_with_grouped_operand_is_rejected() {
    rejected("fn f() = 1 < (2) < 3;");
}

#[test]
#[ignore = "Known Orna 1.0.0 gap: #781; run explicitly for review"]
fn comparison_chain_with_additive_operand_is_rejected() {
    rejected("fn f() = 1 < 2 + 3 < 6;");
}

#[test]
#[ignore = "Known Orna 1.0.0 gap: #781; run explicitly for review"]
fn boolean_comparison_chain_is_rejected() {
    rejected("fn f() = true == false == true;");
}

#[test]
#[ignore = "Known Orna 1.0.0 gap: #781; run explicitly for review"]
fn membership_comparison_chain_is_rejected() {
    rejected("fn f() = 1 in [1] == true;");
}

#[test]
#[ignore = "Known Orna 1.0.0 gap: #781; run explicitly for review"]
fn block_tail_comparison_chain_is_rejected() {
    rejected("fn f() { 1 < 2 < 3 }");
}

#[test]
fn simple_comparison_chain_is_rejected() {
    rejected("fn f() = 1 < 2 < 3;");
}

#[test]
fn comparison_conjunction_is_accepted() {
    accepted_body("fn f() = 1 < 2 && 2 < 3;");
}

#[test]
fn explicitly_grouped_comparison_is_accepted() {
    accepted_body("fn f() = (1 < 2) == true;");
}

// grammar/orna.ebnf: range_expression, range_operator and date_literal;
// source/04-lexical.md: ORNA-LEX-009, ORNA-LIT-005 and ORNA-LIT-006.
// Module AST endpoints must retain the Date class across adjacent punctuation.
#[test]
#[ignore = "Known Orna 1.0.0 gap: #781; run explicitly for review"]
fn adjacent_exclusive_date_range_is_accepted() {
    date_range_body("fn f() = 2026-09-01..2026-09-02;", "..");
}

#[test]
#[ignore = "Known Orna 1.0.0 gap: #781; run explicitly for review"]
fn adjacent_inclusive_date_range_is_accepted() {
    date_range_body("fn f() = 2026-09-01..=2026-09-02;", "..=");
}

#[test]
fn spaced_date_ranges_are_accepted() {
    date_range_body("fn f() = 2026-09-01 .. 2026-09-02;", "..");
    date_range_body("fn f() = 2026-09-01 ..= 2026-09-02;", "..=");
}

// source/04-lexical.md: Numeric and string token boundaries, ORNA-SYNTAX-002:
// a calendar-invalid date-shaped literal must not be reinterpreted as subtraction.
#[test]
fn calendar_invalid_date_is_rejected() {
    rejected("fn f() = 2026-02-30;");
}
