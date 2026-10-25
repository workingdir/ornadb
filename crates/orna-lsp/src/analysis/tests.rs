use std::sync::Arc;

use lsp_types::{CompletionItemKind, Diagnostic};
use orna_syntax_v1::Keyword;

use super::{
    completion_at, declaration_symbols, definition, document_symbols, hover,
    lsp_diagnostic_message, references, signature_help, syntax_diagnostics,
};
use crate::documents::{Document, PositionMapper};
use crate::workspace::{FileAnalysis, Workspace};

const LEXICAL_SOURCE: &str = include_str!("../../tests/fixtures/lexical-v1.orna");
const EXPRESSIONS_SOURCE: &str = include_str!("../../tests/fixtures/expressions-v1.orna");
const CALL_SOURCE: &str = include_str!("../../tests/fixtures/call-v1.orna");
const LOCAL_SCOPES_SOURCE: &str = include_str!("../../tests/fixtures/local-scopes-v1.orna");
const STANDARD_MATH_SOURCE: &str = include_str!("fixtures/standard-math-import.orna");
const DOCUMENT_SYMBOLS_SOURCE: &str = include_str!("../../tests/fixtures/document-symbols-v1.orna");

fn document(text: &str) -> Document {
    Document::new(
        "file:///workspace/editor.orna".parse().unwrap(),
        text.to_owned(),
        1,
    )
}

/// One detached document; its module is 0.
fn workspace(text: &str) -> Workspace {
    Workspace::detached(vec![Arc::new(FileAnalysis::new(document(text)))])
}

fn check(text: &str) -> Vec<Diagnostic> {
    let document = document(text);
    let mapper = PositionMapper::new(&document.text);
    syntax_diagnostics(
        &document.uri,
        &orna_syntax_v1::parse_module(&document.text),
        &mapper,
    )
}

#[test]
fn in_crate_fixtures_parse_with_the_frozen_1_0_frontend() {
    for source in [LEXICAL_SOURCE, EXPRESSIONS_SOURCE, CALL_SOURCE] {
        let parsed = orna_syntax_v1::parse_module(source);
        assert!(parsed.diagnostics.is_empty(), "{:#?}", parsed.diagnostics);
    }
}

#[test]
fn nested_document_symbol_model_keeps_enum_variants_and_payload_fields() {
    let parse = orna_syntax_v1::parse_module(DOCUMENT_SYMBOLS_SOURCE);
    assert!(parse.diagnostics.is_empty(), "{:#?}", parse.diagnostics);
    let orna_syntax_v1::Declaration::Enum { variants, .. } = &parse.value.items[0].declaration
    else {
        panic!("first fixture declaration must be an enum")
    };
    assert_eq!(
        variants
            .iter()
            .map(|variant| variant.name.as_str())
            .collect::<Vec<_>>(),
        ["success", "failed"],
        "enum variants are present in syntax tree"
    );
    assert_eq!(variants[0].fields[0].name, "value");
    assert_eq!(variants[1].fields[0].name, "reason");
    let symbols = declaration_symbols(&parse, DOCUMENT_SYMBOLS_SOURCE);
    assert_eq!(symbols[0].name, "Outcome");
    assert_eq!(
        symbols[0]
            .children
            .iter()
            .map(|variant| variant.name.as_str())
            .collect::<Vec<_>>(),
        ["success", "failed"],
        "nested declaration symbols: {:#?}",
        symbols[0]
    );
}

#[test]
fn keyword_completion_vocabulary_matches_lex_007_exactly() {
    const REQUIRED: &[&str] = &[
        "as", "assert", "base", "break", "case", "continue", "dim", "else", "enum", "false", "fn",
        "for", "if", "impl", "in", "let", "loop", "null", "offset", "affine", "protocol", "pub",
        "return", "self", "static", "table", "true", "type", "unit", "use", "while",
    ];
    let actual = Keyword::ALL.map(Keyword::spelling);
    assert_eq!(actual.as_slice(), REQUIRED);
    for spelling in REQUIRED {
        assert_eq!(
            Keyword::from_text(spelling).map(Keyword::spelling),
            Some(*spelling)
        );
    }
    assert!(Keyword::from_text("CREATE").is_none());
    assert!(Keyword::from_text("match").is_none());
}

