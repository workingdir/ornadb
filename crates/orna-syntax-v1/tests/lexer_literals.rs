use orna_syntax_v1::{lex, parse_expression, Expr, LiteralKind, TokenKind};

fn kinds(source: &str) -> Vec<TokenKind> {
    lex(source)
        .unwrap()
        .into_iter()
        .map(|token| token.kind)
        .collect()
}

#[test]
fn plain_string_stays_a_single_lossless_token() {
    let tokens = lex(r#""plain \u{7b} \"quoted\"""#).unwrap();
    assert_eq!(tokens[0].kind, TokenKind::String);
    assert_eq!(tokens[0].text, r#""plain \u{7b} \"quoted\"""#);
}

#[test]
fn interpolation_is_an_explicit_lossless_token_stream() {
    let tokens = lex(r#""hello {person.name}!""#).unwrap();
    assert_eq!(
        tokens.iter().map(|token| &token.kind).collect::<Vec<_>>(),
        vec![
            &TokenKind::StringStart,
            &TokenKind::StringText,
            &TokenKind::InterpolationStart,
            &TokenKind::Identifier {
                normalized: "person".into(),
            },
            &TokenKind::Punct("."),
            &TokenKind::Identifier {
                normalized: "name".into(),
            },
            &TokenKind::InterpolationEnd,
            &TokenKind::StringText,
            &TokenKind::StringEnd,
            &TokenKind::Eof,
        ]
    );
    assert_eq!(tokens[1].text, "hello ");
    assert_eq!(tokens[2].text, "{");
    assert_eq!(tokens[6].text, "}");
    assert_eq!(tokens[7].text, "!");
}

#[test]
fn interpolation_reuses_normal_lexing_with_nested_braces_and_strings() {
    assert_eq!(
        kinds(r#""{format({ value: "nested {name}" })}""#),
        vec![
            TokenKind::StringStart,
            TokenKind::InterpolationStart,
            TokenKind::Identifier {
                normalized: "format".into(),
            },
            TokenKind::Punct("("),
            TokenKind::Punct("{"),
            TokenKind::Identifier {
                normalized: "value".into(),
            },
            TokenKind::Punct(":"),
            TokenKind::StringStart,
            TokenKind::StringText,
            TokenKind::InterpolationStart,
            TokenKind::Identifier {
                normalized: "name".into(),
            },
            TokenKind::InterpolationEnd,
            TokenKind::StringEnd,
            TokenKind::Punct("}"),
            TokenKind::Punct(")"),
            TokenKind::InterpolationEnd,
            TokenKind::StringEnd,
            TokenKind::Eof,
        ]
    );
}

#[test]
fn nested_string_interpolation_is_limited_without_recursing_unboundedly() {
    let source = format!("{}value{}", "\"{".repeat(100), "}\"".repeat(100));
    let errors = lex(&source).expect_err("deep string interpolation must be rejected");
    assert!(
        errors.iter().any(|error| error.code == "ORNA-LEX-013"),
        "expected interpolation nesting diagnostic, got {errors:?}"
    );
}

#[test]
fn malformed_escapes_and_unclosed_interpolations_are_lexical_errors() {
    for source in [
        r#""\u{}""#,
        r#""\u{110000}""#,
        r#""\u{d800}""#,
        r#""\u{abcdef0}""#,
        r#""\q""#,
        r#""open {value""#,
    ] {
        assert!(lex(source).is_err(), "{source}");
    }
}

#[test]
fn numeric_separators_are_limited_to_digit_boundaries() {
    for source in [
        "1__2", "1_", "1_.2", "1._2", "1.2_", "1e_2", "1e2_", "1e+_2", "0x_FF", "0xFF_", "0b_1",
        "0b1_",
    ] {
        assert!(lex(source).is_err(), "{source}");
    }
    for source in ["1_000", "1_000.2_500e-3_0", "0xCA_FE", "0b10_01"] {
        assert!(lex(source).is_ok(), "{source}");
    }
}

#[test]
fn adjacent_calendar_range_is_lexed_without_parser_recovery() {
    let tokens = lex("2026-09-01..2026-09-02").unwrap();
    assert_eq!(
        tokens
            .iter()
            .map(|token| (&token.kind, token.text.as_str()))
            .collect::<Vec<_>>(),
        vec![
            (&TokenKind::Date, "2026-09-01"),
            (&TokenKind::Punct(".."), ".."),
            (&TokenKind::Date, "2026-09-02"),
            (&TokenKind::Eof, ""),
        ]
    );
    assert_eq!(
        tokens
            .iter()
            .map(|token| (token.span.start, token.span.end))
            .collect::<Vec<_>>(),
        vec![(0, 10), (10, 12), (12, 22), (22, 22)]
    );
}

#[test]
fn calendar_literals_stop_before_adjacent_operators() {
    for (source, expected) in [
        (
            "fn f() = 2026-09-01+1;",
            vec![
                (TokenKind::Date, "2026-09-01"),
                (TokenKind::Punct("+"), "+"),
                (TokenKind::Integer, "1"),
            ],
        ),
        (
            "2026-09-01T14:30:00Z+1",
            vec![
                (TokenKind::Instant, "2026-09-01T14:30:00Z"),
                (TokenKind::Punct("+"), "+"),
                (TokenKind::Integer, "1"),
            ],
        ),
        (
            "2026-09-01T14:30:00.123456789-04:30+1",
            vec![
                (TokenKind::Instant, "2026-09-01T14:30:00.123456789-04:30"),
                (TokenKind::Punct("+"), "+"),
                (TokenKind::Integer, "1"),
            ],
        ),
    ] {
        let tokens = lex(source).unwrap();
        let actual = tokens
            .iter()
            .filter(|token| {
                matches!(
                    token.kind,
                    TokenKind::Date
                        | TokenKind::Instant
                        | TokenKind::Integer
                        | TokenKind::Punct("+")
                )
            })
            .map(|token| (token.kind.clone(), token.text.as_str()))
            .collect::<Vec<_>>();
        assert_eq!(actual, expected, "{source}");
    }
}

#[test]
fn calendar_literals_preserve_complete_forms_and_spans() {
    let source = "2026-09-01T14:30:00.123456789+05:30..=2026-09-02";
    let tokens = lex(source).unwrap();
    assert_eq!(tokens[0].kind, TokenKind::Instant);
    assert_eq!(tokens[0].text, "2026-09-01T14:30:00.123456789+05:30");
    assert_eq!(tokens[0].span.start, 0);
    assert_eq!(tokens[0].span.end, 35);
    assert_eq!(tokens[1].kind, TokenKind::Punct("..="));
    assert_eq!(tokens[1].span.start, 35);
    assert_eq!(tokens[1].span.end, 38);
    assert_eq!(tokens[2].kind, TokenKind::Date);
    assert_eq!(tokens[2].span.start, 38);
    assert_eq!(tokens[2].span.end, 48);
}

#[test]
fn calendar_ast_preserves_literal_text_and_byte_spans() {
    for (source, kind) in [
        ("2026-09-01", LiteralKind::Date),
        ("2026-09-01T14:30:00.123456789+05:30", LiteralKind::Instant),
    ] {
        let parsed = parse_expression(source);
        assert!(
            parsed.diagnostics.is_empty(),
            "{source}: {:?}",
            parsed.diagnostics
        );
        let Expr::Literal {
            kind: actual_kind,
            text,
            span,
        } = parsed.value
        else {
            panic!("expected a calendar literal for {source}");
        };
        assert_eq!(actual_kind, kind, "{source}");
        assert_eq!(text, source, "{source}");
        assert_eq!(span.start, 0, "{source}");
        assert_eq!(span.end, source.len(), "{source}");
    }
}

#[test]
fn malformed_instant_tail_is_one_literal_diagnostic() {
    let source = "2024-01-01T12:xx:00Z";
    let errors = lex(source).expect_err("a malformed instant tail must be rejected as one literal");
    let error = errors
        .iter()
        .find(|error| error.code == "ORNA-LEX-008")
        .expect("expected invalid-instant diagnostic");
    assert_eq!(error.span.start, 0);
    assert_eq!(error.span.end, source.len());
}

#[test]
fn malformed_instant_offsets_are_one_full_span_diagnostic() {
    for source in [
        "2024-01-01T12:30:00+xx:00",
        "2024-01-01T12:30:00+01:xx",
        "2024-01-01T12:30:00+24:00",
        "2024-01-01T12:30:00+01:60",
    ] {
        let errors = lex(source).expect_err("a malformed offset must be rejected");
        assert_eq!(
            errors
                .iter()
                .filter(|error| error.code == "ORNA-LEX-008")
                .count(),
            1,
            "{source}: {errors:?}"
        );
        let error = errors
            .iter()
            .find(|error| error.code == "ORNA-LEX-008")
            .expect("expected invalid-instant diagnostic");
        assert_eq!(error.span.start, 0, "{source}");
        assert_eq!(error.span.end, source.len(), "{source}");
    }
}

#[test]
fn malformed_utf8_date_reports_character_boundary_span() {
    let fixture = include_str!("fixtures/unicode_17_identifier.orna");
    let source = format!("{fixture}\n2024-01-0é");
    let start = fixture.len() + 1;
    let errors = lex(&source).expect_err("a date containing a non-ASCII digit must be rejected");
    let error = errors
        .iter()
        .find(|error| error.code == "ORNA-LEX-007" && error.span.start == start)
        .expect("expected invalid-date diagnostic for the fixture suffix");
    assert_eq!(error.span.end, source.len());
    assert!(source.is_char_boundary(error.span.start));
    assert!(source.is_char_boundary(error.span.end));
}

#[test]
fn unicode_16_fixture_preserves_nfc_and_original_utf8_spans() {
    let fixture = include_str!("fixtures/unicode_17_identifier.orna");
    for (line_index, expected_text, expected_normalized, expected_span) in
        [(0, "α", "α", (4, 6)), (2, "e\u{301}", "é", (4, 7))]
    {
        let source = fixture
            .lines()
            .nth(line_index)
            .expect("Unicode 16 identifier source exists in the fixture");
        let token = lex(source)
            .unwrap()
            .into_iter()
            .find(|token| matches!(&token.kind, TokenKind::Identifier { .. }))
            .expect("fixture line contains a Unicode identifier");
        let TokenKind::Identifier { normalized } = &token.kind else {
            unreachable!("identifier token was selected");
        };
        assert_eq!(normalized, expected_normalized);
        assert_eq!(token.text, expected_text);
        assert_eq!((token.span.start, token.span.end), expected_span);
        assert_eq!(
            source.get(token.span.start..token.span.end),
            Some(expected_text)
        );
    }
}
