use orna_syntax_v1::{
    Declaration, Expr, TokenKind, lex, parse_expression, parse_module, parse_repl, parse_row,
};
use std::path::Path;

fn reference(path: &str) -> String {
    std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../reference/Orna-1.0.0")
            .join(path),
    )
    .unwrap()
}

fn fixture(name: &str) -> String {
    std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name),
    )
    .unwrap()
}

#[test]
fn accepts_reference_language_shapes() {
    for (name, source) in [
        (
            "valid-shape-1.orna",
            include_str!("fixtures/valid-shape-1.orna"),
        ),
        (
            "valid-shape-2.orna",
            include_str!("fixtures/valid-shape-2.orna"),
        ),
        (
            "valid-shape-3.orna",
            include_str!("fixtures/valid-shape-3.orna"),
        ),
    ] {
        let parsed = parse_module(source);
        assert!(parsed.is_ok(), "{name}: {:?}", parsed.diagnostics);
    }
}
#[test]
fn entrypoints_and_precedence() {
    assert!(parse_row(&fixture("entry-row.orna")).is_ok());
    assert!(parse_repl(&fixture("entry-repl.orna")).is_ok());
    let expression = fixture("entry-precedence.orna");
    let x = parse_expression(&expression);
    assert!(x.is_ok(), "{:?}", x.diagnostics);
}
#[test]
fn range_token_wins_over_numeric_dot() {
    let t = lex(&fixture("range-token.orna")).unwrap();
    assert!(matches!(t[0].kind, TokenKind::Integer));
    assert_eq!(t[1].text, "..=");
}
#[test]
fn parses_bounded_and_optional_ranges_without_placeholder_endpoints() {
    for (name, lower, upper, operator) in [
        ("range-bounded.orna", true, true, ".."),
        ("range-open-upper.orna", true, false, ".."),
        ("range-open-lower.orna", false, true, ".."),
        ("range-inclusive.orna", true, true, "..="),
        ("range-inclusive-open.orna", false, true, "..="),
    ] {
        let source = fixture(name);
        let parsed = parse_expression(&source);
        assert!(parsed.is_ok(), "{name}: {:?}", parsed.diagnostics);
        match parsed.value {
            Expr::Range {
                lower: actual_lower,
                operator: actual_operator,
                upper: actual_upper,
                ..
            } => {
                assert_eq!(actual_lower.is_some(), lower, "{name}");
                assert_eq!(actual_upper.is_some(), upper, "{name}");
                assert_eq!(actual_operator, operator, "{name}");
            }
            other => panic!("expected range for {name}, got {other:?}"),
        }
    }
}

#[test]
fn rejects_non_associative_range_chaining() {
    let parsed = parse_expression(&fixture("range-chain.orna"));

    assert!(!parsed.is_ok());
    assert_eq!(parsed.diagnostics.len(), 1);
    assert_eq!(parsed.diagnostics[0].code, "ORNA-PARSE-001");
    assert_eq!(
        parsed.diagnostics[0].message,
        "range expressions are non-associative"
    );
    assert_eq!(parsed.diagnostics[0].span.start, 4);
    assert_eq!(parsed.diagnostics[0].span.end, 6);
    match parsed.value {
        Expr::Range { lower, upper, .. } => {
            assert!(matches!(lower.as_deref(), Some(Expr::Literal { text, .. }) if text == "1"));
            assert!(matches!(upper.as_deref(), Some(Expr::Literal { text, .. }) if text == "2"));
        }
        other => panic!("expected the first range to remain as the recovered value, got {other:?}"),
    }
}

#[test]
fn rejects_prefix_range_as_unparenthesized_endpoint() {
    let parsed = parse_expression(&fixture("range-prefix-chain.orna"));

    assert!(!parsed.is_ok());
    assert_eq!(parsed.diagnostics.len(), 1);
    assert_eq!(parsed.diagnostics[0].code, "ORNA-PARSE-001");
    assert_eq!(
        parsed.diagnostics[0].message,
        "range expressions are non-associative"
    );
    assert_eq!(parsed.diagnostics[0].span.start, 4);
    assert_eq!(parsed.diagnostics[0].span.end, 6);
    match parsed.value {
        Expr::Range {
            lower, upper, span, ..
        } => {
            assert!(matches!(lower.as_deref(), Some(Expr::Literal { text, .. }) if text == "1"));
            assert!(upper.is_none());
            assert_eq!(span.start, 0);
            assert_eq!(span.end, 3);
        }
        other => panic!("expected the first range to remain as the recovered value, got {other:?}"),
    }
}

