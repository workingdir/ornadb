use std::collections::BTreeMap;

use orna_conformance_v1::{
    BoundedEvaluator, RuntimeEvaluator, SourceUnit, StageOutcome, TransactionalEvaluator,
};
use orna_evaluator_v1::Limits;
use orna_foundation_v1::{OvbRaw, Value};

fn source(fixture_id: &str, source_id: &str, source: &str) -> SourceUnit {
    SourceUnit {
        fixture_id: fixture_id.into(),
        source_id: source_id.into(),
        parse_as: "module_unit".into(),
        source: source.into(),
    }
}

#[test]
fn relation_integer_aggregates_read_candidate_rows_in_canonical_order() {
    let mut evaluator = TransactionalEvaluator::new("parent", Limits::default());
    let outcome = evaluator.execute_source(&source(
        "relation-aggregate",
        "relation-aggregate-integer-canonical.orna",
        include_str!("fixtures/relation-aggregate-integer-canonical.orna"),
    ));

    assert!(matches!(outcome, StageOutcome::Passed), "{outcome:?}");
    for id in [1, 2, 3, 4] {
        assert!(
            evaluator
                .committed_row("Reading", &Value::int(id.into()))
                .is_some(),
            "row {id} was not published"
        );
    }
}

#[test]
fn relation_float_sum_uses_empty_identity_and_canonical_row_order() {
    let unit = SourceUnit {
        fixture_id: "relation-float-sum".into(),
        source_id: "relation-float-sum.orna".into(),
        parse_as: "module_unit".into(),
        source: include_str!("fixtures/relation-aggregate-float-sum.orna").into(),
    };
    let mut evaluator = TransactionalEvaluator::new("parent", Limits::default());

    let outcome = evaluator.execute_source(&unit);

    assert!(matches!(outcome, StageOutcome::Passed), "{outcome:?}");
    for id in [1, 2, 3] {
        assert!(
            evaluator
                .committed_row("Reading", &Value::int(id.into()))
                .is_some(),
            "row {id} was not published"
        );
    }
}

#[test]
fn relation_integer_aggregate_observes_writes_but_later_failure_rolls_back() {
    let mut evaluator = TransactionalEvaluator::new("parent", Limits::default());
    let outcome = evaluator.execute_source(&source(
        "relation-aggregate",
        "relation-aggregate-integer-rollback.orna",
        include_str!("fixtures/relation-aggregate-integer-rollback.orna"),
    ));

    assert!(matches!(
        outcome,
        StageOutcome::Failed(ref diagnostic) if diagnostic.code() == "ORNA-EVAL-ASSERT"
    ));
    for id in [1, 2] {
        assert_eq!(
            evaluator.committed_row("Reading", &Value::int(id.into())),
            None,
            "row {id} escaped the failed activation"
        );
    }
}

#[test]
fn relation_float_min_and_max_fail_closed() {
    for operation in ["min", "max"] {
        let fixture = format!("relation-aggregate-float-{operation}.orna");
        let unit = source(
            &format!("relation-float-{operation}"),
            &fixture,
            match operation {
                "min" => include_str!("fixtures/relation-aggregate-float-min.orna"),
                "max" => include_str!("fixtures/relation-aggregate-float-max.orna"),
                _ => unreachable!(),
            },
        );
        let mut evaluator = TransactionalEvaluator::new("parent", Limits::default());

        let outcome = evaluator.execute_source(&unit);

        assert!(matches!(
            outcome,
            StageOutcome::Failed(ref diagnostic) if diagnostic.code() == "ORNA-EVAL-UNSUPPORTED"
        ));
        assert_eq!(
            evaluator.committed_row("Reading", &Value::int(1.into())),
            None,
            "{operation} published a row despite being unsupported"
        );
    }
}

#[test]
fn relation_integer_aggregate_requires_a_projection_shape() {
    let mut evaluator = TransactionalEvaluator::new("parent", Limits::default());
    let outcome = evaluator.execute_source(&source(
        "relation-aggregate",
        "relation-aggregate-integer-shape.orna",
        include_str!("fixtures/relation-aggregate-integer-shape.orna"),
    ));

    assert!(matches!(outcome, StageOutcome::Failed(_)), "{outcome:?}");
    assert_eq!(
        evaluator.committed_row("Reading", &Value::int(1.into())),
        None
    );
}

