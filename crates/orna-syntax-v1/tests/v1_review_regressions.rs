//! Parse-only audit regressions for ornadb-gov5.17.11 / GitHub #781.
//!
//! Requirements below refer to immutable reference/Orna-1.0.0. These focused
//! regressions close the confirmed parser findings without broadening legacy
//! syntax. Inputs stay in memory; no filesystem fixtures or temporary files
//! are used.

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

fn calendar_range_body(
    source: &str,
    expected_operator: &str,
    expected_kind: LiteralKind,
    expected_endpoints: [&str; 2],
) {
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
        (lower.as_deref(), expected_endpoints[0]),
        (upper.as_deref(), expected_endpoints[1]),
    ] {
        assert!(
            matches!(endpoint, Some(Expr::Literal { kind, text, .. })
                if *kind == expected_kind && text == expected_text),
            "expected {expected_kind:?} endpoint {expected_text}, got {endpoint:?}"
        );
    }
}

// grammar/orna.ebnf: logical_or_expression; source/06-expressions.md:
// ORNA-LAMBDA-004 and ORNA-OP-001 admit infix OR, but not a pipe closure.
#[test]
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
fn control_condition_binary_rhs_leaves_body_brace_for_control() {
    let body = accepted_body("fn f() = if ready && done { 1 };");
    let Expr::Control {
        condition: Some(condition),
        body: Some(body),
        ..
    } = body
    else {
        panic!("expected if control expression");
    };
    assert!(matches!(*condition, Expr::Binary { op, .. } if op == "&&"));
    assert!(matches!(*body, Expr::Block { .. }));
}

fn assert_nested_nominal_condition(source: &str, expected_path: &[&str]) {
    let body = accepted_body(source);
    let Expr::Control {
        condition: Some(condition),
        body: Some(body),
        ..
    } = body
    else {
        panic!("expected a braced control expression");
    };
    let Expr::Binary { rhs, .. } = *condition else {
        panic!("expected a compound control condition");
    };
    let Expr::Nominal { path, .. } = *rhs else {
        panic!("expected a nominal constructor in the condition");
    };
    assert_eq!(
        path.iter()
            .map(|segment| segment.text.as_str())
            .collect::<Vec<_>>(),
        expected_path
    );
    assert!(matches!(*body, Expr::Block { .. }));
}

#[test]
fn compound_control_conditions_allow_nested_nominal_constructors() {
    assert_nested_nominal_condition(
        "fn f() = if ready && Status { value: raw } { 1 };",
        &["Status"],
    );
    assert_nested_nominal_condition("fn f() = while ready && Status {} { 1 };", &["Status"]);
    assert_nested_nominal_condition(
        "fn f() = for item in ready && Model.Status { value: raw } { 1 };",
        &["Model", "Status"],
    );
}

#[test]
fn compound_case_conditions_allow_nested_nominal_constructors() {
    let body = accepted_body("fn f() = case ready && Status {} { true: 1 };");
    let Expr::Control {
        condition: Some(condition),
        arms,
        ..
    } = body
    else {
        panic!("expected a case expression");
    };
    assert!(matches!(*condition, Expr::Binary { op, .. } if op == "&&"));
    assert_eq!(arms.len(), 1);
}

#[test]
fn immediate_nominal_constructor_still_reserves_control_body_brace() {
    rejected("type Status { value: Int } fn f() = if Status { value: raw } { 1 };");
}