#[test]
fn unicode_nfc_comments_and_literals() {
    let t = lex(&fixture("unicode-nfc-comments.orna")).unwrap();
    let ident = t
        .iter()
        .find(|t| matches!(t.kind, TokenKind::Identifier { .. }))
        .unwrap();
    match &ident.kind {
        TokenKind::Identifier { normalized } => assert_eq!(normalized, "café"),
        _ => unreachable!(),
    };
    assert!(t.iter().any(|t| matches!(t.kind, TokenKind::Instant)));
}
#[test]
fn malformed_calendar_instant_and_escape_literals_are_rejected() {
    for name in [
        "bad-calendar-leap.orna",
        "bad-calendar-month.orna",
        "bad-instant-hour.orna",
        "bad-year-zero.orna",
        "bad-instant-precision.orna",
        "bad-instant-offset.orna",
        "bad-binary.orna",
        "bad-hex.orna",
        "bad-escape.orna",
        "bad-unicode-escape.orna",
        "bad-interpolation.orna",
    ] {
        let source = fixture(name);
        assert!(lex(&source).is_err(), "{name}: {source}");
    }
}
#[test]
fn stable_errors_reject_legacy_and_bad_lexemes() {
    let old = parse_module(&fixture("legacy-sql.orna"));
    assert_eq!(old.diagnostics[0].code, "ORNA-PARSE-001");
    let bad = lex(&fixture("bad-comment.orna")).unwrap_err();
    assert_eq!(bad[0].code, "ORNA-LEX-004");
}

#[test]
fn grammar_recovery_codes_are_token_driven_under_layout_variations() {
    for (name, padded_name, code) in [
        (
            "bad-comparison-chain.orna",
            "bad-comparison-chain-padded.orna",
            "E1302",
        ),
        (
            "bad-pipeline-lambda.orna",
            "bad-pipeline-lambda-padded.orna",
            "E1204",
        ),
        (
            "bad-assignment-expression.orna",
            "bad-assignment-expression-padded.orna",
            "E1301",
        ),
        (
            "bad-empty-assert.orna",
            "bad-empty-assert-padded.orna",
            "ORNA-A091-011",
        ),
        (
            "bad-missing-assert-semicolon.orna",
            "bad-missing-assert-semicolon-padded.orna",
            "ORNA-A091-005",
        ),
        (
            "bad-assert-else.orna",
            "bad-assert-else-padded.orna",
            "ORNA-A091-006",
        ),
    ] {
        let source = fixture(name);
        let parsed = parse_module(&source);
        assert_eq!(parsed.diagnostics[0].code, code, "{name}");
        let padded = fixture(padded_name);
        let parsed = parse_module(&padded);
        assert_eq!(parsed.diagnostics[0].code, code, "{padded_name}");
    }
}

#[test]
fn authoritative_valid_fixture_corpus_parses() {
    let root =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../reference/Orna-1.0.0/examples/valid");
    let mut files = std::fs::read_dir(root)
        .unwrap()
        .map(Result::unwrap)
        .map(|e| e.path())
        .collect::<Vec<_>>();
    files.sort();
    assert_eq!(files.len(), 86, "authoritative valid corpus changed");
    for file in files {
        let source = std::fs::read_to_string(&file).unwrap();
        let p = if file.file_name().unwrap() == "row-body.orna" {
            parse_row(&source).diagnostics
        } else {
            parse_module(&source).diagnostics
        };
        assert!(p.is_empty(), "{}: {:?}", file.display(), p);
    }
}

#[test]
fn authoritative_parse_invalid_fixture_corpus_fails() {
    let files = [
        "assert-empty",
        "assert-missing-semicolon",
        "assignment-expression",
        "comparison-chain",
        "legacy-assert-else",
        "legacy-assert-pipe-bang",
        "legacy-colon-bound",
        "legacy-constraints-block",
        "legacy-currency-declaration",
        "legacy-empty-closure",
        "legacy-ensure",
        "legacy-fact",
        "legacy-field-check",
        "legacy-field-unique",
        "legacy-ingest",
        "legacy-log",
        "legacy-match",
        "legacy-opaque",
        "legacy-pipe-lambda",
        "legacy-postfix-question",
        "legacy-refined-where",
        "legacy-return-arrow",
        "legacy-store",
        "legacy-top-level-impl",
        "legacy-var",
        "legacy-view",
        "question-coalesce-adjacent",
        "question-on-int",
        "record-punning",
        "row-declaration",
        "static-protocol-function",
        "top-level-expression",
        "top-level-on",
        "transaction-block",
        "unparenthesized-lambda-stage",
    ];
    assert_eq!(files.len(), 35);
    for name in files {
        let source = reference(&format!("examples/invalid/{name}.orna"));
        let diagnostics = if name == "row-declaration" {
            parse_row(&source).diagnostics
        } else {
            parse_module(&source).diagnostics
        };
        assert!(!diagnostics.is_empty(), "{name} unexpectedly parsed");
    }
}

