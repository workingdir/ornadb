use super::{
    StandardLibrary, check_document, completion_at, declaration_at, hover, references,
    type_owner_name_from_source,
};
use crate::documents::{Document, PositionMapper};
use lsp_types::{
    CompletionContext, CompletionItemKind, CompletionTriggerKind, DiagnosticSeverity,
    DiagnosticTag, Hover, HoverContents, NumberOrString, Position, Range,
};

fn hover_at(text: &str, byte: usize) -> Option<Hover> {
    let document = Document::new("file:///hover.orna".parse().unwrap(), text.to_owned(), 1);
    let parse = orna_syntax::parse(text);
    let mapper = PositionMapper::new(text);
    hover(&document, &parse, None, mapper.position(byte), &mapper)
}

fn hover_markdown(hover: &Hover) -> &str {
    match &hover.contents {
        HoverContents::Markup(markup) => &markup.value,
        other => panic!("expected markdown hover, got {other:?}"),
    }
}

#[test]
fn compiler_diagnostics_preserve_raw_message_and_related_metadata() {
    let standard = StandardLibrary::load().expect("retained V11 standard must load");
    let text = "CREATE SCHEMA app;\nCREATE SCHEMA app;";
    let document = Document::new(
        "file:///duplicate.orna".parse().unwrap(),
        text.to_owned(),
        1,
    );
    let mapper = PositionMapper::new(text);

    let diagnostics = check_document(&document, Some(&standard), &mapper);

    assert_eq!(diagnostics.len(), 1);
    let diagnostic = &diagnostics[0];
    assert_eq!(diagnostic.severity, Some(DiagnosticSeverity::ERROR));
    assert_eq!(
        diagnostic.code,
        Some(NumberOrString::String("ORNA0103".to_owned()))
    );
    assert_eq!(diagnostic.message, "duplicate schema definition app");
    let primary_start = text.rfind("app").expect("redefined schema");
    assert_eq!(
        diagnostic.range,
        mapper.range(&orna_syntax::SourceSpan {
            start: primary_start,
            end: primary_start + "app".len(),
        })
    );

    let related = diagnostic
        .related_information
        .as_ref()
        .expect("duplicate diagnostic has related information");
    assert_eq!(related.len(), 1);
    assert_eq!(related[0].message, "first defined here");
    let first_start = text.find("app").expect("first schema");
    assert_eq!(
        related[0].location.range,
        mapper.range(&orna_syntax::SourceSpan {
            start: first_start,
            end: first_start + "app".len(),
        })
    );

    let data = diagnostic
        .data
        .as_ref()
        .expect("structured diagnostic data");
    assert_eq!(data["severity"], "error");
    assert_eq!(data["primaryLabel"], "redefined here");
    assert_eq!(
        data["help"],
        "rename one of the definitions or remove the duplicate"
    );
    assert_eq!(data["notes"], serde_json::json!([]));
    assert_eq!(data["related"][0]["path"], document.logical_path());
    assert_eq!(data["related"][0]["start"], first_start);
    assert_eq!(data["related"][0]["end"], first_start + "app".len());
    assert_eq!(data["related"][0]["label"], "first defined here");
}