#[test]
fn immediate_record_control_condition_requires_parentheses() {
    for source in [
        "fn f() = if { value: 1 } { 1 };",
        "fn f() = while { value: 1 } { 1 };",
        "fn f() = for item in { value: 1 } { 1 };",
        "fn f() = case { value: 1 } { true: 1 };",
    ] {
        let parsed = parse_module(source);
        assert!(
            parsed.diagnostics.iter().any(|diagnostic| {
                diagnostic.code == "ORNA-PARSE-001"
                    && diagnostic.message == "record conditions must be parenthesized"
            }),
            "expected ORNA-RECORD-004 parser diagnostic for {source:?}: {:?}",
            parsed.diagnostics
        );
    }

    let body = accepted_body("fn f() = if ({ value: 1 }) { 1 };");
    let Expr::Control {
        condition: Some(condition),
        body: Some(body),
        ..
    } = body
    else {
        panic!("expected a parenthesized-record if expression");
    };
    assert!(matches!(
        *condition,
        Expr::Group { inner, .. } if matches!(*inner, Expr::Record { .. })
    ));
    assert!(matches!(*body, Expr::Block { .. }));
}

#[test]
fn legacy_empty_pipe_closure_is_rejected() {
    rejected("fn f() = || true;");
}

// `var` is a contextual identifier in Orna 1.0.0. Only the removed mutable
// declaration form remains reserved, including destructuring patterns.
#[test]
fn var_is_accepted_in_ordinary_identifier_positions() {
    accepted_body(
        "type var { var: var, } \
         table var(var: var) { var: var, } \
         protocol var { static var: var; fn var(var: var): var; } \
         enum var { var { var: var }, } \
         fn var(var: var): var { \
             let var: var = var; \
             var = replacement; \
             { var: var }; \
             var.member; \
             namespace.var; \
             call(var: var); \
             var \
         }",
    );
}

#[test]
fn legacy_var_declarations_remain_rejected_at_statement_boundaries() {
    for source in [
        "fn f() { var value = 1; }",
        "fn f() { var (left, [right], { field: nested }, Variant { payload: value }) = source; }",
    ] {
        let parsed = parse_module(source);
        assert_eq!(
            parsed.diagnostics.first().map(|diagnostic| diagnostic.code),
            Some("ORNA091-E-VAR"),
            "{source}: {:?}",
            parsed.diagnostics
        );
    }
}

// `opaque` and `currency` are ordinary identifiers in Orna 1.0.0. Their
// targeted diagnostics apply only to the removed top-level declaration forms.
#[test]
fn opaque_and_currency_are_accepted_in_ordinary_identifier_positions() {
    for name in ["opaque", "currency", "check", "unique", "where"] {
        accepted_body(&format!(
            "type {name} {{ {name}: {name}, }} \
             table {name}({name}: {name}) {{ {name}: {name}, }} \
             protocol {name} {{ static {name}: {name}; fn {name}({name}: {name}): {name}; }} \
             enum {name} {{ {name} {{ {name}: {name} }}, }} \
             fn {name}({name}: {name}): {name} {{ \
                 let {name}: {name} = {name}; \
                 {name} = replacement; \
                 {{ {name}: {name} }}; \
                 {name}.member; \
                 namespace.{name}; \
                 call({name}: {name}); \
                 {name} \
             }}"
        ));
    }
}

#[test]
fn legacy_opaque_and_currency_declarations_remain_rejected() {
    for (source, expected) in [
        ("opaque EmailAddress = Str;", "ORNA091-E-OPAQUE"),
        ("pub opaque EmailAddress = Str;", "ORNA091-E-OPAQUE"),
        (
            "currency GBP { code: \"GBP\", symbol: \"£\", minor_digits: 2 }",
            "ORNA091-E-CURRENCY",
        ),
        (
            "pub currency GBP { code: \"GBP\", symbol: \"£\", minor_digits: 2 }",
            "ORNA091-E-CURRENCY",
        ),
    ] {
        let parsed = parse_module(source);
        assert_eq!(
            parsed.diagnostics.first().map(|diagnostic| diagnostic.code),
            Some(expected),
            "{source}: {:?}",
            parsed.diagnostics
        );
    }
}

