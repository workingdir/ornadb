use orna_semantic_v1::{DIAG_TYPE, ModuleInput, Namespace, Type, analyze};

#[test]
fn relation_values_expose_count_and_first_members() {
    let result = analyze(&[ModuleInput::new(
        "relation-members.orna",
        include_str!("fixtures/relation-members.orna"),
    )]);

    assert!(result.is_ok(), "{:#?}", result.diagnostics);
    let module = result
        .modules
        .get(&Namespace(vec!["relation-members".into()]))
        .expect("relation-members module");

    assert_eq!(
        module.symbols["note_count"].ty,
        Type::Function {
            parameters: vec![Type::Relation(Box::new(Type::Named("Note".into())))],
            parameter_names: Some(vec!["rows".into()]),
            result: Box::new(Type::Int),
            default_parameters: Default::default(),
        }
    );
    assert_eq!(
        module.symbols["first_note"].ty,
        Type::Function {
            parameters: vec![Type::Relation(Box::new(Type::Named("Note".into())))],
            parameter_names: Some(vec!["rows".into()]),
            result: Box::new(Type::Optional(Box::new(Type::Named("Note".into())))),
            default_parameters: Default::default(),
        }
    );
    assert_eq!(
        module.symbols["table_count"].ty,
        Type::Function {
            parameters: Vec::new(),
            parameter_names: Some(Vec::new()),
            result: Box::new(Type::Int),
            default_parameters: Default::default(),
        }
    );
    assert_eq!(
        module.symbols["table_first"].ty,
        Type::Function {
            parameters: Vec::new(),
            parameter_names: Some(Vec::new()),
            result: Box::new(Type::Optional(Box::new(Type::Named("Note".into())))),
            default_parameters: Default::default(),
        }
    );
}

#[test]
fn non_relation_field_access_keeps_the_record_error() {
    let result = analyze(&[ModuleInput::new(
        "non-relation-field.orna",
        include_str!("fixtures/non-relation-field.orna"),
    )]);

    assert_eq!(result.diagnostics.len(), 1, "{:#?}", result.diagnostics);
    assert_eq!(result.diagnostics[0].code(), DIAG_TYPE);
    assert_eq!(
        result.diagnostics[0].message(),
        "field access requires a record"
    );
}
