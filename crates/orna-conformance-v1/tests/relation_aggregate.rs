use orna_conformance_v1::{SourceUnit, StageOutcome, TransactionalEvaluator};
use orna_evaluator_v1::Limits;
use orna_foundation_v1::Value;

fn source(body: &str) -> SourceUnit {
    SourceUnit {
        fixture_id: "relation-aggregate".into(),
        source_id: "relation-aggregate.orna".into(),
        parse_as: "module_unit".into(),
        source: format!(
            r#"
                pub table Reading(id: Int) {{ value: Int, }}
                fn minimum(): Int? = Reading | map(reading => reading.value) | min();
                fn maximum(): Int? = Reading | map(reading => reading.value) | max();
                fn total(): Int = Reading | map(reading => reading.value) | sum;
                fn parent() {{ {body} }}
            "#
        ),
    }
}

#[test]
fn relation_integer_aggregates_read_candidate_rows_in_canonical_order() {
    let mut evaluator = TransactionalEvaluator::new("parent", Limits::default());
    let outcome = evaluator.execute_source(&source(
        r#"
            assert (minimum() ?? 0) == 0;
            assert (maximum() ?? 0) == 0;
            assert total() == 0;
            Reading.insert({ id: 2, value: 9007199254740993 });
            Reading.insert({ id: 1, value: -3 });
            Reading.insert({ id: 3, value: -9007199254740992 });
            Reading.insert({ id: 4, value: 9 });
            assert (minimum() ?? 0) == -9007199254740992;
            assert (maximum() ?? 0) == 9007199254740993;
            assert total() == 7;
        "#,
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
fn relation_integer_aggregate_observes_writes_but_later_failure_rolls_back() {
    let mut evaluator = TransactionalEvaluator::new("parent", Limits::default());
    let outcome = evaluator.execute_source(&source(
        r#"
            Reading.insert({ id: 2, value: 20 });
            Reading.insert({ id: 1, value: 10 });
            assert total() == 30;
            assert (minimum() ?? 0) == 10;
            assert (maximum() ?? 0) == 20;
            assert false;
        "#,
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
fn relation_non_integer_projection_aggregate_fails_closed() {
    let unit = SourceUnit {
        fixture_id: "relation-float-aggregate".into(),
        source_id: "relation-float-aggregate.orna".into(),
        parse_as: "module_unit".into(),
        source: r#"
            pub table Reading(id: Int) { value: Float, }
            fn minimum() = Reading | map(reading => reading.value) | min();
            fn parent() {
                Reading.insert({ id: 1, value: 1.5f });
                minimum();
            }
        "#
        .into(),
    };
    let mut evaluator = TransactionalEvaluator::new("parent", Limits::default());

    let outcome = evaluator.execute_source(&unit);

    assert!(matches!(
        outcome,
        StageOutcome::Failed(ref diagnostic) if diagnostic.code() == "ORNA-EVAL-UNSUPPORTED"
    ));
    assert_eq!(
        evaluator.committed_row("Reading", &Value::int(1.into())),
        None
    );
}

#[test]
fn relation_integer_aggregate_requires_a_projection_shape() {
    let mut evaluator = TransactionalEvaluator::new("parent", Limits::default());
    let outcome = evaluator.execute_source(&source(
        r#"
            Reading.insert({ id: 1, value: 2 });
            Reading | map(reading => reading.value + 1) | sum;
        "#,
    ));

    assert!(matches!(outcome, StageOutcome::Failed(_)));
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
        source: r#"
            pub table Reading(id: Int) { value: Int, }
            fn parent() {
                Reading.insert({ id: 1, value: 1 });
                Reading.insert({ id: 2, value: 2 });
                Reading.insert({ id: 3, value: 3 });
                Reading | map(reading => reading.value) | sum;
            }
        "#
        .into(),
    };
    let mut limits = Limits::default();
    limits.max_collection_items = 2;
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
        source: r#"
            pub table Reading(id: Int) { value: Int, }
            fn parent() {
                Reading.insert({ id: 1, value: 99 });
                Reading.insert({ id: 2, value: 1 });
                Reading | map(reading => reading.value) | sum;
            }
        "#
        .into(),
    };
    let mut limits = Limits::default();
    limits.max_integer_digits = 2;
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