#[test]
fn assertion_aliases_are_accepted_in_ordinary_identifier_positions() {
    for name in ["ensure", "fact", "constraint", "constraints"] {
        accepted_body(&format!(
            "type {name} {{ {name}: {name}, }} \
             table {name}({name}: {name}) {{ {name}: {name}, }} \
             protocol {name} {{ static {name}: {name}; fn {name}({name}: {name}): {name}; }} \
             enum {name} {{ {name} {{ {name}: {name} }}, }} \
             fn {name}({name}: {name}): {name} {{ \
                 let {name}: {name} = {name}; \
                 {name} = replacement; \
                 {{ {name}: {name} }}; \
                 {name}.member; \
                 namespace.{name}; \
                 call({name}: {name}); \
                 {name} \
             }}"
        ));
    }
}

#[test]
fn legacy_assertion_aliases_remain_rejected_in_their_removed_shapes() {
    for (source, expected) in [
        (
            "pub fn bad(amount: Int) { ensure amount > 0; }",
            "ORNA-A091-010",
        ),
        (
            "pub fn bad(amount: Int) { fact amount > 0; }",
            "ORNA-A091-010",
        ),
        (
            "pub fn bad(amount: Int) { constraint amount > 0; }",
            "ORNA-A091-010",
        ),
        ("pub ensure amount > 0;", "ORNA-A091-010"),
        ("pub fact amount > 0;", "ORNA-A091-010"),
        ("pub constraint amount > 0;", "ORNA-A091-010"),
        (
            "pub table User(id: Uuid) { name: Str, constraints { unique(name); } }",
            "ORNA-A091-010",
        ),
        (
            "pub table User(id: Uuid) { assert id > 0; constraints { unique(id); } }",
            "ORNA-A091-010",
        ),
        ("constraints { unique(id); }", "ORNA-A091-010"),
        ("pub constraints { unique(id); }", "ORNA-A091-010"),
        (
            "pub table User(id: Uuid) { username: Str unique, }",
            "ORNA091-E-FIELD-CONSTRAINT",
        ),
        (
            "pub table User(id: Uuid) { age: Int check(age >= 0), }",
            "ORNA091-E-FIELD-CONSTRAINT",
        ),
        (
            "pub table User(id: Uuid) { callback: fn(Int): Str check(value > 0), }",
            "ORNA091-E-FIELD-CONSTRAINT",
        ),
        ("type Port = Int where self >= 1;", "ORNA-A091-001"),
        ("pub type Port = Int where self >= 1;", "ORNA-A091-001"),
    ] {
        let parsed = parse_module(source);
        assert_eq!(
            parsed.diagnostics.first().map(|diagnostic| diagnostic.code),
            Some(expected),
            "{source}: {:?}",
            parsed.diagnostics
        );
    }

    for alias in ["ensure", "fact"] {
        for operator in [
            "<",
            "<=",
            ">",
            ">=",
            "==",
            "!=",
            "in",
            "-amount >",
            "+amount >",
        ] {
            for source in [
                format!("pub fn bad(amount: Int) {{ {alias} {operator} 0; }}"),
                format!("pub table User(id: Uuid) {{ name: Str, {alias} {operator} 0; }}"),
                format!("pub table User(id: Uuid) {{ assert id > 0; {alias} {operator} 0; }}"),
            ] {
                let parsed = parse_module(&source);
                assert_eq!(
                    parsed.diagnostics.first().map(|diagnostic| diagnostic.code),
                    Some("ORNA-A091-010"),
                    "{source}: {:?}",
                    parsed.diagnostics
                );
            }
        }
    }
}

#[test]
fn constraints_remains_an_ordinary_nominal_constructor_in_nested_impl() {
    let parsed = parse_module(
        "table T(id: Int) { \
            impl P { fn make() { constraints { value: raw } } } \
         }",
    );
    assert!(
        parsed.diagnostics.is_empty(),
        "nested nominal construction should remain valid: {:?}",
        parsed.diagnostics
    );
}