#[test]
fn unreachable_diagnostic_preserves_warning_metadata_and_return_cause() {
    let standard = StandardLibrary::load().expect("retained V11 standard must load");
    let text = concat!(
        "CREATE SCHEMA app;\n",
        "CREATE CLIENT FUNCTION app.unreachable()\n",
        "RETURNS BOOLEAN\n",
        "IS\n",
        "BEGIN\n",
        "RETURN TRUE;\n",
        "LET ignored := FALSE;\n",
        "END;",
    );
    let document = Document::new("file:///warning.orna".parse().unwrap(), text.to_owned(), 1);
    let mapper = PositionMapper::new(text);

    let diagnostics = check_document(&document, Some(&standard), &mapper);

    assert_eq!(diagnostics.len(), 1);
    let diagnostic = &diagnostics[0];
    assert_eq!(diagnostic.severity, Some(DiagnosticSeverity::WARNING));
    assert_eq!(
        diagnostic.code,
        Some(NumberOrString::String("ORNA0401".to_owned()))
    );
    assert_eq!(diagnostic.message, "unreachable statement");
    assert_eq!(diagnostic.tags, Some(vec![DiagnosticTag::UNNECESSARY]));

    let unreachable_start = text.find("LET ignored").expect("unreachable statement");
    let unreachable_end = unreachable_start + "LET ignored := FALSE;".len();
    assert_eq!(
        diagnostic.range,
        mapper.range(&orna_syntax::SourceSpan {
            start: unreachable_start,
            end: unreachable_end,
        })
    );

    let related = diagnostic
        .related_information
        .as_ref()
        .expect("warning has return-cause information");
    assert_eq!(related.len(), 1);
    assert_eq!(
        related[0].message,
        "this statement returns from the function"
    );
    let return_start = text.find("RETURN TRUE;").expect("return statement");
    assert_eq!(
        related[0].location.range,
        mapper.range(&orna_syntax::SourceSpan {
            start: return_start,
            end: return_start + "RETURN TRUE;".len(),
        })
    );

    let data = diagnostic
        .data
        .as_ref()
        .expect("structured diagnostic data");
    assert_eq!(data["severity"], "warning");
    assert_eq!(data["primaryLabel"], "unreachable code");
    assert_eq!(
        data["notes"][0],
        "unreachable statements are still checked but can never execute"
    );
    assert_eq!(data["related"][0]["start"], return_start);
    assert_eq!(
        data["related"][0]["end"],
        return_start + "RETURN TRUE;".len()
    );
    assert_eq!(
        data["related"][0]["label"],
        "this statement returns from the function"
    );
}

#[test]
fn hover_keyword_is_preserves_procedural_and_null_contexts() {
    let procedural_text =
        "CREATE CLIENT FUNCTION app.probe() RETURNS BOOLEAN IS\nBEGIN\n    RETURN TRUE;\nEND;";
    let procedural_is = procedural_text.find(" IS\n").expect("procedural IS") + 1;
    let procedural_hover = hover_at(procedural_text, procedural_is).expect("procedural IS hover");
    let procedural_markdown = hover_markdown(&procedural_hover);
    assert!(procedural_markdown.contains("declarative function body"));
    assert!(procedural_markdown.contains("IS declarations BEGIN statements END;"));
    assert!(procedural_markdown.contains("expression IS [NOT] NULL."));

    let expression_text =
        "CREATE CLIENT FUNCTION app.probe(value BOOLEAN) RETURNS BOOLEAN AS value IS NULL;";
    let expression_is = expression_text.find(" IS NULL").expect("expression IS") + 1;
    let expression_hover = hover_at(expression_text, expression_is).expect("expression IS hover");
    let expression_markdown = hover_markdown(&expression_hover);
    assert!(expression_markdown.contains("compares an expression with NULL"));
    assert!(expression_markdown.contains("expression IS [NOT] NULL."));
    assert!(!expression_markdown.contains("IS declarations BEGIN statements END;"));
}

#[test]
fn hover_keyword_is_recognizes_pre_begin_declarations_as_procedural() {
    let text = "CREATE CLIENT FUNCTION app.probe() RETURNS BOOLEAN IS\n    STATE stored BOOLEAN;\nBEGIN\n    RETURN stored;\nEND;";
    let is = text.find(" IS\n").expect("procedural IS") + 1;
    let hover = hover_at(text, is).expect("procedural IS hover");
    let markdown = hover_markdown(&hover);
    assert!(markdown.contains("IS declarations BEGIN statements END;"));
}
#[test]
fn standard_library_loads_verified_v11_snapshot() {
    let standard = StandardLibrary::load().expect("retained V11 standard must load");
    let snapshot = standard.checked.verified_snapshot();

    assert_eq!(
        snapshot.revision(),
        orna_standard::STANDARD_LIBRARY_V11_REVISION_ID
    );
    assert_eq!(
        snapshot.source().id(),
        orna_standard::STANDARD_SOURCE_V11_REVISION_ID
    );
}

#[test]
fn completion_includes_canonical_scalar_type_spellings() {
    let parse = orna_syntax::parse("");
    let labels: Vec<_> = completion_at(&parse, None, None, None)
        .into_iter()
        .map(|item| item.label)
        .collect();

    for expected in [
        "BOOLEAN",
        "INTEGER",
        "CHARACTER LARGE OBJECT",
        "BINARY LARGE OBJECT",
    ] {
        assert!(
            labels.iter().any(|label| label == expected),
            "missing {expected}"
        );
    }
}

