//! Parse-only audit regressions for ornadb-gov5.17.11 / GitHub #781.
//!
//! Requirements below refer to immutable reference/Orna-1.0.0. These focused
//! regressions close the confirmed parser findings without broadening legacy
//! syntax. ORNA source is kept in reviewable `.orna` fixtures.

use orna_syntax_v1::{Declaration, Expr, LiteralKind, parse_module};

fn fixture_case(name: &str) -> &str {
    include_str!("fixtures/v1_review_alias_cases.orna")
        .split("// CASE: ")
        .find_map(|section| {
            let (case, source) = section.split_once('\n')?;
            (case.trim() == name).then_some(source)
        })
        .unwrap_or_else(|| panic!("missing ORNA fixture case {name}"))
}

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
    let body = accepted_body(include_str!("fixtures/v1_review_source_000.orna"));
    assert!(matches!(body, Expr::Binary { op, .. } if op == "||"));
}

#[test]
fn logical_and_is_accepted() {
    let body = accepted_body(include_str!("fixtures/v1_review_source_001.orna"));
    assert!(matches!(body, Expr::Binary { op, .. } if op == "&&"));
}

#[test]
fn control_condition_binary_rhs_leaves_body_brace_for_control() {
    let body = accepted_body(include_str!("fixtures/v1_review_source_002.orna"));
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
        include_str!("fixtures/v1_review_source_003.orna"),
        &["Status"],
    );
    assert_nested_nominal_condition(
        include_str!("fixtures/v1_review_source_004.orna"),
        &["Status"],
    );
    assert_nested_nominal_condition(
        include_str!("fixtures/v1_review_source_005.orna"),
        &["Model", "Status"],
    );
}

#[test]
fn compound_case_conditions_allow_nested_nominal_constructors() {
    let body = accepted_body(include_str!("fixtures/v1_review_source_006.orna"));
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
    rejected(include_str!("fixtures/v1_review_source_007.orna"));
}

