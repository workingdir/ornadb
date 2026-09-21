use orna_conformance_v1::{SourceUnit, StageOutcome, TransactionalEvaluator};
use orna_evaluator_v1::Limits;
use orna_foundation_v1::Value;

fn source(parent_body: &str) -> SourceUnit {
    SourceUnit {
        fixture_id: "txn-source".into(),
        source_id: "txn-source.orna".into(),
        parse_as: "module_unit".into(),
        source: format!(
            "pub table Note(id: Int) {{ text: Str, }} fn child() {{ Note.insert({{ id: 7, text: \"nested\" }}); }} fn parent() {{ child(); {parent_body} }}"
        ),
    }
}

fn composite_source(parent_body: &str) -> SourceUnit {
    SourceUnit {
        fixture_id: "txn-source".into(),
        source_id: "txn-source.orna".into(),
        parse_as: "module_unit".into(),
        source: format!(
            "pub table Stock(location: Str, sku: Str) {{ quantity: Int, }} fn parent() {{ Stock.insert({{ location: \"north\", sku: \"pencil\", quantity: 12 }}); {parent_body} }}"
        ),
    }
}

fn source_with_count_function(count_body: &str, parent_body: &str) -> SourceUnit {
    SourceUnit {
        fixture_id: "txn-source".into(),
        source_id: "txn-source.orna".into(),
        parse_as: "module_unit".into(),
        source: format!(
            "pub table Note(id: Int) {{ text: Str, }} fn count_notes() = {count_body}; fn parent() {{ {parent_body} }}"
        ),
    }
}

fn source_with_table_assertion(assertion: &str, parent_body: &str) -> SourceUnit {
    SourceUnit {
        fixture_id: "txn-source".into(),
        source_id: "txn-source.orna".into(),
        parse_as: "module_unit".into(),
        source: format!(
            "pub table Note(id: Int) {{ text: Str, assert {assertion}; }} fn parent() {{ {parent_body} }}"
        ),
    }
}
fn decimal_body_table_assertion_source(assertion: &str, parent_body: &str) -> SourceUnit {
    SourceUnit {
        fixture_id: "txn-decimal-table-assertion".into(),
        source_id: "txn-decimal-table-assertion.orna".into(),
        parse_as: "module_unit".into(),
        source: format!(
            "pub table Reading(id: Int) {{ value: Decimal, label: Str, assert {assertion}; }} fn parent() {{ {parent_body} }}"
        ),
    }
}

fn decimal_key_table_assertion_source(assertion: &str, parent_body: &str) -> SourceUnit {
    SourceUnit {
        fixture_id: "txn-decimal-key-table-assertion".into(),
        source_id: "txn-decimal-key-table-assertion.orna".into(),
        parse_as: "module_unit".into(),
        source: format!(
            "pub table Reading(value: Decimal) {{ label: Str, assert {assertion}; }} fn parent() {{ {parent_body} }}"
        ),
    }
}


fn source_with_module_assertion(assertion: &str, parent_body: &str) -> SourceUnit {
    SourceUnit {
        fixture_id: "txn-source".into(),
        source_id: "txn-source.orna".into(),
        parse_as: "module_unit".into(),
        source: format!(
            "pub table Book(id: Int) {{ title: Str, }} pub table Loan(id: Int) {{ book_id: Int, }} assert {assertion}; fn parent() {{ {parent_body} }}"
        ),
    }
}
fn decimal_module_assertion_source(assertion: &str, parent_body: &str) -> SourceUnit {
    SourceUnit {
        fixture_id: "txn-decimal-module-assertion".into(),
        source_id: "txn-decimal-module-assertion.orna".into(),
        parse_as: "module_unit".into(),
        source: format!(
            "pub table Invoice(id: Int) {{ amount: Decimal, }} pub table Payment(id: Int) {{ invoice_id: Int, amount: Decimal, }} assert {assertion}; fn parent() {{ {parent_body} }}"
        ),
    }
}

fn quantifier_source(fixture_id: &str, definitions: &str, parent_body: &str) -> SourceUnit {
    SourceUnit {
        fixture_id: fixture_id.into(),
        source_id: format!("{fixture_id}.orna"),
        parse_as: "module_unit".into(),
        source: format!(
            "pub table Note(id: Int) {{ text: Str, }} {definitions} fn parent() {{ {parent_body} }}"
        ),
    }
}


#[test]
fn parsed_nested_insert_is_rolled_back_when_parent_assertion_escapes() {
    let mut runtime = TransactionalEvaluator::new("parent", Limits::default());
    let outcome = runtime.execute_source(&source("assert false;"));

    assert!(matches!(
        outcome,
        StageOutcome::Failed(ref diagnostic) if diagnostic.code() == "ORNA-EVAL-ASSERT"
    ));
    assert_eq!(runtime.committed_row("Note", &Value::int(7.into())), None);
}

#[test]
fn parsed_nested_insert_commits_when_parent_returns_successfully() {
    let mut runtime = TransactionalEvaluator::new("parent", Limits::default());
    assert!(matches!(
        runtime.execute_source(&source("")),
        StageOutcome::Passed
    ));
    assert!(
        runtime
            .committed_row("Note", &Value::int(7.into()))
            .is_some()
    );
}

#[test]
fn parsed_duplicate_insert_rolls_back_the_complete_activation() {
    let mut runtime = TransactionalEvaluator::new("parent", Limits::default());
    let outcome = runtime.execute_source(&source("Note.insert({ id: 7, text: \"duplicate\" });"));

    assert!(matches!(
        outcome,
        StageOutcome::Failed(ref diagnostic) if diagnostic.code() == "ORNA-EVAL-TABLE-DUPLICATE"
    ));
    assert_eq!(runtime.committed_row("Note", &Value::int(7.into())), None);
}

#[test]
fn parsed_repeated_primary_key_declaration_is_rejected_before_transaction_admission() {
    let mut runtime = TransactionalEvaluator::new("write", Limits::default());
    let unit = SourceUnit {
        fixture_id: "transaction-key-schema".into(),
        source_id: "transaction-key-schema.orna".into(),
        parse_as: "module_unit".into(),
        source: "pub table Reading(sensor: Str, sensor: Str) { value: Int, } fn write() { Reading.insert({ sensor: \"north\", value: 1 }); }".into(),
    };

    let outcome = runtime.execute_source(&unit);

    assert!(
        matches!(
            outcome,
            StageOutcome::Failed(ref diagnostic) if diagnostic.code() == "ORNA-S013-DUPLICATE"
        ),
        "{outcome:?}"
    );
    let key = Value::new(orna_foundation_v1::OvbRaw::Text("north".into()))
        .expect("canonical primary key");
    assert_eq!(runtime.committed_row("Reading", &key), None);
}

