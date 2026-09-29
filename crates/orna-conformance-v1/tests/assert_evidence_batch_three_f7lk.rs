use orna_conformance_v1::{
    ConformanceAdapter, SemanticAdapter, SourceUnit, StageOutcome, TransactionalEvaluator,
};
use orna_evaluator_v1::Limits;
use orna_foundation_v1::Value;
use orna_semantic_v1::{AssertionOwner, ModuleInput, analyze};

const COHERENT_CANDIDATE: &str =
    include_str!("fixtures/assert-evidence-batch-three/coherent-candidate.orna");
const MODULE_SHAPES: &str = include_str!("fixtures/assert-evidence-batch-three/module-shapes.orna");
const MODULE_EFFECT: &str = include_str!("fixtures/assert-evidence-batch-three/module-effect.orna");

fn unit(name: &str, source: &str) -> SourceUnit {
    SourceUnit {
        fixture_id: format!("assert-evidence-batch-three-{name}"),
        source_id: format!("assert-evidence-batch-three/{name}.orna"),
        parse_as: "module_unit".into(),
        source: source.into(),
    }
}

fn mapped_semantic_diagnostic(source: &str) -> (String, String) {
    let mut adapter = SemanticAdapter::default();
    let source = unit("legacy-owner", source);
    assert!(matches!(adapter.parse(&source), StageOutcome::Passed));
    let resolved = adapter.resolve(&source);
    if let StageOutcome::Failed(diagnostic) = resolved {
        return (
            adapter.diagnostic_code(&diagnostic),
            diagnostic.message().to_owned(),
        );
    }
    match adapter.typecheck(&source) {
        StageOutcome::Failed(diagnostic) => (
            adapter.diagnostic_code(&diagnostic),
            diagnostic.message().to_owned(),
        ),
        outcome => panic!("expected the legacy assertion form to be rejected, got {outcome:?}"),
    }
}

#[test]
fn assert_003_table_assertion_plan_is_owned_by_its_lexical_table() {
    let source = include_str!("../../../../reference/Orna-1.0.0/examples/valid/table-assertions.orna");
    let analysis = analyze(&[ModuleInput::new("table-assertions.orna", source)]);
    assert!(analysis.diagnostics.is_empty(), "{:?}", analysis.diagnostics);
    let plans = analysis.assertions.values().flatten().collect::<Vec<_>>();
    assert_eq!(plans.len(), 2);
    assert!(plans.iter().all(|plan| plan.owner == AssertionOwner::Table("User".into())));
}

#[test]
fn assert_015_table_and_module_assertions_see_one_coherent_cross_table_candidate() {
    let mut runtime = TransactionalEvaluator::new("coherent_candidate", Limits::default());
    let outcome = runtime.execute_source(&unit("coherent-candidate", COHERENT_CANDIDATE));
    assert!(matches!(outcome, StageOutcome::Passed), "{outcome:?}");
    assert!(runtime.committed_row("Book", &Value::int(1.into())).is_some());
    assert!(runtime.committed_row("Loan", &Value::int(2.into())).is_some());
}

#[test]
fn assert_016_legacy_self_pipeline_receives_the_mapped_diagnostic_and_specific_guidance() {
    let source = include_str!("../../../../reference/Orna-1.0.0/examples/invalid/legacy-assert-self-pipe.orna");
    let (code, message) = mapped_semantic_diagnostic(source);
    assert_eq!(code, "ORNA-A091-002");
    assert!(message.contains("remove `self |`"), "{message}");
}

#[test]
fn assert_017_legacy_repeated_owner_pipeline_receives_the_mapped_diagnostic_and_guidance() {
    let source = include_str!("../../../../reference/Orna-1.0.0/examples/invalid/legacy-assert-owner-pipe.orna");
    let (code, message) = mapped_semantic_diagnostic(source);
    assert_eq!(code, "ORNA-A091-002");
    assert!(message.contains("remove the repeated table owner"), "{message}");
}

#[test]
fn assert_024_module_assertion_is_a_closed_boolean_without_an_implicit_table_owner() {
    let analysis = analyze(&[ModuleInput::new("module-shapes.orna", MODULE_SHAPES)]);
    assert!(analysis.diagnostics.is_empty(), "{:?}", analysis.diagnostics);
    let plans = analysis.assertions.values().flatten().collect::<Vec<_>>();
    assert_eq!(plans.len(), 2);
    assert!(plans.iter().all(|plan| plan.owner == AssertionOwner::Module));
    assert_eq!(plans[0].dependencies.len(), 2);
    assert_eq!(plans[1].dependencies.len(), 2);
}

#[test]
fn assert_025_module_assertion_rejects_a_forbidden_network_effect() {
    let analysis = analyze(&[ModuleInput::new("module-effect.orna", MODULE_EFFECT)]);
    assert!(
        analysis.diagnostics.iter().any(|diagnostic| {
            diagnostic.code() == orna_semantic_v1::DIAG_ASSERTION_EFFECT
        }),
        "expected forbidden network effect diagnostic, got {:?}",
        analysis.diagnostics
    );
}

#[test]
fn assert_028_preserves_source_span_order_within_one_module() {
    let analysis = analyze(&[ModuleInput::new("module-shapes.orna", MODULE_SHAPES)]);
    assert!(analysis.diagnostics.is_empty(), "{:?}", analysis.diagnostics);
    let plans = analysis.assertions.values().next().expect("module assertion plans");
    assert_eq!(plans.len(), 2);
    assert_eq!(plans[0].dependencies, ["Alpha".to_owned(), "Beta".to_owned()].into());
    assert_eq!(plans[1].dependencies, ["Alpha".to_owned(), "Gamma".to_owned()].into());
}

#[test]
fn assert_030_cross_table_assertion_has_module_not_table_ownership() {
    let source = include_str!("../../../../reference/Orna-1.0.0/examples/valid/cross-table-assertion.orna");
    let analysis = analyze(&[ModuleInput::new("cross-table-assertion.orna", source)]);
    assert!(analysis.diagnostics.is_empty(), "{:?}", analysis.diagnostics);
    let plan = analysis.assertions.values().flatten().next().expect("module assertion plan");
    assert_eq!(plan.owner, AssertionOwner::Module);
    assert_eq!(plan.dependencies.len(), 2);
}

#[test]
fn assert_055_recognized_legacy_owner_form_has_specific_migration_guidance() {
    let source = include_str!("../../../../reference/Orna-1.0.0/examples/invalid/legacy-assert-self-pipe.orna");
    let (_, message) = mapped_semantic_diagnostic(source);
    assert!(message.contains("remove `self |`"), "{message}");
}

#[test]
fn assert_056_ordinary_relation_pipeline_remains_valid_outside_declaration_assertions() {
    let source = include_str!("fixtures/assert-evidence-batch-three/ordinary-pipeline.orna");
    let analysis = analyze(&[ModuleInput::new("ordinary-pipeline.orna", source)]);
    assert!(analysis.diagnostics.is_empty(), "{:?}", analysis.diagnostics);
}
