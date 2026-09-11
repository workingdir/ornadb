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

fn table_assertion_source(assertion: &str, body: &str) -> SourceUnit {
    SourceUnit {
        fixture_id: "relation-assertion-budget".into(),
        source_id: "relation-assertion-budget.orna".into(),
        parse_as: "module_unit".into(),
        source: format!(
            r#"
                pub table Reading(id: Int) {{ value: Int, assert {assertion}; }}
                fn parent() {{ {body} }}
            "#
        ),
    }
}

fn table_source(body: &str) -> SourceUnit {
    SourceUnit {
        fixture_id: "relation-assertion-budget".into(),
        source_id: "relation-assertion-budget.orna".into(),
        parse_as: "module_unit".into(),
        source: format!(
            r#"
                pub table Reading(id: Int) {{ value: Int, }}
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
fn relation_float_sum_uses_empty_identity_and_canonical_row_order() {
    let unit = SourceUnit {
        fixture_id: "relation-float-sum".into(),
        source_id: "relation-float-sum.orna".into(),
        parse_as: "module_unit".into(),
        source: r#"
            pub table Reading(id: Int) { value: Float, }
            fn total(): Float = Reading | map(reading => reading.value) | sum;
            fn parent() {
                assert total() == 0.0f;
                Reading.insert({ id: 3, value: 1.0f });
                Reading.insert({ id: 2, value: -10000000000000000.0f });
                Reading.insert({ id: 1, value: 10000000000000000.0f });
                assert total() == 1.0f;
            }
        "#
        .into(),
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
fn relation_float_min_and_max_fail_closed() {
    for (name, operation) in [("minimum", "min"), ("maximum", "max")] {
        let unit = SourceUnit {
            fixture_id: format!("relation-float-{operation}"),
            source_id: format!("relation-float-{operation}.orna"),
            parse_as: "module_unit".into(),
            source: format!(
                r#"
                    pub table Reading(id: Int) {{ value: Float, }}
                    fn {name}() = Reading | map(reading => reading.value) | {operation}();
                    fn parent() {{
                        Reading.insert({{ id: 1, value: 1.5f }});
                        {name}();
                    }}
                "#
            ),
        };
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

#[test]
fn relation_scan_shares_evaluator_budget_and_rolls_back_on_exhaustion() {
    let mut limits = Limits::default();
    limits.max_steps = 14;
    let mut evaluator = TransactionalEvaluator::new("parent", limits);
    let outcome = evaluator.execute_source(&source(
        r#"
            Reading.insert({ id: 1, value: 2 });
            Reading.insert({ id: 2, value: 3 });
            Reading | map(reading => reading.value) | sum;
        "#,
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
    let mut limits = Limits::default();
    limits.max_steps = 0;
    let mut evaluator = TransactionalEvaluator::new("parent", limits);
    let outcome = evaluator.execute_source(&source(
        r#"
            Reading.insert({ id: 1, value: 2 });
            Reading | map(reading => reading.value) | sum;
        "#,
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
        source: r#"
            pub table Reading(id: Int) {
                value: Int,
                assert every(reading => reading.value > 0);
            }
            fn parent() {
                Reading.insert({ id: 1, value: 2 });
                Reading.insert({ id: 2, value: 3 });
            }
        "#
        .into(),
    };
    let mut limits = Limits::default();
    limits.max_steps = 10;
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
    let mut limits = Limits::default();
    limits.max_steps = 20;
    let mut evaluator = TransactionalEvaluator::new("parent", limits);
    assert!(matches!(
        evaluator.execute_source(&table_source(
            "Reading.insert({ id: 1, value: 2 }); Reading.insert({ id: 2, value: 2 });",
        )),
        StageOutcome::Passed
    ));
    let outcome = evaluator.execute_source(&table_assertion_source(
        "every(reading => reading.value + 1 + 1 + 1 + 1 == 6)",
        "",
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
        source: r#"
            pub table Book(id: Int) { title: Str, }
            pub table Loan(id: Int) { book_id: Int, }
            fn parent() {
                Book.insert({ id: 1, title: "one" });
                Book.insert({ id: 2, title: "two" });
                Loan.insert({ id: 10, book_id: 1 });
                Loan.insert({ id: 11, book_id: 2 });
            }
        "#
        .into(),
    };
    let assertion = SourceUnit {
        fixture_id: "nested-assertion-budget".into(),
        source_id: "nested-assertion-budget.orna".into(),
        parse_as: "module_unit".into(),
        source: r#"
            pub table Book(id: Int) { title: Str, }
            pub table Loan(id: Int) { book_id: Int, }
            assert every(Loan, loan => exists(Book, book => book.id == loan.book_id));
            fn parent() { }
        "#
        .into(),
    };
    let mut limits = Limits::default();
    limits.max_steps = 20;
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
    let unit = table_assertion_source(
        "every(reading => reading.value > 0)",
        "Reading.insert({ id: 1, value: 1 }); Reading.insert({ id: 2, value: 2 });",
    );

    let mut zero_limits = Limits::default();
    zero_limits.max_steps = 0;
    let mut zero = TransactionalEvaluator::new("parent", zero_limits);
    let zero_outcome = zero.execute_source(&unit);
    assert!(matches!(
        zero_outcome,
        StageOutcome::Failed(ref diagnostic) if diagnostic.code() == "ORNA-EVAL-LIMIT"
    ));
    for id in [1, 2] {
        assert_eq!(zero.committed_row("Reading", &Value::int(id.into())), None);
    }

    let mut exhausted_limits = Limits::default();
    exhausted_limits.max_steps = 10;
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
    let mut limits = Limits::default();
    limits.max_steps = 1_000;
    let mut evaluator = TransactionalEvaluator::new("parent", limits);
    let outcome = evaluator.execute_source(&table_assertion_source(
        "every(reading => reading.value > 0)",
        "Reading.insert({ id: 1, value: 1 }); Reading.insert({ id: 2, value: 2 });",
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
