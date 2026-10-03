use orna_syntax::parse;

#[test]
fn attaches_contiguous_doc_comments_to_declarations_fields_and_parameters() {
    let source = include_str!("fixtures/documentation-comments.orna");
    let parsed = parse(source);
    assert!(
        parsed.diagnostics().is_empty(),
        "{:?}",
        parsed.diagnostics()
    );

    let object = &parsed.object_types()[0];
    assert_eq!(
        parsed.documentation_comment(&object.name.parts[1]),
        Some("Account data exchanged with the editor.\nThe description supports Markdown `code`.")
    );
    assert_eq!(
        parsed.documentation_comment(&object.fields[0].name),
        Some("Stable identity for this account.")
    );
    assert_eq!(
        parsed.documentation_comment(&object.fields[1].name),
        Some("Display name visible in the UI.")
    );

    let function = &parsed.client_functions()[0];
    assert_eq!(
        parsed.documentation_comment(&function.name.parts[1]),
        Some("Looks up an account by its stable identifier.")
    );
    assert_eq!(
        parsed.documentation_comment(&function.parameters[0].name),
        Some("The identity to resolve.")
    );
    assert_eq!(
        parsed.documentation_comment(&parsed.schemas()[0].name.parts[0]),
        None,
        "ordinary comments are not exposed as documentation"
    );
}