#[test]
fn model_drives_function_completions_hover_signature_navigation_and_rename_ranges() {
    let workspace = workspace(EXPRESSIONS_SOURCE);
    let file = workspace.file(0);
    assert!(file.parse.diagnostics.is_empty(), "{:#?}", file.parse.diagnostics);
    let text = file.text();
    let mapper = PositionMapper::new(text);
    let symbols = document_symbols(&file.symbols, &mapper);
    assert!(symbols.iter().any(|symbol| symbol.name == "add"));

    let completions = completion_at(&workspace, 0, None, None);
    let add = completions
        .iter()
        .find(|item| item.label == "add")
        .expect("function completion");
    assert_eq!(add.kind, Some(CompletionItemKind::FUNCTION));
    assert!(
        add.detail
            .as_deref()
            .is_some_and(|detail| detail.contains("add(left: Int, right: Int)"))
    );
    assert_eq!(
        add.insert_text.as_deref(),
        Some("add(${1:left}, ${2:right})")
    );

    let call_start = text.find("add(value, 2)").unwrap();
    let call_name_byte = call_start + 1;
    let definition = definition(&workspace, 0, call_name_byte).expect("go to definition");
    assert_eq!(
        mapper.byte_offset(definition.range.start),
        text.find("add").unwrap()
    );
    let refs = references(&workspace, 0, call_name_byte, true);
    assert_eq!(refs.len(), 2, "declaration and call reference");

    let hover_card = hover(&workspace, 0, call_name_byte).expect("function hover");
    let hover_text = match hover_card.contents {
        lsp_types::HoverContents::Markup(content) => content.value,
        _ => panic!("expected markdown hover"),
    };
    assert!(hover_text.contains("Add two integer values."));
    assert!(hover_text.contains("fn add(left: Int, right: Int): Int"));

    let parameter_byte = text.find("left: Int").unwrap() + 1;
    let parameter_hover = hover(&workspace, 0, parameter_byte).expect("parameter hover");
    let parameter_hover = match parameter_hover.contents {
        lsp_types::HoverContents::Markup(content) => content.value,
        _ => panic!("expected markdown parameter hover"),
    };
    assert!(parameter_hover.contains("**parameter** `left`"));
    assert!(parameter_hover.contains("left: Int"));

    let argument_cursor = text.find("add(value, 2)").unwrap() + "add(value, ".len();
    let signature = signature_help(&workspace, 0, argument_cursor).expect("signature help");
    assert_eq!(signature.active_parameter, Some(1));
    assert!(
        signature.signatures[0]
            .label
            .contains("fn add(left: Int, right: Int): Int")
    );
}

#[test]
fn standard_library_exports_complete_and_hover_from_the_pinned_source_bundle() {
    let workspace = workspace(STANDARD_MATH_SOURCE);
    let file = workspace.file(0);
    assert!(file.parse.diagnostics.is_empty(), "{:#?}", file.parse.diagnostics);
    let text = file.text();

    let import_offset = text.find("increment};").unwrap() + "inc".len();
    let import_completions = completion_at(&workspace, 0, Some(import_offset), None);
    let increment = import_completions
        .iter()
        .find(|item| item.label == "increment")
        .expect("public math export in import completion");
    assert_eq!(increment.kind, Some(CompletionItemKind::FUNCTION));
    assert_eq!(increment.insert_text.as_deref(), Some("increment"));
    assert_eq!(increment.insert_text_format, None);
    assert!(
        increment
            .detail
            .as_deref()
            .unwrap_or_default()
            .contains("std.math")
    );
    assert!(
        increment
            .documentation
            .as_ref()
            .is_some_and(|documentation| {
                matches!(documentation, lsp_types::Documentation::MarkupContent(content)
            if content.value.contains("exact successor"))
            })
    );

    let qualified_offset = text.find("std.math.increment(value)").unwrap() + "std.math.inc".len();
    let qualified_completions = completion_at(&workspace, 0, Some(qualified_offset), None);
    assert!(qualified_completions.iter().any(|item| {
        item.label == "increment" && item.insert_text.as_deref() == Some("increment(${1:value})")
    }));
    let qualified_hover =
        hover(&workspace, 0, qualified_offset - 1).expect("qualified standard export hover");
    let lsp_types::HoverContents::Markup(qualified_hover) = qualified_hover.contents else {
        panic!("expected standard-library markdown hover")
    };
    assert!(
        qualified_hover
            .value
            .contains("fn increment(value: Int): Int")
    );
    assert!(qualified_hover.value.contains("exact successor"));

    let imported_offset = text.find("increment(value)").unwrap() + 1;
    let imported_hover =
        hover(&workspace, 0, imported_offset).expect("imported standard export hover");
    let lsp_types::HoverContents::Markup(imported_hover) = imported_hover.contents else {
        panic!("expected standard-library markdown hover")
    };
    assert!(imported_hover.value.contains("exact successor"));

    let module_offset = text.find("std.math.increment").unwrap() + "std.ma".len();
    let module_completions = completion_at(&workspace, 0, Some(module_offset), None);
    assert!(
        module_completions
            .iter()
            .any(|item| { item.label == "math" && item.kind == Some(CompletionItemKind::MODULE) })
    );
}