#[test]
fn relation_integer_aggregate_rejects_too_many_candidate_rows_without_publication() {
    let unit = SourceUnit {
        fixture_id: "relation-aggregate-row-limit".into(),
        source_id: "relation-aggregate-row-limit.orna".into(),
        parse_as: "module_unit".into(),
        source: include_str!("fixtures/relation-aggregate-integer-row-limit.orna").into(),
    };
    let limits = Limits {
        max_collection_items: 2,
        ..Default::default()
    };
    let mut evaluator = TransactionalEvaluator::new("parent", limits);

    let outcome = evaluator.execute_source(&unit);

    assert!(matches!(
        outcome,
        StageOutcome::Failed(ref diagnostic) if diagnostic.code() == "ORNA-EVAL-LIMIT"
    ));
    for id in [1, 2, 3] {
        assert_eq!(
            evaluator.committed_row("Reading", &Value::int(id.into())),
            None,
            "row {id} escaped the failed activation"
        );
    }
}

#[test]
fn relation_integer_sum_rejects_an_intermediate_integer_that_exceeds_the_limit() {
    let unit = SourceUnit {
        fixture_id: "relation-aggregate-integer-limit".into(),
        source_id: "relation-aggregate-integer-limit.orna".into(),
        parse_as: "module_unit".into(),
        source: include_str!("fixtures/relation-aggregate-integer-digit-limit.orna").into(),
    };
    let limits = Limits {
        max_integer_digits: 2,
        ..Default::default()
    };
    let mut evaluator = TransactionalEvaluator::new("parent", limits);

    let outcome = evaluator.execute_source(&unit);

    assert!(matches!(
        outcome,
        StageOutcome::Failed(ref diagnostic) if diagnostic.code() == "ORNA-EVAL-LIMIT"
    ));
    for id in [1, 2] {
        assert_eq!(
            evaluator.committed_row("Reading", &Value::int(id.into())),
            None,
            "row {id} escaped the failed activation"
        );
    }
}

#[test]
fn relation_scan_shares_evaluator_budget_and_rolls_back_on_exhaustion() {
    let limits = Limits {
        max_steps: 14,
        ..Default::default()
    };
    let mut evaluator = TransactionalEvaluator::new("parent", limits);
    let outcome = evaluator.execute_source(&source(
        "relation-aggregate",
        "relation-aggregate-integer-scan-budget.orna",
        include_str!("fixtures/relation-aggregate-integer-scan-budget.orna"),
    ));

    assert!(matches!(
        outcome,
        StageOutcome::Failed(ref diagnostic) if diagnostic.code() == "ORNA-EVAL-LIMIT"
    ));
    for id in [1, 2] {
        assert_eq!(
            evaluator.committed_row("Reading", &Value::int(id.into())),
            None,
            "row {id} escaped the exhausted activation"
        );
    }
}

#[test]
fn zero_step_budget_fails_before_any_publication() {
    let limits = Limits {
        max_steps: 0,
        ..Default::default()
    };
    let mut evaluator = TransactionalEvaluator::new("parent", limits);
    let outcome = evaluator.execute_source(&source(
        "relation-aggregate",
        "relation-aggregate-integer-zero-budget.orna",
        include_str!("fixtures/relation-aggregate-integer-zero-budget.orna"),
    ));

    assert!(matches!(
        outcome,
        StageOutcome::Failed(ref diagnostic) if diagnostic.code() == "ORNA-EVAL-LIMIT"
    ));
    assert_eq!(
        evaluator.committed_row("Reading", &Value::int(1.into())),
        None
    );
}

#[test]
fn assertion_scan_consumes_shared_budget_and_preserves_rollback() {
    let unit = SourceUnit {
        fixture_id: "relation-assertion-step-limit".into(),
        source_id: "relation-assertion-step-limit.orna".into(),
        parse_as: "module_unit".into(),
        source: include_str!("fixtures/relation-aggregate-assertion-step-limit.orna").into(),
    };
    let limits = Limits {
        max_steps: 10,
        ..Default::default()
    };
    let mut evaluator = TransactionalEvaluator::new("parent", limits);

    let outcome = evaluator.execute_source(&unit);

    assert!(matches!(
        outcome,
        StageOutcome::Failed(ref diagnostic) if diagnostic.code() == "ORNA-EVAL-LIMIT"
    ));
    for id in [1, 2] {
        assert_eq!(
            evaluator.committed_row("Reading", &Value::int(id.into())),
            None,
            "row {id} escaped assertion-limit rollback"
        );
    }
}

