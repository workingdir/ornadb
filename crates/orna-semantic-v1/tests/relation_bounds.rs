use orna_semantic_v1::{
    Catalogue, DIAG_TYPE, DIAG_UNRESOLVED, ModuleInput, Type, analyze, analyze_with_catalogue,
};

fn has_message(result: &orna_semantic_v1::Analysis, expected: &str) -> bool {
    result
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.message() == expected)
}

macro_rules! fixture {
    ($name:literal) => {
        ModuleInput::new($name, include_str!(concat!("fixtures/", $name)))
    };
}

#[test]
fn direct_relation_bounds_reject_nonpositive_constants() {
    let result = analyze(&[fixture!("relation-direct-bounds.orna")]);
    let diagnostic_count = |expected: &str| {
        result
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.message() == expected)
            .count()
    };
    assert_eq!(
        diagnostic_count("relation take count must be nonnegative"),
        2,
        "take bounds: {:?}",
        result.diagnostics
    );
    assert_eq!(
        diagnostic_count("relation drop count must be nonnegative"),
        2,
        "drop bounds: {:?}",
        result.diagnostics
    );
    assert_eq!(
        diagnostic_count("window size must be positive"),
        3,
        "window sizes: {:?}",
        result.diagnostics
    );
    assert_eq!(
        diagnostic_count("window step must be positive"),
        2,
        "window steps: {:?}",
        result.diagnostics
    );
}

#[test]
fn declared_relation_helper_shadows_the_core_filter_name() {
    let result = analyze(&[fixture!("relation-bound.orna")]);
    assert!(result.is_ok(), "{:#?}", result.diagnostics);
}

#[test]
fn piped_relation_flat_map_rejects_wrong_argument_names_and_respects_shadowing() {
    let invalid = analyze(&[fixture!("relation-piped-flat-map-shape.orna")]);
    assert!(
        has_message(
            &invalid,
            "relation flat_map argument name does not match its static signature"
        ),
        "{:#?}",
        invalid.diagnostics
    );
    let module = invalid
        .modules
        .values()
        .next()
        .expect("invalid flat_map module");
    assert!(matches!(
        &module.symbols["invalid"].ty,
        Type::Function { result, .. } if result.as_ref() == &Type::Error
    ));

    let shadowed = analyze(&[fixture!("relation-piped-flat-map-shadowing.orna")]);
    assert!(shadowed.is_ok(), "{:#?}", shadowed.diagnostics);
}

#[test]
fn relational_callbacks_preserve_effects_and_failure_before_planning() {
    let result = analyze_with_catalogue(
        &[fixture!("relation-callback-effects.orna")],
        &Catalogue::authoritative_core(),
    );
    assert!(result.is_ok(), "{:#?}", result.diagnostics);
    let module = result
        .modules
        .values()
        .next()
        .expect("relation callback module");

    assert!(module.symbols["pure"].effects.effects.is_empty());
    assert!(!module.symbols["pure"].effects.may_fail);
    assert!(module.symbols["database"].effects.effects.contains("database read"));
    assert!(module.symbols["database"].effects.may_fail);
    assert!(module.symbols["direct_one"].effects.may_fail);
    assert!(module.symbols["predicate_one"].effects.may_fail);
    assert!(module.symbols["failed"].effects.may_fail);
    for name in ["direct_flat_map", "piped_flat_map"] {
        assert!(matches!(
            &module.symbols[name].ty,
            Type::Function { result, .. }
                if result.as_ref() == &Type::Relation(Box::new(Type::Int))
        ));
    }
    for name in ["direct_one", "predicate_one"] {
        assert!(matches!(
            &module.symbols[name].ty,
            Type::Function { result, .. } if result.as_ref() == &Type::Named("Note".into())
        ));
    }
}