#[test]
fn diagnostics_are_exact_parser_errors_and_legacy_create_is_not_accepted() {
    let invalid = "pub fn broken(value: Int): Int = value + ;";
    let mapper = PositionMapper::new(invalid);
    let diagnostics = check(invalid);
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].source.as_deref(), Some("orna"));
    assert_eq!(
        diagnostics[0].severity,
        Some(lsp_types::DiagnosticSeverity::ERROR)
    );
    assert!(diagnostics[0].code.is_some());
    assert_eq!(
        diagnostics[0].data.as_ref().unwrap()["title"].as_str(),
        Some(diagnostics[0].message.as_str())
    );
    let semicolon = invalid.find(';').unwrap();
    assert_eq!(mapper.byte_offset(diagnostics[0].range.start), semicolon);

    let diagnostics = check("CREATE SCHEMA app;");
    assert!(
        !diagnostics.is_empty(),
        "legacy SQL syntax must be rejected"
    );
    assert!(
        diagnostics
            .iter()
            .all(|diagnostic| diagnostic.source.as_deref() == Some("orna"))
    );
}

#[test]
fn diagnostic_help_and_notes_are_visible_in_the_lsp_message() {
    let message = lsp_diagnostic_message(
        "Missing value",
        "expected an expression",
        &["Add a value after `=`.".to_owned()],
        &["The declaration is incomplete.".to_owned()],
    );
    assert_eq!(
        message,
        "Missing value: expected an expression\n\nHelp: Add a value after `=`.\n\nNote: The declaration is incomplete."
    );

    assert_eq!(lsp_diagnostic_message("same", "same", &[], &[]), "same");
}

#[test]
fn local_definitions_and_references_respect_shadowing_scopes() {
    let workspace = workspace(LOCAL_SCOPES_SOURCE);
    let file = workspace.file(0);
    assert!(file.parse.diagnostics.is_empty(), "{:#?}", file.parse.diagnostics);
    let text = file.text();
    let mapper = PositionMapper::new(text);

    let parameter = text.find("shadow(input").unwrap() + "shadow(".len();
    let outer_use = text.find("let copied: Int = input").unwrap() + "let copied: Int = ".len();
    let parameter_definition =
        definition(&workspace, 0, outer_use + 1).expect("outer parameter definition");
    assert_eq!(
        mapper.byte_offset(parameter_definition.range.start),
        parameter
    );
    assert_eq!(
        references(&workspace, 0, outer_use + 1, true).len(),
        4,
        "parameter declaration, condition, initializer, and shadow initializer"
    );

    let inner_declaration = text.find("let input").unwrap() + "let ".len();
    let inner_use = text.rfind("input\n").unwrap();
    let inner_definition =
        definition(&workspace, 0, inner_use + 1).expect("inner local definition");
    assert_eq!(
        mapper.byte_offset(inner_definition.range.start),
        inner_declaration
    );
    assert_eq!(
        references(&workspace, 0, inner_use + 1, true).len(),
        2,
        "inner local declaration and use"
    );

    let copied_declaration = text.find("copied:").unwrap();
    let copied_use = text.rfind("copied\n").unwrap();
    let copied_definition =
        definition(&workspace, 0, copied_use + 1).expect("local let definition");
    assert_eq!(
        mapper.byte_offset(copied_definition.range.start),
        copied_declaration
    );
    assert_eq!(
        references(&workspace, 0, copied_use + 1, true).len(),
        2,
        "local declaration and use"
    );

    for (declaration_text, declaration_offset, use_text, expected_references) in [
        ("result:", 0, "result }", 2),
        ("for item", "for ".len(), "item }", 2),
        ("value =>", 0, "value + value", 3),
    ] {
        let declaration_start = text.find(declaration_text).unwrap() + declaration_offset;
        let use_start = text.rfind(use_text).unwrap();
        let selected_definition =
            definition(&workspace, 0, use_start + 1).expect("scoped binding definition");
        assert_eq!(
            mapper.byte_offset(selected_definition.range.start),
            declaration_start,
            "declaration for {declaration_text}"
        );
        assert_eq!(
            references(&workspace, 0, use_start + 1, true).len(),
            expected_references,
            "references for {declaration_text}"
        );
    }
}