#[test]
fn table_assertion_predicate_vm_work_shares_one_activation_budget() {
    let limits = Limits {
        max_steps: 20,
        ..Default::default()
    };
    let mut evaluator = TransactionalEvaluator::new("parent", limits);
    assert!(matches!(
        evaluator.execute_source(&source(
            "relation-assertion-budget",
            "relation-aggregate-assertion-seed.orna",
            include_str!("fixtures/relation-aggregate-assertion-seed.orna")
        )),
        StageOutcome::Passed
    ));
    let outcome = evaluator.execute_source(&source(
        "relation-assertion-budget",
        "relation-aggregate-assertion-predicate-budget.orna",
        include_str!("fixtures/relation-aggregate-assertion-predicate-budget.orna"),
    ));

    assert!(matches!(
        outcome,
        StageOutcome::Failed(ref diagnostic) if diagnostic.code() == "ORNA-EVAL-LIMIT"
    ));
    for id in [1, 2] {
        assert!(
            evaluator
                .committed_row("Reading", &Value::int(id.into()))
                .is_some(),
            "seed row {id} disappeared after predicate-budget exhaustion"
        );
    }
}

#[test]
fn nested_module_quantifier_cannot_reset_the_activation_budget() {
    let seed = SourceUnit {
        fixture_id: "nested-assertion-budget".into(),
        source_id: "nested-assertion-budget.orna".into(),
        parse_as: "module_unit".into(),
        source: include_str!("fixtures/relation-aggregate-nested-seed.orna").into(),
    };
    let assertion = SourceUnit {
        fixture_id: "nested-assertion-budget".into(),
        source_id: "nested-assertion-budget.orna".into(),
        parse_as: "module_unit".into(),
        source: include_str!("fixtures/relation-aggregate-nested-assertion.orna").into(),
    };
    let limits = Limits {
        max_steps: 20,
        ..Default::default()
    };
    let mut evaluator = TransactionalEvaluator::new("parent", limits);
    assert!(matches!(
        evaluator.execute_source(&seed),
        StageOutcome::Passed
    ));

    let outcome = evaluator.execute_source(&assertion);

    assert!(matches!(
        outcome,
        StageOutcome::Failed(ref diagnostic) if diagnostic.code() == "ORNA-EVAL-LIMIT"
    ));
    for (table, ids) in [("Book", [1, 2]), ("Loan", [10, 11])] {
        for id in ids {
            assert!(
                evaluator
                    .committed_row(table, &Value::int(id.into()))
                    .is_some(),
                "seed {table} row {id} disappeared after nested-budget exhaustion"
            );
        }
    }
}

#[test]
fn zero_and_exhausted_assertion_budgets_publish_nothing() {
    let unit = source(
        "relation-assertion-budget",
        "relation-aggregate-assertion-publish-budgets.orna",
        include_str!("fixtures/relation-aggregate-assertion-publish-budgets.orna"),
    );

    let zero_limits = Limits {
        max_steps: 0,
        ..Default::default()
    };
    let mut zero = TransactionalEvaluator::new("parent", zero_limits);
    let zero_outcome = zero.execute_source(&unit);
    assert!(matches!(
        zero_outcome,
        StageOutcome::Failed(ref diagnostic) if diagnostic.code() == "ORNA-EVAL-LIMIT"
    ));
    for id in [1, 2] {
        assert_eq!(zero.committed_row("Reading", &Value::int(id.into())), None);
    }

    let exhausted_limits = Limits {
        max_steps: 10,
        ..Default::default()
    };
    let mut exhausted = TransactionalEvaluator::new("parent", exhausted_limits);
    let exhausted_outcome = exhausted.execute_source(&unit);
    assert!(matches!(
        exhausted_outcome,
        StageOutcome::Failed(ref diagnostic) if diagnostic.code() == "ORNA-EVAL-LIMIT"
    ));
    for id in [1, 2] {
        assert_eq!(
            exhausted.committed_row("Reading", &Value::int(id.into())),
            None
        );
    }
}

