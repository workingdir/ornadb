use lsp_types::CompletionItemKind;
use orna_syntax_v1::Keyword;

use super::{
    check_document, completion_at, declaration_symbols, definition, document_symbols, hover,
    lsp_diagnostic_message, parse_document, references, signature_help,
};
use crate::documents::{Document, PositionMapper};

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

#[test]
fn in_crate_fixtures_parse_with_the_frozen_1_0_frontend() {
    for source in [LEXICAL_SOURCE, EXPRESSIONS_SOURCE, CALL_SOURCE] {
        let parsed = orna_syntax_v1::parse_module(source);
        assert!(parsed.diagnostics.is_empty(), "{:#?}", parsed.diagnostics);
    }
}

#[test]
fn nested_document_symbol_model_keeps_enum_variants_and_payload_fields() {
    let document = document(DOCUMENT_SYMBOLS_SOURCE);
    let parse = parse_document(&document);
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
    let symbols = declaration_symbols(&parse, &document.text);
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
    let document = document(EXPRESSIONS_SOURCE);
    let parse = parse_document(&document);
    assert!(parse.diagnostics.is_empty(), "{:#?}", parse.diagnostics);
    let mapper = PositionMapper::new(&document.text);
    let symbols = document_symbols(&parse, &document.text, &mapper);
    assert!(symbols.iter().any(|symbol| symbol.name == "add"));

    let completions = completion_at(&parse, &document.text, None, None);
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

    let call_start = document.text.find("add(value, 2)").unwrap();
    let call_name_byte = call_start + 1;
    let call_position = mapper.position(call_name_byte);
    let definition =
        definition(&document, &parse, call_position, &mapper).expect("go to definition");
    assert_eq!(
        mapper.byte_offset(definition.range.start),
        document.text.find("add").unwrap()
    );
    let refs = references(&document, &parse, call_position, &mapper, true);
    assert_eq!(refs.len(), 2, "declaration and call reference");

    let hover_card = hover(&document, &parse, call_position, &mapper).expect("function hover");
    let hover_text = match hover_card.contents {
        lsp_types::HoverContents::Markup(content) => content.value,
        _ => panic!("expected markdown hover"),
    };
    assert!(hover_text.contains("Add two integer values."));
    assert!(hover_text.contains("fn add(left: Int, right: Int): Int"));

    let parameter_position = mapper.position(document.text.find("left: Int").unwrap() + 1);
    let parameter_hover =
        hover(&document, &parse, parameter_position, &mapper).expect("parameter hover");
    let parameter_hover = match parameter_hover.contents {
        lsp_types::HoverContents::Markup(content) => content.value,
        _ => panic!("expected markdown parameter hover"),
    };
    assert!(parameter_hover.contains("**parameter** `left`"));
    assert!(parameter_hover.contains("left: Int"));

    let argument_cursor = document.text.find("add(value, 2)").unwrap() + "add(value, ".len();
    let signature = signature_help(&document, &parse, mapper.position(argument_cursor), &mapper)
        .expect("signature help");
    assert_eq!(signature.active_parameter, Some(1));
    assert!(
        signature.signatures[0]
            .label
            .contains("fn add(left: Int, right: Int): Int")
    );
}

#[test]
fn standard_library_exports_complete_and_hover_from_the_pinned_source_bundle() {
    let document = document(STANDARD_MATH_SOURCE);
    let parse = parse_document(&document);
    assert!(parse.diagnostics.is_empty(), "{:#?}", parse.diagnostics);
    let mapper = PositionMapper::new(&document.text);

    let import_offset = document.text.find("increment};").unwrap() + "inc".len();
    let import_completions = completion_at(&parse, &document.text, Some(import_offset), None);
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

    let qualified_offset =
        document.text.find("std.math.increment(value)").unwrap() + "std.math.inc".len();
    let qualified_completions = completion_at(&parse, &document.text, Some(qualified_offset), None);
    assert!(qualified_completions.iter().any(|item| {
        item.label == "increment" && item.insert_text.as_deref() == Some("increment(${1:value})")
    }));
    let qualified_hover = hover(
        &document,
        &parse,
        mapper.position(qualified_offset - 1),
        &mapper,
    )
    .expect("qualified standard export hover");
    let lsp_types::HoverContents::Markup(qualified_hover) = qualified_hover.contents else {
        panic!("expected standard-library markdown hover")
    };
    assert!(
        qualified_hover
            .value
            .contains("fn increment(value: Int): Int")
    );
    assert!(qualified_hover.value.contains("exact successor"));

    let imported_offset = document.text.find("increment(value)").unwrap() + 1;
    let imported_hover = hover(&document, &parse, mapper.position(imported_offset), &mapper)
        .expect("imported standard export hover");
    let lsp_types::HoverContents::Markup(imported_hover) = imported_hover.contents else {
        panic!("expected standard-library markdown hover")
    };
    assert!(imported_hover.value.contains("exact successor"));

    let module_offset = document.text.find("std.math.increment").unwrap() + "std.ma".len();
    let module_completions = completion_at(&parse, &document.text, Some(module_offset), None);
    assert!(
        module_completions
            .iter()
            .any(|item| { item.label == "math" && item.kind == Some(CompletionItemKind::MODULE) })
    );
}

