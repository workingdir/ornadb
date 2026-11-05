use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

use orna_syntax::{Keyword, TokenKind, editor};

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

    let tokens = orna_syntax::lex(KEYWORD_FIXTURE).expect("keyword proof fixture lexes");
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
    let parsed = orna_syntax::parse_module(EXPRESSION_FIXTURE);
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
    let manifest =
        serde_json::from_str::<serde_json::Value>(content(editor::ARTIFACT_MANIFEST_PATH))
            .expect("artifact manifest is JSON");
    assert_eq!(manifest["source"], "orna-syntax");
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
fn lsp_attachment_artifacts_expose_completion_on_every_editor_surface() {
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

    let vscode = content("editors/vscode/extension.js");
    assert!(vscode.contains("vscode-languageclient"));
    assert!(vscode.contains("new LanguageClient"));
    assert!(vscode.contains("documentSelector"));

    let sublime = content("editors/sublime/LSP-orna.sublime-settings");
    assert!(sublime.contains("\"clients\""));
    assert!(sublime.contains(editor::SOURCE_SCOPE));
}

/// The one start contract: every client starts `orna-lsp`, names exactly one
/// override, and cites the document that states the contract once. The
/// document's table is checked against the same table the renderers use, so
/// prose and generated files cannot disagree.
#[test]
fn every_editor_surface_starts_the_same_server_command() {
    let artifacts = editor::generated_artifacts();
    let content = |path: &str| {
        artifacts
            .iter()
            .find(|artifact| artifact.path == path)
            .unwrap_or_else(|| panic!("missing generated artifact {path}"))
            .contents
            .as_str()
    };
    let root = repository_root();
    let contract = fs::read_to_string(root.join(editor::LSP_CONTRACT_DOCUMENT))
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", editor::LSP_CONTRACT_DOCUMENT));

    for entry in editor::EDITOR_SERVER_COMMANDS {
        let client = content(entry.artifact);
        assert!(
            client.contains(editor::LSP_SERVER_COMMAND),
            "{} must default to {}",
            entry.artifact,
            editor::LSP_SERVER_COMMAND
        );
        assert!(
            client.contains(entry.override_setting),
            "{} must name its override {}",
            entry.artifact,
            entry.override_setting
        );
        assert!(
            client.contains(editor::LSP_CONTRACT_DOCUMENT),
            "{} must cite {}",
            entry.artifact,
            editor::LSP_CONTRACT_DOCUMENT
        );

        let rows = contract
            .lines()
            .filter(|line| {
                line.starts_with("| ")
                    && line.contains(entry.artifact)
                    && line.contains(entry.override_setting)
            })
            .collect::<Vec<_>>();
        assert_eq!(
            rows.len(),
            1,
            "{} must have exactly one contract row naming {} in {}",
            entry.artifact,
            entry.override_setting,
            editor::LSP_CONTRACT_DOCUMENT
        );
        let row = rows[0];
        assert!(row.contains(entry.editor), "{row}");
        assert!(row.contains(editor::LSP_SERVER_COMMAND), "{row}");
    }
}

/// The VS Code entry point is a real language client: `package.json` declares
/// `main`, the activation event, and the client dependency, the declared entry
/// point is a generated artifact, and the extension carries no language logic
/// of its own (grammar and semantic classification stay in the server and the
/// contributed grammar).
#[test]
fn vscode_extension_declares_a_generated_language_client() {
    let artifacts = editor::generated_artifacts();
    let content = |path: &str| {
        artifacts
            .iter()
            .find(|artifact| artifact.path == path)
            .unwrap_or_else(|| panic!("missing generated artifact {path}"))
            .contents
            .as_str()
    };
    let package: serde_json::Value = serde_json::from_str(content("editors/vscode/package.json"))
        .expect("VS Code package.json is JSON");

    assert_eq!(package["main"], "./extension.js");
    assert_eq!(
        package["activationEvents"],
        serde_json::json!(["onLanguage:orna"])
    );
    assert_eq!(
        package["dependencies"]["vscode-languageclient"].as_str(),
        Some("^9.0.1")
    );
    assert!(
        package["contributes"]["grammars"].is_array(),
        "the grammar contribution stays"
    );
    let server = &package["contributes"]["configuration"]["properties"]["orna.lsp.serverCommand"];
    assert_eq!(server["default"], serde_json::json!(["orna-lsp"]));
    assert_eq!(server["items"]["type"], "string");

    let entry = format!(
        "editors/vscode/{}",
        package["main"]
            .as_str()
            .expect("main is a string")
            .trim_start_matches("./")
    );
    assert!(
        artifacts.iter().any(|artifact| artifact.path == entry),
        "{entry} must be generated"
    );

    let extension = content("editors/vscode/extension.js");
    for language_logic in ["tmLanguage", "scopeName", "source.orna", "syntaxes/"] {
        assert!(
            !extension.contains(language_logic),
            "the extension must not carry language logic ({language_logic})"
        );
    }
    assert!(extension.contains("vscode-languageclient"));
    assert!(
        extension.ends_with("module.exports = { activate, deactivate };\n"),
        "the extension exports the entry points VS Code loads"
    );
}