#[test]
fn authoritative_parse_invalid_primary_codes_match_manifest() {
    let cases = [
        ("assert-empty", "ORNA-A091-011"),
        ("assert-missing-semicolon", "ORNA-A091-005"),
        ("assignment-expression", "E1301"),
        ("comparison-chain", "E1302"),
        ("legacy-assert-else", "ORNA-A091-006"),
        ("legacy-assert-pipe-bang", "ORNA-A091-010"),
        ("legacy-colon-bound", "ORNA091-E-BOUND-COLON"),
        ("legacy-constraints-block", "ORNA-A091-010"),
        ("legacy-currency-declaration", "ORNA091-E-CURRENCY"),
        ("legacy-empty-closure", "E1012"),
        ("legacy-ensure", "ORNA-A091-010"),
        ("legacy-fact", "ORNA-A091-010"),
        ("legacy-field-check", "ORNA091-E-FIELD-CONSTRAINT"),
        ("legacy-field-unique", "ORNA091-E-FIELD-CONSTRAINT"),
        ("legacy-ingest", "E1004"),
        ("legacy-log", "E1001"),
        ("legacy-match", "ORNA091-E-MATCH"),
        ("legacy-opaque", "ORNA091-E-OPAQUE"),
        ("legacy-pipe-lambda", "E1011"),
        ("legacy-postfix-question", "ORNA091-E-POSTFIX-QUESTION"),
        ("legacy-refined-where", "ORNA-A091-001"),
        ("legacy-return-arrow", "ORNA091-E-RETURN-ARROW"),
        ("legacy-store", "E1003"),
        ("legacy-top-level-impl", "ORNA091-E-IMPL-FOR"),
        ("legacy-var", "ORNA091-E-VAR"),
        ("legacy-view", "E1002"),
        ("question-coalesce-adjacent", "ORNA091-E-POSTFIX-QUESTION"),
        ("question-on-int", "ORNA091-E-POSTFIX-QUESTION"),
        ("record-punning", "E1013"),
        ("row-declaration", "E8001"),
        ("static-protocol-function", "ORNA091-E-STATIC-FN"),
        ("top-level-expression", "E1006"),
        ("top-level-on", "E1005"),
        ("transaction-block", "E1007"),
        ("unparenthesized-lambda-stage", "E1204"),
    ];
    let mut mismatches = Vec::new();
    for (name, expected) in cases {
        let source = reference(&format!("examples/invalid/{name}.orna"));
        let diagnostics = if name == "row-declaration" {
            parse_row(&source).diagnostics
        } else {
            parse_module(&source).diagnostics
        };
        let actual = diagnostics.first().map(|d| d.code).unwrap_or("<none>");
        if actual != expected {
            mismatches.push(format!("{name}: {actual} != {expected}"));
        }
    }
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
}

#[test]
fn expression_ast_observes_precedence() {
    let parsed = parse_expression(&fixture("expression-precedence.orna"));
    assert!(parsed.is_ok(), "{:?}", parsed.diagnostics);
    match parsed.value {
        Expr::Binary { op, lhs, .. } => {
            assert_eq!(op, "+");
            assert!(matches!(*lhs, Expr::Name { .. }));
        }
        other => panic!("unexpected AST: {other:?}"),
    }
}

#[test]
fn expression_ast_retains_control_and_postfix_structure() {
    let indexed = parse_expression(&fixture("expression-index-field.orna"));
    assert!(
        matches!(indexed.value, Expr::Field { base, .. } if matches!(*base, Expr::Index { .. }))
    );
    let control = parse_expression(&fixture("expression-control.orna"));
    assert!(matches!(
        control.value,
        Expr::Control {
            condition: Some(_),
            body: Some(_),
            alternate: Some(_),
            ..
        }
    ));
}

#[test]
fn generic_type_constructor_is_admitted_as_a_call_callee() {
    let parsed = parse_module(&fixture("generic-money-call.orna"));
    assert!(parsed.is_ok(), "{:?}", parsed.diagnostics);
    let Declaration::Function { body, .. } = &parsed.value.items[0].declaration else {
        panic!("expected function declaration");
    };
    assert!(matches!(
        body,
        Expr::GenericCall {
            callee,
            type_arguments,
            ..
        } if matches!(callee.as_ref(), Expr::Name { text, .. } if text == "Money")
            && matches!(
                type_arguments.as_slice(),
                [orna_syntax_v1::TypeExpr::Name { path, arguments, .. }]
                    if path == &["GBP".to_owned()] && arguments.is_empty()
            )
    ));
}