#[test]
fn diagnostics_are_exact_parser_errors_and_legacy_create_is_not_accepted() {
    let invalid = document("pub fn broken(value: Int): Int = value + ;");
    let mapper = PositionMapper::new(&invalid.text);
    let diagnostics = check_document(&invalid, &mapper);
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].source.as_deref(), Some("orna-syntax-v1"));
    assert_eq!(
        diagnostics[0].severity,
        Some(lsp_types::DiagnosticSeverity::ERROR)
    );
    assert!(diagnostics[0].code.is_some());
    assert_eq!(
        diagnostics[0].data.as_ref().unwrap()["title"].as_str(),
        Some(diagnostics[0].message.as_str())
    );
    let semicolon = invalid.text.find(';').unwrap();
    assert_eq!(mapper.byte_offset(diagnostics[0].range.start), semicolon);

    let legacy = document("CREATE SCHEMA app;");
    let mapper = PositionMapper::new(&legacy.text);
    let diagnostics = check_document(&legacy, &mapper);
    assert!(
        !diagnostics.is_empty(),
        "legacy SQL syntax must be rejected"
    );
    assert!(
        diagnostics
            .iter()
            .all(|diagnostic| diagnostic.source.as_deref() == Some("orna-syntax-v1"))
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
    let document = document(LOCAL_SCOPES_SOURCE);
    let parse = parse_document(&document);
    assert!(parse.diagnostics.is_empty(), "{:#?}", parse.diagnostics);
    let mapper = PositionMapper::new(&document.text);

    let parameter = document.text.find("shadow(input").unwrap() + "shadow(".len();
    let outer_use =
        document.text.find("let copied: Int = input").unwrap() + "let copied: Int = ".len();
    let parameter_definition =
        definition(&document, &parse, mapper.position(outer_use + 1), &mapper)
            .expect("outer parameter definition");
    assert_eq!(
        mapper.byte_offset(parameter_definition.range.start),
        parameter
    );
    assert_eq!(
        references(
            &document,
            &parse,
            mapper.position(outer_use + 1),
            &mapper,
            true
        )
        .len(),
        4,
        "parameter declaration, condition, initializer, and shadow initializer"
    );

    let inner_declaration = document.text.find("let input").unwrap() + "let ".len();
    let inner_use = document.text.rfind("input\n").unwrap();
    let inner_definition = definition(&document, &parse, mapper.position(inner_use + 1), &mapper)
        .expect("inner local definition");
    assert_eq!(
        mapper.byte_offset(inner_definition.range.start),
        inner_declaration
    );
    assert_eq!(
        references(
            &document,
            &parse,
            mapper.position(inner_use + 1),
            &mapper,
            true
        )
        .len(),
        2,
        "inner local declaration and use"
    );

    let copied_declaration = document.text.find("copied:").unwrap();
    let copied_use = document.text.rfind("copied\n").unwrap();
    let copied_definition = definition(&document, &parse, mapper.position(copied_use + 1), &mapper)
        .expect("local let definition");
    assert_eq!(
        mapper.byte_offset(copied_definition.range.start),
        copied_declaration
    );
    assert_eq!(
        references(
            &document,
            &parse,
            mapper.position(copied_use + 1),
            &mapper,
            true
        )
        .len(),
        2,
        "local declaration and use"
    );

    for (declaration_text, declaration_offset, use_text, expected_references) in [
        ("result:", 0, "result }", 2),
        ("for item", "for ".len(), "item }", 2),
        ("value =>", 0, "value + value", 3),
    ] {
        let declaration_start = document.text.find(declaration_text).unwrap() + declaration_offset;
        let use_start = document.text.rfind(use_text).unwrap();
        let selected_definition =
            definition(&document, &parse, mapper.position(use_start + 1), &mapper)
                .expect("scoped binding definition");
        assert_eq!(
            mapper.byte_offset(selected_definition.range.start),
            declaration_start,
            "declaration for {declaration_text}"
        );
        assert_eq!(
            references(
                &document,
                &parse,
                mapper.position(use_start + 1),
                &mapper,
                true
            )
            .len(),
            expected_references,
            "references for {declaration_text}"
        );
    }
}