#[test]
fn generous_assertion_budget_preserves_successful_publication() {
    let limits = Limits {
        max_steps: 1_000,
        ..Default::default()
    };
    let mut evaluator = TransactionalEvaluator::new("parent", limits);
    let outcome = evaluator.execute_source(&source(
        "relation-assertion-budget",
        "relation-aggregate-assertion-generous-budget.orna",
        include_str!("fixtures/relation-aggregate-assertion-generous-budget.orna"),
    ));

    assert!(matches!(outcome, StageOutcome::Passed), "{outcome:?}");
    for id in [1, 2] {
        assert!(
            evaluator
                .committed_row("Reading", &Value::int(id.into()))
                .is_some(),
            "row {id} was not published under generous limits"
        );
    }
}

#[test]
fn relation_decimal_sum_normalizes_scales_through_source_execution() {
    let mut evaluator = TransactionalEvaluator::new("parent", Limits::default());
    let outcome = evaluator.execute_source(&source(
        "relation-decimal-aggregate",
        "relation-aggregate-decimal-normalize.orna",
        include_str!("fixtures/relation-aggregate-decimal-normalize.orna"),
    ));

    assert!(matches!(outcome, StageOutcome::Passed), "{outcome:?}");
    for id in [1, 2] {
        assert!(
            evaluator
                .committed_row("Reading", &Value::int(id.into()))
                .is_some(),
            "Decimal row {id} was not published"
        );
    }
}

#[test]
fn relation_decimal_sum_returns_decimal_additive_zero_for_empty_input() {
    let mut evaluator = TransactionalEvaluator::new("parent", Limits::default());
    let outcome = evaluator.execute_source(&source(
        "relation-decimal-aggregate",
        "relation-aggregate-decimal-empty.orna",
        include_str!("fixtures/relation-aggregate-decimal-empty.orna"),
    ));

    assert!(matches!(outcome, StageOutcome::Passed), "{outcome:?}");
}

#[test]
fn relation_decimal_sum_observes_candidate_read_your_writes() {
    let mut evaluator = TransactionalEvaluator::new("parent", Limits::default());
    let outcome = evaluator.execute_source(&source(
        "relation-decimal-aggregate",
        "relation-aggregate-decimal-read-your-writes.orna",
        include_str!("fixtures/relation-aggregate-decimal-read-your-writes.orna"),
    ));

    assert!(matches!(outcome, StageOutcome::Passed), "{outcome:?}");
    for id in [1, 2] {
        assert!(
            evaluator
                .committed_row("Reading", &Value::int(id.into()))
                .is_some(),
            "candidate Decimal row {id} was not published"
        );
    }
}

#[test]
fn relation_decimal_sum_rolls_back_candidate_rows_after_assertion_failure() {
    let mut evaluator = TransactionalEvaluator::new("parent", Limits::default());
    let outcome = evaluator.execute_source(&source(
        "relation-decimal-aggregate",
        "relation-aggregate-decimal-rollback.orna",
        include_str!("fixtures/relation-aggregate-decimal-rollback.orna"),
    ));

    assert!(matches!(
        &outcome,
        StageOutcome::Failed(diagnostic) if diagnostic.code() == "ORNA-EVAL-ASSERT"
    ));
    for id in [1, 2] {
        assert_eq!(
            evaluator.committed_row("Reading", &Value::int(id.into())),
            None,
            "Decimal row {id} escaped assertion rollback"
        );
    }
}

#[test]
fn relation_decimal_sum_rejects_too_many_candidate_rows_at_the_limit() {
    let limits = Limits {
        max_collection_items: 2,
        ..Default::default()
    };
    let mut evaluator = TransactionalEvaluator::new("parent", limits);
    let outcome = evaluator.execute_source(&source(
        "relation-decimal-aggregate",
        "relation-aggregate-decimal-row-limit.orna",
        include_str!("fixtures/relation-aggregate-decimal-row-limit.orna"),
    ));

    assert!(matches!(
        &outcome,
        StageOutcome::Failed(diagnostic) if diagnostic.code() == "ORNA-EVAL-LIMIT"
    ));
    for id in [1, 2, 3] {
        assert_eq!(
            evaluator.committed_row("Reading", &Value::int(id.into())),
            None,
            "Decimal row {id} escaped aggregate-limit rollback"
        );
    }
}

