use super::{
    StandardLibrary, check_document, completion_at, declaration_at, hover, references,
    syntax_diagnostics, type_owner_name_from_source,
};
use crate::documents::{Document, PositionMapper};
use lsp_types::{
    CompletionContext, CompletionItemKind, CompletionTriggerKind, DiagnosticSeverity, Hover,
    HoverContents, NumberOrString, Position, Range,
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
fn hover_keyword_is_preserves_procedural_and_null_contexts() {
    let procedural_text = include_str!(
        "fixtures/analysis-001-hover-keyword-is-preserves-procedural-and-null-contexts-procedural-text.orna"
    );
    let procedural_is = procedural_text.find(" IS\n").expect("procedural IS") + 1;
    let procedural_hover = hover_at(procedural_text, procedural_is).expect("procedural IS hover");
    let procedural_markdown = hover_markdown(&procedural_hover);
    assert!(procedural_markdown.contains("declarative function body"));
    assert!(procedural_markdown.contains("IS declarations BEGIN statements END;"));
    assert!(procedural_markdown.contains("expression IS [NOT] NULL."));

    let expression_text = include_str!(
        "fixtures/analysis-002-hover-keyword-is-preserves-procedural-and-null-contexts-expression-text.orna"
    );
    let expression_is = expression_text.find(" IS NULL").expect("expression IS") + 1;
    let expression_hover = hover_at(expression_text, expression_is).expect("expression IS hover");
    let expression_markdown = hover_markdown(&expression_hover);
    assert!(expression_markdown.contains("compares an expression with NULL"));
    assert!(expression_markdown.contains("expression IS [NOT] NULL."));
    assert!(!expression_markdown.contains("IS declarations BEGIN statements END;"));
}

#[test]
fn hover_keyword_is_recognizes_pre_begin_declarations_as_procedural() {
    let text = include_str!(
        "fixtures/analysis-003-hover-keyword-is-recognizes-pre-begin-declarations-as-procedural-text.orna"
    );
    let is = text.find(" IS\n").expect("procedural IS") + 1;
    let hover = hover_at(text, is).expect("procedural IS hover");
    let markdown = hover_markdown(&hover);
    assert!(markdown.contains("IS declarations BEGIN statements END;"));
}

#[test]
fn declaration_hovers_and_completions_reuse_syntax_doc_comments() {
    let text = include_str!("fixtures/editor-doc-comments.orna");
    let parse = orna_syntax::parse(text);

    let account_name = text.find("Account AS").expect("type name");
    let account_hover = hover_at(text, account_name).expect("type hover");
    assert!(hover_markdown(&account_hover).contains("Data presented by the account picker."));

    let field_name = text.find("display_name").expect("field name");
    let field_hover = hover_at(text, field_name).expect("field hover");
    assert!(hover_markdown(&field_hover).contains("Display name shown in the picker."));

    let function_name = text.find("find_account").expect("function name");
    let function_hover = hover_at(text, function_name).expect("function hover");
    assert!(hover_markdown(&function_hover).contains("Find one account by its stable identifier."));

    let parameter_name = text.find("account_id").expect("parameter name");
    let parameter_hover = hover_at(text, parameter_name).expect("parameter hover");
    assert!(hover_markdown(&parameter_hover).contains("Identifier to search for."));

    let completions = completion_at(&parse, None, None, None);
    for (label, expected) in [
        ("Account", "Data presented by the account picker."),
        ("find_account", "Find one account by its stable identifier."),
    ] {
        let item = completions
            .iter()
            .find(|item| item.label == label)
            .unwrap_or_else(|| panic!("missing completion {label}"));
        assert!(matches!(
            &item.documentation,
            Some(lsp_types::Documentation::String(documentation)) if documentation == expected
        ));
    }

    let member_byte = text.find("account.display_name").expect("field path") + "account.".len();
    let member_completions = completion_at(&parse, None, Some(member_byte), None);
    let display_name = member_completions
        .iter()
        .find(|item| item.label == "display_name")
        .expect("documented field completion");
    assert!(matches!(
        &display_name.documentation,
        Some(lsp_types::Documentation::String(documentation))
            if documentation == "Display name shown in the picker."
    ));
}

#[test]
fn standard_library_loads_the_pinned_1_0_profile() {
    let standard = StandardLibrary::load().expect("pinned 1.0 standard must load");
    let profile = standard
        .catalogue
        .standard_dependency_profile()
        .expect("source-backed standard profile");

    assert_eq!(profile.snapshot(), "orna.std/v1-reference-library");
    assert!(
        standard
            .modules
            .contains_key(&orna_semantic_v1::Namespace(vec![
                "std".to_owned(),
                "math".to_owned(),
            ]))
    );
}

#[test]
fn lsp_import_analysis_uses_pinned_1_0_standard_modules() {
    let standard = StandardLibrary::load().expect("pinned 1.0 standard must load");
    let valid = Document::new(
        "file:///app/main.orna".parse().unwrap(),
        include_str!("fixtures/standard-math-import.orna").to_owned(),
        1,
    );
    let valid_mapper = PositionMapper::new(&valid.text);
    assert!(check_document(&valid, Some(&standard), &valid_mapper).is_empty());

    let invalid = Document::new(
        "file:///app/invalid.orna".parse().unwrap(),
        include_str!("fixtures/standard-missing-import.orna").to_owned(),
        1,
    );
    let invalid_mapper = PositionMapper::new(&invalid.text);
    let diagnostics = check_document(&invalid, Some(&standard), &invalid_mapper);
    assert!(
        diagnostics.iter().any(|diagnostic| diagnostic.code
            == Some(NumberOrString::String(
                orna_semantic_v1::DIAG_IMPORT.to_owned()
            ))),
        "missing 1.0 import diagnostic: {diagnostics:?}"
    );
    assert!(diagnostics.iter().all(|diagnostic| {
        diagnostic.severity == Some(DiagnosticSeverity::ERROR)
            && diagnostic.range
                == invalid_mapper.range(&orna_syntax::SourceSpan {
                    start: 0,
                    end: invalid.text.len(),
                })
    }));
    assert_eq!(
        diagnostics[0].data.as_ref().unwrap()["standardProfile"],
        "orna.std/v1-reference-library"
    );
}

#[test]
fn syntax_diagnostics_project_real_compiler_code_message_and_span() {
    let text = include_str!("fixtures/compiler-syntax-diagnostic.orna");
    let document = Document::new(
        "file:///app/invalid-schema.orna".parse().unwrap(),
        text.to_owned(),
        1,
    );
    let mapper = PositionMapper::new(&document.text);
    let compiler = orna_compiler::parse_bundle(
        &orna_core::source::SourceBundle::new([orna_core::source::SourceUnit::new(
            document.logical_path(),
            document.text.clone(),
        )])
        .expect("valid open-document source bundle"),
    );
    let expected = compiler.diagnostics().first().expect("compiler diagnostic");
    let diagnostics = syntax_diagnostics(&document, &mapper);
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(
        diagnostics[0].code,
        Some(NumberOrString::String(expected.code().as_str().to_owned()))
    );
    assert_eq!(diagnostics[0].message, expected.message());
    assert_eq!(
        diagnostics[0].range,
        mapper.range(&orna_syntax::SourceSpan {
            start: expected.location().span().start(),
            end: expected.location().span().end(),
        })
    );
}

#[test]
fn declaration_documents_report_compiler_semantic_errors_on_the_source_token() {
    let text = include_str!("fixtures/compiler-semantic-diagnostic.orna");
    let standard = StandardLibrary::load().expect("standard library");
    let document = Document::new(
        "file:///app/unknown-type.orna".parse().unwrap(),
        text.to_owned(),
        1,
    );
    let mapper = PositionMapper::new(&document.text);
    let diagnostics = check_document(&document, Some(&standard), &mapper);
    let diagnostic = diagnostics
        .iter()
        .find(|diagnostic| {
            diagnostic.code
                == Some(NumberOrString::String(
                    orna_compiler::DiagnosticCode::UnknownQualifiedName
                        .as_str()
                        .to_owned(),
                ))
        })
        .expect("compiler unknown-type diagnostic");
    let start = text.find("MISSING_TYPE").expect("bad type token");
    assert_eq!(
        diagnostic.range,
        mapper.range(&orna_syntax::SourceSpan {
            start,
            end: start + "MISSING_TYPE".len(),
        })
    );
    assert!(diagnostic.message.contains("missing_type"));
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
    let text = include_str!(
        "fixtures/analysis-004-completion-resolves-nested-client-fields-through-utf16-cursor-text.orna"
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
fn completion_marks_targets_only_inside_accepted_constructor_arguments() {
    let text = include_str!(
        "fixtures/analysis-005-completion-marks-targets-only-inside-accepted-constructor-arguments-text.orna"
    );
    let parse = orna_syntax::parse(text);
    assert!(
        parse.diagnostics().is_empty(),
        "target completion fixture must parse: {:?}",
        parse.diagnostics()
    );
    let mapper = PositionMapper::new(text);
    let context = CompletionContext {
        trigger_kind: CompletionTriggerKind::TRIGGER_CHARACTER,
        trigger_character: Some(".".to_owned()),
    };
    let items_at = |prefix: &str| {
        let byte = text.find(prefix).expect("completion prefix") + prefix.len();
        completion_at(
            &parse,
            None,
            Some(mapper.byte_offset(mapper.position(byte))),
            Some(&context),
        )
    };
    let detail = |items: &[lsp_types::CompletionItem], label: &str| {
        items
            .iter()
            .find(|item| item.label == label)
            .unwrap_or_else(|| panic!("missing completion {label}: {items:?}"))
            .detail
            .as_deref()
            .unwrap_or("missing completion detail")
            .to_owned()
    };

    let resource_items = items_at("target => resource_fixture.");
    assert_eq!(detail(&resource_items, "scalar"), "server function target");
    assert_eq!(detail(&resource_items, "stream"), "server function");
    assert_eq!(detail(&resource_items, "client"), "client function");

    let stream_items = items_at("target => stream_fixture.");
    assert_eq!(detail(&stream_items, "stream"), "server function target");
    assert_eq!(detail(&stream_items, "scalar"), "server function");
    assert_eq!(detail(&stream_items, "client"), "client function");
    assert_eq!(detail(&stream_items, "unsupported"), "server function");

    let action_items = items_at("target => action_fixture.");
    assert_eq!(detail(&action_items, "client"), "client function target");
    assert_eq!(detail(&action_items, "scalar"), "server function target");
    assert_eq!(detail(&action_items, "stream"), "server function");
    let shadowed_items = items_at("target => std.");
    assert_eq!(detail(&shadowed_items, "scalar"), "server function");
    assert_eq!(detail(&shadowed_items, "client"), "client function");
    assert!(
        shadowed_items.iter().all(|item| {
            !item
                .detail
                .as_deref()
                .is_some_and(|detail| detail.contains("function target"))
        }),
        "a local std binding leaked target details: {shadowed_items:?}"
    );

    let field_items = items_at("AS p_item.");
    assert!(
        field_items
            .iter()
            .any(|item| item.label == "title" && item.kind == Some(CompletionItemKind::FIELD)),
        "ordinary field completion missing: {field_items:?}"
    );
    assert!(
        field_items.iter().all(|item| {
            !item
                .detail
                .as_deref()
                .is_some_and(|detail| detail.contains("function target"))
        }),
        "ordinary field path leaked target details: {field_items:?}"
    );
}

#[test]
fn standard_function_hover_and_signature_use_catalogue_data() {
    let standard = StandardLibrary::load().expect("standard library");
    let text = include_str!(
        "fixtures/analysis-006-standard-function-hover-and-signature-use-catalogue-data-text.orna"
    );
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
    let hover_text = hover_markdown(&hover);
    assert!(hover_text.contains("Returns the exact successor of `value`"));
    assert!(hover_text.contains("std.math.increment(41)"));
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
    assert!(signature.signatures[0].label.contains("value"));
}

#[test]
fn system_registry_hover_renders_signature_and_source_documentation() {
    let standard = StandardLibrary::load().expect("standard library");
    let text = include_str!("fixtures/analysis-041-system-registry-hover-text.orna");
    let document = Document::new(
        "file:///system-registry-hover.orna".parse().unwrap(),
        text.to_owned(),
        1,
    );
    let parse = orna_syntax::parse(text);
    let mapper = PositionMapper::new(text);
    let position = text.find("sys.describe").expect("system function call") + 1;
    let hover = hover(
        &document,
        &parse,
        Some(&standard),
        mapper.position(position),
        &mapper,
    )
    .expect("generated system function hover");
    let value = hover_markdown(&hover);
    assert!(value.contains("**sys function · read**"), "{value}");
    assert!(
        value.contains("fn sys.describe(object: sys.ObjectRef): sys.ObjectDescription"),
        "{value}"
    );
    assert!(
        value.contains("Returns the structured description associated with an already resolved object reference"),
        "{value}"
    );
    assert!(value.contains("does not execute the object"), "{value}");
}

#[test]
fn signature_help_accepts_unicode_and_quoted_callable_names() {
    let text = include_str!(
        "fixtures/analysis-007-signature-help-accepts-unicode-and-quoted-callable-names-text.orna"
    );
    let document = Document::new(
        "file:///signature-name-forms.orna".parse().unwrap(),
        text.to_owned(),
        1,
    );
    let parse = orna_syntax::parse(text);
    assert!(
        parse.diagnostics().is_empty(),
        "unexpected parse diagnostics"
    );
    let mapper = PositionMapper::new(text);

    for (call, expected) in [
        ("AS café(", "café"),
        ("AS \"app\".\"item\"(", "\"app\".\"item\""),
    ] {
        let open = text.find(call).expect("call");
        let signature = super::signature_help(
            &document,
            &parse,
            None,
            mapper.position(open + call.len()),
            &mapper,
        )
        .expect("signature help");
        assert!(
            signature.signatures[0].label.contains(expected),
            "signature label for {expected}: {}",
            signature.signatures[0].label
        );
    }
}
#[test]
fn hover_multiword_scalars_cover_the_complete_type_span() {
    let text = include_str!(
        "fixtures/analysis-008-hover-multiword-scalars-cover-the-complete-type-span-text.orna"
    );
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
    let text = include_str!(
        "fixtures/analysis-009-hover-multiword-scalars-respect-utf16-positions-and-context-text.orna"
    );
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

    let generic_text = include_str!(
        "fixtures/analysis-010-hover-multiword-scalars-respect-utf16-positions-and-context-generic-text.orna"
    );
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
    let text = include_str!(
        "fixtures/analysis-011-hover-client-local-type-sources-cover-complete-multiword-ranges-text.orna"
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
    let text = include_str!(
        "fixtures/analysis-012-hover-client-procedural-local-use-resolves-type-text.orna"
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
    let text = include_str!(
        "fixtures/analysis-013-hover-client-local-type-sources-reject-comment-separators-text.orna"
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
    let text = include_str!(
        "fixtures/analysis-014-hover-client-local-initializers-and-assignments-do-not-resolve-as-scalars-text.orna"
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
    let parse = orna_syntax::parse(include_str!(
        "fixtures/analysis-015-declaration-lookup-folds-unquoted-identifier-case-but-preserves-quotes-parse-input.orna"
    ));

    assert!(declaration_at(&parse, "foo").is_some());
    assert!(declaration_at(&parse, "Foo").is_some());

    let quoted = orna_syntax::parse(include_str!(
        "fixtures/analysis-016-declaration-lookup-folds-unquoted-identifier-case-but-preserves-quotes-parse-input.orna"
    ));
    assert!(declaration_at(&quoted, "\"Foo\"").is_some());
    assert!(declaration_at(&quoted, "\"foo\"").is_none());
    assert!(declaration_at(&quoted, "foo").is_none());
}
#[test]
fn qualified_type_navigation_uses_full_path_for_hover_definition_and_references() {
    let text = include_str!(
        "fixtures/analysis-017-qualified-type-navigation-uses-full-path-for-hover-definition-and-references-text.orna"
    );
    let document = Document::new(
        "file:///qualified-navigation.orna".parse().unwrap(),
        text.to_owned(),
        1,
    );
    let parse = orna_syntax::parse(text);
    let mapper = PositionMapper::new(text);
    let b_type_declaration =
        text.find("CREATE TYPE b.item").expect("b type declaration") + "CREATE TYPE ".len();
    let b_type_use = text.find("RETURNS b.item").expect("b type use") + "RETURNS ".len();
    let b_type_final = b_type_use + "b.".len();

    let hover = hover_at(text, b_type_final + 1).expect("qualified b.item hover");
    let hover_value = hover_markdown(&hover);
    assert!(
        hover_value.contains("b_value"),
        "b.item hover: {hover_value}"
    );
    assert!(
        !hover_value.contains("a_value"),
        "cross-schema hover leak: {hover_value}"
    );

    let definition = super::definition(
        &document,
        &parse,
        mapper.position(b_type_final + 1),
        &mapper,
    )
    .expect("qualified b.item definition");
    assert_eq!(definition.range.start, mapper.position(b_type_declaration));

    let references = references(
        &document,
        &parse,
        mapper.position(b_type_final + 1),
        &mapper,
        true,
    );
    let reference_starts: Vec<_> = references
        .iter()
        .map(|reference| reference.range.start)
        .collect();
    assert_eq!(
        reference_starts,
        vec![
            mapper.position(b_type_declaration + "b.".len()),
            mapper.position(b_type_use + "b.".len()),
        ],
        "qualified b.item references leaked across schemas: {references:?}",
    );
}
#[test]
fn qualified_type_navigation_consumes_line_comments_between_components() {
    let text = include_str!(
        "fixtures/analysis-018-qualified-type-navigation-consumes-line-comments-between-components-text.orna"
    );
    let document = Document::new(
        "file:///qualified-comment-navigation.orna".parse().unwrap(),
        text.to_owned(),
        1,
    );
    let parse = orna_syntax::parse(text);
    let mapper = PositionMapper::new(text);
    let b_type_declaration =
        text.find("CREATE TYPE b.item").expect("b type declaration") + "CREATE TYPE ".len();
    let b_type_use = text.find(".item AS SELECT").expect("b type use") + 1;

    let hover = hover_at(text, b_type_use + 1).expect("comment-separated b.item hover");
    let hover_value = hover_markdown(&hover);
    assert!(
        hover_value.contains("b_value"),
        "b.item hover: {hover_value}"
    );
    assert!(
        !hover_value.contains("a_value"),
        "cross-schema comment-separated hover leak: {hover_value}"
    );

    let definition = super::definition(&document, &parse, mapper.position(b_type_use + 1), &mapper)
        .expect("comment-separated b.item definition");
    assert_eq!(definition.range.start, mapper.position(b_type_declaration));

    let references = references(
        &document,
        &parse,
        mapper.position(b_type_use + 1),
        &mapper,
        true,
    );
    let reference_starts: Vec<_> = references
        .iter()
        .map(|reference| reference.range.start)
        .collect();
    assert_eq!(
        reference_starts,
        vec![
            mapper.position(b_type_declaration + "b.".len()),
            mapper.position(b_type_use),
        ],
        "comment-separated b.item references leaked across schemas: {references:?}",
    );
}

#[test]
fn quoted_top_level_references_do_not_include_same_path_fields() {
    let text = include_str!(
        "fixtures/analysis-019-quoted-top-level-references-do-not-include-same-path-fields-text.orna"
    );
    let document = Document::new(
        "file:///quoted-top-level-reference-scope.orna"
            .parse()
            .unwrap(),
        text.to_owned(),
        1,
    );
    let parse = orna_syntax::parse(text);
    let mapper = PositionMapper::new(text);
    let b_type_declaration = text
        .find("CREATE TYPE \"b\".\"item\"")
        .expect("quoted b type declaration")
        + "CREATE TYPE ".len();
    let b_type_use = text.find("FROM \"b\".\"item\"").expect("quoted b type use") + "FROM ".len();
    let b_type_final = b_type_use + "\"b\".".len();

    let definition = super::definition(
        &document,
        &parse,
        mapper.position(b_type_final + 1),
        &mapper,
    )
    .expect("quoted top-level type definition");
    assert_eq!(definition.range.start, mapper.position(b_type_declaration));

    let references = references(
        &document,
        &parse,
        mapper.position(b_type_final + 1),
        &mapper,
        true,
    );
    let reference_starts: Vec<_> = references
        .iter()
        .map(|reference| reference.range.start)
        .collect();
    assert_eq!(
        reference_starts,
        vec![
            mapper.position(b_type_declaration + "\"b\".".len()),
            mapper.position(b_type_final),
        ],
        "same-path quoted field leaked into top-level references: {references:?}",
    );
}
#[test]
fn quoted_type_and_function_references_keep_declaration_categories() {
    let text = include_str!(
        "fixtures/analysis-020-quoted-type-and-function-references-keep-declaration-categories-text.orna"
    );
    let document = Document::new(
        "file:///quoted-type-function-categories.orna"
            .parse()
            .unwrap(),
        text.to_owned(),
        1,
    );
    let parse = orna_syntax::parse(text);
    let mapper = PositionMapper::new(text);
    let type_declaration = text
        .find("CREATE TYPE \"app\".\"item\"")
        .expect("quoted type declaration")
        + "CREATE TYPE ".len();
    let type_use = text.find("FROM \"app\".\"item\"").expect("quoted type use") + "FROM ".len();
    let type_final = type_use + "\"app\".".len();
    let function_declaration = text
        .find("CREATE CLIENT FUNCTION \"app\".\"item\"")
        .expect("quoted function declaration")
        + "CREATE CLIENT FUNCTION ".len();
    let function_use = text
        .find("AS \"app\".\"item\"(TRUE);")
        .expect("quoted function use")
        + "AS ".len();
    let function_final = function_use + "\"app\".".len();

    let type_hover = hover_at(text, type_final + 1).expect("quoted type hover");
    assert!(
        hover_markdown(&type_hover).contains("object type"),
        "quoted type hover: {}",
        hover_markdown(&type_hover),
    );
    let type_definition =
        super::definition(&document, &parse, mapper.position(type_final + 1), &mapper)
            .expect("quoted type definition");
    assert_eq!(
        type_definition.range.start,
        mapper.position(type_declaration)
    );
    let type_references = references(
        &document,
        &parse,
        mapper.position(type_final + 1),
        &mapper,
        true,
    );
    let type_reference_starts: Vec<_> = type_references
        .iter()
        .map(|reference| reference.range.start)
        .collect();
    assert_eq!(
        type_reference_starts,
        vec![
            mapper.position(type_declaration + "\"app\".".len()),
            mapper.position(type_final),
        ],
        "quoted type references crossed into the function: {type_references:?}",
    );
    let type_declaration_references = references(
        &document,
        &parse,
        mapper.position(type_declaration + "\"app\".".len() + 1),
        &mapper,
        true,
    );
    let type_declaration_reference_starts: Vec<_> = type_declaration_references
        .iter()
        .map(|reference| reference.range.start)
        .collect();
    assert_eq!(
        type_declaration_reference_starts,
        vec![
            mapper.position(type_declaration + "\"app\".".len()),
            mapper.position(type_final),
        ],
        "quoted type declaration references omitted SQL use: {type_declaration_references:?}",
    );

    let function_hover = hover_at(text, function_final + 1).expect("quoted function hover");
    assert!(
        hover_markdown(&function_hover).contains("client function"),
        "quoted function hover: {}",
        hover_markdown(&function_hover),
    );
    let function_definition = super::definition(
        &document,
        &parse,
        mapper.position(function_final + 1),
        &mapper,
    )
    .expect("quoted function definition");
    assert_eq!(
        function_definition.range.start,
        mapper.position(function_declaration),
    );
    let function_references = references(
        &document,
        &parse,
        mapper.position(function_final + 1),
        &mapper,
        true,
    );
    let function_reference_starts: Vec<_> = function_references
        .iter()
        .map(|reference| reference.range.start)
        .collect();
    assert_eq!(
        function_reference_starts,
        vec![
            mapper.position(function_declaration + "\"app\".".len()),
            mapper.position(function_final),
        ],
        "quoted function references crossed into the type: {function_references:?}",
    );
    let function_declaration_references = references(
        &document,
        &parse,
        mapper.position(function_declaration + "\"app\".".len() + 1),
        &mapper,
        true,
    );
    let function_declaration_reference_starts: Vec<_> = function_declaration_references
        .iter()
        .map(|reference| reference.range.start)
        .collect();
    assert_eq!(
        function_declaration_reference_starts,
        vec![
            mapper.position(function_declaration + "\"app\".".len()),
            mapper.position(function_final),
        ],
        "quoted function declaration references omitted SQL use: {function_declaration_references:?}",
    );
}
#[test]
fn quoted_dml_aliases_do_not_resolve_as_top_level_names() {
    let text = include_str!(
        "fixtures/analysis-021-quoted-dml-aliases-do-not-resolve-as-top-level-names-text.orna"
    );
    let document = Document::new(
        "file:///quoted-dml-aliases.orna".parse().unwrap(),
        text.to_owned(),
        1,
    );
    let parse = orna_syntax::parse(text);
    assert_eq!(parse.server_functions().len(), 3, "DML fixture must parse");
    let mapper = PositionMapper::new(text);
    let aliases = [
        (
            "INSERT target alias",
            text.find("INSERT INTO \"app\".\"item\" AS \"app\"")
                .expect("INSERT target alias")
                + "INSERT INTO \"app\".\"item\" AS ".len(),
        ),
        (
            "INSERT returning alias",
            text.find("RETURNING REF(\"app\")")
                .expect("INSERT returning alias")
                + "RETURNING REF(".len(),
        ),
        (
            "UPDATE target alias",
            text.find("UPDATE \"app\".\"item\" AS \"app\"")
                .expect("UPDATE target alias")
                + "UPDATE \"app\".\"item\" AS ".len(),
        ),
        (
            "UPDATE selector alias",
            text.find("WHERE REF(\"app\")")
                .expect("UPDATE selector alias")
                + "WHERE REF(".len(),
        ),
        (
            "UPDATE returning alias",
            text.rfind("RETURNING REF(\"app\")")
                .expect("UPDATE returning alias")
                + "RETURNING REF(".len(),
        ),
        (
            "DELETE target alias",
            text.find("DELETE FROM \"app\".\"item\" AS \"app\"")
                .expect("DELETE target alias")
                + "DELETE FROM \"app\".\"item\" AS ".len(),
        ),
        (
            "DELETE selector alias",
            text.rfind("WHERE REF(\"app\")")
                .expect("DELETE selector alias")
                + "WHERE REF(".len(),
        ),
    ];
    for (label, alias) in aliases {
        let references = references(&document, &parse, mapper.position(alias + 1), &mapper, true);
        assert!(
            references.is_empty(),
            "{label} incorrectly resolved as schema: {references:?}"
        );
    }
}
#[test]
fn qualified_sql_type_path_wins_over_shadowing_parameter() {
    let text = include_str!(
        "fixtures/analysis-022-qualified-sql-type-path-wins-over-shadowing-parameter-text.orna"
    );
    let document = Document::new(
        "file:///qualified-shadowed-type.orna".parse().unwrap(),
        text.to_owned(),
        1,
    );
    let parse = orna_syntax::parse(text);
    assert_eq!(parse.server_functions().len(), 1, "fixture must parse");
    let mapper = PositionMapper::new(text);
    let type_declaration = text
        .find("CREATE TYPE \"app\".\"item\"")
        .expect("quoted type declaration")
        + "CREATE TYPE ".len();
    let type_declaration_final = type_declaration + "\"app\".".len();
    let type_use = text.find("FROM \"app\".\"item\"").expect("quoted type use") + "FROM ".len();
    let type_use_final = type_use + "\"app\".".len();

    let references = references(
        &document,
        &parse,
        mapper.position(type_use_final + 1),
        &mapper,
        true,
    );
    let reference_starts: Vec<_> = references
        .iter()
        .map(|reference| reference.range.start)
        .collect();
    assert_eq!(
        reference_starts,
        vec![
            mapper.position(type_declaration_final),
            mapper.position(type_use_final),
        ],
        "shadowing parameter displaced qualified type references: {references:?}",
    );
}

#[test]
fn quoted_sql_type_prefers_type_over_same_path_schema() {
    let text = include_str!(
        "fixtures/analysis-023-quoted-sql-type-prefers-type-over-same-path-schema-text.orna"
    );
    let document = Document::new(
        "file:///quoted-nested-schema-type.orna".parse().unwrap(),
        text.to_owned(),
        1,
    );
    let parse = orna_syntax::parse(text);
    assert_eq!(parse.schemas().len(), 1, "schema fixture must parse");
    assert_eq!(parse.object_types().len(), 1, "type fixture must parse");
    assert_eq!(
        parse.server_functions().len(),
        1,
        "query fixture must parse"
    );
    let mapper = PositionMapper::new(text);
    let type_declaration = text
        .find("CREATE TYPE \"app\".\"item\"")
        .expect("quoted type declaration")
        + "CREATE TYPE ".len();
    let type_declaration_final = type_declaration + "\"app\".".len();
    let type_use = text.find("FROM \"app\".\"item\"").expect("quoted type use") + "FROM ".len();
    let type_use_final = type_use + "\"app\".".len();
    let schema_declaration = text
        .find("CREATE SCHEMA \"app\"")
        .expect("quoted schema declaration")
        + "CREATE SCHEMA ".len();
    let schema_declaration_final = schema_declaration + "\"app\".".len();

    let type_declaration_hover =
        hover_at(text, type_declaration_final + 1).expect("quoted type declaration hover");
    assert!(
        hover_markdown(&type_declaration_hover).contains("object type"),
        "quoted type declaration resolved the wrong declaration: {}",
        hover_markdown(&type_declaration_hover),
    );
    let type_declaration_definition = super::definition(
        &document,
        &parse,
        mapper.position(type_declaration_final + 1),
        &mapper,
    )
    .expect("quoted type declaration definition");
    assert_eq!(
        type_declaration_definition.range.start,
        mapper.position(type_declaration),
    );
    let type_declaration_references = references(
        &document,
        &parse,
        mapper.position(type_declaration_final + 1),
        &mapper,
        true,
    );
    let type_declaration_reference_starts: Vec<_> = type_declaration_references
        .iter()
        .map(|reference| reference.range.start)
        .collect();
    assert_eq!(
        type_declaration_reference_starts,
        vec![
            mapper.position(type_declaration_final),
            mapper.position(type_use_final),
        ],
        "quoted type declaration references omitted SQL use: {type_declaration_references:?}",
    );

    let schema_hover =
        hover_at(text, schema_declaration_final + 1).expect("quoted schema declaration hover");
    assert!(
        hover_markdown(&schema_hover).contains("schema"),
        "quoted schema declaration resolved the wrong declaration: {}",
        hover_markdown(&schema_hover),
    );
    assert!(
        !hover_markdown(&schema_hover).contains("object type"),
        "quoted schema declaration resolved as a type: {}",
        hover_markdown(&schema_hover),
    );
    let schema_definition = super::definition(
        &document,
        &parse,
        mapper.position(schema_declaration_final + 1),
        &mapper,
    )
    .expect("quoted schema declaration definition");
    assert_eq!(
        schema_definition.range.start,
        mapper.position(schema_declaration),
    );
    let schema_references = references(
        &document,
        &parse,
        mapper.position(schema_declaration_final + 1),
        &mapper,
        true,
    );
    let schema_reference_starts: Vec<_> = schema_references
        .iter()
        .map(|reference| reference.range.start)
        .collect();
    assert_eq!(
        schema_reference_starts,
        vec![mapper.position(schema_declaration_final)],
        "quoted schema declaration picked up the same-path type: {schema_references:?}",
    );

    let hover = hover_at(text, type_use_final + 1).expect("quoted nested type hover");
    assert!(
        hover_markdown(&hover).contains("object type"),
        "nested schema/type hover resolved the wrong declaration: {}",
        hover_markdown(&hover),
    );
    let definition = super::definition(
        &document,
        &parse,
        mapper.position(type_use_final + 1),
        &mapper,
    )
    .expect("quoted nested type definition");
    assert_eq!(definition.range.start, mapper.position(type_declaration));

    let references = references(
        &document,
        &parse,
        mapper.position(type_use_final + 1),
        &mapper,
        true,
    );
    let reference_starts: Vec<_> = references
        .iter()
        .map(|reference| reference.range.start)
        .collect();
    assert_eq!(
        reference_starts,
        vec![
            mapper.position(type_declaration_final),
            mapper.position(type_use_final),
        ],
        "same-path schema was selected for a quoted SQL type: {references:?}",
    );
}

#[test]
fn quoted_dml_target_prefers_type_over_same_path_schema() {
    let text = include_str!(
        "fixtures/analysis-024-quoted-dml-target-prefers-type-over-same-path-schema-text.orna"
    );
    let document = Document::new(
        "file:///quoted-dml-target-type.orna".parse().unwrap(),
        text.to_owned(),
        1,
    );
    let parse = orna_syntax::parse(text);
    assert_eq!(parse.schemas().len(), 1, "schema fixture must parse");
    assert_eq!(parse.object_types().len(), 1, "type fixture must parse");
    assert_eq!(parse.server_functions().len(), 1, "DML fixture must parse");
    let mapper = PositionMapper::new(text);
    let type_declaration = text
        .find("CREATE TYPE \"app\".\"item\"")
        .expect("quoted type declaration")
        + "CREATE TYPE ".len();
    let type_use = text
        .find("INSERT INTO \"app\".\"item\"")
        .expect("quoted DML target")
        + "INSERT INTO ".len();
    let type_use_final = type_use + "\"app\".".len();

    let hover = hover_at(text, type_use_final + 1).expect("quoted DML target hover");
    assert!(
        hover_markdown(&hover).contains("object type"),
        "quoted DML target resolved the wrong declaration: {}",
        hover_markdown(&hover),
    );
    let definition = super::definition(
        &document,
        &parse,
        mapper.position(type_use_final + 1),
        &mapper,
    )
    .expect("quoted DML target definition");
    assert_eq!(definition.range.start, mapper.position(type_declaration));
    let references = references(
        &document,
        &parse,
        mapper.position(type_use_final + 1),
        &mapper,
        true,
    );
    let reference_starts: Vec<_> = references
        .iter()
        .map(|reference| reference.range.start)
        .collect();
    assert_eq!(
        reference_starts,
        vec![
            mapper.position(type_declaration + "\"app\".".len()),
            mapper.position(type_use_final),
        ],
        "quoted DML target references resolved the wrong declaration: {references:?}",
    );
}

#[test]
fn quoted_query_object_reference_alias_is_not_a_schema_reference() {
    let text = include_str!(
        "fixtures/analysis-025-quoted-query-object-reference-alias-is-not-a-schema-reference-text.orna"
    );
    let document = Document::new(
        "file:///quoted-query-object-reference.orna"
            .parse()
            .unwrap(),
        text.to_owned(),
        1,
    );
    let parse = orna_syntax::parse(text);
    assert_eq!(
        parse.server_functions().len(),
        1,
        "query fixture must parse"
    );
    let mapper = PositionMapper::new(text);
    let schema_declaration = text
        .find("CREATE SCHEMA \"app\"")
        .expect("quoted schema declaration")
        + "CREATE SCHEMA ".len();
    let type_namespace = text
        .find("CREATE TYPE \"app\"")
        .expect("quoted type namespace")
        + "CREATE TYPE ".len();
    let alias_use = text
        .find("SELECT REF(\"app\")")
        .expect("quoted object reference alias")
        + "SELECT REF(".len();

    let references = references(
        &document,
        &parse,
        mapper.position(schema_declaration + 1),
        &mapper,
        true,
    );
    let reference_starts: Vec<_> = references
        .iter()
        .map(|reference| reference.range.start)
        .collect();
    assert_eq!(
        reference_starts,
        vec![
            mapper.position(schema_declaration),
            mapper.position(type_namespace),
        ],
        "query object-reference alias leaked into schema references: {references:?}",
    );
    assert!(
        !reference_starts.contains(&mapper.position(alias_use)),
        "query object-reference alias was treated as schema: {references:?}",
    );
}

#[test]
fn quoted_top_level_type_references_exclude_field_and_return_declarations() {
    let text = include_str!(
        "fixtures/analysis-026-quoted-top-level-type-references-exclude-field-and-return-declarations-text.orna"
    );
    let document = Document::new(
        "file:///quoted-field-return-scope.orna".parse().unwrap(),
        text.to_owned(),
        1,
    );
    let parse = orna_syntax::parse(text);
    assert_eq!(parse.server_functions().len(), 1, "fixture must parse");
    let mapper = PositionMapper::new(text);
    let type_declaration = text
        .find("CREATE TYPE \"item\"")
        .expect("quoted type declaration")
        + "CREATE TYPE ".len();
    let type_use = text.find("FROM \"item\"").expect("quoted type use") + "FROM ".len();

    let references = references(
        &document,
        &parse,
        mapper.position(type_use + 1),
        &mapper,
        true,
    );
    let reference_starts: Vec<_> = references
        .iter()
        .map(|reference| reference.range.start)
        .collect();
    assert_eq!(
        reference_starts,
        vec![mapper.position(type_declaration), mapper.position(type_use),],
        "quoted field/return declarations leaked into top-level references: {references:?}",
    );
}

#[test]
fn qualified_function_navigation_uses_full_path_for_hover_definition_and_references() {
    let text = include_str!(
        "fixtures/analysis-027-qualified-function-navigation-uses-full-path-for-hover-definition-and-references-text.orna"
    );
    let document = Document::new(
        "file:///qualified-function-navigation.orna"
            .parse()
            .unwrap(),
        text.to_owned(),
        1,
    );
    let parse = orna_syntax::parse(text);
    let mapper = PositionMapper::new(text);
    let b_function_declaration = text
        .find("CREATE CLIENT FUNCTION b.item")
        .expect("b function declaration")
        + "CREATE CLIENT FUNCTION ".len();
    let b_function_use = text
        .find("RETURNS BOOLEAN AS b.item")
        .expect("b function use")
        + "RETURNS BOOLEAN AS ".len();
    let b_function_final = b_function_use + "b.".len();

    let hover = hover_at(text, b_function_final + 1).expect("qualified b.item function hover");
    let hover_value = hover_markdown(&hover);
    assert!(
        hover_value.contains("b.item"),
        "b.item function hover: {hover_value}"
    );
    assert!(
        !hover_value.contains("a.item"),
        "cross-schema function hover leak: {hover_value}"
    );

    let definition = super::definition(
        &document,
        &parse,
        mapper.position(b_function_final + 1),
        &mapper,
    )
    .expect("qualified b.item function definition");
    assert_eq!(
        definition.range.start,
        mapper.position(b_function_declaration),
    );

    let references = references(
        &document,
        &parse,
        mapper.position(b_function_final + 1),
        &mapper,
        true,
    );
    let reference_starts: Vec<_> = references
        .iter()
        .map(|reference| reference.range.start)
        .collect();
    assert_eq!(
        reference_starts,
        vec![
            mapper.position(b_function_declaration + "b.".len()),
            mapper.position(b_function_use + "b.".len()),
        ],
        "qualified b.item function references leaked across schemas: {references:?}",
    );
}

#[test]
fn qualified_quoted_type_navigation_preserves_identifier_semantics() {
    let text = include_str!(
        "fixtures/analysis-028-qualified-quoted-type-navigation-preserves-identifier-semantics-text.orna"
    );
    let document = Document::new(
        "file:///qualified-quoted-navigation.orna".parse().unwrap(),
        text.to_owned(),
        1,
    );
    let parse = orna_syntax::parse(text);
    let mapper = PositionMapper::new(text);
    let b_type_declaration = text
        .find("CREATE TYPE \"b\".\"item\"")
        .expect("quoted b type declaration")
        + "CREATE TYPE ".len();
    let b_type_use = text
        .find("RETURNS \"b\".\"item\"")
        .expect("quoted b type use")
        + "RETURNS ".len();
    let b_type_final = b_type_use + "\"b\".".len();

    let hover = hover_at(text, b_type_final + 1).expect("quoted b.item hover");
    let hover_value = hover_markdown(&hover);
    assert!(
        hover_value.contains("\"b\".\"item\""),
        "quoted b.item hover: {hover_value}"
    );
    assert!(
        !hover_value.contains("a_value"),
        "cross-schema quoted hover leak: {hover_value}"
    );

    let definition = super::definition(
        &document,
        &parse,
        mapper.position(b_type_final + 1),
        &mapper,
    )
    .expect("quoted b.item definition");
    assert_eq!(definition.range.start, mapper.position(b_type_declaration),);

    let references = references(
        &document,
        &parse,
        mapper.position(b_type_final + 1),
        &mapper,
        true,
    );
    let reference_starts: Vec<_> = references
        .iter()
        .map(|reference| reference.range.start)
        .collect();
    assert_eq!(
        reference_starts,
        vec![
            mapper.position(b_type_declaration + "\"b\".".len()),
            mapper.position(b_type_use + "\"b\".".len()),
        ],
        "quoted b.item references leaked across schemas: {references:?}",
    );
}

#[test]
fn qualified_quoted_sql_type_navigation_uses_full_path() {
    let text = include_str!(
        "fixtures/analysis-029-qualified-quoted-sql-type-navigation-uses-full-path-text.orna"
    );
    let document = Document::new(
        "file:///qualified-quoted-sql-navigation.orna"
            .parse()
            .unwrap(),
        text.to_owned(),
        1,
    );
    let parse = orna_syntax::parse(text);
    let mapper = PositionMapper::new(text);
    let b_type_declaration = text
        .find("CREATE TYPE \"b\".\"item\"")
        .expect("quoted b type declaration")
        + "CREATE TYPE ".len();
    let b_type_use = text.find("FROM \"b\".\"item\"").expect("quoted b type use") + "FROM ".len();
    let b_type_final = b_type_use + "\"b\".".len();

    let hover = hover_at(text, b_type_final + 1).expect("quoted SQL b.item hover");
    let hover_value = hover_markdown(&hover);
    assert!(
        hover_value.contains("\"b\".\"item\""),
        "quoted SQL b.item hover: {hover_value}"
    );
    assert!(
        !hover_value.contains("a_value"),
        "cross-schema quoted SQL hover leak: {hover_value}"
    );

    let definition = super::definition(
        &document,
        &parse,
        mapper.position(b_type_final + 1),
        &mapper,
    )
    .expect("quoted SQL b.item definition");
    assert_eq!(definition.range.start, mapper.position(b_type_declaration),);

    let references = references(
        &document,
        &parse,
        mapper.position(b_type_final + 1),
        &mapper,
        true,
    );
    let reference_starts: Vec<_> = references
        .iter()
        .map(|reference| reference.range.start)
        .collect();
    assert_eq!(
        reference_starts,
        vec![
            mapper.position(b_type_declaration + "\"b\".".len()),
            mapper.position(b_type_use + "\"b\".".len()),
        ],
        "quoted SQL b.item references leaked across schemas: {references:?}",
    );
}

#[test]
fn mixed_qualified_type_path_preserves_schema_prefix() {
    let text = include_str!(
        "fixtures/analysis-030-mixed-qualified-type-path-preserves-schema-prefix-text.orna"
    );
    let document = Document::new(
        "file:///mixed-qualified-navigation.orna".parse().unwrap(),
        text.to_owned(),
        1,
    );
    let parse = orna_syntax::parse(text);
    let mapper = PositionMapper::new(text);
    let schema_declaration =
        text.find("CREATE SCHEMA app").expect("schema declaration") + "CREATE SCHEMA ".len();
    let type_declaration = text
        .find("CREATE TYPE app.\"item\"")
        .expect("type declaration")
        + "CREATE TYPE ".len();
    let type_declaration_final = type_declaration + "app.".len();
    let type_use = text.find("FROM app.\"item\"").expect("qualified type use") + "FROM ".len();
    let type_use_final = type_use + "app.".len();

    let schema_hover = hover_at(text, type_use + 1).expect("mixed schema hover");
    let schema_hover_value = hover_markdown(&schema_hover);
    assert!(
        schema_hover_value.contains("schema"),
        "mixed qualified schema hover: {schema_hover_value}"
    );
    assert!(
        !schema_hover_value.contains("object type"),
        "mixed qualified schema resolved as type: {schema_hover_value}"
    );
    let schema_definition =
        super::definition(&document, &parse, mapper.position(type_use + 1), &mapper)
            .expect("mixed schema definition");
    assert_eq!(
        schema_definition.range.start,
        mapper.position(schema_declaration)
    );
    let schema_references = references(
        &document,
        &parse,
        mapper.position(type_use + 1),
        &mapper,
        true,
    );
    let schema_reference_starts: Vec<_> = schema_references
        .iter()
        .map(|reference| reference.range.start)
        .collect();
    assert_eq!(
        schema_reference_starts,
        vec![
            mapper.position(schema_declaration),
            mapper.position(type_declaration),
            mapper.position(type_use),
        ],
        "mixed qualified schema references lost prefix semantics: {schema_references:?}"
    );

    let type_hover = hover_at(text, type_use_final + 1).expect("mixed type hover");
    let type_hover_value = hover_markdown(&type_hover);
    assert!(
        type_hover_value.contains("object type"),
        "mixed qualified type hover: {type_hover_value}"
    );
    let type_definition = super::definition(
        &document,
        &parse,
        mapper.position(type_use_final + 1),
        &mapper,
    )
    .expect("mixed type definition");
    assert_eq!(
        type_definition.range.start,
        mapper.position(type_declaration)
    );
    let type_references = references(
        &document,
        &parse,
        mapper.position(type_use_final + 1),
        &mapper,
        true,
    );
    let type_reference_starts: Vec<_> = type_references
        .iter()
        .map(|reference| reference.range.start)
        .collect();
    assert_eq!(
        type_reference_starts,
        vec![
            mapper.position(type_declaration_final),
            mapper.position(type_use_final),
        ],
        "mixed qualified type references lost final-component semantics: {type_references:?}"
    );
}

#[test]
fn references_fold_unquoted_case_and_exclude_qualified_declaration_component() {
    let text = include_str!(
        "fixtures/analysis-031-references-fold-unquoted-case-and-exclude-qualified-declaration-component-text.orna"
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

    let qualified_text = include_str!(
        "fixtures/analysis-032-references-fold-unquoted-case-and-exclude-qualified-declaration-component-qualified-text.orna"
    );
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
    let field_text = include_str!(
        "fixtures/analysis-033-references-exclude-field-and-parameter-declarations-field-text.orna"
    );
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

    let parameter_text = include_str!(
        "fixtures/analysis-034-references-exclude-field-and-parameter-declarations-parameter-text.orna"
    );
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
    let text = include_str!(
        "fixtures/analysis-035-definitions-scope-rows-columns-before-unrelated-fields-text.orna"
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
    let text = include_str!(
        "fixtures/analysis-036-variable-definitions-and-references-stay-within-the-containing-function-text.orna"
    );
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
    let text = include_str!(
        "fixtures/analysis-037-client-state-definitions-stay-within-their-function-text.orna"
    );
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
    let text = include_str!(
        "fixtures/analysis-038-client-pre-begin-local-shadows-parameter-in-navigation-text.orna"
    );
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
    let text = include_str!(
        "fixtures/analysis-039-client-local-definitions-stay-within-their-function-text.orna"
    );
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
    let text = include_str!(
        "fixtures/analysis-040-references-fold-unicode-unquoted-identifier-case-text.orna"
    );
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