#[test]
fn field_constraint_names_remain_ordinary_outside_removed_modifiers() {
    let parsed = parse_module(
        "table T(id: Int) { \
            check: Str, \
            unique: Str, \
            impl P { fn make() { check(raw); unique(raw); where; } } \
         }",
    );
    assert!(
        parsed.diagnostics.is_empty(),
        "ordinary check/unique/where uses should remain valid: {:?}",
        parsed.diagnostics
    );
}

#[test]
fn where_remains_an_ordinary_name_after_a_closed_type_refinement() {
    let parsed = parse_module("type T = Int { assert self > 0; } fn where() = 1;");
    assert!(
        parsed.diagnostics.is_empty(),
        "where should remain ordinary after a complete refinement: {:?}",
        parsed.diagnostics
    );
}

// `match` is a contextual identifier in Orna 1.0.0. The removed expression
// form is recognized only when its statement/function-body shape includes
// legacy `=>` arms.
#[test]
fn match_is_accepted_in_ordinary_identifier_positions() {
    accepted_body(
        "type match { match: match, } \
         table match(match: match) { match: match, } \
         protocol match { static match: match; fn match(match: match): match; } \
         enum match { match { match: match }, } \
         fn match(match: match): match { \
             let match: match = match; \
             match = replacement; \
             { match: match }; \
             match.member; \
             namespace.match; \
             call(match: match); \
             match \
         }",
    );
}

#[test]
fn match_is_accepted_as_a_lambda_parameter() {
    accepted_body("fn f() = match => { let f = x => x; f };");
}

#[test]
fn match_lambda_is_accepted_as_a_record_field_value() {
    accepted_body("fn f() = { value: match => { let f = x => x; f } };");
}

#[test]
fn match_is_accepted_as_a_field_name_before_a_nested_lambda_record() {
    accepted_body("fn f() = { match: { value: x => x } };");
}

#[test]
fn match_is_accepted_in_a_return_type_product_before_a_lambda_body() {
    accepted_body("fn f(): match * match { let f = x => x; f }");
}

#[test]
fn legacy_match_expression_remains_rejected_at_a_function_body_boundary() {
    let parsed = parse_module(
        "pub fn bad(status: Status): Str = match status { Status.ready => \"ready\" };",
    );
    assert_eq!(
        parsed.diagnostics.first().map(|diagnostic| diagnostic.code),
        Some("ORNA091-E-MATCH"),
        "{:?}",
        parsed.diagnostics
    );
}

#[test]
fn legacy_match_expression_is_rejected_after_record_or_nominal_constructor_scrutinee() {
    for source in [
        "fn f() = match Status { value: raw } { Status.ready => \"ready\" };",
        "fn f() = match Status {} { Status.ready => \"ready\" };",
        "fn f() = match { value: raw } { Status.ready => \"ready\" };",
    ] {
        let parsed = parse_module(source);
        assert_eq!(
            parsed.diagnostics.first().map(|diagnostic| diagnostic.code),
            Some("ORNA091-E-MATCH"),
            "{source:?}: {:?}",
            parsed.diagnostics
        );
    }
}

#[test]
fn legacy_match_expression_is_rejected_at_nested_expression_boundaries() {
    for source in [
        "fn f() = call(match status { Status.ready => \"ready\" });",
        "fn f() = [match status { Status.ready => \"ready\" }];",
        "fn f() = (match status { Status.ready => \"ready\" });",
        "fn f() = { value: match status { Status.ready => \"ready\" } };",
    ] {
        let parsed = parse_module(source);
        assert_eq!(
            parsed.diagnostics.first().map(|diagnostic| diagnostic.code),
            Some("ORNA091-E-MATCH"),
            "{source:?}: {:?}",
            parsed.diagnostics
        );
    }
}

