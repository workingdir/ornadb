use orna_semantic_v1::{
    analyze, AssertionOwner, DIAG_UNRESOLVED, ModuleInput, Type,
};

#[test]
fn refined_type_declarations_have_owner_plans_and_static_identity() {
    let analysis = analyze(&[ModuleInput::new(
        "ports.orna",
        include_str!("fixtures/refine-evidence-yhre/ports.orna"),
    )]);

    assert!(analysis.is_ok(), "{:?}", analysis.diagnostics);
    let module = analysis.modules.values().next().expect("module header");
    assert_eq!(
        module.symbols.get("default_port").expect("refined return").ty,
        Type::Function {
            parameters: vec![],
            parameter_names: Some(vec![]),
            result: Box::new(Type::Named("Port".into())),
            default_parameters: Default::default(),
        }
    );
    assert_eq!(
        module.symbols.get("raw_port").expect("base return").ty,
        Type::Function {
            parameters: vec![],
            parameter_names: Some(vec![]),
            result: Box::new(Type::Int),
            default_parameters: Default::default(),
        }
    );
    assert_ne!(
        module.symbols.get("default_port").unwrap().ty,
        module.symbols.get("raw_port").unwrap().ty,
        "the refined nominal name remains distinct in semantic signatures"
    );
    assert_eq!(
        analysis.assertions.values().next().expect("refined plans"),
        &vec![
            orna_semantic_v1::AssertionPlan {
                owner: AssertionOwner::RefinedType("Port".into()),
                dependencies: Default::default(),
                effects: Default::default(),
            },
            orna_semantic_v1::AssertionPlan {
                owner: AssertionOwner::RefinedType("Port".into()),
                dependencies: Default::default(),
                effects: Default::default(),
            },
        ],
        "both declared predicates have analyzer owner plans"
    );
}

#[test]
fn implicit_refined_subject_does_not_become_an_ordinary_module_name() {
    let analysis = analyze(&[ModuleInput::new(
        "owner_scope.orna",
        include_str!("fixtures/refine-evidence-yhre/owner_scope.orna"),
    )]);

    assert_eq!(
        analysis
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.code())
            .collect::<Vec<_>>(),
        vec![DIAG_UNRESOLVED]
    );
}
