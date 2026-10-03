use std::path::Path;

use orna_syntax::{HighlightKind, grammar, highlight};

const HIGHLIGHT_FIXTURE: &str = include_str!("fixtures/editor-highlighting.orna");

#[test]
fn accepted_fixture_uses_the_shared_highlight_classifications() {
    let tokens = highlight(HIGHLIGHT_FIXTURE);
    for (spelling, kind) in [
        (
            "/* Comments, literals, names, and operators all have stable classifications. */",
            HighlightKind::Comment,
        ),
        ("CREATE", HighlightKind::Keyword),
        ("tools", HighlightKind::NamespaceName),
        ("note", HighlightKind::TypeName),
        ("echo", HighlightKind::FunctionName),
        ("p_value", HighlightKind::VariableName),
        ("title", HighlightKind::PropertyName),
        ("TEXT", HighlightKind::TypeName),
        ("42", HighlightKind::NumberLiteral),
        (",", HighlightKind::Punctuation),
        ("'ready'", HighlightKind::StringLiteral),
        ("||", HighlightKind::Operator),
    ] {
        assert!(
            tokens.iter().any(|token| {
                token.kind == kind && HIGHLIGHT_FIXTURE.get(token.range.clone()) == Some(spelling)
            }),
            "expected {spelling:?} to classify as {kind:?}"
        );
    }
}

#[test]
fn checked_in_editor_artifacts_match_the_grammar_generator() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("orna-syntax is under crates/");

    for artifact in grammar::generated_editor_artifacts() {
        let path = workspace_root.join(artifact.path);
        let contents = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()));
        assert_eq!(
            contents, artifact.contents,
            "stale artifact: {}",
            artifact.path
        );
        serde_json::from_str::<serde_json::Value>(&contents)
            .unwrap_or_else(|error| panic!("invalid JSON in {}: {error}", artifact.path));
    }
}