#[test]
fn legacy_match_expression_is_rejected_after_expression_introducers() {
    for source in [
        "fn f() = true && match status { Status.ready => \"ready\" };",
        "fn f() = 1 + match status { Status.ready => \"ready\" };",
        "fn f() = value | match status { Status.ready => \"ready\" };",
        "fn f() = if match status { Status.ready => true } { 1 };",
        "fn f() = while match status { Status.ready => true } { 1 };",
        "fn f() = for item in match status { Status.ready => item } { item };",
        "fn f() = case match status { Status.ready => true } { true: 1 };",
        "fn f() { return match status { Status.ready => \"ready\" }; }",
        "fn f() { break match status { Status.ready => \"ready\" }; }",
        "fn f() { assert match status { Status.ready => true }; }",
    ] {
        let parsed = parse_module(source);
        assert_eq!(
            parsed.diagnostics.first().map(|diagnostic| diagnostic.code),
            Some("ORNA091-E-MATCH"),
            "{source:?}: {:?}",
            parsed.diagnostics
        );
    }
}

// grammar/orna.ebnf: qualified_name and nominal_constructor (optional fields);
// source/04-lexical.md: Contextual names and construction;
// source/06-expressions.md: ORNA-ENUM-003 requires explicit payload fields.
#[test]
fn qualified_nominal_construction_is_accepted() {
    nominal_body(
        "enum E { v { x: Int } } fn f() = E.v { x: 1 };",
        &["E", "v"],
        &["x"],
    );
}

#[test]
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
fn comparison_chain_with_grouped_operand_is_rejected() {
    rejected("fn f() = 1 < (2) < 3;");
}

#[test]
fn comparison_chain_with_additive_operand_is_rejected() {
    rejected("fn f() = 1 < 2 + 3 < 6;");
}

#[test]
fn boolean_comparison_chain_is_rejected() {
    rejected("fn f() = true == false == true;");
}

#[test]
fn membership_comparison_chain_is_rejected() {
    rejected("fn f() = 1 in [1] == true;");
}

#[test]
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
fn adjacent_exclusive_date_range_is_accepted() {
    calendar_range_body(
        "fn f() = 2026-09-01..2026-09-02;",
        "..",
        LiteralKind::Date,
        ["2026-09-01", "2026-09-02"],
    );
}

#[test]
fn adjacent_inclusive_date_range_is_accepted() {
    calendar_range_body(
        "fn f() = 2026-09-01..=2026-09-02;",
        "..=",
        LiteralKind::Date,
        ["2026-09-01", "2026-09-02"],
    );
}

#[test]
fn adjacent_instant_range_is_accepted() {
    calendar_range_body(
        "fn f() = 2026-09-01T14:30:00Z..2026-09-02T14:30:00Z;",
        "..",
        LiteralKind::Instant,
        ["2026-09-01T14:30:00Z", "2026-09-02T14:30:00Z"],
    );
}

#[test]
fn adjacent_date_range_does_not_rewrite_quoted_text() {
    let body = accepted_body("fn f() = 2026-09-01..2026-09-02 || \"2026-09-01..2026-09-02\";");
    let Expr::Binary { lhs, op, rhs, .. } = body else {
        panic!("expected range/string logical-or expression");
    };
    assert_eq!(op, "||");
    assert!(matches!(*lhs, Expr::Range { operator, .. } if operator == ".."));
    assert!(matches!(
        *rhs,
        Expr::Literal {
            kind: LiteralKind::String,
            text,
            ..
        } if text == "\"2026-09-01..2026-09-02\""
    ));
}

#[test]
fn spaced_date_ranges_are_accepted() {
    calendar_range_body(
        "fn f() = 2026-09-01 .. 2026-09-02;",
        "..",
        LiteralKind::Date,
        ["2026-09-01", "2026-09-02"],
    );
    calendar_range_body(
        "fn f() = 2026-09-01 ..= 2026-09-02;",
        "..=",
        LiteralKind::Date,
        ["2026-09-01", "2026-09-02"],
    );
}

// source/04-lexical.md: Numeric and string token boundaries, ORNA-SYNTAX-002:
// a calendar-invalid date-shaped literal must not be reinterpreted as subtraction.
#[test]
fn calendar_invalid_date_is_rejected() {
    rejected("fn f() = 2026-02-30;");
}