#[test]
fn standard_functions_appear_in_completion() {
    let standard = StandardLibrary::load().expect("standard library");
    let parse = orna_syntax::parse("");
    let items = completion_at(&parse, Some(&standard), None, None);
    assert!(items.iter().any(|item| item.label == "increment"));
}

#[test]
fn completion_resolves_nested_client_fields_through_utf16_cursor() {
    let text = concat!(
        "CREATE SCHEMA expr;\n",
        "CREATE TYPE expr.inner AS OBJECT (label TEXT);\n",
        "CREATE TYPE expr.item AS OBJECT (nested REF expr.inner, title TEXT);\n",
        "CREATE CLIENT FUNCTION expr.read(p_item REF expr.item)\n",
        "RETURNS TEXT\n",
        "AS '😀' || p_item.nested.label;\n",
    );
    let parse = orna_syntax::parse(text);
    assert!(
        parse.diagnostics().is_empty(),
        "accepted CLIENT field-path fixture must parse: {:?}",
        parse.diagnostics()
    );
    let mapper = PositionMapper::new(text);
    let outer_cursor =
        text.find("p_item.nested.").expect("outer field cursor") + "p_item.nested.".len();
    let outer_position = mapper.position(outer_cursor);
    assert_eq!(outer_position.line, 5);
    let body_line = text.lines().nth(5).expect("CLIENT body line");
    let body_start = text.find(body_line).expect("CLIENT body line start");
    assert_eq!(
        outer_position.character as usize,
        body_line[..outer_cursor - body_start]
            .encode_utf16()
            .count()
    );
    assert_eq!(mapper.byte_offset(outer_position), outer_cursor);

    let context = CompletionContext {
        trigger_kind: CompletionTriggerKind::TRIGGER_CHARACTER,
        trigger_character: Some(".".to_owned()),
    };
    let outer_items = completion_at(
        &parse,
        None,
        Some(mapper.byte_offset(outer_position)),
        Some(&context),
    );
    assert!(
        outer_items
            .iter()
            .any(|item| item.label == "label" && item.kind == Some(CompletionItemKind::FIELD)),
        "nested field completion at the UTF-16 cursor: {outer_items:?}"
    );
    assert!(
        outer_items.iter().any(|item| item.label == "CREATE"),
        "global completion at the UTF-16 cursor: {outer_items:?}"
    );

    let root_cursor = text.find("p_item.").expect("root field cursor") + "p_item.".len();
    let root_items = completion_at(
        &parse,
        None,
        Some(mapper.byte_offset(mapper.position(root_cursor))),
        Some(&context),
    );
    assert!(
        root_items
            .iter()
            .any(|item| item.label == "nested" && item.kind == Some(CompletionItemKind::FIELD)),
        "root field completion at the cursor: {root_items:?}"
    );
    assert!(
        root_items
            .iter()
            .any(|item| item.label == "title" && item.kind == Some(CompletionItemKind::FIELD)),
        "all root fields remain available at the cursor: {root_items:?}"
    );
}

#[test]
fn standard_function_hover_and_signature_use_catalogue_data() {
    let standard = StandardLibrary::load().expect("standard library");
    let text = "CREATE CLIENT FUNCTION app.probe() RETURNS INTEGER RETURN std.math.increment(1);";
    let document = Document::new(
        "file:///standard-function.orna".parse().unwrap(),
        text.to_owned(),
        1,
    );
    let parse = orna_syntax::parse(text);
    let mapper = PositionMapper::new(text);
    let function_start = text.find("std.math.increment").expect("function");
    let hover = hover(
        &document,
        &parse,
        Some(&standard),
        mapper.position(function_start + 1),
        &mapper,
    )
    .expect("standard function hover");
    assert!(!hover_markdown(&hover).is_empty());
    let open = text.find("std.math.increment(").expect("call");
    let signature = super::signature_help(
        &document,
        &parse,
        Some(&standard),
        mapper.position(open + "std.math.increment(".len()),
        &mapper,
    )
    .expect("standard function signature");
    assert_eq!(signature.signatures.len(), 1);
    assert!(signature.signatures[0].label.contains("std.math.increment"));
    assert!(signature.signatures[0].label.contains("p_value"));
}
#[test]
fn hover_multiword_scalars_cover_the_complete_type_span() {
    let text = "CREATE TYPE files.document AS OBJECT (body CHARACTER LARGE OBJECT, data BINARY LARGE OBJECT);";
    let mapper = PositionMapper::new(text);
    for (spelling, canonical) in [
        ("CHARACTER LARGE OBJECT", "CHARACTER_LARGE_OBJECT"),
        ("BINARY LARGE OBJECT", "BINARY_LARGE_OBJECT"),
    ] {
        let start = text.find(spelling).expect("scalar spelling");
        let end = start + spelling.len();
        for byte in [start, start + "LARGE".len(), end - 1] {
            let result = hover_at(text, byte).expect("scalar hover");
            assert_eq!(
                result.range,
                Some(mapper.range(&orna_syntax::SourceSpan { start, end })),
                "hover range for {spelling} at byte {byte}",
            );
            assert!(
                hover_markdown(&result).contains(canonical)
                    && hover_markdown(&result).contains("standard type"),
                "hover content for {spelling}: {}",
                hover_markdown(&result),
            );
        }
    }
}