#[test]
fn generated_editor_bytes_are_stable_and_cover_the_complete_editor_tree() {
    let first = editor::generated_artifacts();
    let second = editor::generated_artifacts();
    assert_eq!(first, second, "emission order and contents must be stable");

    let listed = first
        .iter()
        .map(|artifact| artifact.path)
        .collect::<Vec<_>>();
    assert_eq!(
        listed.iter().collect::<BTreeSet<_>>().len(),
        listed.len(),
        "every artifact path is emitted once"
    );
    for artifact in &first {
        assert!(!artifact.contents.is_empty(), "{} is empty", artifact.path);
        assert!(
            artifact.path.starts_with("editors/"),
            "{} is not an editor artifact",
            artifact.path
        );
    }

    let root = repository_root();
    let expected = first
        .iter()
        .filter(|artifact| artifact.path.starts_with("editors/"))
        .map(|artifact| artifact.path.to_owned())
        .collect::<BTreeSet<_>>();
    let observed = walk_editor_tree(&root.join("editors"), root);
    assert_eq!(
        observed.files, expected,
        "every editor file must be generated; add a file the generator does not \
         produce to the emitter or remove it"
    );
    for directory in &observed.directories {
        assert!(
            expected
                .iter()
                .any(|path| path.starts_with(&format!("{directory}/"))),
            "editor directory {directory} holds no generated artifact; an editor \
             surface must join the emitter before its directory lands"
        );
    }
}

#[test]
fn checked_in_editor_artifacts_match_the_v1_generator() {
    let root = repository_root();
    let mut stale = Vec::new();
    for artifact in editor::generated_artifacts() {
        let path = root.join(artifact.path);
        match std::fs::read_to_string(&path) {
            Ok(contents) if contents == artifact.contents => {}
            Ok(_) => stale.push(format!("{} differs from the emitter", artifact.path)),
            Err(error) => stale.push(format!("{}: {error}", artifact.path)),
        }
    }
    assert!(
        stale.is_empty(),
        "stale editor artifacts; regenerate with `just editor-artifacts`:\n{}",
        stale.join("\n")
    );
}

/// The repository root: `crates/orna-syntax/tests/editor_artifacts.rs` sits
/// three levels below it.
fn repository_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("orna-syntax is under crates/")
}

struct EditorTree {
    files: BTreeSet<String>,
    directories: BTreeSet<String>,
}

/// Every file and directory under `editors/`, skipping editor build output
/// (`node_modules` and `package-lock.json` from `npm install`).
fn walk_editor_tree(directory: &Path, root: &Path) -> EditorTree {
    let mut tree = EditorTree {
        files: BTreeSet::new(),
        directories: BTreeSet::new(),
    };
    collect_editor_paths(directory, root, &mut tree);
    tree
}

fn collect_editor_paths(directory: &Path, root: &Path, tree: &mut EditorTree) {
    for entry in fs::read_dir(directory).expect("editor directory is readable") {
        let path: PathBuf = entry.expect("editor directory entry is readable").path();
        let relative = path
            .strip_prefix(root)
            .expect("editor path is under the repository root")
            .to_string_lossy()
            .replace('\\', "/");
        if editor::editor_build_output(&relative) {
            continue;
        }
        if path.is_dir() {
            tree.directories.insert(relative);
            collect_editor_paths(&path, root, tree);
        } else if path.is_file() {
            tree.files.insert(relative);
        }
    }
}

