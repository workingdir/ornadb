use orna_conformance_v1::{SourceUnit, StageOutcome, TransactionalEvaluator};
use orna_evaluator_v1::Limits;

fn relation_source(source: &str) -> SourceUnit {
    SourceUnit {
        fixture_id: "relation-plan".into(),
        source_id: "relation-plan.orna".into(),
        parse_as: "module_unit".into(),
        source: source.into(),
    }
}

fn assert_relation_one_error(fixture: &str, expected: &str) {
    let mut runtime = TransactionalEvaluator::new("parent", Limits::default());
    let unit = relation_source(fixture);

    match runtime.execute_source(&unit) {
        StageOutcome::Failed(diagnostic) => assert_eq!(diagnostic.code(), expected),
        outcome => panic!("expected relation one failure, got {outcome:?}"),
    }
}

#[test]
fn take_before_sort_bounds_source_and_post_sort_callbacks() {
    let mut runtime = TransactionalEvaluator::new("parent", Limits::default());
    let unit = relation_source(include_str!("fixtures/relation-plan-take-before-sort.orna"));

    let outcome = runtime.execute_source(&unit);
    assert!(matches!(outcome, StageOutcome::Passed), "{outcome:?}");
}

#[test]
fn bound_relation_variables_support_direct_terminals() {
    let mut runtime = TransactionalEvaluator::new("parent", Limits::default());
    let unit = relation_source(include_str!("fixtures/relation-plan-bound-variables.orna"));

    let outcome = runtime.execute_source(&unit);
    assert!(matches!(outcome, StageOutcome::Passed), "{outcome:?}");
}

#[test]
fn relation_sort_by_orders_signed_scalar_keys_and_preserves_canonical_ties() {
    let mut runtime = TransactionalEvaluator::new("parent", Limits::default());
    let unit = relation_source(include_str!("fixtures/relation-plan-signed-sort.orna"));

    let outcome = runtime.execute_source(&unit);
    assert!(matches!(outcome, StageOutcome::Passed), "{outcome:?}");
}

#[test]
fn relation_union_preserves_left_then_right_bounded_order() {
    let mut runtime = TransactionalEvaluator::new("parent", Limits::default());
    let unit = relation_source(include_str!("fixtures/relation-plan-union-order.orna"));

    let outcome = runtime.execute_source(&unit);
    assert!(matches!(outcome, StageOutcome::Passed), "{outcome:?}");
}

#[test]
fn lexical_filter_shadow_is_not_hijacked_by_relation_intrinsic() {
    let mut runtime = TransactionalEvaluator::new("parent", Limits::default());
    let unit = relation_source(include_str!("fixtures/relation-plan-shadow.orna"));

    assert!(matches!(
        runtime.execute_source(&unit),
        StageOutcome::Failed(_)
    ));
    assert_eq!(
        runtime.committed_row("Note", &orna_foundation_v1::Value::int(1.into())),
        None
    );
}

#[test]
fn declared_filter_executes_instead_of_the_relation_intrinsic() {
    let mut runtime = TransactionalEvaluator::new("parent", Limits::default());
    let unit = relation_source(include_str!("fixtures/relation-plan-declared-filter.orna"));
    let outcome = runtime.execute_source(&unit);
    assert!(matches!(outcome, StageOutcome::Passed), "{outcome:?}");
}

#[test]
fn declared_relation_helpers_shadow_all_specialized_lowering_paths() {
    let mut runtime = TransactionalEvaluator::new("parent", Limits::default());
    let unit = relation_source(include_str!("fixtures/relation-plan-declared-helpers.orna"));
    let outcome = runtime.execute_source(&unit);
    assert!(matches!(outcome, StageOutcome::Passed), "{outcome:?}");
}

#[test]
fn relation_one_preserves_exact_cardinality_errors_before_and_after_sort() {
    const ZERO: &str = "ORNA-EVAL-RELATION-ONE-ZERO";
    const MULTIPLE: &str = "ORNA-EVAL-RELATION-ONE-MULTIPLE";

    for (fixture, expected) in [
        (include_str!("fixtures/relation-one-zero.orna"), ZERO),
        (
            include_str!("fixtures/relation-one-multiple.orna"),
            MULTIPLE,
        ),
        (
            include_str!("fixtures/relation-one-filtered-zero.orna"),
            ZERO,
        ),
        (
            include_str!("fixtures/relation-one-filtered-multiple.orna"),
            MULTIPLE,
        ),
        (include_str!("fixtures/relation-one-sorted-zero.orna"), ZERO),
        (
            include_str!("fixtures/relation-one-sorted-multiple.orna"),
            MULTIPLE,
        ),
        (
            include_str!("fixtures/relation-one-sorted-filtered-zero.orna"),
            ZERO,
        ),
        (
            include_str!("fixtures/relation-one-sorted-filtered-multiple.orna"),
            MULTIPLE,
        ),
    ] {
        assert_relation_one_error(fixture, expected);
    }
}
