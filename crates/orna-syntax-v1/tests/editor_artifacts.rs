use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

use orna_syntax_v1::{Keyword, TokenKind, editor};

// The reserved-word sample is transcribed from ORNA-LEX-007 in
// reference/source/04-lexical.md. The expression sample follows the enum,
// case, function, and interpolation forms in reference/source/06-expressions.md.
const KEYWORD_FIXTURE: &str = include_str!("fixtures/editor-keywords.orna");
const EXPRESSION_FIXTURE: &str = include_str!("fixtures/editor-expressions.orna");

const ORNA_LEX_007: &[&str] = &[
    "as", "assert", "base", "break", "case", "continue", "dim", "else", "enum", "false", "fn",
    "for", "if", "impl", "in", "let", "loop", "null", "offset", "affine", "protocol", "pub",
    "return", "self", "static", "table", "true", "type", "unit", "use", "while",
];

#[test]
fn lexer_keyword_table_and_reference_fixture_match_orna_lex_007_exactly() {
    let lexer_inventory = Keyword::ALL
        .iter()
        .map(|keyword| keyword.spelling())
        .collect::<Vec<_>>();
    assert_eq!(lexer_inventory, ORNA_LEX_007);

    let tokens = orna_syntax_v1::lex(KEYWORD_FIXTURE).expect("keyword proof fixture lexes");
    let fixture_keywords = tokens
        .iter()
        .filter_map(|token| match token.kind {
            TokenKind::Keyword(_) => Some(token.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(fixture_keywords, ORNA_LEX_007);
    for ordinary_identifier in ["T", "Z", "f", "CREATE", "TEXT", "VARCHAR", "AS"] {
        assert!(
            tokens.iter().any(|token| {
                token.text == ordinary_identifier
                    && matches!(token.kind, TokenKind::Identifier { .. })
            }),
            "{ordinary_identifier} must remain an identifier in the v1 lexer"
        );
    }
}

#[test]
fn expression_fixture_uses_v1_interpolation_and_case_arm_tokenization() {
    let parsed = orna_syntax_v1::parse_module(EXPRESSION_FIXTURE);
    assert!(
        parsed.is_ok(),
        "reference expression fixture diagnostics: {:?}",
        parsed.diagnostics
    );

    let highlights = editor::highlight(EXPRESSION_FIXTURE);
    let has = |spelling: &str, class| {
        highlights.iter().any(|token| {
            token.class == class && EXPRESSION_FIXTURE.get(token.range.clone()) == Some(spelling)
        })
    };
    assert!(has("case", editor::TokenClass::Keyword));
    assert!(has("=", editor::TokenClass::Operator));
    assert!(has("{", editor::TokenClass::Punctuation));
    assert!(has("name", editor::TokenClass::Identifier));
    assert!(has("\"", editor::TokenClass::String));
    assert!(has("hello ", editor::TokenClass::String));
}

#[test]
fn semantic_legend_and_editor_grammars_have_only_v1_lexical_classes() {
    let expected_types = [
        "keyword", "variable", "number", "string", "comment", "operator",
    ];
    assert_eq!(
        editor::semantic_token_types().collect::<Vec<_>>(),
        expected_types
    );
    for (index, class) in [
        editor::TokenClass::Keyword,
        editor::TokenClass::Identifier,
        editor::TokenClass::Number,
        editor::TokenClass::String,
        editor::TokenClass::Comment,
        editor::TokenClass::Operator,
    ]
    .into_iter()
    .enumerate()
    {
        assert_eq!(editor::semantic_token_index(class), Some(index));
    }
    assert_eq!(
        editor::semantic_token_index(editor::TokenClass::Punctuation),
        None
    );

    let artifacts = editor::generated_artifacts();
    let content = |path: &str| {
        artifacts
            .iter()
            .find(|artifact| artifact.path == path)
            .unwrap_or_else(|| panic!("missing generated artifact {path}"))
            .contents
            .as_str()
    };
    for path in [
        "editors/semantic-token-legend.json",
        "editors/textmate/orna.tmLanguage.json",
        "editors/vscode/syntaxes/orna.tmLanguage.json",
        "editors/vscode/package.json",
        "editors/tree-sitter-orna/package.json",
        "editors/tree-sitter-orna/tree-sitter.json",
        "editors/generated-artifacts.json",
    ] {
        serde_json::from_str::<serde_json::Value>(content(path))
            .unwrap_or_else(|error| panic!("invalid JSON in {path}: {error}"));
    }
    let all_generated = artifacts
        .iter()
        .map(|artifact| artifact.contents.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    for removed in [
        "TypeName",
        "FunctionName",
        "NamespaceName",
        "PropertyName",
        "QuotedIdentifier",
        "scalar-types",
        "entity.name.type.orna",
        "support.function.orna",
        "constant.language.orna",
    ] {
        assert!(
            !all_generated.contains(removed),
            "legacy editor class or scope {removed} leaked into v1 output"
        );
    }
    let textmate = content("editors/textmate/orna.tmLanguage.json");
    for keyword in ORNA_LEX_007 {
        assert!(
            textmate.contains(keyword),
            "TextMate is missing ORNA-LEX-007 keyword {keyword}"
        );
    }
    let tree_sitter = content("editors/tree-sitter-orna/grammar.js");
    for keyword in ORNA_LEX_007 {
        assert!(
            tree_sitter.contains(&format!("\"{keyword}\"")),
            "Tree-sitter is missing {keyword}"
        );
    }
    let monaco = serde_json::from_str::<serde_json::Value>(content(editor::MONACO_CONFIG_PATH))
        .expect("generated Monaco configuration is JSON");
    assert_eq!(monaco["language"]["id"], "orna");
    assert_eq!(monaco["language"]["extensions"][0], ".orna");
    assert_eq!(
        monaco["languageConfiguration"]["comments"]["lineComment"],
        "//"
    );
    assert_eq!(monaco["editorOptions"]["tabSize"], 4);
    let monarch = monaco["monarchLanguage"]["keywords"]
        .as_array()
        .expect("Monaco configuration lists lexer keywords")
        .iter()
        .filter_map(serde_json::Value::as_str)
        .collect::<Vec<_>>();
    assert_eq!(monarch, ORNA_LEX_007);

    let manifest =
        serde_json::from_str::<serde_json::Value>(content(editor::ARTIFACT_MANIFEST_PATH))
            .expect("artifact manifest is JSON");
    assert_eq!(manifest["source"], "orna-syntax-v1");
    assert_eq!(
        manifest["emitters"].as_array().unwrap().len(),
        editor::EDITOR_EMITTERS.len()
    );
    let manifest_paths = manifest["generatedFiles"]
        .as_array()
        .expect("manifest lists generated files")
        .iter()
        .map(|path| path.as_str().expect("manifest paths are strings"))
        .collect::<Vec<_>>();
    assert_eq!(
        manifest_paths,
        artifacts
            .iter()
            .map(|artifact| artifact.path)
            .collect::<Vec<_>>()
    );
}

#[test]
fn lsp_attachment_artifacts_expose_completion_on_all_three_editor_surfaces() {
    let artifacts = editor::generated_artifacts();
    let content = |path: &str| {
        artifacts
            .iter()
            .find(|artifact| artifact.path == path)
            .unwrap_or_else(|| panic!("missing generated artifact {path}"))
            .contents
            .as_str()
    };

    let neovim = content("editors/neovim/lua/orna/init.lua");
    assert!(neovim.contains("vim.filetype.add"));
    assert!(neovim.contains("nvim_create_autocmd(\"FileType\""));
    assert!(neovim.contains("vim.lsp.start"));
    assert!(neovim.contains("pattern = \"orna\""));

    let vim = content("editors/vim/plugin/orna-lsp.vim");
    assert!(vim.contains("lsp#register_server"));
    assert!(vim.contains("'allowlist': ['orna']"));
    assert!(vim.contains("setlocal omnifunc=lsp#complete"));

    let emacs = content("editors/emacs/orna-eglot.el");
    assert!(emacs.contains("orna-eglot-server-command"));
    assert!(emacs.contains("(cons '(orna-mode) orna-eglot-server-command)"));
    assert!(emacs.contains("(add-hook 'orna-mode-hook #'eglot-ensure)"));
}

#[test]
fn generated_editor_bytes_are_stable_and_cover_the_complete_editor_tree() {
    let first = editor::generated_artifacts();
    let second = editor::generated_artifacts();
    assert_eq!(first, second, "emission order and contents must be stable");

    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("orna-syntax-v1 is under crates/");
    let mut actual = BTreeSet::new();
    collect_editor_files(&root.join("editors"), root, &mut actual);
    let expected = first
        .iter()
        .filter(|artifact| artifact.path.starts_with("editors/"))
        .map(|artifact| artifact.path.to_owned())
        .collect::<BTreeSet<_>>();
    assert_eq!(actual, expected, "every editor file must be generated");
}

#[test]
fn checked_in_editor_artifacts_match_the_v1_generator() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("orna-syntax-v1 is under crates/");
    for artifact in editor::generated_artifacts() {
        let path = root.join(artifact.path);
        let contents = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()));
        assert_eq!(
            contents, artifact.contents,
            "stale artifact: {}",
            artifact.path
        );
    }
}

fn collect_editor_files(directory: &Path, root: &Path, files: &mut BTreeSet<String>) {
    for entry in fs::read_dir(directory).expect("editor directory is readable") {
        let path: PathBuf = entry.expect("editor directory entry is readable").path();
        if path.is_dir() {
            collect_editor_files(&path, root, files);
        } else if path.is_file() {
            files.insert(
                path.strip_prefix(root)
                    .expect("editor file is under the repository root")
                    .to_string_lossy()
                    .replace('\\', "/"),
            );
        }
    }
}