#[test]
fn semantic_legend_markdown_lists_every_token_type_and_modifier_bit() {
    let markdown = editor::semantic_legend_markdown();
    let type_rows = editor::legend_token_types()
        .enumerate()
        .map(|(index, token_type)| format!("| {index} | {token_type} |"))
        .collect::<Vec<_>>();
    for row in &type_rows {
        assert!(markdown.contains(row), "missing row {row}");
    }
    for (index, modifier) in editor::TOKEN_MODIFIERS.iter().enumerate() {
        assert!(markdown.contains(&format!("| 1 << {index} | {modifier} |")));
    }
}

#[test]
fn every_legend_token_type_has_a_hover_sample_in_markdown() {
    let markdown = editor::semantic_legend_markdown();
    for token_type in editor::legend_token_types() {
        let sample = editor::token_type_sample(token_type)
            .unwrap_or_else(|| panic!("no hover sample for {token_type}"));
        assert!(
            markdown.contains(&format!("| {token_type} | `{sample}` |")),
            "markdown row for {token_type} lacks its sample"
        );
    }
    assert!(markdown.contains("| Index | Token type | Hover sample |"));
}

#[test]
fn semantic_legend_csv_has_one_row_per_token_type_and_modifier() {
    let csv = editor::semantic_legend_csv();
    let rows = csv.lines().collect::<Vec<_>>();
    assert_eq!(rows[0], "kind,index,name,sample");
    let type_count = editor::legend_token_types().count();
    assert_eq!(rows.len(), 1 + type_count + editor::TOKEN_MODIFIERS.len());
    for (index, token_type) in editor::legend_token_types().enumerate() {
        // One row per token type. The sample field is left to the sibling
        // quoting test: a sample that needs CSV quoting (the `string` sample
        // `"text"`) is not spelled here, so the row is identified by its
        // non-quoted columns.
        let prefix = format!("token_type,{index},{token_type},");
        assert_eq!(
            rows.iter().filter(|row| row.starts_with(&prefix)).count(),
            1,
            "exactly one CSV row for token type {token_type}"
        );
    }
    for (index, modifier) in editor::TOKEN_MODIFIERS.iter().enumerate() {
        assert!(rows.contains(&format!("token_modifier,{index},{modifier},").as_str()));
    }
}

#[test]
fn semantic_legend_csv_quotes_samples_that_need_it() {
    let csv = editor::semantic_legend_csv();
    assert!(csv.contains("token_type,3,string,\"\"\"text\"\"\"\n"));
}

#[test]
fn semantic_legend_version_tracks_the_crate_version() {
    assert_eq!(editor::SEMANTIC_LEGEND_VERSION, env!("CARGO_PKG_VERSION"));
    assert_eq!(editor::SEMANTIC_LEGEND_VERSION, "1.0.0");
}

#[test]
fn semantic_legend_table_aligns_every_row_to_the_same_columns() {
    let table = editor::semantic_legend_table();
    let lines = table.lines().collect::<Vec<_>>();
    assert_eq!(lines.len(), 1 + editor::legend_token_types().count());
    let header = lines[0];
    let sample_column = header.find("Hover sample").expect("sample column");
    for (index, token_type) in editor::legend_token_types().enumerate() {
        let line = lines[index + 1];
        assert!(line.starts_with(&format!("{index} ")), "{line}");
        assert!(line.contains(token_type), "{line}");
        assert_eq!(
            line.find(editor::token_type_sample(token_type).unwrap()),
            Some(sample_column),
            "{line}"
        );
    }
}

#[test]
fn semantic_legend_counts_match_the_listed_legend() {
    let (token_types, token_modifiers) = editor::semantic_legend_counts();
    assert_eq!(token_types, editor::legend_token_types().count());
    assert_eq!(token_modifiers, editor::TOKEN_MODIFIERS.len());
    assert_eq!((token_types, token_modifiers), (14, 4));
}