#[test]
fn hover_multiword_scalars_respect_utf16_positions_and_context() {
    let text = "CREATE TYPE files.document AS OBJECT (\"😀body\" CHARACTER LARGE OBJECT, data BINARY LARGE OBJECT);";
    let mapper = PositionMapper::new(text);
    let character_start = text
        .find("CHARACTER LARGE OBJECT")
        .expect("character scalar");
    let character_end = character_start + "CHARACTER LARGE OBJECT".len();
    let start_position = mapper.position(character_start);
    assert_eq!(
        start_position.character as usize,
        text[..character_start].encode_utf16().count(),
        "scalar start uses UTF-16 code units",
    );
    let scalar_hover = hover_at(text, character_start).expect("scalar hover");
    assert_eq!(
        scalar_hover.range,
        Some(Range {
            start: Position {
                line: 0,
                character: 47,
            },
            end: Position {
                line: 0,
                character: 69,
            },
        }),
        "hover range uses UTF-16 units after the quoted emoji name",
    );
    assert!(hover_at(text, character_end - 1).is_some());
    assert!(hover_at(text, character_end).is_none());

    let generic_text = "CREATE TYPE files.document AS OBJECT (LARGE BOOLEAN, value OBJECT);";
    for word in ["LARGE", "OBJECT"] {
        let byte = generic_text.rfind(word).expect("generic word");
        if let Some(result) = hover_at(generic_text, byte) {
            assert!(
                !hover_markdown(&result).contains("standard type"),
                "generic word {word} incorrectly resolved as scalar: {}",
                hover_markdown(&result),
            );
        }
    }
}

#[test]
fn hover_client_local_type_sources_cover_complete_multiword_ranges() {
    let text = concat!(
        "CREATE CLIENT FUNCTION files.document() RETURNS TEXT IS\n",
        "    LET body CHARACTER LARGE OBJECT := \x27body\x27;\n",
        "BEGIN\n",
        "    LET data BINARY LARGE OBJECT := body;\n",
        "    RETURN body;\n",
        "END;",
    );
    for (spelling, canonical) in [
        ("CHARACTER LARGE OBJECT", "CHARACTER_LARGE_OBJECT"),
        ("BINARY LARGE OBJECT", "BINARY_LARGE_OBJECT"),
    ] {
        let mapper = PositionMapper::new(text);
        let start = text.find(spelling).expect("local scalar spelling");
        let end = start + spelling.len();
        for byte in [start, start + "LARGE".len(), end - 1] {
            let result = hover_at(text, byte).expect("local scalar hover");
            assert_eq!(
                result.range,
                Some(mapper.range(&orna_syntax::SourceSpan { start, end })),
                "hover range for {spelling} at byte {byte}",
            );
            assert!(
                hover_markdown(&result).contains(canonical)
                    && hover_markdown(&result).contains("standard type"),
                "hover content for {spelling}: {}",
                hover_markdown(&result),
            );
        }
    }
}

#[test]
fn hover_client_procedural_local_use_resolves_type() {
    let text = concat!(
        "CREATE CLIENT FUNCTION files.document() RETURNS BOOLEAN IS\n",
        "    LET body BOOLEAN := TRUE;\n",
        "BEGIN\n",
        "    RETURN body;\n",
        "END;",
    );
    let byte = text.rfind("body").expect("local use");
    let result = hover_at(text, byte).expect("procedural local hover");
    let markdown = hover_markdown(&result);
    assert!(
        markdown.starts_with("**parameter**"),
        "local hover kind: {markdown}"
    );
    assert!(markdown.contains("BOOLEAN"), "local hover type: {markdown}");
}