const KEYWORD_COVERAGE_SOURCE: &str = include_str!("../../tests/fixtures/keyword-coverage-v1.orna");

#[test]
fn every_lex_007_keyword_hovers_from_the_lexer_table() {
    let workspace = workspace(KEYWORD_COVERAGE_SOURCE);
    let mut hovered = Vec::new();
    for token in orna_syntax_v1::lex(KEYWORD_COVERAGE_SOURCE).expect("keyword fixture lexes") {
        let orna_syntax_v1::TokenKind::Keyword(keyword) = token.kind else {
            continue;
        };
        let card = hover(&workspace, 0, token.span.start).expect("keyword hover");
        let text = match card.contents {
            lsp_types::HoverContents::Markup(content) => content.value,
            _ => panic!("expected markdown hover"),
        };
        assert!(
            text.starts_with(&format!("**keyword** `{}`", keyword.spelling())),
            "{text}"
        );
        assert!(text.contains("ORNA-LEX-007"), "{text}");
        hovered.push(keyword);
    }
    assert_eq!(hovered, Keyword::ALL.to_vec(), "every reserved word hovers");
}

#[test]
fn keyword_completion_documentation_matches_the_keyword_hover_card() {
    let workspace = workspace(KEYWORD_COVERAGE_SOURCE);
    let completions = completion_at(&workspace, 0, None, None);
    let mut checked = Vec::new();
    for token in orna_syntax_v1::lex(KEYWORD_COVERAGE_SOURCE).expect("keyword fixture lexes") {
        let orna_syntax_v1::TokenKind::Keyword(keyword) = token.kind else {
            continue;
        };
        let item = completions
            .iter()
            .find(|item| item.label == keyword.spelling())
            .expect("keyword completion");
        assert_eq!(item.kind, Some(CompletionItemKind::KEYWORD));
        assert_eq!(item.detail.as_deref(), Some("keyword"));
        let Some(lsp_types::Documentation::String(documentation)) = item.documentation.clone()
        else {
            panic!("keyword completion carries string documentation");
        };
        assert!(documentation.contains("ORNA-LEX-007"), "{documentation}");

        let card = hover(&workspace, 0, token.span.start).expect("keyword hover");
        let lsp_types::HoverContents::Markup(content) = card.contents else {
            panic!("expected markdown hover");
        };
        assert!(
            content.value.ends_with(&documentation),
            "hover and completion share one documentation source: {}",
            content.value
        );
        checked.push(keyword);
    }
    assert_eq!(checked, Keyword::ALL.to_vec(), "every reserved word is checked");
}

const SIGNATURE_KEYWORD_ARGS_SOURCE: &str =
    include_str!("../../tests/fixtures/signature-keyword-args-v1.orna");

