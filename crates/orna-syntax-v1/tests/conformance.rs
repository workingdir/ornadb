use orna_syntax_v1::{Expr, TokenKind, lex, parse_expression, parse_module, parse_repl, parse_row};
use std::path::Path;

fn reference(path: &str) -> String {
    std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../reference/Orna-1.0.0")
            .join(path),
    )
    .unwrap()
}

#[test]
fn accepts_reference_language_shapes() {
    for source in [
        "use std.math.{abs, min,};\npub table Book(id: Int) { title: Text, assert > 0; }\nfn f(x: Int): Int = x + 1;",
        "pub enum Outcome<T> { Ok { value: T }, Err { message: Text }, }\npub type Point { x: Int, y: Int, }",
        "fn map<T impl std.Show>(x: T) { let y = x | show; if true { y } else { x } }",
    ] {
        let parsed = parse_module(source);
        assert!(parsed.is_ok(), "{source:?}: {:?}", parsed.diagnostics);
    }
}
#[test]
fn entrypoints_and_precedence() {
    assert!(parse_row("{ name: \"Orna\", count: 1, };").is_ok());
    assert!(parse_repl("$_ |? recover").is_ok());
    let x = parse_expression("a + b * c ^ d ?? e");
    assert!(x.is_ok(), "{:?}", x.diagnostics);
}
#[test]
fn range_token_wins_over_numeric_dot() {
    let t = lex("1..=2").unwrap();
    assert!(matches!(t[0].kind, TokenKind::Integer));
    assert_eq!(t[1].text, "..=");
}
#[test]
fn unicode_nfc_comments_and_literals() {
    let t = lex("/* one /* two */ one */ fn cafe\u{301}() = 2026-09-05T12:30:00Z;").unwrap();
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
    for source in [
        "2025-02-29",
        "2024-13-01",
        "2024-01-01T24:00:00Z",
        "0000-01-01",
        "2024-01-01T01:02:03.1234567890Z",
        "2024-01-01T01:02:03+24:00",
        "0b102",
        "0x_FF",
        "\"bad\\q\"",
        "\"bad\\u{D800}\"",
        "\"bad ${value\"",
    ] {
        assert!(lex(source).is_err(), "{source}");
    }
}
#[test]
fn stable_errors_reject_legacy_and_bad_lexemes() {
    let old = parse_module("CREATE TABLE things;");
    assert_eq!(old.diagnostics[0].code, "ORNA-PARSE-001");
    let bad = lex("/* never").unwrap_err();
    assert_eq!(bad[0].code, "ORNA-LEX-004");
}

#[test]
fn grammar_recovery_codes_are_token_driven_under_layout_variations() {
    for (source, code) in [
        ("fn f() = a < b < c;", "E1302"),
        ("fn f() = value | item => item;", "E1204"),
        ("fn f() = (slot = value);", "E1301"),
        ("assert ;", "ORNA-A091-011"),
        ("assert true", "ORNA-A091-005"),
        ("assert true else false;", "ORNA-A091-006"),
    ] {
        let parsed = parse_module(source);
        assert_eq!(parsed.diagnostics[0].code, code, "{source}");
        let padded = source.replace(' ', " /* gap */ ");
        let parsed = parse_module(&padded);
        assert_eq!(parsed.diagnostics[0].code, code, "{padded}");
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
    let parsed = parse_expression("a + b * c");
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
    let indexed = parse_expression("items[0].name");
    assert!(
        matches!(indexed.value, Expr::Field { base, .. } if matches!(*base, Expr::Index { .. }))
    );
    let control = parse_expression("if ready { value } else { fallback }");
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
fn lexical_layout_is_semantically_inert_and_strings_are_not_comments() {
    let forms = [
        "fn f() = 1 + 2;",
        "/* outer /* nested */ */ fn /*x*/ f ( ) = 1+2 ;",
        "fn f()=\"/* not comment */\";",
    ];
    for source in forms {
        assert!(parse_module(source).is_ok(), "{source}");
    }
    for source in [
        "fn f() = { value };",
        "fn f() = a < b < c;",
        "fn f() = value | item => item;",
    ] {
        assert!(!parse_module(source).is_ok(), "{source}");
    }
}
