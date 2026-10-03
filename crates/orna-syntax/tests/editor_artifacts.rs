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
        if artifact.path.ends_with(".json") {
            serde_json::from_str::<serde_json::Value>(&contents)
                .unwrap_or_else(|error| panic!("invalid JSON in {}: {error}", artifact.path));
        }
    }
}

#[test]
fn editor_packages_share_keyword_inventory_and_highlight_roles() {
    let artifacts = grammar::generated_editor_artifacts();
    let content = |path: &str| {
        artifacts
            .iter()
            .find(|artifact| artifact.path == path)
            .unwrap_or_else(|| panic!("missing generated artifact {path}"))
            .contents
            .as_str()
    };

    let tree_sitter = content("editors/tree-sitter-orna/grammar.js");
    let tree_sitter_query = content("editors/tree-sitter-orna/queries/highlights.scm");
    let vim = content("editors/vim/syntax/orna.vim");
    let emacs = content("editors/emacs/orna-eglot.el");
    let sublime = content("editors/sublime/Orna.sublime-syntax");
    let textmate = content("editors/textmate/orna.tmLanguage.json");

    for spelling in grammar::keywords().iter().chain(grammar::scalar_types()) {
        let lower = spelling.to_ascii_lowercase();
        for word in lower.split_whitespace() {
            assert!(
                tree_sitter.contains(&format!("\"{word}\"")),
                "Tree-sitter grammar is missing shared spelling {word:?}"
            );
            assert!(
                tree_sitter_query.contains(&format!("(kw_{word})")),
                "Tree-sitter query is missing shared node kw_{word}"
            );
            for (editor, generated) in [
                ("Vim", vim),
                ("Emacs", emacs),
                ("Sublime", sublime),
                ("TextMate", textmate),
            ] {
                let has_word = generated
                    .to_ascii_lowercase()
                    .split(|character: char| !character.is_ascii_alphanumeric() && character != '_')
                    .any(|candidate| candidate == word);
                assert!(
                    has_word,
                    "{editor} output is missing shared spelling {word:?}"
                );
            }
        }
    }
    for capture in [
        "@comment",
        "@string",
        "@number",
        "@keyword",
        "@type",
        "@function",
        "@variable",
        "@namespace",
        "@property",
        "@operator",
        "@punctuation",
    ] {
        assert!(
            tree_sitter_query.contains(capture),
            "missing {capture} capture"
        );
    }
    assert!(content("editors/tree-sitter-orna/package.json").contains("tree-sitter-orna"));
    assert!(content("editors/vscode/package.json").contains("orna-syntax"));
}