const KEYWORD_COVERAGE_SOURCE: &str = include_str!("../../tests/fixtures/keyword-coverage-v1.orna");

#[test]
fn every_lex_007_keyword_hovers_from_the_lexer_table() {
    let document = document(KEYWORD_COVERAGE_SOURCE);
    let parse = parse_document(&document);
    let mapper = PositionMapper::new(&document.text);
    let mut hovered = Vec::new();
    for token in orna_syntax_v1::lex(&document.text).expect("keyword fixture lexes") {
        let orna_syntax_v1::TokenKind::Keyword(keyword) = token.kind else {
            continue;
        };
        let position = mapper.position(token.span.start);
        let card = hover(&document, &parse, position, &mapper).expect("keyword hover");
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
    let document = document(KEYWORD_COVERAGE_SOURCE);
    let parse = parse_document(&document);
    let mapper = PositionMapper::new(&document.text);
    let completions = completion_at(&parse, &document.text, None, None);
    let mut checked = Vec::new();
    for token in orna_syntax_v1::lex(&document.text).expect("keyword fixture lexes") {
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

        let card = hover(&document, &parse, mapper.position(token.span.start), &mapper)
            .expect("keyword hover");
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
    let document = document(SIGNATURE_KEYWORD_ARGS_SOURCE);
    let parse = parse_document(&document);
    assert!(parse.diagnostics.is_empty(), "{:#?}", parse.diagnostics);
    let mapper = PositionMapper::new(&document.text);

    let keyword_byte = document.text.find("true").unwrap();
    let keyword_position = mapper.position(keyword_byte);
    assert_eq!(mapper.byte_offset(keyword_position), keyword_byte);
    let help = signature_help(&document, &parse, keyword_position, &mapper)
        .expect("signature help inside the call");
    assert_eq!(help.active_parameter, Some(0));
    assert!(
        help.signatures[0].label.contains("choose"),
        "{:?}",
        help.signatures[0].label
    );

    let literal_byte = document.text.find("2)").unwrap();
    let literal_help = signature_help(&document, &parse, mapper.position(literal_byte), &mapper)
        .expect("signature help on the second argument");
    assert_eq!(literal_help.active_parameter, Some(1));
}

const DIAGNOSTIC_KEYWORD_NAME_SOURCE: &str =
    include_str!("../../tests/fixtures/diagnostic-keyword-name-v1.orna");

#[test]
fn reserved_word_declaration_name_reports_a_syntax_v1_error_anchor() {
    let document = document(DIAGNOSTIC_KEYWORD_NAME_SOURCE);
    let mapper = PositionMapper::new(&document.text);
    let diagnostics = check_document(&document, &mapper);
    assert!(!diagnostics.is_empty(), "a reserved word cannot name a declaration");

    let keyword_start = document.text.find("in(").unwrap();
    let keyword_end = keyword_start + "in".len();
    for diagnostic in &diagnostics {
        assert_eq!(diagnostic.source.as_deref(), Some("orna-syntax-v1"));
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
    let document = document(DIAGNOSTIC_CLEAN_SOURCE);
    let mapper = PositionMapper::new(&document.text);
    let diagnostics = check_document(&document, &mapper);
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
    let document = document(DIAGNOSTIC_SEVERITY_SOURCE);
    let mapper = PositionMapper::new(&document.text);
    let diagnostics = check_document(&document, &mapper);
    assert!(!diagnostics.is_empty(), "the fixture has a syntax error");
    for diagnostic in &diagnostics {
        assert_eq!(
            diagnostic.severity,
            Some(lsp_types::DiagnosticSeverity::ERROR),
            "{diagnostic:?}"
        );
        assert_eq!(diagnostic.source.as_deref(), Some("orna-syntax-v1"));
    }
}