#[test]
fn signature_help_round_trips_through_a_reserved_word_argument() {
    let workspace = workspace(SIGNATURE_KEYWORD_ARGS_SOURCE);
    let file = workspace.file(0);
    assert!(file.parse.diagnostics.is_empty(), "{:#?}", file.parse.diagnostics);
    let text = file.text();
    let mapper = PositionMapper::new(text);

    let keyword_byte = text.find("true").unwrap();
    assert_eq!(mapper.byte_offset(mapper.position(keyword_byte)), keyword_byte);
    let help =
        signature_help(&workspace, 0, keyword_byte).expect("signature help inside the call");
    assert_eq!(help.active_parameter, Some(0));
    assert!(
        help.signatures[0].label.contains("choose"),
        "{:?}",
        help.signatures[0].label
    );

    let literal_byte = text.find("2)").unwrap();
    let literal_help =
        signature_help(&workspace, 0, literal_byte).expect("signature help on the second argument");
    assert_eq!(literal_help.active_parameter, Some(1));
}

const DIAGNOSTIC_KEYWORD_NAME_SOURCE: &str =
    include_str!("../../tests/fixtures/diagnostic-keyword-name-v1.orna");

#[test]
fn reserved_word_declaration_name_reports_a_syntax_v1_error_anchor() {
    let mapper = PositionMapper::new(DIAGNOSTIC_KEYWORD_NAME_SOURCE);
    let diagnostics = check(DIAGNOSTIC_KEYWORD_NAME_SOURCE);
    assert!(!diagnostics.is_empty(), "a reserved word cannot name a declaration");

    let keyword_start = DIAGNOSTIC_KEYWORD_NAME_SOURCE.find("in(").unwrap();
    let keyword_end = keyword_start + "in".len();
    for diagnostic in &diagnostics {
        assert_eq!(diagnostic.source.as_deref(), Some("orna"));
        let Some(lsp_types::NumberOrString::String(code)) = &diagnostic.code else {
            panic!("syntax diagnostics carry the ORNA error code: {diagnostic:?}");
        };
        assert!(code.starts_with("ORNA"), "{code}");
        assert!(!diagnostic.message.trim().is_empty());
        let start = mapper.byte_offset(diagnostic.range.start);
        assert!(
            keyword_start <= start && start <= keyword_end,
            "diagnostic {code} starts at byte {start}, outside the reserved word"
        );
    }
}

const DIAGNOSTIC_CLEAN_SOURCE: &str = include_str!("../../tests/fixtures/diagnostic-clean-v1.orna");

#[test]
fn clean_syntax_v1_fixture_renders_an_empty_diagnostic_list() {
    let diagnostics = check(DIAGNOSTIC_CLEAN_SOURCE);
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    assert_eq!(
        serde_json::to_value(&diagnostics).unwrap(),
        serde_json::json!([])
    );
}

const DIAGNOSTIC_SEVERITY_SOURCE: &str =
    include_str!("../../tests/fixtures/diagnostic-severity-v1.orna");

#[test]
fn every_syntax_v1_error_maps_to_an_error_severity_diagnostic() {
    let diagnostics = check(DIAGNOSTIC_SEVERITY_SOURCE);
    assert!(!diagnostics.is_empty(), "the fixture has a syntax error");
    for diagnostic in &diagnostics {
        assert_eq!(
            diagnostic.severity,
            Some(lsp_types::DiagnosticSeverity::ERROR),
            "{diagnostic:?}"
        );
        assert_eq!(diagnostic.source.as_deref(), Some("orna"));
    }
}

const SIGNATURE_OVERLOADS_SOURCE: &str =
    include_str!("../../tests/fixtures/signature-overloads-v1.orna");

#[test]
fn signature_help_lists_every_same_name_declaration_as_a_signature() {
    let workspace = workspace(SIGNATURE_OVERLOADS_SOURCE);
    let argument_byte = SIGNATURE_OVERLOADS_SOURCE.find("scale(1, 2)").unwrap() + "scale(1, ".len();
    let help = signature_help(&workspace, 0, argument_byte)
        .expect("the overloaded call resolves to its declarations");
    let mut counts = help
        .signatures
        .iter()
        .map(|signature| signature.parameters.as_ref().map_or(0, Vec::len))
        .collect::<Vec<_>>();
    counts.sort_unstable();
    assert_eq!(counts, vec![1, 2], "{:?}", help.signatures);
    assert_eq!(help.active_signature, Some(0));
}