#[test]
fn bare_generic_call_retains_type_arguments_structurally() {
    let parsed = parse_expression(&fixture("generic-call.orna"));
    assert!(parsed.is_ok(), "{:?}", parsed.diagnostics);
    let Expr::GenericCall {
        callee,
        type_arguments,
        arguments,
        ..
    } = parsed.value
    else {
        panic!("expected a structured generic call");
    };
    assert!(matches!(
        callee.as_ref(),
        Expr::Name { text, .. } if text == "f"
    ));
    assert!(matches!(
        type_arguments.as_slice(),
        [orna_syntax_v1::TypeExpr::Name { path, arguments, .. }]
            if path == &["Int".to_owned()] && arguments.is_empty()
    ));
    assert!(matches!(
        arguments.as_slice(),
        [orna_syntax_v1::Argument { name: None, value: Expr::Name { text, .. }, .. }]
            if text == "x"
    ));
}

#[test]
fn qualified_generic_call_retains_type_arguments_structurally() {
    let parsed = parse_expression(&fixture("generic-qualified-call.orna"));
    assert!(parsed.is_ok(), "{:?}", parsed.diagnostics);
    let Expr::GenericCall {
        callee,
        type_arguments,
        arguments,
        ..
    } = parsed.value
    else {
        panic!("expected a structured generic call");
    };
    assert!(matches!(
        callee.as_ref(),
        Expr::Field { base, name, .. }
            if name == "await"
                && matches!(base.as_ref(), Expr::Name { text, .. } if text == "sys")
    ));
    assert!(matches!(
        type_arguments.as_slice(),
        [orna_syntax_v1::TypeExpr::Name { path, arguments, .. }]
            if path == &["Int".to_owned()] && arguments.is_empty()
    ));
    assert!(matches!(
        arguments.as_slice(),
        [orna_syntax_v1::Argument { name: None, value: Expr::Name { text, .. }, .. }]
            if text == "job"
    ));
}

#[test]
fn generic_calls_require_a_qualified_name_callee() {
    for name in [
        "generic-call-after-call.orna",
        "generic-call-after-index.orna",
    ] {
        let source = fixture(name);
        let parsed = parse_expression(&source);
        assert!(!parsed.is_ok(), "{name} unexpectedly parsed");
        assert!(!parsed.diagnostics.is_empty(), "{name}");
    }
}

#[test]
fn empty_generic_call_arguments_are_rejected_at_parse_stage() {
    for name in [
        "generic-call-empty.orna",
        "generic-qualified-call-empty.orna",
    ] {
        let source = fixture(name);
        let parsed = parse_expression(&source);
        assert!(!parsed.is_ok(), "{name} unexpectedly parsed");
        assert!(
            parsed.diagnostics.iter().any(|diagnostic| {
                diagnostic.code == "ORNA-PARSE-001"
                    && diagnostic.message == "generic calls require at least one type argument"
            }),
            "{name}: {:?}",
            parsed.diagnostics
        );
    }
}

#[test]
fn generic_type_arguments_reject_trailing_commas_at_every_type_argument_level() {
    macro_rules! assert_rejected {
        ($source:expr, $parsed:expr) => {{
            let parsed = $parsed;
            assert!(
                !parsed.is_ok(),
                "{source:?} unexpectedly parsed",
                source = $source
            );
            assert!(
                parsed.diagnostics.iter().any(|diagnostic| {
                    diagnostic.code == "ORNA-PARSE-001"
                        && diagnostic.message
                            == "trailing commas are not allowed in generic type arguments"
                }),
                "{:?}: {:?}",
                $source,
                parsed.diagnostics
            );
        }};
    }
    for name in [
        "generic-call-trailing-comma.orna",
        "generic-nested-trailing-comma.orna",
    ] {
        let source = fixture(name);
        assert_rejected!(&source, parse_expression(&source));
    }
    let source = fixture("generic-type-trailing-comma.orna");
    assert_rejected!(&source, parse_module(&source));
}

#[test]
fn lexical_layout_is_semantically_inert_and_strings_are_not_comments() {
    for name in [
        "layout-plain.orna",
        "layout-commented.orna",
        "layout-comment-in-string.orna",
    ] {
        let source = fixture(name);
        assert!(parse_module(&source).is_ok(), "{name}");
    }
    for name in [
        "layout-bad-block-expression.orna",
        "layout-bad-comparison-chain.orna",
        "layout-bad-pipeline-lambda.orna",
    ] {
        let source = fixture(name);
        assert!(!parse_module(&source).is_ok(), "{name}");
    }
}
