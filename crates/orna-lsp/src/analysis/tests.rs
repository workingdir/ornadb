use lsp_types::CompletionItemKind;
use orna_syntax_v1::Keyword;

use super::{
    check_document, completion_at, definition, document_symbols, hover, parse_document, references,
    signature_help,
};
use crate::documents::{Document, PositionMapper};

const LEXICAL_SOURCE: &str = include_str!("../../tests/fixtures/lexical-v1.orna");
const EXPRESSIONS_SOURCE: &str = include_str!("../../tests/fixtures/expressions-v1.orna");
const CALL_SOURCE: &str = include_str!("../../tests/fixtures/call-v1.orna");

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
fn diagnostics_are_exact_parser_errors_and_legacy_create_is_not_accepted() {
    let invalid = document("pub fn broken(value: Int): Int = value + ;");
    let mapper = PositionMapper::new(&invalid.text);
    let diagnostics = check_document(&invalid, &mapper);
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].source.as_deref(), Some("orna-syntax-v1"));
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
