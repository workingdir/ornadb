use orna_syntax_v1::{TokenKind, lex};

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
