use orna_syntax_v1::{EDITOR_TOKEN_TYPES, Keyword, generated_editor_artifacts, lex, parse_module};
use std::path::Path;

const V1_FIXTURE: &str = include_str!("fixtures/editor-v1.orna");
const LEGACY_FIXTURE: &str = include_str!("fixtures/editor-legacy-rejected.orna");
const ORNA_LEX_007: [&str; 31] = [
    "as", "assert", "base", "break", "case", "continue", "dim", "else", "enum", "false", "fn",
    "for", "if", "impl", "in", "let", "loop", "null", "offset", "affine", "protocol", "pub",
    "return", "self", "static", "table", "true", "type", "unit", "use", "while",
];

#[test]
fn generated_editor_artifacts_are_current_and_contain_no_legacy_syntax() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crate is under crates/");
    let generated = generated_editor_artifacts();
    assert_eq!(generated.len(), 13);
    for artifact in &generated {
        let checked_in = std::fs::read_to_string(root.join(artifact.path))
            .unwrap_or_else(|error| panic!("{}: {error}", artifact.path));
        assert_eq!(checked_in, artifact.contents, "stale {}", artifact.path);
        for forbidden in [
            "CREATE SCHEMA",
            "CREATE TYPE",
            "SELECT ",
            "INTEGER",
            "BIGINT",
            "TEXT",
            "TIMESTAMP",
            "namespace",
            "property",
        ] {
            assert!(
                !artifact.contents.contains(forbidden),
                "legacy {forbidden:?} in {}",
                artifact.path
            );
        }
        assert!(
            !artifact.contents.contains("--[^"),
            "legacy line comment in {}",
            artifact.path
        );
    }
}

#[test]
fn editor_keyword_inventory_matches_orna_lex_007_exactly() {
    let actual = Keyword::ALL.map(Keyword::spelling);
    assert_eq!(actual, ORNA_LEX_007);
    for (spelling, keyword) in ORNA_LEX_007.into_iter().zip(Keyword::ALL) {
        assert_eq!(Keyword::from_text(spelling), Some(keyword));
    }
    for legacy in ["CREATE", "SELECT", "INTEGER", "TEXT"] {
        assert!(
            Keyword::from_text(legacy).is_none(),
            "legacy token {legacy}"
        );
    }
    assert_eq!(
        EDITOR_TOKEN_TYPES,
        [
            "keyword", "function", "type", "enum", "variable", "string", "number", "operator",
            "comment"
        ]
    );
}

#[test]
fn local_editor_fixture_uses_only_v1_lexing_and_parsing() {
    assert!(lex(V1_FIXTURE).is_ok());
    let parsed = parse_module(V1_FIXTURE);
    assert!(
        parsed.is_ok(),
        "v1 fixture diagnostics: {:?}",
        parsed.diagnostics
    );
    assert!(
        !parse_module(LEGACY_FIXTURE).is_ok(),
        "legacy SQL must be rejected"
    );
}