#[test]
fn immediate_record_control_condition_requires_parentheses() {
    for source in [
        include_str!("fixtures/v1_review_source_008.orna"),
        include_str!("fixtures/v1_review_source_009.orna"),
        include_str!("fixtures/v1_review_source_010.orna"),
        include_str!("fixtures/v1_review_source_011.orna"),
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

    let body = accepted_body(include_str!("fixtures/v1_review_source_012.orna"));
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
    rejected(include_str!("fixtures/v1_review_source_013.orna"));
}

// `var` is a contextual identifier in Orna 1.0.0. Only the removed mutable
// declaration form remains reserved, including destructuring patterns.
#[test]
fn var_is_accepted_in_ordinary_identifier_positions() {
    accepted_body(include_str!("fixtures/v1_review_source_014.orna"));
}

#[test]
fn legacy_var_declarations_remain_rejected_at_statement_boundaries() {
    for source in [
        include_str!("fixtures/v1_review_source_015.orna"),
        include_str!("fixtures/v1_review_source_016.orna"),
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
        accepted_body(
            &include_str!("fixtures/v1_review_ordinary_name_declarations.orna")
                .replace("fixture_name", name),
        );
    }
}

#[test]
fn legacy_opaque_and_currency_declarations_remain_rejected() {
    for (source, expected) in [
        (
            include_str!("fixtures/v1_review_source_017.orna"),
            "ORNA091-E-OPAQUE",
        ),
        (
            include_str!("fixtures/v1_review_source_018.orna"),
            "ORNA091-E-OPAQUE",
        ),
        (
            include_str!("fixtures/v1_review_source_019.orna"),
            "ORNA091-E-CURRENCY",
        ),
        (
            include_str!("fixtures/v1_review_source_020.orna"),
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
        accepted_body(
            &include_str!("fixtures/v1_review_ordinary_name_declarations.orna")
                .replace("fixture_name", name),
        );
    }
}

#[test]
fn legacy_assertion_aliases_remain_rejected_in_their_removed_shapes() {
    for (source, expected) in [
        (
            include_str!("fixtures/v1_review_source_021.orna"),
            "ORNA-A091-010",
        ),
        (
            include_str!("fixtures/v1_review_source_022.orna"),
            "ORNA-A091-010",
        ),
        (
            include_str!("fixtures/v1_review_source_023.orna"),
            "ORNA-A091-010",
        ),
        (
            include_str!("fixtures/v1_review_source_024.orna"),
            "ORNA-A091-010",
        ),
        (
            include_str!("fixtures/v1_review_source_025.orna"),
            "ORNA-A091-010",
        ),
        (
            include_str!("fixtures/v1_review_source_026.orna"),
            "ORNA-A091-010",
        ),
        (
            include_str!("fixtures/v1_review_source_027.orna"),
            "ORNA-A091-010",
        ),
        (
            include_str!("fixtures/v1_review_source_028.orna"),
            "ORNA-A091-010",
        ),
        (
            include_str!("fixtures/v1_review_source_029.orna"),
            "ORNA-A091-010",
        ),
        (
            include_str!("fixtures/v1_review_source_030.orna"),
            "ORNA-A091-010",
        ),
        (
            include_str!("fixtures/v1_review_source_031.orna"),
            "ORNA091-E-FIELD-CONSTRAINT",
        ),
        (
            include_str!("fixtures/v1_review_source_032.orna"),
            "ORNA091-E-FIELD-CONSTRAINT",
        ),
        (
            include_str!("fixtures/v1_review_source_033.orna"),
            "ORNA091-E-FIELD-CONSTRAINT",
        ),
        (
            include_str!("fixtures/v1_review_source_034.orna"),
            "ORNA-A091-001",
        ),
        (
            include_str!("fixtures/v1_review_source_035.orna"),
            "ORNA-A091-001",
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

    for alias_index in 0..2 {
        for operator_index in 0..9 {
            for shape_index in 0..3 {
                let case = format!("alias_{alias_index}_{operator_index}_{shape_index}");
                let source = fixture_case(&case);
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

    let parsed = parse_module(include_str!("fixtures/v1_review_source_036.orna"));
    assert_ne!(
        parsed.diagnostics.first().map(|diagnostic| diagnostic.code),
        Some("E1005"),
        "on must not inspect a later unrelated lambda: {:?}",
        parsed.diagnostics
    );
}

#[test]
fn constraints_remains_an_ordinary_nominal_constructor_in_nested_impl() {
    let parsed = parse_module(include_str!("fixtures/v1_review_source_038.orna"));
    assert!(
        parsed.diagnostics.is_empty(),
        "nested nominal construction should remain valid: {:?}",
        parsed.diagnostics
    );
}

#[test]
fn field_constraint_names_remain_ordinary_outside_removed_modifiers() {
    let parsed = parse_module(include_str!("fixtures/v1_review_source_039.orna"));
    assert!(
        parsed.diagnostics.is_empty(),
        "ordinary check/unique/where uses should remain valid: {:?}",
        parsed.diagnostics
    );
}

#[test]
fn where_remains_an_ordinary_name_after_a_closed_type_refinement() {
    let parsed = parse_module(include_str!("fixtures/v1_review_source_040.orna"));
    assert!(
        parsed.diagnostics.is_empty(),
        "where should remain ordinary after a complete refinement: {:?}",
        parsed.diagnostics
    );
}

#[test]
fn legacy_declaration_words_are_scoped_to_removed_module_shapes() {
    for name in ["ingest", "log", "store", "view", "transaction", "on"] {
        accepted_body(
            &include_str!("fixtures/v1_review_legacy_name_declarations.orna")
                .replace("fixture_name", name),
        );
    }

    for (source, expected) in [
        (include_str!("fixtures/v1_review_source_041.orna"), "E1004"),
        (include_str!("fixtures/v1_review_source_042.orna"), "E1001"),
        (include_str!("fixtures/v1_review_source_043.orna"), "E1003"),
        (include_str!("fixtures/v1_review_source_044.orna"), "E1002"),
        (include_str!("fixtures/v1_review_source_045.orna"), "E1007"),
        (include_str!("fixtures/v1_review_source_046.orna"), "E1005"),
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

// `match` is a contextual identifier in Orna 1.0.0. The removed expression
// form is recognized only when its statement/function-body shape includes
// legacy `=>` arms.
#[test]
fn match_is_accepted_in_ordinary_identifier_positions() {
    accepted_body(include_str!("fixtures/v1_review_source_047.orna"));
}

#[test]
fn match_is_accepted_as_a_lambda_parameter() {
    accepted_body(include_str!("fixtures/v1_review_source_048.orna"));
}

#[test]
fn match_lambda_is_accepted_as_a_record_field_value() {
    accepted_body(include_str!("fixtures/v1_review_source_049.orna"));
}

#[test]
fn match_is_accepted_as_a_field_name_before_a_nested_lambda_record() {
    accepted_body(include_str!("fixtures/v1_review_source_050.orna"));
}

#[test]
fn match_is_accepted_in_a_return_type_product_before_a_lambda_body() {
    accepted_body(include_str!("fixtures/v1_review_source_051.orna"));
}

#[test]
fn legacy_match_expression_remains_rejected_at_a_function_body_boundary() {
    let parsed = parse_module(include_str!("fixtures/v1_review_source_052.orna"));
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
        include_str!("fixtures/v1_review_source_053.orna"),
        include_str!("fixtures/v1_review_source_054.orna"),
        include_str!("fixtures/v1_review_source_055.orna"),
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
        include_str!("fixtures/v1_review_source_056.orna"),
        include_str!("fixtures/v1_review_source_057.orna"),
        include_str!("fixtures/v1_review_source_058.orna"),
        include_str!("fixtures/v1_review_source_059.orna"),
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
        include_str!("fixtures/v1_review_source_060.orna"),
        include_str!("fixtures/v1_review_source_061.orna"),
        include_str!("fixtures/v1_review_source_062.orna"),
        include_str!("fixtures/v1_review_source_063.orna"),
        include_str!("fixtures/v1_review_source_064.orna"),
        include_str!("fixtures/v1_review_source_065.orna"),
        include_str!("fixtures/v1_review_source_066.orna"),
        include_str!("fixtures/v1_review_source_067.orna"),
        include_str!("fixtures/v1_review_source_068.orna"),
        include_str!("fixtures/v1_review_source_069.orna"),
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
        include_str!("fixtures/v1_review_source_070.orna"),
        &["E", "v"],
        &["x"],
    );
}

#[test]
fn empty_nominal_construction_is_accepted() {
    nominal_body(
        include_str!("fixtures/v1_review_source_071.orna"),
        &["T"],
        &[],
    );
}

#[test]
fn nonempty_unqualified_nominal_construction_is_accepted() {
    nominal_body(
        include_str!("fixtures/v1_review_source_072.orna"),
        &["T"],
        &["x"],
    );
}

// grammar/orna.ebnf: qualified_name starts with identifier; contextual names
// are admitted only after a qualified-name dot.
#[test]
fn reserved_words_cannot_start_type_references() {
    for keyword in ["true", "false", "null", "self", "if", "type", "pub", "as"] {
        let source = include_str!("fixtures/v1_review_reserved_type_head.orna")
            .replace("fixture_keyword", keyword);
        let parsed = parse_module(&source);
        assert!(
            !parsed.is_ok(),
            "reserved word {keyword:?} was accepted as a type head: {:?}",
            parsed.diagnostics
        );
    }
}

// source/04-lexical.md: ORNA-RECORD-001 and ORNA-PATTERN-002.
#[test]
fn nominal_construction_punning_is_rejected() {
    rejected(include_str!("fixtures/v1_review_source_073.orna"));
}

// grammar/orna.ebnf: comparison_expression admits only one comparison_operator
// (including `in`). source/06-expressions.md: ORNA-COMPARE-001 and ORNA-OP-001.
// Grouping an operand does not group the first comparison; grouping a whole
// comparison explicitly does permit its use as the next comparison's operand.
#[test]
fn comparison_chain_with_grouped_operand_is_rejected() {
    rejected(include_str!("fixtures/v1_review_source_074.orna"));
}

#[test]
fn comparison_chain_with_additive_operand_is_rejected() {
    rejected(include_str!("fixtures/v1_review_source_075.orna"));
}

#[test]
fn boolean_comparison_chain_is_rejected() {
    rejected(include_str!("fixtures/v1_review_source_076.orna"));
}

#[test]
fn membership_comparison_chain_is_rejected() {
    rejected(include_str!("fixtures/v1_review_source_077.orna"));
}

#[test]
fn block_tail_comparison_chain_is_rejected() {
    rejected(include_str!("fixtures/v1_review_source_078.orna"));
}

#[test]
fn simple_comparison_chain_is_rejected() {
    rejected(include_str!("fixtures/v1_review_source_079.orna"));
}

#[test]
fn comparison_conjunction_is_accepted() {
    accepted_body(include_str!("fixtures/v1_review_source_080.orna"));
}

#[test]
fn explicitly_grouped_comparison_is_accepted() {
    accepted_body(include_str!("fixtures/v1_review_source_081.orna"));
}

// grammar/orna.ebnf: range_expression, range_operator and date_literal;
// source/04-lexical.md: ORNA-LEX-009, ORNA-LIT-005 and ORNA-LIT-006.
// Module AST endpoints must retain the Date class across adjacent punctuation.
#[test]
fn adjacent_exclusive_date_range_is_accepted() {
    calendar_range_body(
        include_str!("fixtures/v1_review_source_082.orna"),
        "..",
        LiteralKind::Date,
        ["2026-09-01", "2026-09-02"],
    );
}

#[test]
fn adjacent_inclusive_date_range_is_accepted() {
    calendar_range_body(
        include_str!("fixtures/v1_review_source_083.orna"),
        "..=",
        LiteralKind::Date,
        ["2026-09-01", "2026-09-02"],
    );
}

#[test]
fn adjacent_instant_range_is_accepted() {
    calendar_range_body(
        include_str!("fixtures/v1_review_source_084.orna"),
        "..",
        LiteralKind::Instant,
        ["2026-09-01T14:30:00Z", "2026-09-02T14:30:00Z"],
    );
}

#[test]
fn adjacent_date_range_does_not_rewrite_quoted_text() {
    let body = accepted_body(include_str!("fixtures/v1_review_source_085.orna"));
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
        include_str!("fixtures/v1_review_source_086.orna"),
        "..",
        LiteralKind::Date,
        ["2026-09-01", "2026-09-02"],
    );
    calendar_range_body(
        include_str!("fixtures/v1_review_source_087.orna"),
        "..=",
        LiteralKind::Date,
        ["2026-09-01", "2026-09-02"],
    );
}

// source/04-lexical.md: Numeric and string token boundaries, ORNA-SYNTAX-002:
// a calendar-invalid date-shaped literal must not be reinterpreted as subtraction.
#[test]
fn calendar_invalid_date_is_rejected() {
    rejected(include_str!("fixtures/v1_review_source_088.orna"));
}