#[test]
fn hover_client_local_type_sources_reject_comment_separators() {
    let text = concat!(
        "CREATE CLIENT FUNCTION files.document() RETURNS TEXT IS\n",
        "    LET body CHARACTER /* kept */ LARGE OBJECT := \x27body\x27;\n",
        "    LET data BINARY /* kept */ LARGE OBJECT := body;\n",
        "    LET invalid CHARACTERLARGEOBJECT := body;\n",
        "BEGIN\n",
        "    RETURN body;\n",
        "END;",
    );

    for (spelling, canonical) in [
        (
            "CHARACTER /* kept */ LARGE OBJECT",
            "CHARACTER_LARGE_OBJECT",
        ),
        ("BINARY /* kept */ LARGE OBJECT", "BINARY_LARGE_OBJECT"),
    ] {
        let start = text.find(spelling).expect("commented scalar spelling");
        let end = start + spelling.len();
        let words = [
            spelling
                .split_ascii_whitespace()
                .next()
                .expect("first scalar word"),
            "LARGE",
            "OBJECT",
        ];
        for word in words {
            let byte = text[start..end]
                .find(word)
                .map(|offset| start + offset)
                .expect("commented scalar word");
            let result = hover_at(text, byte);
            let has_standard_hover = result.as_ref().is_some_and(|hover| {
                hover_markdown(hover).contains(canonical)
                    && hover_markdown(hover).contains("standard type")
            });
            let description = result
                .as_ref()
                .map(|hover| hover_markdown(hover).to_owned());
            assert!(
                !has_standard_hover,
                "commented local must not acquire standard scalar hover for {spelling}: {description:?}",
            );
        }
    }

    let invalid = text
        .find("CHARACTERLARGEOBJECT")
        .expect("invalid scalar spelling");
    assert!(
        !hover_at(text, invalid)
            .is_some_and(|hover| { hover_markdown(&hover).contains("CHARACTER_LARGE_OBJECT") })
    );
}

#[test]
fn quoted_local_type_owner_allows_comment_markers_inside_identifier() {
    let owner =
        type_owner_name_from_source("REF owners.\"foo--bar\"").expect("quoted owner type source");
    assert_eq!(
        owner.parts.last().map(|part| part.text.as_str()),
        Some("\"foo--bar\"")
    );
}

#[test]
fn hover_client_local_initializers_and_assignments_do_not_resolve_as_scalars() {
    let text = concat!(
        "CREATE CLIENT FUNCTION files.document() RETURNS TEXT IS\n",
        "    LET body CHARACTER LARGE OBJECT := std.large.object();\n",
        "BEGIN\n",
        "    LET data BINARY LARGE OBJECT := body;\n",
        "    data := std.binary.large.object();\n",
        "    RETURN body;\n",
        "END;",
    );

    for occurrence in ["std.large.object", "std.binary.large.object"] {
        let start = text.find(occurrence).expect("non-type occurrence");
        for word in occurrence.split(".") {
            let byte = text[start..]
                .find(word)
                .map(|offset| start + offset)
                .expect("occurrence word");
            let result = hover_at(text, byte);
            assert!(!result.is_some_and(|hover| {
                hover_markdown(&hover).contains("CHARACTER_LARGE_OBJECT")
                    || hover_markdown(&hover).contains("BINARY_LARGE_OBJECT")
            }));
        }
    }
}

#[test]
fn declaration_lookup_folds_unquoted_identifier_case_but_preserves_quotes() {
    let parse = orna_syntax::parse("CREATE SCHEMA foo;");

    assert!(declaration_at(&parse, "foo").is_some());
    assert!(declaration_at(&parse, "Foo").is_some());

    let quoted = orna_syntax::parse("CREATE SCHEMA \"Foo\";");
    assert!(declaration_at(&quoted, "\"Foo\"").is_some());
    assert!(declaration_at(&quoted, "\"foo\"").is_none());
    assert!(declaration_at(&quoted, "foo").is_none());
}

