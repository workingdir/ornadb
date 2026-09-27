use orna_semantic_v1::{ModuleInput, Namespace, Type, analyze};

#[test]
fn local_private_nominal_constructor_satisfies_generic_protocol_bounds() {
    let analysis = analyze(&[ModuleInput::new(
        "main.orna",
        include_str!("fixtures/local-nominal-generic-bounds.orna"),
    )]);

    assert!(analysis.is_ok(), "{:#?}", analysis.diagnostics);
    let main = analysis
        .modules
        .get(&Namespace(Vec::new()))
        .expect("main module");
    assert_eq!(
        main.symbols.get("accepted").expect("accepted function").ty,
        Type::Function {
            parameters: Vec::new(),
            parameter_names: Some(Vec::new()),
            result: Box::new(Type::Named("Ranked".into())),
            default_parameters: Default::default(),
        }
    );
}