#[test]
fn relational_callbacks_reject_mutations_and_external_effects() {
    let result = analyze_with_catalogue(
        &[fixture!("relation-callback-effect-rejection.orna")],
        &Catalogue::authoritative_core(),
    );
    let rejections = result
        .diagnostics
        .iter()
        .filter(|diagnostic| {
            diagnostic.code() == DIAG_TYPE
                && diagnostic.message() == "relation query callback must be read-only"
        })
        .count();
    assert_eq!(rejections, 2, "{:#?}", result.diagnostics);
    assert!(
        result.diagnostics.iter().all(|diagnostic| {
            diagnostic.message() != "relation query callback must be read-only"
                || diagnostic.code() == DIAG_TYPE
        }),
        "relation callback effect diagnostic: {:#?}",
        result.diagnostics
    );
}

#[test]
fn relation_flat_map_rejects_mutations_and_external_effects_in_both_call_forms() {
    let result = analyze_with_catalogue(
        &[fixture!("relation-flat-map-effect-rejection.orna")],
        &Catalogue::authoritative_core(),
    );
    let rejections = result
        .diagnostics
        .iter()
        .filter(|diagnostic| {
            diagnostic.code() == DIAG_TYPE
                && diagnostic.message() == "relation query callback must be read-only"
        })
        .count();
    assert_eq!(rejections, 2, "{:#?}", result.diagnostics);
}

#[test]
fn relation_every_exists_pipeline_callbacks_are_read_only_in_all_call_forms() {
    let result = analyze_with_catalogue(
        &[fixture!("relation-every-exists-effect-rejection.orna")],
        &Catalogue::authoritative_core(),
    );
    let read_only = result
        .diagnostics
        .iter()
        .filter(|diagnostic| {
            diagnostic.code() == DIAG_TYPE
                && diagnostic.message() == "relation query callback must be read-only"
        })
        .count();
    assert_eq!(read_only, 4, "{:#?}", result.diagnostics);
    assert!(
        !result.is_ok(),
        "effectful relation callbacks were accepted"
    );

    let module = result
        .modules
        .values()
        .next()
        .expect("relation every/exists module");
    for name in ["pure_every", "pure_exists"] {
        assert!(matches!(
            &module.symbols[name].ty,
            Type::Function { result, .. } if result.as_ref() == &Type::Bool
        ));
    }
}

#[test]
fn relational_callbacks_keep_existing_shape_diagnostics() {
    let result = analyze(&[fixture!("relation-callback-shape.orna")]);
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == DIAG_TYPE),
        "{:#?}",
        result.diagnostics
    );
}

#[test]
fn malformed_direct_relation_shape_preserves_later_argument_diagnostic() {
    let result = analyze(&[fixture!("relation-callback-order.orna")]);
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == DIAG_UNRESOLVED),
        "later argument unresolved diagnostic: {:#?}",
        result.diagnostics
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message() == "relation every requires rows and predicate"),
        "relation shape diagnostic: {:#?}",
        result.diagnostics
    );
}

#[test]
fn relation_callback_and_later_argument_diagnostics_are_both_preserved() {
    let result = analyze(&[fixture!("relation-callback-later-argument.orna")]);
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == DIAG_TYPE),
        "callback type diagnostic: {:#?}",
        result.diagnostics
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == DIAG_UNRESOLVED),
        "later argument unresolved diagnostic: {:#?}",
        result.diagnostics
    );
}

#[test]
fn malformed_direct_one_calls_report_the_static_signature_diagnostic() {
    let result = analyze(&[fixture!("relation-malformed-one.orna")]);
    assert!(
        has_message(
            &result,
            "relation one arguments do not match its static signature"
        ),
        "{:#?}",
        result.diagnostics
    );
    assert_eq!(
        result
            .diagnostics
            .iter()
            .filter(|diagnostic| {
                diagnostic.message() == "relation one arguments do not match its static signature"
            })
            .count(),
        3,
        "{:#?}",
        result.diagnostics
    );
}