#[test]
fn parsed_update_patches_only_stored_fields() {
    let mut runtime = TransactionalEvaluator::new("parent", Limits::default());
    assert!(matches!(
        runtime.execute_source(&source(r#"Note.update(7, { text: "changed" });"#)),
        StageOutcome::Passed
    ));
    let row = runtime
        .committed_row("Note", &Value::int(7.into()))
        .expect("updated row");
    assert!(matches!(
        row.raw(),
        orna_foundation_v1::OvbRaw::Map(fields)
            if fields.iter().any(|(key, value)| key == &orna_foundation_v1::OvbRaw::Text("text".into())
                && value == &orna_foundation_v1::OvbRaw::Text("changed".into()))
    ));
}

#[test]
fn parsed_upsert_patches_existing_rows_and_inserts_absent_rows() {
    let mut runtime = TransactionalEvaluator::new("parent", Limits::default());
    assert!(matches!(
        runtime.execute_source(&source(
            r#"Note.upsert({ id: 7, text: "updated" }); Note.upsert({ id: 8, text: "new" });"#,
        )),
        StageOutcome::Passed
    ));
    let updated = runtime
        .committed_row("Note", &Value::int(7.into()))
        .expect("updated row");
    assert!(matches!(
        updated.raw(),
        orna_foundation_v1::OvbRaw::Map(fields)
            if fields.iter().any(|(key, value)| key == &orna_foundation_v1::OvbRaw::Text("text".into())
                && value == &orna_foundation_v1::OvbRaw::Text("updated".into()))
    ));
    assert!(
        runtime
            .committed_row("Note", &Value::int(8.into()))
            .is_some()
    );
}

#[test]
fn parsed_table_count_observes_nested_read_your_writes() {
    let mut runtime = TransactionalEvaluator::new("parent", Limits::default());
    assert!(matches!(
        runtime.execute_source(&source("assert Note.count() == 1;")),
        StageOutcome::Passed
    ));
}

#[test]
fn parsed_relation_first_reads_canonical_candidate_rows_and_never_publishes_on_failure() {
    let mut empty = TransactionalEvaluator::new("parent", Limits::default());
    let empty_outcome = empty.execute_source(&source(
        r#"assert (Note.first() ?? Note.insert({ id: 7, text: "fallback" })).id == 7;"#,
    ));
    assert!(
        matches!(empty_outcome, StageOutcome::Passed),
        "{empty_outcome:?}"
    );
    assert!(empty.committed_row("Note", &Value::int(7.into())).is_some());

    let mut committed = TransactionalEvaluator::new("parent", Limits::default());
    assert!(matches!(
        committed.execute_source(&source(
            r#"Note.insert({ id: 2, text: "two" }); Note.insert({ id: 1, text: "one" }); assert (Note.first() ?? Note.insert({ id: 99, text: "fallback" })).id == 1; assert (Note.first() ?? Note.insert({ id: 99, text: "fallback" })).text == "one";"#,
        )),
        StageOutcome::Passed
    ));
    assert!(
        committed
            .committed_row("Note", &Value::int(1.into()))
            .is_some()
    );
    assert_eq!(
        committed.committed_row("Note", &Value::int(99.into())),
        None
    );

    let mut rolled_back = TransactionalEvaluator::new("parent", Limits::default());
    let outcome = rolled_back.execute_source(&source(
        r#"Note.insert({ id: 2, text: "two" }); Note.insert({ id: 1, text: "one" }); assert (Note.first() ?? Note.insert({ id: 99, text: "fallback" })).id == 1; assert false;"#,
    ));
    assert!(matches!(
        outcome,
        StageOutcome::Failed(ref diagnostic) if diagnostic.code() == "ORNA-EVAL-ASSERT"
    ));
    assert_eq!(
        rolled_back.committed_row("Note", &Value::int(1.into())),
        None
    );
    assert_eq!(
        rolled_back.committed_row("Note", &Value::int(2.into())),
        None
    );
}

#[test]
fn parsed_keyed_relation_one_observes_candidate_rows_and_absence_rolls_back() {
    let unit = SourceUnit {
        fixture_id: "relation-one".into(),
        source_id: "relation-one.orna".into(),
        parse_as: "module_unit".into(),
        source: r#"
            pub table Note(id: Int) { text: Str, }
            fn find_note(id: Int) = Note | filter(note => note.id == id) | one();
            fn parent() {
                Note.insert({ id: 7, text: "candidate" });
                assert find_note(7).text == "candidate";
            }
        "#
        .into(),
    };
    let mut committed = TransactionalEvaluator::new("parent", Limits::default());
    assert!(matches!(
        committed.execute_source(&unit),
        StageOutcome::Passed
    ));
    assert!(
        committed
            .committed_row("Note", &Value::int(7.into()))
            .is_some()
    );

    let missing = SourceUnit {
        fixture_id: "relation-one-missing".into(),
        source_id: "relation-one-missing.orna".into(),
        parse_as: "module_unit".into(),
        source: r#"
            pub table Note(id: Int) { text: Str, }
            fn find_note(id: Int) = Note | filter(note => note.id == id) | one();
            fn parent() {
                Note.insert({ id: 7, text: "candidate" });
                find_note(99);
            }
        "#
        .into(),
    };
    let mut rolled_back = TransactionalEvaluator::new("parent", Limits::default());
    assert!(matches!(
        rolled_back.execute_source(&missing),
        StageOutcome::Failed(ref diagnostic) if diagnostic.code() == "ORNA-EVAL-TABLE-MISSING"
    ));
    assert_eq!(
        rolled_back.committed_row("Note", &Value::int(7.into())),
        None
    );
}

#[test]
fn parsed_relation_one_rejects_multiple_candidate_matches_and_rolls_back() {
    let unit = SourceUnit {
        fixture_id: "relation-one-multiple".into(),
        source_id: "relation-one-multiple.orna".into(),
        parse_as: "module_unit".into(),
        source: r#"
            pub table Note(id: Int) { text: Str, }
            fn find_note(text: Str) = Note | filter(note => note.text == text) | one();
            fn parent() {
                Note.insert({ id: 1, text: "duplicate" });
                Note.insert({ id: 2, text: "duplicate" });
                find_note("duplicate");
            }
        "#
        .into(),
    };
    let mut evaluator = TransactionalEvaluator::new("parent", Limits::default());

    assert!(matches!(
        evaluator.execute_source(&unit),
        StageOutcome::Failed(ref diagnostic) if diagnostic.code() == "ORNA-EVAL-RELATION-ONE-MULTIPLE"
    ));
    for id in [1, 2] {
        assert_eq!(
            evaluator.committed_row("Note", &Value::int(id.into())),
            None,
            "a multiple-match one() failure must roll back candidate rows"
        );
    }
}

#[test]
fn parsed_pipeline_count_in_a_direct_function_body_observes_activation_writes() {
    let mut runtime = TransactionalEvaluator::new("parent", Limits::default());
    let outcome = runtime.execute_source(&source_with_count_function(
        "Note | count",
        r#"Note.insert({ id: 7, text: "first" }); assert count_notes() == 1;"#,
    ));

    assert!(matches!(outcome, StageOutcome::Passed));
    assert!(
        runtime
            .committed_row("Note", &Value::int(7.into()))
            .is_some()
    );
}

#[test]
fn parsed_relation_windows_execute_direct_and_piped_complete_windows() {
    let mut runtime = TransactionalEvaluator::new("parent", Limits::default());
    let unit = SourceUnit {
        fixture_id: "relation-window".into(),
        source_id: "relation-window.orna".into(),
        parse_as: "module_unit".into(),
        source: r#"
            pub table Note(id: Int) { text: Str, }
            fn default_step() = Note | window(3) | count;
            fn piped() = Note | window(3, step: 3) | count;
            fn direct() = window(Note, size: 2, step: 3) | count;
            fn parent() {
                Note.insert({ id: 1, text: "one" });
                Note.insert({ id: 2, text: "two" });
                Note.insert({ id: 3, text: "three" });
                Note.insert({ id: 4, text: "four" });
                Note.insert({ id: 5, text: "five" });
                assert default_step() == 3;
                assert piped() == 1;
                assert direct() == 2;
            }
        "#
        .into(),
    };

    assert!(matches!(
        runtime.execute_source(&unit),
        StageOutcome::Passed
    ));
}

#[test]
fn parsed_relation_windows_reject_dynamic_non_positive_parameters_without_publish() {
    let mut runtime = TransactionalEvaluator::new("parent", Limits::default());
    let unit = SourceUnit {
        fixture_id: "relation-window-invalid".into(),
        source_id: "relation-window-invalid.orna".into(),
        parse_as: "module_unit".into(),
        source: r#"
            pub table Note(id: Int) { text: Str, }
            fn count_windows(size: Int, step: Int) = Note | window(size, step) | count;
            fn parent() {
                Note.insert({ id: 1, text: "one" });
                assert count_windows(0, 1) == 0;
            }
        "#
        .into(),
    };

    assert!(matches!(
        runtime.execute_source(&unit),
        StageOutcome::Failed(ref diagnostic) if diagnostic.code() == "ORNA-EVAL-TABLE-ARGUMENT"
    ));
    assert_eq!(runtime.committed_row("Note", &Value::int(1.into())), None);
}

#[test]
fn parsed_relation_windows_reject_dynamic_negative_parameters_without_publish() {
    let mut runtime = TransactionalEvaluator::new("parent", Limits::default());
    let unit = SourceUnit {
        fixture_id: "relation-window-negative".into(),
        source_id: "relation-window-negative.orna".into(),
        parse_as: "module_unit".into(),
        source: r#"
            pub table Note(id: Int) { text: Str, }
            fn count_windows(size: Int, step: Int) = window(Note, size, step) | count;
            fn parent() {
                Note.insert({ id: 1, text: "one" });
                assert count_windows(0 - 1, 1) == 0;
            }
        "#
        .into(),
    };

    assert!(matches!(
        runtime.execute_source(&unit),
        StageOutcome::Failed(ref diagnostic) if diagnostic.code() == "ORNA-EVAL-TABLE-ARGUMENT"
    ));
    assert_eq!(runtime.committed_row("Note", &Value::int(1.into())), None);
}

#[test]
fn parsed_pipeline_count_call_in_a_direct_function_body_observes_activation_writes() {
    let mut runtime = TransactionalEvaluator::new("parent", Limits::default());
    let outcome = runtime.execute_source(&source_with_count_function(
        "Note | count()",
        r#"Note.insert({ id: 7, text: "first" }); assert count_notes() == 1;"#,
    ));

    assert!(matches!(outcome, StageOutcome::Passed));
    assert!(
        runtime
            .committed_row("Note", &Value::int(7.into()))
            .is_some()
    );
}

#[test]
fn parsed_filter_count_pipeline_observes_candidate_rows_and_read_your_writes() {
    let mut runtime = TransactionalEvaluator::new("parent", Limits::default());
    let outcome = runtime.execute_source(&source(
        r#"Note.insert({ id: 8, text: "second" }); assert Note | filter(note => note.text == "nested") | count == 1; assert Note | filter(note => note.text == "second") | count() == 1;"#,
    ));

    assert!(matches!(outcome, StageOutcome::Passed));
    assert!(
        runtime
            .committed_row("Note", &Value::int(7.into()))
            .is_some()
    );
    assert!(
        runtime
            .committed_row("Note", &Value::int(8.into()))
            .is_some()
    );
}

#[test]
fn parsed_filter_count_failure_rolls_back_candidate_rows() {
    let mut runtime = TransactionalEvaluator::new("parent", Limits::default());
    let outcome = runtime.execute_source(&source(
        r#"Note.insert({ id: 8, text: "second" }); assert Note | filter(note => note.text == "second") | count == 1; assert false;"#,
    ));

    assert!(matches!(
        outcome,
        StageOutcome::Failed(ref diagnostic) if diagnostic.code() == "ORNA-EVAL-ASSERT"
    ));
    assert_eq!(runtime.committed_row("Note", &Value::int(7.into())), None);
    assert_eq!(runtime.committed_row("Note", &Value::int(8.into())), None);
}

#[test]
fn parsed_undocumented_table_count_where_member_fails_closed() {
    let mut runtime = TransactionalEvaluator::new("parent", Limits::default());
    let outcome = runtime.execute_source(&source(r#"Note.count_where("text", "nested");"#));

    assert!(matches!(outcome, StageOutcome::Failed(_)));
    assert_eq!(runtime.committed_row("Note", &Value::int(7.into())), None);
}

#[test]
fn parsed_pipeline_count_bare_statement_observes_activation_writes() {
    let mut runtime = TransactionalEvaluator::new("parent", Limits::default());
    let outcome = runtime.execute_source(&source(
        r#"Note | count; Note.insert({ id: 8, text: "second" }); assert Note | count == 2;"#,
    ));

    assert!(matches!(outcome, StageOutcome::Passed));
    assert!(
        runtime
            .committed_row("Note", &Value::int(7.into()))
            .is_some()
    );
    assert!(
        runtime
            .committed_row("Note", &Value::int(8.into()))
            .is_some()
    );
}

#[test]
fn parsed_pipeline_count_read_your_writes_succeeds_before_a_distinct_failure_rolls_back() {
    let mut runtime = TransactionalEvaluator::new("parent", Limits::default());
    let outcome = runtime.execute_source(&source(
        r#"Note.insert({ id: 8, text: "second" }); assert Note | count() == 2; assert 1 == 2;"#,
    ));

    assert!(matches!(
        outcome,
        StageOutcome::Failed(ref diagnostic) if diagnostic.code() == "ORNA-EVAL-ASSERT"
    ));
    assert_eq!(runtime.committed_row("Note", &Value::int(7.into())), None);
    assert_eq!(runtime.committed_row("Note", &Value::int(8.into())), None);
}

#[test]
fn parsed_relation_filter_and_map_preserve_candidate_values() {
    let mut runtime = TransactionalEvaluator::new("parent", Limits::default());
    let outcome = runtime.execute_source(&source(
        r#"
            assert Note | filter(note => note.id != 7) | count == 0;
            Note.insert({ id: 8, text: "second" });
            let selected = Note | filter(note => note.id != 7);
            assert selected | count == 1;
            let texts = selected | map(note => note.text);
            assert (texts | first() ?? "missing") == "second";
        "#,
    ));

    assert!(matches!(outcome, StageOutcome::Passed), "{outcome:?}");
    assert!(
        runtime
            .committed_row("Note", &Value::int(7.into()))
            .is_some()
    );
    assert!(
        runtime
            .committed_row("Note", &Value::int(8.into()))
            .is_some()
    );
}

#[test]
fn parsed_delete_removes_the_candidate_row() {
    let mut runtime = TransactionalEvaluator::new("parent", Limits::default());
    assert!(matches!(
        runtime.execute_source(&source("Note.delete(7);")),
        StageOutcome::Passed
    ));
    assert_eq!(runtime.committed_row("Note", &Value::int(7.into())), None);
}

#[test]
fn parsed_rekey_moves_the_row_atomically() {
    let mut runtime = TransactionalEvaluator::new("parent", Limits::default());
    assert!(matches!(
        runtime.execute_source(&source("Note.rekey(7, 8);")),
        StageOutcome::Passed
    ));
    assert_eq!(runtime.committed_row("Note", &Value::int(7.into())), None);
    assert!(
        runtime
            .committed_row("Note", &Value::int(8.into()))
            .is_some()
    );
}

#[test]
fn parsed_composite_rekey_moves_every_key_component_in_declaration_order() {
    let mut runtime = TransactionalEvaluator::new("parent", Limits::default());
    let outcome = runtime.execute_source(&composite_source(
        r#"Stock.rekey(("north", "pencil"), ("south", "pencil"));"#,
    ));
    assert!(matches!(outcome, StageOutcome::Passed));

    let north = Value::new(orna_foundation_v1::OvbRaw::Array(vec![
        orna_foundation_v1::OvbRaw::Text("north".into()),
        orna_foundation_v1::OvbRaw::Text("pencil".into()),
    ]))
    .expect("canonical composite key");
    let south = Value::new(orna_foundation_v1::OvbRaw::Array(vec![
        orna_foundation_v1::OvbRaw::Text("south".into()),
        orna_foundation_v1::OvbRaw::Text("pencil".into()),
    ]))
    .expect("canonical composite key");
    assert_eq!(runtime.committed_row("Stock", &north), None);
    let row = runtime
        .committed_row("Stock", &south)
        .expect("rekeyed composite row");
    assert!(matches!(
        row.raw(),
        orna_foundation_v1::OvbRaw::Map(fields)
            if fields.iter().any(|(key, value)| key == &orna_foundation_v1::OvbRaw::Text("location".into())
                && value == &orna_foundation_v1::OvbRaw::Text("south".into()))
                && fields.iter().any(|(key, value)| key == &orna_foundation_v1::OvbRaw::Text("sku".into())
                    && value == &orna_foundation_v1::OvbRaw::Text("pencil".into()))
                && fields.iter().any(|(key, value)| key == &orna_foundation_v1::OvbRaw::Text("quantity".into())
                    && value == &orna_foundation_v1::OvbRaw::Int(12.into()))
    ));
}

#[test]
fn parsed_composite_rekey_rejects_a_non_tuple_target_key_without_publication() {
    let mut runtime = TransactionalEvaluator::new("parent", Limits::default());
    let outcome = runtime.execute_source(&composite_source(
        r#"Stock.rekey(("north", "pencil"), "south");"#,
    ));

    assert!(matches!(
        outcome,
        StageOutcome::Failed(ref diagnostic) if diagnostic.code() == "ORNA-S021-TYPE"
    ));
    let north = Value::new(orna_foundation_v1::OvbRaw::Array(vec![
        orna_foundation_v1::OvbRaw::Text("north".into()),
        orna_foundation_v1::OvbRaw::Text("pencil".into()),
    ]))
    .expect("canonical composite key");
    assert_eq!(runtime.committed_row("Stock", &north), None);
}

#[test]
fn parsed_rekey_collision_rolls_back_all_activation_writes() {
    let mut runtime = TransactionalEvaluator::new("parent", Limits::default());
    let outcome = runtime.execute_source(&source(
        "Note.insert({ id: 8, text: \"competing\" }); Note.rekey(7, 8);",
    ));

    assert!(matches!(
        outcome,
        StageOutcome::Failed(ref diagnostic) if diagnostic.code() == "ORNA-EVAL-TABLE-DUPLICATE"
    ));
    assert_eq!(runtime.committed_row("Note", &Value::int(7.into())), None);
    assert_eq!(runtime.committed_row("Note", &Value::int(8.into())), None);
}

#[test]
fn table_every_assertion_observes_all_candidate_rows_before_publication() {
    let mut runtime = TransactionalEvaluator::new("parent", Limits::default());
    let outcome = runtime.execute_source(&source_with_table_assertion(
        r#"every(note => note.text != "")"#,
        r#"Note.insert({ id: 7, text: "valid" }); Note.insert({ id: 8, text: "" });"#,
    ));

    assert!(matches!(
        outcome,
        StageOutcome::Failed(ref diagnostic) if diagnostic.code() == "ORNA-EVAL-TABLE-ASSERT"
    ));
    assert_eq!(runtime.committed_row("Note", &Value::int(7.into())), None);
    assert_eq!(runtime.committed_row("Note", &Value::int(8.into())), None);
}

#[test]
fn table_every_assertion_permits_atomic_publication() {
    let mut runtime = TransactionalEvaluator::new("parent", Limits::default());
    let outcome = runtime.execute_source(&source_with_table_assertion(
        r#"every(note => note.text != "")"#,
        r#"Note.insert({ id: 7, text: "first" }); Note.insert({ id: 8, text: "second" });"#,
    ));

    assert!(matches!(outcome, StageOutcome::Passed));
    assert!(
        runtime
            .committed_row("Note", &Value::int(7.into()))
            .is_some()
    );
    assert!(
        runtime
            .committed_row("Note", &Value::int(8.into()))
            .is_some()
    );
}

#[test]
fn table_every_assertion_evaluation_failure_rolls_back_the_activation() {
    let mut runtime = TransactionalEvaluator::new("parent", Limits::default());
    let outcome = runtime.execute_source(&source_with_table_assertion(
        r#"every(note => note.text[1] == "x")"#,
        r#"Note.insert({ id: 7, text: "a" }); Note.insert({ id: 8, text: "" });"#,
    ));

    assert!(matches!(outcome, StageOutcome::Failed(_)));
    assert_eq!(runtime.committed_row("Note", &Value::int(7.into())), None);
    assert_eq!(runtime.committed_row("Note", &Value::int(8.into())), None);
}

#[test]
fn table_all_unique_assertion_rejects_duplicate_candidate_projections() {
    let mut runtime = TransactionalEvaluator::new("parent", Limits::default());
    let outcome = runtime.execute_source(&source_with_table_assertion(
        "all_unique(note => note.text)",
        r#"Note.insert({ id: 7, text: "duplicate" }); Note.insert({ id: 8, text: "duplicate" });"#,
    ));

    assert!(matches!(
        outcome,
        StageOutcome::Failed(ref diagnostic) if diagnostic.code() == "ORNA-EVAL-TABLE-ASSERT"
    ));
    assert_eq!(runtime.committed_row("Note", &Value::int(7.into())), None);
    assert_eq!(runtime.committed_row("Note", &Value::int(8.into())), None);
}

#[test]
fn table_all_unique_assertion_permits_atomic_publication() {
    let mut runtime = TransactionalEvaluator::new("parent", Limits::default());
    let outcome = runtime.execute_source(&source_with_table_assertion(
        "all_unique(note => note.text)",
        r#"Note.insert({ id: 7, text: "first" }); Note.insert({ id: 8, text: "second" });"#,
    ));

    assert!(matches!(outcome, StageOutcome::Passed));
    assert!(
        runtime
            .committed_row("Note", &Value::int(7.into()))
            .is_some()
    );
    assert!(
        runtime
            .committed_row("Note", &Value::int(8.into()))
            .is_some()
    );
}

#[test]
fn table_all_unique_decimal_body_rejects_scale_alias_and_rolls_back_candidates() {
    let mut runtime = TransactionalEvaluator::new("parent", Limits::default());
    let outcome = runtime.execute_source(&decimal_body_table_assertion_source(
        "all_unique(reading => reading.value)",
        r#"
            Reading.insert({ id: 1, value: 18.25, label: "canonical" });
            Reading.insert({ id: 2, value: 18.2500, label: "scale-alias" });
        "#,
    ));

    assert!(
        matches!(
            &outcome,
            StageOutcome::Failed(diagnostic)
                if diagnostic.code() == "ORNA-EVAL-TABLE-ASSERT"
        ),
        "scale-alias Decimal body did not fail all_unique: {outcome:?}"
    );
    for id in [1, 2] {
        assert_eq!(
            runtime.committed_row("Reading", &Value::int(id.into())),
            None,
            "failed Decimal body assertion published candidate row {id}"
        );
    }
}

#[test]
fn table_all_unique_decimal_body_permits_distinct_values() {
    let mut runtime = TransactionalEvaluator::new("parent", Limits::default());
    let outcome = runtime.execute_source(&decimal_body_table_assertion_source(
        "all_unique(reading => reading.value)",
        r#"
            Reading.insert({ id: 1, value: 18.25, label: "first" });
            Reading.insert({ id: 2, value: 18.26, label: "second" });
        "#,
    ));

    assert!(matches!(&outcome, StageOutcome::Passed), "{outcome:?}");
    for id in [1, 2] {
        assert!(
            runtime
                .committed_row("Reading", &Value::int(id.into()))
                .is_some(),
            "distinct Decimal body row {id} was not published"
        );
    }
}

#[test]
fn table_all_unique_decimal_primary_key_rejects_scale_alias_before_assertion_and_rolls_back_candidates() {
    let mut runtime = TransactionalEvaluator::new("parent", Limits::default());
    let outcome = runtime.execute_source(&decimal_key_table_assertion_source(
        "all_unique(reading => reading.value)",
        r#"
            Reading.insert({ value: 18.25, label: "canonical" });
            Reading.insert({ value: 18.2500, label: "scale-alias" });
        "#,
    ));

    assert!(
        matches!(
            &outcome,
            StageOutcome::Failed(diagnostic)
                if diagnostic.code() == "ORNA-EVAL-TABLE-DUPLICATE"
        ),
        "scale-alias Decimal primary key did not fail at key identity admission: {outcome:?}"
    );
    let canonical_key =
        Value::decimal(1825.into(), (-2).into()).expect("canonical first Decimal key");
    assert_eq!(
        runtime.committed_row("Reading", &canonical_key),
        None,
        "failed Decimal primary-key admission published candidate row"
    );
}

#[test]
fn table_all_unique_decimal_primary_key_permits_distinct_values() {
    let mut runtime = TransactionalEvaluator::new("parent", Limits::default());
    let outcome = runtime.execute_source(&decimal_key_table_assertion_source(
        "all_unique(reading => reading.value)",
        r#"
            Reading.insert({ value: 18.25, label: "first" });
            Reading.insert({ value: 18.26, label: "second" });
        "#,
    ));

    assert!(matches!(&outcome, StageOutcome::Passed), "{outcome:?}");
    for key in [
        Value::decimal(1825.into(), (-2).into()).expect("canonical first Decimal key"),
        Value::decimal(1826.into(), (-2).into()).expect("canonical second Decimal key"),
    ] {
        assert!(
            runtime.committed_row("Reading", &key).is_some(),
            "distinct Decimal primary-key row was not published"
        );
    }
}

#[test]
fn module_every_exists_assertion_rolls_back_cross_table_candidate_writes() {
    let mut runtime = TransactionalEvaluator::new("parent", Limits::default());
    let outcome = runtime.execute_source(&source_with_module_assertion(
        "every(Loan, loan => exists(Book, book => book.id == loan.book_id))",
        "Book.insert({ id: 7, title: \"present\" }); Loan.insert({ id: 1, book_id: 8 });",
    ));

    assert!(matches!(
        outcome,
        StageOutcome::Failed(ref diagnostic) if diagnostic.code() == "ORNA-EVAL-MODULE-ASSERT"
    ));
    assert_eq!(runtime.committed_row("Book", &Value::int(7.into())), None);
    assert_eq!(runtime.committed_row("Loan", &Value::int(1.into())), None);
}

#[test]
fn module_every_exists_assertion_permits_atomic_cross_table_publication() {
    let mut runtime = TransactionalEvaluator::new("parent", Limits::default());
    let outcome = runtime.execute_source(&source_with_module_assertion(
        "every(Loan, loan => exists(Book, book => book.id == loan.book_id))",
        "Book.insert({ id: 7, title: \"present\" }); Loan.insert({ id: 1, book_id: 7 });",
    ));

    assert!(matches!(outcome, StageOutcome::Passed));
    assert!(
        runtime
            .committed_row("Book", &Value::int(7.into()))
            .is_some()
    );
    assert!(
        runtime
            .committed_row("Loan", &Value::int(1.into()))
            .is_some()
    );
}

#[test]
fn module_decimal_assertion_accepts_scale_aliases_and_publishes_candidate_rows() {
    let mut runtime = TransactionalEvaluator::new("parent", Limits::default());
    let outcome = runtime.execute_source(&decimal_module_assertion_source(
        "every(Invoice, invoice => exists(Payment, payment => payment.invoice_id == invoice.id && payment.amount == invoice.amount))",
        r#"
            Invoice.insert({ id: 1, amount: 18.25 });
            Payment.insert({ id: 1, invoice_id: 1, amount: 18.2500 });
        "#,
    ));

    assert!(matches!(&outcome, StageOutcome::Passed), "{outcome:?}");
    assert!(
        runtime
            .committed_row("Invoice", &Value::int(1.into()))
            .is_some(),
        "candidate Invoice row was not published"
    );
    assert!(
        runtime
            .committed_row("Payment", &Value::int(1.into()))
            .is_some(),
        "candidate Payment row was not published"
    );
}

#[test]
fn module_decimal_assertion_rejects_nonmatching_candidate_and_rolls_back_all_writes() {
    let mut runtime = TransactionalEvaluator::new("parent", Limits::default());
    let outcome = runtime.execute_source(&decimal_module_assertion_source(
        "every(Invoice, invoice => exists(Payment, payment => payment.invoice_id == invoice.id && payment.amount == invoice.amount))",
        r#"
            Invoice.insert({ id: 1, amount: 18.25 });
            Invoice.insert({ id: 2, amount: 9.99 });
            Payment.insert({ id: 1, invoice_id: 1, amount: 18.26 });
            Payment.insert({ id: 2, invoice_id: 2, amount: 9.9900 });
        "#,
    ));

    assert!(
        matches!(
            &outcome,
            StageOutcome::Failed(diagnostic)
                if diagnostic.code() == "ORNA-EVAL-MODULE-ASSERT"
        ),
        "nonmatching Decimal candidate did not fail module assertion: {outcome:?}"
    );
    for (table, id) in [("Invoice", 1), ("Invoice", 2), ("Payment", 1), ("Payment", 2)] {
        assert_eq!(
            runtime.committed_row(table, &Value::int(id.into())),
            None,
            "failed module assertion published candidate {table} row {id}"
        );
    }
}

#[test]
fn table_assertions_precede_module_assertions_and_abort_the_candidate_database() {
    let mut runtime = TransactionalEvaluator::new("parent", Limits::default());
    let unit = SourceUnit {
        fixture_id: "assertion-order".into(),
        source_id: "assertion-order.orna".into(),
        parse_as: "module_unit".into(),
        source: r#"
            pub table Book(id: Int) {
                title: Str,
                assert every(book => book.title != "");
            }
            pub table Loan(id: Int) { book_id: Int, }
            assert every(Loan, loan => exists(Book, book => book.id == loan.book_id));
            fn parent() {
                Book.insert({ id: 7, title: "" });
                Loan.insert({ id: 1, book_id: 8 });
            }
        "#
        .into(),
    };

    let outcome = runtime.execute_source(&unit);

    assert!(matches!(
        outcome,
        StageOutcome::Failed(ref diagnostic) if diagnostic.code() == "ORNA-EVAL-TABLE-ASSERT"
    ));
    assert_eq!(runtime.committed_row("Book", &Value::int(7.into())), None);
    assert_eq!(runtime.committed_row("Loan", &Value::int(1.into())), None);
}

#[test]
fn ordinary_function_quantifiers_use_empty_identities_and_candidate_rows() {
    let unit = quantifier_source(
        "txn-quantifier-empty-ryw",
        r#"
            fn every_note() = every(Note, note => note.text == "ok");
            fn exists_note() = exists(Note, note => note.id == 2);
        "#,
        r#"
            assert every_note() == true;
            assert exists_note() == false;
            Note.insert({ id: 2, text: "ok" });
            Note.insert({ id: 1, text: "ok" });
            assert every_note() == true;
            assert exists_note() == true;
        "#,
    );
    let mut runtime = TransactionalEvaluator::new("parent", Limits::default());

    let outcome = runtime.execute_source(&unit);

    assert!(matches!(outcome, StageOutcome::Passed), "{outcome:?}");
    for id in [1, 2] {
        assert!(
            runtime
                .committed_row("Note", &Value::int(id.into()))
                .is_some(),
            "candidate row {id} was not published"
        );
    }
}

#[test]
fn ordinary_function_quantifiers_short_circuit_in_canonical_key_order() {
    let unit = quantifier_source(
        "txn-quantifier-order",
        r#"
            fn all_first() = every(Note, note =>
                if note.id == 1 { false } else { 1 / 0 == 0 }
            );
            fn any_first() = exists(Note, note =>
                if note.id == 1 { true } else { 1 / 0 == 0 }
            );
        "#,
        r#"
            Note.insert({ id: 2, text: "later" });
            Note.insert({ id: 1, text: "decisive" });
            assert all_first() == false;
            assert any_first() == true;
        "#,
    );
    let mut runtime = TransactionalEvaluator::new("parent", Limits::default());

    let outcome = runtime.execute_source(&unit);

    assert!(
        matches!(outcome, StageOutcome::Passed),
        "a later callback failure must be skipped after the canonical decisive row: {outcome:?}"
    );
    assert!(runtime.committed_row("Note", &Value::int(1.into())).is_some());
    assert!(runtime.committed_row("Note", &Value::int(2.into())).is_some());
}

#[test]
fn ordinary_function_quantifier_callback_failures_propagate_and_roll_back() {
    for (operator, fixture_id) in [
        ("every", "txn-quantifier-every-failure"),
        ("exists", "txn-quantifier-exists-failure"),
    ] {
        let definitions =
            format!(r#"fn failing() = {operator}(Note, note => 1 / 0 == 0);"#);
        let unit = quantifier_source(
            fixture_id,
            &definitions,
            r#"Note.insert({ id: 1, text: "bad" }); failing();"#,
        );
        let mut runtime = TransactionalEvaluator::new("parent", Limits::default());

        let outcome = runtime.execute_source(&unit);

        assert!(
            matches!(
                outcome,
                StageOutcome::Failed(ref diagnostic)
                    if diagnostic.code() == "ORNA-EVAL-DIVIDE-BY-ZERO"
            ),
            "{operator} callback failure escaped as {outcome:?}"
        );
        assert_eq!(
            runtime.committed_row("Note", &Value::int(1.into())),
            None,
            "{operator} callback failure published a candidate row"
        );
    }
}

#[test]
fn ordinary_function_quantifiers_share_collection_limit_and_roll_back() {
    let limits = Limits {
        max_collection_items: 1,
        ..Limits::default()
    };

    for (operator, fixture_id) in [
        ("every", "txn-quantifier-every-limit"),
        ("exists", "txn-quantifier-exists-limit"),
    ] {
        let definitions =
            format!(r#"fn bounded() = {operator}(Note, note => note.id > 0);"#);
        let unit = quantifier_source(
            fixture_id,
            &definitions,
            r#"Note.insert({ id: 2, text: "two" }); Note.insert({ id: 1, text: "one" }); bounded();"#,
        );
        let mut runtime = TransactionalEvaluator::new("parent", limits);

        let outcome = runtime.execute_source(&unit);

        assert!(
            matches!(
                outcome,
                StageOutcome::Failed(ref diagnostic) if diagnostic.code() == "ORNA-EVAL-LIMIT"
            ),
            "{operator} collection limit did not surface: {outcome:?}"
        );
        for id in [1, 2] {
            assert_eq!(
                runtime.committed_row("Note", &Value::int(id.into())),
                None,
                "{operator} collection limit published candidate row {id}"
            );
        }
    }
}
fn decimal_quantifier_source(fixture_id: &str, definitions: &str, parent_body: &str) -> SourceUnit {
    SourceUnit {
        fixture_id: fixture_id.into(),
        source_id: format!("{fixture_id}.orna"),
        parse_as: "module_unit".into(),
        source: format!(
            "pub table Reading(id: Int) {{ value: Decimal, label: Str, }} {definitions} fn parent() {{ {parent_body} }}"
        ),
    }
}

fn decimal_key_quantifier_source(
    fixture_id: &str,
    definitions: &str,
    parent_body: &str,
) -> SourceUnit {
    SourceUnit {
        fixture_id: fixture_id.into(),
        source_id: format!("{fixture_id}.orna"),
        parse_as: "module_unit".into(),
        source: format!(
            "pub table Reading(value: Decimal) {{ label: Str, }} {definitions} fn parent() {{ {parent_body} }}"
        ),
    }
}

#[test]
fn ordinary_function_quantifiers_compare_decimal_body_fields_without_scale() {
    let unit = decimal_quantifier_source(
        "txn-decimal-quantifier-body",
        r#"
            fn every_match() = every(Reading, reading =>
                reading.value == 18.25 && reading.label != ""
            );
            fn exists_match() = exists(Reading, reading => reading.value == 18.2500);
        "#,
        r#"
            assert every_match() == true;
            assert exists_match() == false;
            Reading.insert({ id: 2, value: 2.0, label: "other" });
            Reading.insert({ id: 1, value: 18.2500, label: "match" });
            assert every_match() == false;
            assert exists_match() == true;
        "#,
    );
    let mut runtime = TransactionalEvaluator::new("parent", Limits::default());

    let outcome = runtime.execute_source(&unit);

    assert!(matches!(&outcome, StageOutcome::Passed), "{outcome:?}");
    for id in [1, 2] {
        assert!(
            runtime
                .committed_row("Reading", &Value::int(id.into()))
                .is_some(),
            "Decimal body candidate row {id} was not published"
        );
    }
}

#[test]
fn ordinary_function_decimal_quantifiers_short_circuit_after_scale_insensitive_match() {
    let unit = decimal_quantifier_source(
        "txn-decimal-quantifier-short-circuit",
        r#"
            fn all_first() = every(Reading, reading =>
                if reading.value == 18.25 { false } else { 1 / 0 == 0 }
            );
            fn any_first() = exists(Reading, reading =>
                if reading.value == 18.2500 { true } else { 1 / 0 == 0 }
            );
        "#,
        r#"
            Reading.insert({ id: 2, value: 2.0, label: "later" });
            Reading.insert({ id: 1, value: 18.2500, label: "decisive" });
            assert all_first() == false;
            assert any_first() == true;
        "#,
    );
    let mut runtime = TransactionalEvaluator::new("parent", Limits::default());

    let outcome = runtime.execute_source(&unit);

    assert!(
        matches!(&outcome, StageOutcome::Passed),
        "a later callback failure must be skipped after the scale-alias decisive row: {outcome:?}"
    );
    for id in [1, 2] {
        assert!(
            runtime
                .committed_row("Reading", &Value::int(id.into()))
                .is_some(),
            "short-circuit candidate row {id} was not published"
        );
    }
}

#[test]
fn ordinary_function_decimal_quantifier_false_results_roll_back_candidate_rows() {
    let mut every_runtime = TransactionalEvaluator::new("parent", Limits::default());
    let every_unit = decimal_quantifier_source(
        "txn-decimal-quantifier-every-rollback",
        "fn every_match() = every(Reading, reading => reading.value == 18.25);",
        r#"
            Reading.insert({ id: 2, value: 2.0, label: "other" });
            Reading.insert({ id: 1, value: 18.2500, label: "match" });
            assert every_match() == true;
        "#,
    );
    let every_outcome = every_runtime.execute_source(&every_unit);
    assert!(
        matches!(&every_outcome, StageOutcome::Failed(diagnostic)
            if diagnostic.code() == "ORNA-EVAL-ASSERT"),
        "false every result did not fail its assertion: {every_outcome:?}"
    );
    for id in [1, 2] {
        assert_eq!(
            every_runtime.committed_row("Reading", &Value::int(id.into())),
            None,
            "false every result published candidate row {id}"
        );
    }

    let mut exists_runtime = TransactionalEvaluator::new("parent", Limits::default());
    let exists_unit = decimal_quantifier_source(
        "txn-decimal-quantifier-exists-rollback",
        "fn exists_miss() = exists(Reading, reading => reading.value == 99.990);",
        r#"
            Reading.insert({ id: 1, value: 18.2500, label: "present" });
            assert exists_miss() == true;
        "#,
    );
    let exists_outcome = exists_runtime.execute_source(&exists_unit);
    assert!(
        matches!(&exists_outcome, StageOutcome::Failed(diagnostic)
            if diagnostic.code() == "ORNA-EVAL-ASSERT"),
        "false exists result did not fail its assertion: {exists_outcome:?}"
    );
    assert_eq!(
        exists_runtime.committed_row("Reading", &Value::int(1.into())),
        None,
        "false exists result published its candidate row"
    );
}

#[test]
fn ordinary_function_decimal_quantifiers_share_collection_limit_and_roll_back() {
    let limits = Limits {
        max_collection_items: 1,
        ..Limits::default()
    };
    for (operator, fixture_id) in [
        ("every", "txn-decimal-quantifier-every-limit"),
        ("exists", "txn-decimal-quantifier-exists-limit"),
    ] {
        let definitions =
            format!(r#"fn bounded() = {operator}(Reading, reading => reading.value == 18.25);"#);
        let unit = decimal_quantifier_source(
            fixture_id,
            &definitions,
            r#"
                Reading.insert({ id: 2, value: 2.0, label: "other" });
                Reading.insert({ id: 1, value: 18.2500, label: "match" });
                bounded();
            "#,
        );
        let mut runtime = TransactionalEvaluator::new("parent", limits);

        let outcome = runtime.execute_source(&unit);

        assert!(
            matches!(&outcome, StageOutcome::Failed(diagnostic)
                if diagnostic.code() == "ORNA-EVAL-LIMIT"),
            "{operator} Decimal quantifier collection limit did not surface: {outcome:?}"
        );
        for id in [1, 2] {
            assert_eq!(
                runtime.committed_row("Reading", &Value::int(id.into())),
                None,
                "bounded {operator} Decimal quantifier published candidate row {id}"
            );
        }
    }
}

#[test]
fn ordinary_function_quantifiers_compare_decimal_primary_keys_without_scale() {
    let unit = decimal_key_quantifier_source(
        "txn-decimal-key-quantifier",
        r#"
            fn every_match() = every(Reading, reading => reading.value == 18.25);
            fn exists_match() = exists(Reading, reading => reading.value == 18.2500);
            fn every_miss() = every(Reading, reading => reading.value == 99.990);
            fn exists_miss() = exists(Reading, reading => reading.value == 99.990);
        "#,
        r#"
            Reading.insert({ value: 18.2500, label: "match" });
            assert every_match() == true;
            assert exists_match() == true;
            assert every_miss() == false;
            assert exists_miss() == false;
        "#,
    );
    let mut runtime = TransactionalEvaluator::new("parent", Limits::default());

    let outcome = runtime.execute_source(&unit);

    assert!(matches!(&outcome, StageOutcome::Passed), "{outcome:?}");
    let key = Value::decimal((182500).into(), (-4).into()).expect("canonical Decimal key");
    assert!(
        runtime.committed_row("Reading", &key).is_some(),
        "Decimal-key candidate row was not published"
    );
}

fn decimal_filtered_first_source(parent_body: &str) -> SourceUnit {
    SourceUnit {
        fixture_id: "txn-decimal-filtered-first".into(),
        source_id: "txn-decimal-filtered-first.orna".into(),
        parse_as: "module_unit".into(),
        source: format!(
            "pub table Reading(id: Int) {{ value: Decimal, }} fn parent() {{ {parent_body} }}"
        ),
    }
}
fn decimal_filtered_relation_source(parent_body: &str) -> SourceUnit {
    SourceUnit {
        fixture_id: "txn-decimal-filtered-relation".into(),
        source_id: "txn-decimal-filtered-relation.orna".into(),
        parse_as: "module_unit".into(),
        source: format!(
            "pub table Reading(id: Int) {{ value: Decimal, }} fn parent() {{ {parent_body} }}"
        ),
    }
}
fn decimal_distinct_source(parent_body: &str) -> SourceUnit {
    SourceUnit {
        fixture_id: "txn-decimal-distinct".into(),
        source_id: "txn-decimal-distinct.orna".into(),
        parse_as: "module_unit".into(),
        source: format!(
            "pub table Reading(id: Int) {{ value: Decimal, }} fn unique_values(): [Decimal] = [18.2500, 2.0, 18.25, 3.000] | distinct(); fn parent() {{ {parent_body} }}"
        ),
    }
}


#[test]
fn parsed_decimal_relation_distinct_source_is_rejected_before_execution() {
    let mut runtime = TransactionalEvaluator::new("parent", Limits::default());
    let outcome = runtime.execute_source(&decimal_distinct_source(
        r#"
            Reading.insert({ id: 1, value: 18.2500 });
            Reading.insert({ id: 2, value: 2.0 });
            Reading.insert({ id: 3, value: 18.25 });
            Reading.insert({ id: 4, value: 3.000 });
            assert unique_values() | count() == 3;
        "#,
    ));

    assert!(matches!(
        &outcome,
        StageOutcome::Failed(diagnostic) if diagnostic.code() == "ORNA-S012-UNRESOLVED"
    ));
    for id in 1..=4 {
        assert_eq!(
            runtime.committed_row("Reading", &Value::int(id.into())),
            None,
            "unresolved distinct source published candidate row {id}"
        );
    }
}

#[test]
fn parsed_decimal_relation_distinct_source_failure_is_preempted_by_unresolved_distinct() {
    let mut runtime = TransactionalEvaluator::new("parent", Limits::default());
    let outcome = runtime.execute_source(&decimal_distinct_source(
        r#"
            Reading.insert({ id: 1, value: 18.2500 });
            Reading.insert({ id: 2, value: 2.0 });
            Reading.insert({ id: 3, value: 18.25 });
            Reading.insert({ id: 4, value: 3.000 });
            assert unique_values() | count() == 3;
            assert false;
        "#,
    ));

    assert!(matches!(
        &outcome,
        StageOutcome::Failed(diagnostic) if diagnostic.code() == "ORNA-S012-UNRESOLVED"
    ));
    for id in 1..=4 {
        assert_eq!(
            runtime.committed_row("Reading", &Value::int(id.into())),
            None,
            "unresolved distinct source published candidate row {id}"
        );
    }
}

#[test]
fn parsed_decimal_reversed_filtered_count_is_scale_insensitive_and_canonical() {
    let mut runtime = TransactionalEvaluator::new("parent", Limits::default());
    let outcome = runtime.execute_source(&decimal_filtered_relation_source(
        r#"
            Reading.insert({ id: 2, value: 18.2500 });
            Reading.insert({ id: 1, value: 18.25 });
            Reading.insert({ id: 3, value: 2.0 });
            assert (Reading | filter(reading => 18.250 == reading.value) | count()) == 2;
            assert (Reading | filter(reading => 99.990 == reading.value) | count()) == 0;
        "#,
    ));

    assert!(matches!(&outcome, StageOutcome::Passed), "{outcome:?}");
    for id in [1, 2, 3] {
        assert!(
            runtime
                .committed_row("Reading", &Value::int(id.into()))
                .is_some(),
            "canonical reverse-scale count did not publish row {id}"
        );
    }
}

#[test]
fn parsed_decimal_reversed_filtered_count_observes_candidate_writes_and_rolls_back() {
    let mut committed = TransactionalEvaluator::new("parent", Limits::default());
    let committed_outcome = committed.execute_source(&decimal_filtered_relation_source(
        r#"
            Reading.insert({ id: 2, value: 18.2500 });
            Reading.insert({ id: 1, value: 2.0 });
            assert (Reading | filter(reading => 18.25 == reading.value) | count()) == 1;
        "#,
    ));
    assert!(
        matches!(&committed_outcome, StageOutcome::Passed),
        "{committed_outcome:?}"
    );
    for id in [1, 2] {
        assert!(
            committed
                .committed_row("Reading", &Value::int(id.into()))
                .is_some(),
            "candidate reverse-scale count row {id} was not published"
        );
    }

    let mut rolled_back = TransactionalEvaluator::new("parent", Limits::default());
    let failed = rolled_back.execute_source(&decimal_filtered_relation_source(
        r#"
            Reading.insert({ id: 2, value: 18.2500 });
            assert (Reading | filter(reading => 18.25 == reading.value) | count()) == 1;
            assert false;
        "#,
    ));
    assert!(matches!(
        &failed,
        StageOutcome::Failed(diagnostic) if diagnostic.code() == "ORNA-EVAL-ASSERT"
    ));
    assert_eq!(
        rolled_back.committed_row("Reading", &Value::int(2.into())),
        None,
        "reverse-scale count candidate escaped rollback"
    );
}

#[test]
fn parsed_decimal_reversed_filtered_one_is_scale_insensitive_and_reads_candidate_rows() {
    let mut runtime = TransactionalEvaluator::new("parent", Limits::default());
    let outcome = runtime.execute_source(&decimal_filtered_relation_source(
        r#"
            Reading.insert({ id: 2, value: 18.2500 });
            Reading.insert({ id: 1, value: 2.0 });
            assert (Reading | filter(reading => 18.25 == reading.value) | one()).id == 2;
        "#,
    ));

    assert!(matches!(&outcome, StageOutcome::Passed), "{outcome:?}");
    for id in [1, 2] {
        assert!(
            runtime
                .committed_row("Reading", &Value::int(id.into()))
                .is_some(),
            "candidate reverse-scale one row {id} was not published"
        );
    }
}

#[test]
fn parsed_decimal_reversed_filtered_one_reports_zero_and_rolls_back() {
    let mut runtime = TransactionalEvaluator::new("parent", Limits::default());
    let outcome = runtime.execute_source(&decimal_filtered_relation_source(
        r#"
            Reading.insert({ id: 1, value: 18.2500 });
            (Reading | filter(reading => 99.990 == reading.value) | one());
        "#,
    ));

    assert!(matches!(
        &outcome,
        StageOutcome::Failed(diagnostic)
            if diagnostic.code() == "ORNA-EVAL-RELATION-ONE-ZERO"
    ));
    assert_eq!(
        runtime.committed_row("Reading", &Value::int(1.into())),
        None,
        "zero-match reverse-scale one published a candidate row"
    );
}

#[test]
fn parsed_decimal_reversed_filtered_one_reports_multiple_and_rolls_back() {
    let mut runtime = TransactionalEvaluator::new("parent", Limits::default());
    let outcome = runtime.execute_source(&decimal_filtered_relation_source(
        r#"
            Reading.insert({ id: 2, value: 18.2500 });
            Reading.insert({ id: 1, value: 18.25 });
            (Reading | filter(reading => 18.250 == reading.value) | one());
        "#,
    ));

    assert!(matches!(
        &outcome,
        StageOutcome::Failed(diagnostic)
            if diagnostic.code() == "ORNA-EVAL-RELATION-ONE-MULTIPLE"
    ));
    for id in [1, 2] {
        assert_eq!(
            runtime.committed_row("Reading", &Value::int(id.into())),
            None,
            "multiple-match reverse-scale one published candidate row {id}"
        );
    }
}

#[test]
fn parsed_decimal_reversed_filtered_count_and_one_respect_shared_step_budget() {
    for (operation, query) in [
        (
            "count",
            r#"assert (Reading | filter(reading => 18.25 == reading.value) | count()) == 1;"#,
        ),
        (
            "one",
            r#"(Reading | filter(reading => 18.25 == reading.value) | one());"#,
        ),
    ] {
        let limits = Limits {
            max_steps: 10,
            ..Limits::default()
        };
        let mut runtime = TransactionalEvaluator::new("parent", limits);
        let outcome = runtime.execute_source(&decimal_filtered_relation_source(&format!(
            r#"
                Reading.insert({{ id: 1, value: 2.0 }});
                Reading.insert({{ id: 2, value: 18.2500 }});
                {query}
            "#
        )));

        assert!(
            matches!(&outcome, StageOutcome::Failed(diagnostic) if diagnostic.code() == "ORNA-EVAL-LIMIT"),
            "{operation} reverse-scale filter did not surface the step limit: {outcome:?}"
        );
        for id in [1, 2] {
            assert_eq!(
                runtime.committed_row("Reading", &Value::int(id.into())),
                None,
                "bounded reverse-scale {operation} execution published row {id}"
            );
        }
    }
}

#[test]
fn parsed_decimal_filtered_first_is_scale_insensitive_and_canonical() {
    let mut runtime = TransactionalEvaluator::new("parent", Limits::default());
    let outcome = runtime.execute_source(&decimal_filtered_first_source(
        r#"
            Reading.insert({ id: 2, value: 1.2000 });
            Reading.insert({ id: 1, value: 1.20 });
            Reading.insert({ id: 3, value: 2.0 });
            assert ((Reading | filter(reading => reading.value == 1.200) | first())
                ?? Reading.insert({ id: 99, value: 0.0 })).id == 1;
        "#,
    ));

    assert!(matches!(outcome, StageOutcome::Passed), "{outcome:?}");
    assert!(runtime.committed_row("Reading", &Value::int(1.into())).is_some());
    assert!(runtime.committed_row("Reading", &Value::int(2.into())).is_some());
    assert_eq!(
        runtime.committed_row("Reading", &Value::int(99.into())),
        None,
        "non-empty filtered relation evaluated its fallback"
    );
}

#[test]
fn parsed_decimal_filtered_first_returns_null_for_no_match() {
    let mut runtime = TransactionalEvaluator::new("parent", Limits::default());
    let outcome = runtime.execute_source(&decimal_filtered_first_source(
        r#"
            Reading.insert({ id: 1, value: 1.20 });
            assert ((Reading | filter(reading => reading.value == 9.99) | first())
                ?? Reading.insert({ id: 99, value: 0.0 })).id == 99;
        "#,
    ));

    assert!(matches!(outcome, StageOutcome::Passed), "{outcome:?}");
    assert!(runtime.committed_row("Reading", &Value::int(1.into())).is_some());
    assert!(runtime.committed_row("Reading", &Value::int(99.into())).is_some());
}

#[test]
fn parsed_decimal_filtered_first_observes_candidate_writes_and_rolls_back_on_failure() {
    let mut committed = TransactionalEvaluator::new("parent", Limits::default());
    let committed_outcome = committed.execute_source(&decimal_filtered_first_source(
        r#"
            Reading.insert({ id: 2, value: 2.0 });
            Reading.insert({ id: 1, value: 1.2000 });
            assert ((Reading | filter(reading => reading.value == 1.20) | first())
                ?? Reading.insert({ id: 99, value: 0.0 })).id == 1;
        "#,
    ));
    assert!(
        matches!(committed_outcome, StageOutcome::Passed),
        "{committed_outcome:?}"
    );
    for id in [1, 2] {
        assert!(
            committed
                .committed_row("Reading", &Value::int(id.into()))
                .is_some(),
            "candidate Decimal row {id} was not published"
        );
    }

    let mut rolled_back = TransactionalEvaluator::new("parent", Limits::default());
    let failed = rolled_back.execute_source(&decimal_filtered_first_source(
        r#"
            Reading.insert({ id: 2, value: 2.0 });
            Reading.insert({ id: 1, value: 1.2000 });
            assert ((Reading | filter(reading => reading.value == 1.20) | first())
                ?? Reading.insert({ id: 99, value: 0.0 })).id == 1;
            assert false;
        "#,
    ));
    assert!(matches!(
        &failed,
        StageOutcome::Failed(diagnostic) if diagnostic.code() == "ORNA-EVAL-ASSERT"
    ));
    for id in [1, 2, 99] {
        assert_eq!(
            rolled_back.committed_row("Reading", &Value::int(id.into())),
            None,
            "row {id} escaped filtered-first rollback"
        );
    }
}

#[test]
fn parsed_decimal_filtered_first_respects_shared_step_budget() {
    let limits = Limits {
        max_steps: 10,
        ..Limits::default()
    };
    let mut runtime = TransactionalEvaluator::new("parent", limits);
    let outcome = runtime.execute_source(&decimal_filtered_first_source(
        r#"
            Reading.insert({ id: 1, value: 1.20 });
            Reading.insert({ id: 2, value: 2.0 });
            (Reading | filter(reading => reading.value == 2.00) | first());
        "#,
    ));

    assert!(
        matches!(&outcome, StageOutcome::Failed(diagnostic) if diagnostic.code() == "ORNA-EVAL-LIMIT"),
        "{outcome:?}"
    );
    for id in [1, 2] {
        assert_eq!(
            runtime.committed_row("Reading", &Value::int(id.into())),
            None,
            "bounded filtered-first execution published row {id}"
        );
    }
}

#[test]
fn parsed_decimal_reversed_filtered_first_is_scale_insensitive_and_canonical() {
    let mut runtime = TransactionalEvaluator::new("parent", Limits::default());
    let outcome = runtime.execute_source(&decimal_filtered_first_source(
        r#"
            Reading.insert({ id: 2, value: 1.2000 });
            Reading.insert({ id: 1, value: 1.20 });
            Reading.insert({ id: 3, value: 2.0 });
            assert ((Reading | filter(reading => 1.200 == reading.value) | first())
                ?? Reading.insert({ id: 99, value: 0.0 })).id == 1;
        "#,
    ));

    assert!(matches!(outcome, StageOutcome::Passed), "{outcome:?}");
    assert!(runtime.committed_row("Reading", &Value::int(1.into())).is_some());
    assert!(runtime.committed_row("Reading", &Value::int(2.into())).is_some());
    assert_eq!(
        runtime.committed_row("Reading", &Value::int(99.into())),
        None,
        "non-empty reversed filtered relation evaluated its fallback"
    );
}

#[test]
fn parsed_decimal_reversed_filtered_first_returns_fallback_for_no_match() {
    let mut runtime = TransactionalEvaluator::new("parent", Limits::default());
    let outcome = runtime.execute_source(&decimal_filtered_first_source(
        r#"
            Reading.insert({ id: 1, value: 1.20 });
            assert ((Reading | filter(reading => 9.990 == reading.value) | first())
                ?? Reading.insert({ id: 99, value: 0.0 })).id == 99;
        "#,
    ));

    assert!(matches!(outcome, StageOutcome::Passed), "{outcome:?}");
    assert!(runtime.committed_row("Reading", &Value::int(1.into())).is_some());
    assert!(runtime.committed_row("Reading", &Value::int(99.into())).is_some());
}

#[test]
fn parsed_decimal_reversed_filtered_first_observes_candidate_writes_and_rolls_back_on_failure() {
    let mut committed = TransactionalEvaluator::new("parent", Limits::default());
    let committed_outcome = committed.execute_source(&decimal_filtered_first_source(
        r#"
            Reading.insert({ id: 2, value: 2.0 });
            Reading.insert({ id: 1, value: 1.2000 });
            assert ((Reading | filter(reading => 1.20 == reading.value) | first())
                ?? Reading.insert({ id: 99, value: 0.0 })).id == 1;
        "#,
    ));
    assert!(
        matches!(committed_outcome, StageOutcome::Passed),
        "{committed_outcome:?}"
    );
    for id in [1, 2] {
        assert!(
            committed
                .committed_row("Reading", &Value::int(id.into()))
                .is_some(),
            "candidate Decimal row {id} was not published"
        );
    }

    let mut rolled_back = TransactionalEvaluator::new("parent", Limits::default());
    let failed = rolled_back.execute_source(&decimal_filtered_first_source(
        r#"
            Reading.insert({ id: 2, value: 2.0 });
            Reading.insert({ id: 1, value: 1.2000 });
            assert ((Reading | filter(reading => 1.20 == reading.value) | first())
                ?? Reading.insert({ id: 99, value: 0.0 })).id == 1;
            assert false;
        "#,
    ));
    assert!(matches!(
        &failed,
        StageOutcome::Failed(diagnostic) if diagnostic.code() == "ORNA-EVAL-ASSERT"
    ));
    for id in [1, 2, 99] {
        assert_eq!(
            rolled_back.committed_row("Reading", &Value::int(id.into())),
            None,
            "row {id} escaped reversed filtered-first rollback"
        );
    }
}

#[test]
fn parsed_decimal_reversed_filtered_first_respects_shared_step_budget() {
    let limits = Limits {
        max_steps: 10,
        ..Limits::default()
    };
    let mut runtime = TransactionalEvaluator::new("parent", limits);
    let outcome = runtime.execute_source(&decimal_filtered_first_source(
        r#"
            Reading.insert({ id: 1, value: 1.20 });
            Reading.insert({ id: 2, value: 2.0 });
            (Reading | filter(reading => 2.00 == reading.value) | first());
        "#,
    ));

    assert!(
        matches!(&outcome, StageOutcome::Failed(diagnostic) if diagnostic.code() == "ORNA-EVAL-LIMIT"),
        "{outcome:?}"
    );
    for id in [1, 2] {
        assert_eq!(
            runtime.committed_row("Reading", &Value::int(id.into())),
            None,
            "bounded reversed filtered-first execution published row {id}"
        );
    }
}
fn decimal_key_relation_source(parent_body: &str) -> SourceUnit {
    SourceUnit {
        fixture_id: "txn-decimal-key-relation".into(),
        source_id: "txn-decimal-key-relation.orna".into(),
        parse_as: "module_unit".into(),
        source: format!(
            "pub table Reading(value: Decimal) {{ label: Str, }} fn parent() {{ {parent_body} }}"
        ),
    }
}

#[test]
fn parsed_decimal_primary_key_reversed_filtered_count_is_scale_insensitive() {
    let mut runtime = TransactionalEvaluator::new("parent", Limits::default());
    let outcome = runtime.execute_source(&decimal_key_relation_source(
        r#"
            Reading.insert({ value: 18.2500, label: "match" });
            Reading.insert({ value: 2.0, label: "other" });
            assert (Reading | filter(reading => 18.250 == reading.value) | count()) == 1;
            assert (Reading | filter(reading => 99.990 == reading.value) | count()) == 0;
        "#,
    ));

    assert!(matches!(&outcome, StageOutcome::Passed), "{outcome:?}");
}

#[test]
fn parsed_decimal_primary_key_reversed_filtered_one_is_scale_insensitive() {
    let mut runtime = TransactionalEvaluator::new("parent", Limits::default());
    let outcome = runtime.execute_source(&decimal_key_relation_source(
        r#"
            Reading.insert({ value: 2.0, label: "other" });
            Reading.insert({ value: 18.2500, label: "match" });
            assert (Reading | filter(reading => 18.25 == reading.value) | one()).label == "match";
        "#,
    ));

    assert!(matches!(&outcome, StageOutcome::Passed), "{outcome:?}");
}

#[test]
fn parsed_decimal_primary_key_reversed_filtered_one_reports_no_match() {
    let mut runtime = TransactionalEvaluator::new("parent", Limits::default());
    let outcome = runtime.execute_source(&decimal_key_relation_source(
        r#"
            Reading.insert({ value: 18.2500, label: "present" });
            (Reading | filter(reading => 99.990 == reading.value) | one());
        "#,
    ));

    assert!(matches!(
        &outcome,
        StageOutcome::Failed(diagnostic)
            if diagnostic.code() == "ORNA-EVAL-RELATION-ONE-ZERO"
    ));
}


#[test]
fn parsed_decimal_primary_key_reversed_noncanonical_predicates_use_generic_evaluation() {
    let mut runtime = TransactionalEvaluator::new("parent", Limits::default());
    let outcome = runtime.execute_source(&decimal_key_relation_source(
        r#"
            Reading.insert({ value: 18.25, label: "match" });
            Reading.insert({ value: 2.0, label: "other" });
            assert (Reading | filter(reading => 18.25 == reading.value + 0.0) | count()) == 1;
            assert (Reading | filter(reading => 18.25 != reading.value) | count()) == 1;
        "#,
    ));

    assert!(matches!(&outcome, StageOutcome::Passed), "{outcome:?}");
}
#[test]
fn parsed_decimal_primary_key_first_and_bounded_window_use_canonical_order() {
    let mut runtime = TransactionalEvaluator::new("parent", Limits::default());
    let outcome = runtime.execute_source(&decimal_key_relation_source(
        r#"
            Reading.insert({ value: -2.5000, label: "negative-low" });
            Reading.insert({ value: 0.000, label: "zero" });
            Reading.insert({ value: 1.20, label: "positive-low" });
            assert (Reading | take(1) | one()).label == "negative-low";
            assert (Reading | window(2) | count()) == 2;
        "#,
    ));

    assert!(
        matches!(&outcome, StageOutcome::Passed),
        "canonical Decimal-key first failed: {outcome:?}"
    );
}

#[test]
fn parsed_decimal_primary_key_scale_alias_is_rejected_without_publication() {
    let mut runtime = TransactionalEvaluator::new("parent", Limits::default());
    let outcome = runtime.execute_source(&decimal_key_relation_source(
        r#"
            Reading.insert({ value: -1.20, label: "canonical" });
            Reading.insert({ value: -1.2000, label: "scale-alias" });
        "#,
    ));

    assert!(
        matches!(
            &outcome,
            StageOutcome::Failed(diagnostic)
                if diagnostic.code() == "ORNA-EVAL-TABLE-DUPLICATE"
        ),
        "scale-alias key insertion did not fail as a duplicate: {outcome:?}"
    );
    let canonical_key =
        Value::decimal((-120).into(), (-2).into()).expect("canonical Decimal primary key");
    assert_eq!(
        runtime.committed_row("Reading", &canonical_key),
        None,
        "duplicate Decimal-key activation published a row"
    );
}
#[test]
fn parsed_decimal_rekey_accepts_scale_insensitive_old_key_and_moves_atomically() {
    let mut runtime = TransactionalEvaluator::new("parent", Limits::default());
    let outcome = runtime.execute_source(&decimal_key_relation_source(
        r#"
            Reading.insert({ value: 1.2000, label: "before" });
            assert (Reading | filter(reading => reading.value == 1.20) | one()).label == "before";
            Reading.rekey(1.20, 2.5000);
            assert (Reading | filter(reading => reading.value == 2.50) | one()).label == "before";
            assert (Reading | take(1) | one()).value == 2.5;
        "#,
    ));

    assert!(matches!(&outcome, StageOutcome::Passed), "{outcome:?}");
    let old_key = Value::decimal(12.into(), (-1).into()).expect("canonical old Decimal key");
    let new_key = Value::decimal(25.into(), (-1).into()).expect("canonical new Decimal key");
    assert_eq!(runtime.committed_row("Reading", &old_key), None);
    assert!(runtime.committed_row("Reading", &new_key).is_some());
}

#[test]
fn parsed_decimal_rekey_treats_scale_aliases_as_the_same_old_and_new_identity() {
    let mut runtime = TransactionalEvaluator::new("parent", Limits::default());
    let outcome = runtime.execute_source(&decimal_key_relation_source(
        r#"
            Reading.insert({ value: 1.2000, label: "before" });
            Reading.rekey(1.20, 1.200);
        "#,
    ));

    assert!(
        matches!(
            &outcome,
            StageOutcome::Failed(diagnostic)
                if diagnostic.code() == "ORNA-EVAL-TABLE-DUPLICATE"
        ),
        "scale-alias rekey did not preserve key identity: {outcome:?}"
    );
    let key = Value::decimal(12.into(), (-1).into()).expect("canonical Decimal key");
    assert_eq!(runtime.committed_row("Reading", &key), None);
}

#[test]
fn parsed_decimal_rekey_absent_source_rolls_back_candidate_rows() {
    let mut runtime = TransactionalEvaluator::new("parent", Limits::default());
    let outcome = runtime.execute_source(&decimal_key_relation_source(
        r#"
            Reading.insert({ value: 1.20, label: "candidate" });
            Reading.rekey(9.990, 2.0);
        "#,
    ));

    assert!(
        matches!(
            &outcome,
            StageOutcome::Failed(diagnostic)
                if diagnostic.code() == "ORNA-EVAL-TABLE-MISSING"
        ),
        "absent Decimal source did not fail with missing-row diagnostic: {outcome:?}"
    );
    for key in [
        Value::decimal(12.into(), (-1).into()).expect("canonical inserted key"),
        Value::decimal(2.into(), 0.into()).expect("canonical destination key"),
    ] {
        assert_eq!(
            runtime.committed_row("Reading", &key),
            None,
            "absent-source failure published a candidate row"
        );
    }
}

#[test]
fn parsed_decimal_rekey_destination_collision_rolls_back_all_candidate_rows() {
    let mut runtime = TransactionalEvaluator::new("parent", Limits::default());
    let outcome = runtime.execute_source(&decimal_key_relation_source(
        r#"
            Reading.insert({ value: 1.20, label: "source" });
            Reading.insert({ value: 2.000, label: "destination" });
            Reading.rekey(1.2000, 2.0);
        "#,
    ));

    assert!(
        matches!(
            &outcome,
            StageOutcome::Failed(diagnostic)
                if diagnostic.code() == "ORNA-EVAL-TABLE-DUPLICATE"
        ),
        "Decimal destination collision did not fail as duplicate: {outcome:?}"
    );
    for key in [
        Value::decimal(12.into(), (-1).into()).expect("canonical source key"),
        Value::decimal(2.into(), 0.into()).expect("canonical destination key"),
    ] {
        assert_eq!(
            runtime.committed_row("Reading", &key),
            None,
            "destination collision published a candidate row"
        );
    }
}

#[test]
fn parsed_decimal_rekey_invalid_key_rolls_back_candidate_rows() {
    let mut runtime = TransactionalEvaluator::new("parent", Limits::default());
    let outcome = runtime.execute_source(&decimal_key_relation_source(
        r#"
            Reading.insert({ value: 1.20, label: "candidate" });
            Reading.rekey(1.200, "not-a-decimal");
        "#,
    ));

    assert!(
        matches!(
            &outcome,
            StageOutcome::Failed(diagnostic) if diagnostic.code() == "ORNA-S021-TYPE"
        ),
        "invalid Decimal key did not fail during source admission: {outcome:?}"
    );
    let key = Value::decimal(12.into(), (-1).into()).expect("canonical inserted key");
    assert_eq!(
        runtime.committed_row("Reading", &key),
        None,
        "invalid Decimal key published a candidate row"
    );
}
#[test]
fn parsed_decimal_primary_key_delete_selects_scale_insensitive_candidate_and_is_visible() {
    let mut runtime = TransactionalEvaluator::new("parent", Limits::default());
    let outcome = runtime.execute_source(&decimal_key_relation_source(
        r#"
            Reading.insert({ value: 1.00, label: "remove" });
            Reading.insert({ value: 2.0, label: "keep" });
            Reading.delete(1.0);
            assert (Reading | filter(reading => reading.value == 1.000) | count()) == 0;
            assert (Reading | filter(reading => reading.label == "keep") | count()) == 1;
        "#,
    ));

    assert!(matches!(&outcome, StageOutcome::Passed), "{outcome:?}");
    let deleted_key = Value::decimal(1.into(), 0.into()).expect("canonical deleted Decimal key");
    let retained_key = Value::decimal(2.into(), 0.into()).expect("canonical retained Decimal key");
    assert_eq!(runtime.committed_row("Reading", &deleted_key), None);
    assert!(runtime.committed_row("Reading", &retained_key).is_some());
}

#[test]
fn parsed_decimal_primary_key_delete_rolls_back_when_a_later_failure_occurs() {
    let mut runtime = TransactionalEvaluator::new("parent", Limits::default());
    assert!(matches!(
        runtime.execute_source(&decimal_key_relation_source(
            r#"Reading.insert({ value: 1.00, label: "remove" }); Reading.insert({ value: 2.0, label: "keep" });"#,
        )),
        StageOutcome::Passed
    ));

    let outcome = runtime.execute_source(&decimal_key_relation_source(
        r#"
            Reading.delete(1.0);
            assert (Reading | filter(reading => reading.value == 1.00) | count()) == 0;
            assert false;
        "#,
    ));

    assert!(
        matches!(
            &outcome,
            StageOutcome::Failed(diagnostic) if diagnostic.code() == "ORNA-EVAL-ASSERT"
        ),
        "later source failure did not roll back Decimal delete: {outcome:?}"
    );
    let deleted_key = Value::decimal(1.into(), 0.into()).expect("canonical restored Decimal key");
    let retained_key = Value::decimal(2.into(), 0.into()).expect("canonical retained Decimal key");
    assert!(runtime.committed_row("Reading", &deleted_key).is_some());
    assert!(runtime.committed_row("Reading", &retained_key).is_some());
}

#[test]
fn parsed_decimal_primary_key_delete_absent_key_fails_and_rolls_back_candidates() {
    let mut runtime = TransactionalEvaluator::new("parent", Limits::default());
    let outcome = runtime.execute_source(&decimal_key_relation_source(
        r#"
            Reading.insert({ value: 1.00, label: "candidate" });
            Reading.delete(9.990);
        "#,
    ));

    assert!(
        matches!(
            &outcome,
            StageOutcome::Failed(diagnostic)
                if diagnostic.code() == "ORNA-EVAL-TABLE-MISSING"
        ),
        "absent Decimal delete did not fail with missing-row diagnostic: {outcome:?}"
    );
    let candidate_key = Value::decimal(1.into(), 0.into()).expect("canonical candidate Decimal key");
    assert_eq!(runtime.committed_row("Reading", &candidate_key), None);
}

#[test]
fn parsed_decimal_primary_key_delete_invalid_key_fails_and_rolls_back_candidates() {
    let mut runtime = TransactionalEvaluator::new("parent", Limits::default());
    let outcome = runtime.execute_source(&decimal_key_relation_source(
        r#"
            Reading.insert({ value: 1.00, label: "candidate" });
            Reading.delete("not-a-decimal");
        "#,
    ));

    assert!(
        matches!(
            &outcome,
            StageOutcome::Failed(diagnostic) if diagnostic.code() == "ORNA-S021-TYPE"
        ),
        "invalid Decimal delete key did not fail during source admission: {outcome:?}"
    );
    let candidate_key = Value::decimal(1.into(), 0.into()).expect("canonical candidate Decimal key");
    assert_eq!(runtime.committed_row("Reading", &candidate_key), None);
}
#[test]
fn parsed_decimal_primary_key_update_selects_scale_insensitive_source_and_mutates_non_key_field() {
    let mut runtime = TransactionalEvaluator::new("parent", Limits::default());
    let outcome = runtime.execute_source(&decimal_key_relation_source(
        r#"
            Reading.insert({ value: 1.2000, label: "before" });
            Reading.update(1.20, { label: "after" });
            assert (Reading | filter(reading => reading.value == 1.200) | one()).label == "after";
        "#,
    ));

    assert!(matches!(&outcome, StageOutcome::Passed), "{outcome:?}");
    let key = Value::decimal(12.into(), (-1).into()).expect("canonical Decimal primary key");
    let row = runtime
        .committed_row("Reading", &key)
        .expect("updated Decimal-key row");
    assert!(matches!(
        row.raw(),
        orna_foundation_v1::OvbRaw::Map(fields)
            if fields.iter().any(|(field, value)| field
                == &orna_foundation_v1::OvbRaw::Text("label".into())
                && value == &orna_foundation_v1::OvbRaw::Text("after".into()))
    ));
}

#[test]
fn parsed_decimal_primary_key_update_is_visible_to_following_relation_reads() {
    let mut runtime = TransactionalEvaluator::new("parent", Limits::default());
    let outcome = runtime.execute_source(&decimal_key_relation_source(
        r#"
            Reading.insert({ value: 1.2000, label: "before" });
            Reading.update(1.2, { label: "after" });
            assert (Reading | filter(reading => reading.value == 1.2000) | count()) == 1;
            assert (Reading | filter(reading => reading.label == "after") | one()).value == 1.20;
        "#,
    ));

    assert!(matches!(&outcome, StageOutcome::Passed), "{outcome:?}");
}

#[test]
fn parsed_decimal_primary_key_update_absent_source_rolls_back_candidate_rows() {
    let mut runtime = TransactionalEvaluator::new("parent", Limits::default());
    let outcome = runtime.execute_source(&decimal_key_relation_source(
        r#"
            Reading.insert({ value: 1.20, label: "candidate" });
            Reading.update(9.990, { label: "missing" });
        "#,
    ));

    assert!(
        matches!(
            &outcome,
            StageOutcome::Failed(diagnostic)
                if diagnostic.code() == "ORNA-EVAL-TABLE-MISSING"
        ),
        "absent Decimal update source did not fail with missing-row diagnostic: {outcome:?}"
    );
    let key = Value::decimal(12.into(), (-1).into()).expect("canonical inserted key");
    assert_eq!(
        runtime.committed_row("Reading", &key),
        None,
        "absent Decimal update source published a candidate row"
    );
}

#[test]
fn parsed_decimal_primary_key_update_invalid_source_key_rolls_back_candidate_rows() {
    let mut runtime = TransactionalEvaluator::new("parent", Limits::default());
    let outcome = runtime.execute_source(&decimal_key_relation_source(
        r#"
            Reading.insert({ value: 1.20, label: "candidate" });
            Reading.update("not-a-decimal", { label: "invalid" });
        "#,
    ));

    assert!(
        matches!(
            &outcome,
            StageOutcome::Failed(diagnostic) if diagnostic.code() == "ORNA-S021-TYPE"
        ),
        "invalid Decimal update key did not fail during source admission: {outcome:?}"
    );
    let key = Value::decimal(12.into(), (-1).into()).expect("canonical inserted key");
    assert_eq!(
        runtime.committed_row("Reading", &key),
        None,
        "invalid Decimal update key published a candidate row"
    );
}

#[test]
fn parsed_decimal_primary_key_update_rejects_key_patch_and_rolls_back_candidate_rows() {
    let mut runtime = TransactionalEvaluator::new("parent", Limits::default());
    let outcome = runtime.execute_source(&decimal_key_relation_source(
        r#"
            Reading.insert({ value: 1.20, label: "candidate" });
            Reading.update(1.2000, { value: 2.0 });
        "#,
    ));

    assert!(
        matches!(
            &outcome,
            StageOutcome::Failed(diagnostic)
                if diagnostic.code() == "ORNA-S021-TYPE"
        ),
        "Decimal update key patch did not fail during source admission: {outcome:?}"
    );
    for key in [
        Value::decimal(12.into(), (-1).into()).expect("canonical source key"),
        Value::decimal(2.into(), 0.into()).expect("canonical destination key"),
    ] {
        assert_eq!(
            runtime.committed_row("Reading", &key),
            None,
            "Decimal update key patch published a candidate row"
        );
    }
}


#[test]
fn parsed_decimal_primary_key_filtered_lookup_and_first_use_scale_insensitive_canonical_order() {
    let mut runtime = TransactionalEvaluator::new("parent", Limits::default());
    let outcome = runtime.execute_source(&decimal_key_relation_source(
        r#"
            Reading.insert({ value: 2.000, label: "high" });
            Reading.insert({ value: -1.2000, label: "low" });
            Reading.insert({ value: 0.00, label: "zero" });
            assert (Reading | filter(reading => reading.value == 2.0) | one()).label == "high";
            assert (Reading | take(1) | one()).label == "low";
        "#,
    ));

    assert!(matches!(&outcome, StageOutcome::Passed), "{outcome:?}");
}
fn decimal_upsert_source(fixture_id: &str, parent_body: &str) -> SourceUnit {
    SourceUnit {
        fixture_id: fixture_id.into(),
        source_id: format!("{fixture_id}.orna"),
        parse_as: "module_unit".into(),
        source: format!(
            "pub table Reading(value: Decimal) {{ label: Str, note: Str, }} fn parent() {{ {parent_body} }}"
        ),
    }
}

#[test]
fn parsed_decimal_primary_key_upsert_selects_existing_row_across_scales() {
    let mut runtime = TransactionalEvaluator::new("parent", Limits::default());
    let outcome = runtime.execute_source(&decimal_upsert_source(
        "txn-decimal-upsert-existing-scale",
        r#"
            Reading.insert({ value: 1.2000, label: "before", note: "original" });
            Reading.upsert({ value: 1.20, label: "after", note: "patched" });
            assert (Reading | filter(reading => reading.value == 1.20000) | count()) == 1;
        "#,
    ));

    assert!(matches!(&outcome, StageOutcome::Passed), "{outcome:?}");
    let key = Value::decimal(12.into(), (-1).into()).expect("canonical Decimal primary key");
    let row = runtime
        .committed_row("Reading", &key)
        .expect("scale-insensitive Decimal upsert row");
    assert!(matches!(
        row.raw(),
        orna_foundation_v1::OvbRaw::Map(fields)
            if fields.iter().any(|(field, value)| field
                == &orna_foundation_v1::OvbRaw::Text("label".into())
                && value == &orna_foundation_v1::OvbRaw::Text("after".into()))
    ));
}

#[test]
fn parsed_decimal_primary_key_upsert_preserves_omitted_non_key_fields() {
    let mut runtime = TransactionalEvaluator::new("parent", Limits::default());
    let outcome = runtime.execute_source(&decimal_upsert_source(
        "txn-decimal-upsert-preserve-omitted",
        r#"
            Reading.insert({ value: 1.2000, label: "before", note: "preserve me" });
            Reading.upsert({ value: 1.2, label: "after" });
            assert (Reading | filter(reading => reading.note == "preserve me") | count()) == 1;
        "#,
    ));

    assert!(matches!(&outcome, StageOutcome::Passed), "{outcome:?}");
    let key = Value::decimal(12.into(), (-1).into()).expect("canonical Decimal primary key");
    let row = runtime
        .committed_row("Reading", &key)
        .expect("patched Decimal primary-key row");
    assert!(matches!(
        row.raw(),
        orna_foundation_v1::OvbRaw::Map(fields)
            if fields.iter().any(|(field, value)| field
                == &orna_foundation_v1::OvbRaw::Text("label".into())
                && value == &orna_foundation_v1::OvbRaw::Text("after".into()))
                && fields.iter().any(|(field, value)| field
                    == &orna_foundation_v1::OvbRaw::Text("note".into())
                    && value == &orna_foundation_v1::OvbRaw::Text("preserve me".into()))
    ));
}

#[test]
fn parsed_decimal_primary_key_upsert_inserts_an_absent_key() {
    let mut runtime = TransactionalEvaluator::new("parent", Limits::default());
    let outcome = runtime.execute_source(&decimal_upsert_source(
        "txn-decimal-upsert-absent-key",
        r#"
            Reading.upsert({ value: 2.5000, label: "inserted", note: "new row" });
            assert (Reading | filter(reading => reading.value == 2.50) | count()) == 1;
        "#,
    ));

    assert!(matches!(&outcome, StageOutcome::Passed), "{outcome:?}");
    let key = Value::decimal(25.into(), (-1).into()).expect("canonical inserted Decimal key");
    let row = runtime
        .committed_row("Reading", &key)
        .expect("absent Decimal key was inserted");
    assert!(matches!(
        row.raw(),
        orna_foundation_v1::OvbRaw::Map(fields)
            if fields.iter().any(|(field, value)| field
                == &orna_foundation_v1::OvbRaw::Text("label".into())
                && value == &orna_foundation_v1::OvbRaw::Text("inserted".into()))
    ));
}

#[test]
fn parsed_decimal_primary_key_upsert_reads_candidate_rows_and_rolls_back_on_failure() {
    let mut committed = TransactionalEvaluator::new("parent", Limits::default());
    let committed_outcome = committed.execute_source(&decimal_upsert_source(
        "txn-decimal-upsert-ryw-commit",
        r#"
            Reading.upsert({ value: 3.7500, label: "candidate", note: "visible" });
            assert (Reading | filter(reading => reading.value == 3.75) | count()) == 1;
        "#,
    ));
    assert!(
        matches!(&committed_outcome, StageOutcome::Passed),
        "{committed_outcome:?}"
    );
    let key = Value::decimal(375.into(), (-2).into()).expect("canonical candidate Decimal key");
    assert!(
        committed.committed_row("Reading", &key).is_some(),
        "candidate Decimal upsert row was not published"
    );

    let mut rolled_back = TransactionalEvaluator::new("parent", Limits::default());
    let failed = rolled_back.execute_source(&decimal_upsert_source(
        "txn-decimal-upsert-ryw-rollback",
        r#"
            Reading.upsert({ value: 3.7500, label: "candidate", note: "visible" });
            assert (Reading | filter(reading => reading.value == 3.75) | count()) == 1;
            assert false;
        "#,
    ));
    assert!(matches!(
        &failed,
        StageOutcome::Failed(diagnostic) if diagnostic.code() == "ORNA-EVAL-ASSERT"
    ));
    assert_eq!(
        rolled_back.committed_row("Reading", &key),
        None,
        "failed Decimal upsert activation published its candidate row"
    );
}