#[test]
fn references_fold_unquoted_case_and_exclude_qualified_declaration_component() {
    let text = concat!(
        "CREATE SCHEMA foo;\n",
        "CREATE TYPE foo.bar AS OBJECT (value BOOLEAN);\n",
        "CREATE SERVER FUNCTION baz() RETURNS BOOLEAN AS SELECT Foo;\n",
    );
    let document = Document::new("file:///test.orna".parse().unwrap(), text.to_owned(), 1);
    let parse = orna_syntax::parse(text);
    let mapper = PositionMapper::new(text);

    let foo_references = references(&document, &parse, Position::new(0, 14), &mapper, true);
    assert_eq!(foo_references.len(), 2);
    assert_eq!(foo_references[0].range.start, Position::new(0, 14));
    assert_eq!(foo_references[1].range.start, Position::new(1, 12));
    let without_declaration = references(&document, &parse, Position::new(0, 14), &mapper, false);
    assert_eq!(without_declaration.len(), 1);
    assert_eq!(without_declaration[0].range.start, Position::new(1, 12));
    let unqualified = references(&document, &parse, Position::new(2, 55), &mapper, true);
    assert!(
        unqualified.is_empty(),
        "unqualified variable must not resolve as a schema: {unqualified:?}"
    );

    let qualified_text =
        "CREATE SCHEMA product_test;\nCREATE TYPE product_test.probe AS OBJECT (value BOOLEAN);\n";
    let qualified_document = Document::new(
        "file:///qualified.orna".parse().unwrap(),
        qualified_text.to_owned(),
        1,
    );
    let qualified_parse = orna_syntax::parse(qualified_text);
    let qualified_mapper = PositionMapper::new(qualified_text);
    let probe_without_declaration = references(
        &qualified_document,
        &qualified_parse,
        Position::new(1, 25),
        &qualified_mapper,
        false,
    );
    assert!(probe_without_declaration.is_empty());
    let namespace_without_declaration = references(
        &qualified_document,
        &qualified_parse,
        Position::new(1, 12),
        &qualified_mapper,
        false,
    );
    assert_eq!(namespace_without_declaration.len(), 1);
    assert_eq!(
        namespace_without_declaration[0].range.start,
        Position::new(1, 12)
    );
}

#[test]
fn references_exclude_field_and_parameter_declarations() {
    let field_text = "CREATE SCHEMA people;\n\
              CREATE TYPE people.person AS OBJECT (stored BOOLEAN);\n\
              CREATE SERVER FUNCTION read_value() RETURNS BOOLEAN AS \
              SELECT probe.stored FROM people.person probe;\n";
    let field_document = Document::new(
        "file:///field.orna".parse().unwrap(),
        field_text.to_owned(),
        1,
    );
    let field_parse = orna_syntax::parse(field_text);
    let field_mapper = PositionMapper::new(field_text);
    let field_declaration = field_text
        .find("stored BOOLEAN")
        .expect("field declaration");
    let field_use = field_text.find("probe.stored").expect("field use") + "probe.".len();
    let field_references = references(
        &field_document,
        &field_parse,
        field_mapper.position(field_declaration),
        &field_mapper,
        false,
    );
    assert_eq!(field_references.len(), 1);
    assert_eq!(
        field_references[0].range.start,
        field_mapper.position(field_use)
    );
    let field_use_references = references(
        &field_document,
        &field_parse,
        field_mapper.position(field_use),
        &field_mapper,
        false,
    );
    assert_eq!(field_use_references.len(), 1);
    assert_eq!(
        field_use_references[0].range.start,
        field_mapper.position(field_use)
    );

    let parameter_text =
        "CREATE SERVER FUNCTION read_value(stored BOOLEAN) RETURNS BOOLEAN AS SELECT stored;\n";
    let parameter_document = Document::new(
        "file:///parameter.orna".parse().unwrap(),
        parameter_text.to_owned(),
        1,
    );
    let parameter_parse = orna_syntax::parse(parameter_text);
    let parameter_mapper = PositionMapper::new(parameter_text);
    let parameter_declaration = parameter_text
        .find("stored BOOLEAN")
        .expect("parameter declaration");
    let parameter_use =
        parameter_text.find("SELECT stored").expect("parameter use") + "SELECT ".len();
    let parameter_references = references(
        &parameter_document,
        &parameter_parse,
        parameter_mapper.position(parameter_declaration),
        &parameter_mapper,
        false,
    );
    assert_eq!(parameter_references.len(), 1);
    assert_eq!(
        parameter_references[0].range.start,
        parameter_mapper.position(parameter_use)
    );
    let parameter_use_references = references(
        &parameter_document,
        &parameter_parse,
        parameter_mapper.position(parameter_use),
        &parameter_mapper,
        false,
    );
    assert_eq!(parameter_use_references.len(), 1);
    assert_eq!(
        parameter_use_references[0].range.start,
        parameter_mapper.position(parameter_use)
    );
}