#[test]
fn relation_decimal_min_max_use_exact_order_and_preserve_first_equal_candidate() {
    let mut evaluator = TransactionalEvaluator::new("parent", Limits::default());
    let outcome = evaluator.execute_source(&source(
        "relation-decimal-extrema",
        "relation-aggregate-decimal-extrema-order.orna",
        include_str!("fixtures/relation-aggregate-decimal-extrema-order.orna"),
    ));

    assert!(matches!(outcome, StageOutcome::Passed), "{outcome:?}");
    for id in [1, 2, 3, 4] {
        assert!(
            evaluator
                .committed_row("Reading", &Value::int(id.into()))
                .is_some(),
            "Decimal extrema row {id} was not published"
        );
    }
}

#[test]
fn relation_decimal_min_max_roll_back_candidate_rows_after_assertion_failure() {
    let mut evaluator = TransactionalEvaluator::new("parent", Limits::default());
    let outcome = evaluator.execute_source(&source(
        "relation-decimal-extrema",
        "relation-aggregate-decimal-extrema-rollback.orna",
        include_str!("fixtures/relation-aggregate-decimal-extrema-rollback.orna"),
    ));

    assert!(matches!(
        &outcome,
        StageOutcome::Failed(diagnostic) if diagnostic.code() == "ORNA-EVAL-ASSERT"
    ));
    for id in [1, 2] {
        assert_eq!(
            evaluator.committed_row("Reading", &Value::int(id.into())),
            None,
            "Decimal extrema row {id} escaped assertion rollback"
        );
    }
}

#[test]
fn relation_decimal_min_max_reject_too_many_candidate_rows_without_publication() {
    let limits = Limits {
        max_collection_items: 2,
        ..Default::default()
    };
    let mut evaluator = TransactionalEvaluator::new("parent", limits);
    let outcome = evaluator.execute_source(&source(
        "relation-decimal-extrema",
        "relation-aggregate-decimal-extrema-limit.orna",
        include_str!("fixtures/relation-aggregate-decimal-extrema-limit.orna"),
    ));

    assert!(matches!(
        &outcome,
        StageOutcome::Failed(diagnostic) if diagnostic.code() == "ORNA-EVAL-LIMIT"
    ));
    for id in [1, 2, 3] {
        assert_eq!(
            evaluator.committed_row("Reading", &Value::int(id.into())),
            None,
            "Decimal extrema row {id} escaped aggregate-limit rollback"
        );
    }
}

#[test]
fn finite_list_decimal_min_max_execute_from_source_with_exact_and_empty_results() {
    let mut evaluator = BoundedEvaluator::new(Limits::default());
    let unit = source(
        "finite-decimal-extrema",
        "relation-aggregate-finite-decimal-extrema.orna",
        include_str!("fixtures/relation-aggregate-finite-decimal-extrema.orna"),
    );
    assert!(matches!(evaluator.evaluate(&unit), StageOutcome::Passed));

    let expected_min = Value::option(Some(
        Value::decimal(12.into(), (-1).into()).expect("canonical Decimal minimum"),
    ))
    .expect("canonical minimum option");
    let expected_max = Value::option(Some(
        Value::decimal(2003.into(), (-3).into()).expect("canonical Decimal maximum"),
    ))
    .expect("canonical maximum option");
    let empty = Value::new(OvbRaw::Null).expect("canonical empty aggregate result");

    for (function, expected) in [
        ("minimum", expected_min),
        ("maximum", expected_max),
        ("empty_minimum", empty.clone()),
        ("empty_maximum", empty),
    ] {
        let actual = evaluator
            .invoke_value_with(function, &BTreeMap::new())
            .unwrap_or_else(|diagnostic| panic!("{function} failed: {diagnostic:?}"));
        assert_eq!(
            actual, expected,
            "{function} returned an unexpected Decimal extrema"
        );
    }
}
