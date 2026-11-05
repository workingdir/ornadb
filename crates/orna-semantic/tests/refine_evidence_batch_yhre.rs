use orna_semantic::{AssertionOwner, DIAG_UNRESOLVED, ModuleInput, Type, analyze};

#[test]
fn refined_type_declarations_have_owner_plans_and_static_identity() {
    let analysis = analyze(&[ModuleInput::new(
        "ports.orna",
        include_str!("fixtures/refine-evidence-yhre/ports.orna"),
    )]);

    assert!(analysis.is_ok(), "{:?}", analysis.diagnostics);
    let module = analysis.modules.values().next().expect("module header");
    assert_eq!(
        module
            .symbols
            .get("default_port")
            .expect("refined return")
            .ty,
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
    let plans = analysis.assertions.values().next().expect("refined plans");
    let owners: Vec<&AssertionOwner> = plans.iter().map(|plan| &plan.owner).collect();
    assert_eq!(
        owners,
        [
            &AssertionOwner::RefinedType("Port".into()),
            &AssertionOwner::RefinedType("Port".into()),
        ],
        "both declared predicates have analyzer owner plans"
    );
    assert!(
        plans
            .iter()
            .all(|plan| plan.dependencies.is_empty() && plan.effects == Default::default()),
        "neither predicate references a table or carries effects"
    );
    // A refined assertion runs when a base value is constructed or converted
    // into the refined type (ORNA-ASSERT-041), and the plans keep the
    // declaration order the predicates appear in.
    assert!(
        plans
            .iter()
            .all(|plan| plan.scope == orna_semantic::AssertionScope::ValueConstruction),
        "refined assertions are value-construction scoped"
    );
    let orders: Vec<usize> = plans.iter().map(|plan| plan.source_order).collect();
    assert!(
        orders.windows(2).all(|pair| pair[0] < pair[1]),
        "plans keep declaration order, got {orders:?}"
    );
    assert!(
        plans
            .iter()
            .all(|plan| plan.expression_span.end > plan.expression_span.start),
        "every plan carries the span of its expression"
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
    assert!(
        analysis
            .assertions
            .values()
            .flatten()
            .any(|plan| { plan.owner == AssertionOwner::RefinedType("Positive".into()) })
    );
}