#[test]
fn definitions_scope_rows_columns_before_unrelated_fields() {
    let text = concat!(
        "CREATE SCHEMA people;\n",
        "CREATE SCHEMA other;\n",
        "CREATE TYPE people.person AS OBJECT (stored BOOLEAN);\n",
        "CREATE TYPE other.person AS OBJECT (stored BOOLEAN);\n",
        "CREATE SERVER FUNCTION read_stored() RETURNS ROWS (stored BOOLEAN) AS\n",
        "SELECT probe.stored FROM other.person probe;\n",
    );
    let document = Document::new("file:///rows.orna".parse().unwrap(), text.to_owned(), 1);
    let parse = orna_syntax::parse(text);
    let mapper = PositionMapper::new(text);
    let field_declaration = text.rfind("OBJECT (stored").expect("object field") + "OBJECT (".len();
    let return_declaration = text.find("ROWS (stored").expect("return column") + "ROWS (".len();
    let field_use = text.find("probe.stored").expect("field use") + "probe.".len();

    let return_definition = super::definition(
        &document,
        &parse,
        mapper.position(return_declaration),
        &mapper,
    )
    .expect("return column definition");
    assert_eq!(
        return_definition.range.start,
        mapper.position(return_declaration)
    );

    let field_definition =
        super::definition(&document, &parse, mapper.position(field_use), &mapper)
            .expect("object field definition");
    assert_eq!(
        field_definition.range.start,
        mapper.position(field_declaration)
    );

    let return_references = references(
        &document,
        &parse,
        mapper.position(return_declaration),
        &mapper,
        false,
    );
    assert!(
        return_references.is_empty(),
        "object field references leaked into ROWS column: {return_references:?}"
    );

    let field_references = references(
        &document,
        &parse,
        mapper.position(field_declaration),
        &mapper,
        false,
    );
    assert_eq!(field_references.len(), 1);
    assert_eq!(field_references[0].range.start, mapper.position(field_use));
}

#[test]
fn variable_definitions_and_references_stay_within_the_containing_function() {
    let text = "CREATE SERVER FUNCTION first(stored BOOLEAN) RETURNS BOOLEAN AS SELECT stored;
\
              CREATE SERVER FUNCTION second(stored BOOLEAN) RETURNS BOOLEAN AS SELECT stored;
";
    let document = Document::new(
        "file:///variables.orna".parse().unwrap(),
        text.to_owned(),
        1,
    );
    let parse = orna_syntax::parse(text);
    let mapper = PositionMapper::new(text);
    let first_parameter = text.find("first(stored").expect("first parameter") + "first(".len();
    let second_parameter = text.find("second(stored").expect("second parameter") + "second(".len();
    let first_use = text.find("SELECT stored").expect("first use") + "SELECT ".len();
    let second_use = text.rfind("SELECT stored").expect("second use") + "SELECT ".len();

    let second_definition =
        super::definition(&document, &parse, mapper.position(second_use), &mapper)
            .expect("second parameter definition");
    assert_eq!(
        second_definition.range.start,
        mapper.position(second_parameter)
    );

    let second_references = references(
        &document,
        &parse,
        mapper.position(second_use),
        &mapper,
        false,
    );
    assert_eq!(second_references.len(), 1);
    assert_eq!(
        second_references[0].range.start,
        mapper.position(second_use)
    );
    assert_ne!(second_references[0].range.start, mapper.position(first_use));

    let first_definition =
        super::definition(&document, &parse, mapper.position(first_parameter), &mapper)
            .expect("first parameter definition");
    assert_eq!(
        first_definition.range.start,
        mapper.position(first_parameter)
    );
}

#[test]
fn client_state_definitions_stay_within_their_function() {
    let text = "CREATE CLIENT FUNCTION first() RETURNS BOOLEAN IS
\
              STATE stored BOOLEAN;
\
              BEGIN RETURN stored; END;
\
              CREATE CLIENT FUNCTION second() RETURNS BOOLEAN IS
\
              STATE stored BOOLEAN;
\
              BEGIN RETURN stored; END;
";
    let document = Document::new(
        "file:///client-variables.orna".parse().unwrap(),
        text.to_owned(),
        1,
    );
    let parse = orna_syntax::parse(text);
    let mapper = PositionMapper::new(text);
    let second_state = text.rfind("STATE stored").expect("second state") + "STATE ".len();
    let second_use = text.rfind("RETURN stored").expect("second state use") + "RETURN ".len();

    let definition = super::definition(&document, &parse, mapper.position(second_use), &mapper)
        .expect("second state definition");
    assert_eq!(definition.range.start, mapper.position(second_state));

    let references = references(
        &document,
        &parse,
        mapper.position(second_use),
        &mapper,
        false,
    );
    assert_eq!(references.len(), 1);
    assert_eq!(references[0].range.start, mapper.position(second_use));
}

#[test]
fn client_pre_begin_local_shadows_parameter_in_navigation() {
    let text = "CREATE CLIENT FUNCTION shadowed(p BOOLEAN) RETURNS BOOLEAN IS
\
              LET p BOOLEAN := TRUE;
\
              LET q BOOLEAN := p;
\
              BEGIN RETURN q; END;
";
    let document = Document::new(
        "file:///client-shadowing.orna".parse().unwrap(),
        text.to_owned(),
        1,
    );
    let parse = orna_syntax::parse(text);
    let mapper = PositionMapper::new(text);
    let local_definition = text.find("LET p").expect("local declaration") + "LET ".len();
    let local_use = text.rfind(":= p").expect("local initializer") + ":= ".len();

    let definition = super::definition(&document, &parse, mapper.position(local_use), &mapper)
        .expect("local shadow definition");
    assert_eq!(definition.range.start, mapper.position(local_definition));

    let references = references(
        &document,
        &parse,
        mapper.position(local_use),
        &mapper,
        false,
    );
    assert_eq!(references.len(), 1);
    assert_eq!(references[0].range.start, mapper.position(local_use));
}

#[test]
fn client_local_definitions_stay_within_their_function() {
    let text = "CREATE CLIENT FUNCTION first() RETURNS BOOLEAN IS
\
              LET marker BOOLEAN := TRUE;
\
              BEGIN RETURN marker; END;
\
              CREATE CLIENT FUNCTION second() RETURNS BOOLEAN IS
\
              LET marker BOOLEAN := TRUE;
\
              BEGIN RETURN marker; END;
";
    let document = Document::new(
        "file:///client-locals.orna".parse().unwrap(),
        text.to_owned(),
        1,
    );
    let parse = orna_syntax::parse(text);
    let mapper = PositionMapper::new(text);
    let second_local = text.rfind("LET marker").expect("second local") + "LET ".len();
    let second_use = text.rfind("RETURN marker").expect("second local use") + "RETURN ".len();

    let definition = super::definition(&document, &parse, mapper.position(second_use), &mapper)
        .expect("second local definition");
    assert_eq!(definition.range.start, mapper.position(second_local));

    let references = references(
        &document,
        &parse,
        mapper.position(second_use),
        &mapper,
        false,
    );
    assert_eq!(references.len(), 1);
    assert_eq!(references[0].range.start, mapper.position(second_use));
}

#[test]
fn references_fold_unicode_unquoted_identifier_case() {
    let text = "CREATE SCHEMA café;\nCREATE TYPE CAFÉ.probe AS OBJECT (value BOOLEAN);\n";
    let document = Document::new("file:///unicode.orna".parse().unwrap(), text.to_owned(), 1);
    let parse = orna_syntax::parse(text);
    let mapper = PositionMapper::new(text);
    let declaration = text.find("café").expect("unicode declaration");
    let use_position = text.find("CAFÉ").expect("unicode use");

    let with_declaration = references(
        &document,
        &parse,
        mapper.position(declaration),
        &mapper,
        true,
    );
    assert_eq!(with_declaration.len(), 2);
    assert_eq!(
        with_declaration[0].range.start,
        mapper.position(declaration)
    );
    assert_eq!(
        with_declaration[1].range.start,
        mapper.position(use_position)
    );

    let without_declaration = references(
        &document,
        &parse,
        mapper.position(declaration),
        &mapper,
        false,
    );
    assert_eq!(without_declaration.len(), 1);
    assert_eq!(
        without_declaration[0].range.start,
        mapper.position(use_position)
    );
}
