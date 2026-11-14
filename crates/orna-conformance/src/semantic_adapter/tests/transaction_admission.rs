use super::{
    Catalogue, SourceUnit, StageOutcome, TableEffectHandler, TransactionDatabase,
    TransactionalEvaluator, admit_transaction_project,
};
use crate::{ProjectEnvironment, ProjectExpectations, ProjectUnit};
use num_bigint::BigInt;
use orna_cursor::{
    TRANSACTION_CURSOR_PREFIX, TRANSACTION_INT_CURSOR_PREFIX, TransactionKey, TransactionKeyType,
    TransactionTableKey, encoded_table_key, record_field, table_key_components,
    transaction_continuation_cursor, transaction_cursor_key,
};
use orna_evaluator::{EffectHandler, Limits, StepBudget};
use orna_value::{OvbRaw, Value};
use std::collections::BTreeMap;

#[test]
fn transaction_key_order_uses_logical_integer_and_composite_components() {
    let integer_key = |integer: i64| {
        let value = Value::int(integer.into());
        TransactionKey::typed(
            value.encode().expect("canonical integer encoding"),
            vec![value],
        )
    };
    assert!(integer_key(-2) < integer_key(-1));
    assert!(integer_key(-1) < integer_key(0));

    let composite_key = |integer: i64, text: &str| {
        let values = vec![
            Value::int(integer.into()),
            Value::new(OvbRaw::Text(text.into())).expect("canonical text"),
        ];
        TransactionKey::typed(
            encoded_table_key(&values).expect("canonical composite encoding"),
            values,
        )
    };
    assert!(composite_key(-3, "z") < composite_key(-2, "a"));
    assert!(composite_key(-2, "a") < composite_key(-2, "b"));
}

fn source(body: &str) -> SourceUnit {
    SourceUnit {
        fixture_id: "transaction-effect-admission".into(),
        source_id: "transaction-effect-admission.orna".into(),
        parse_as: "module_unit".into(),
        source: format!("pub table Note(id: Int) {{ text: Str, }} fn main() {{ {body} }}"),
    }
}

#[test]
fn candidate_assertion_rows_bind_names_in_stable_field_id_order() {
    let declared_fields = vec!["first_declared".to_owned(), "second_declared".to_owned()];
    let field_ids = BTreeMap::from([
        ("first_declared".to_owned(), [0x20; 16]),
        ("second_declared".to_owned(), [0x10; 16]),
    ]);
    let ordered_fields =
        super::order_field_names_by_stable_id(&declared_fields, &field_ids).unwrap();
    assert_eq!(ordered_fields, ["second_declared", "first_declared"]);

    let table_fields = BTreeMap::from([("main.Note".to_owned(), ordered_fields)]);
    let key_fields = BTreeMap::from([("main.Note".to_owned(), vec!["id".to_owned()])]);
    let key = Value::new(OvbRaw::Text("note-7".into()))
        .unwrap()
        .encode()
        .unwrap();
    let encoded = Value::new(OvbRaw::Array(vec![
        OvbRaw::Text("value-for-second".into()),
        OvbRaw::Text("value-for-first".into()),
    ]))
    .unwrap()
    .encode()
    .unwrap();
    let row =
        super::candidate_assertion_row("main.Note", &key, &encoded, &table_fields, &key_fields)
            .unwrap();
    let OvbRaw::Map(fields) = row.raw() else {
        panic!("candidate assertion row is a record map");
    };
    let value_for = |name: &str| {
        fields
            .iter()
            .find_map(|(key, value)| (key == &OvbRaw::Text(name.to_owned())).then_some(value))
    };
    assert_eq!(
        value_for("first_declared"),
        Some(&OvbRaw::Text("value-for-first".into()))
    );
    assert_eq!(
        value_for("second_declared"),
        Some(&OvbRaw::Text("value-for-second".into()))
    );
    assert_eq!(value_for("id"), Some(&OvbRaw::Text("note-7".into())));
}

#[test]
fn same_key_rekey_reports_duplicate_without_mutating_the_committed_row() {
    let mut evaluator = TransactionalEvaluator::new("main", Limits::default());
    assert!(matches!(
        evaluator.execute_source(&source(r#"Note.insert({ id: 7, text: "before" });"#)),
        StageOutcome::Passed
    ));

    let outcome = evaluator.execute_source(&source(r#"Note.rekey(7, 7);"#));

    assert!(matches!(
        outcome,
        StageOutcome::Failed(ref diagnostic)
            if diagnostic.code() == "ORNA-EVAL-TABLE-DUPLICATE"
    ));
    assert!(
        evaluator
            .committed_row("Note", &Value::int(7.into()))
            .is_some()
    );
}

#[test]
fn signed_integer_committed_relation_scan_continues_across_pages() {
    let mut evaluator = TransactionalEvaluator::new("main", Limits::default());
    let negative_large = -(BigInt::from(1_u8) << 256_usize);
    let positive_large = BigInt::from(1_u8) << 256_usize;
    assert!(matches!(
        evaluator.execute_source(&source(&format!(
            r#"Note.insert({{ id: {negative_large}, text: "negative-large" }}); Note.insert({{ id: -2, text: "negative-two" }}); Note.insert({{ id: -1, text: "negative-one" }}); Note.insert({{ id: 0, text: "zero" }}); Note.insert({{ id: 1, text: "positive-one" }}); Note.insert({{ id: {positive_large}, text: "positive-large" }});"#,
        ))),
        StageOutcome::Passed
    ));
    assert!(matches!(
        evaluator.execute_source(&source(r#"assert (Note | count) == 6;"#)),
        StageOutcome::Passed
    ));
}

#[test]
fn signed_integer_staged_relation_scan_continues_across_pages() {
    let mut evaluator = TransactionalEvaluator::new("main", Limits::default());
    let negative_large = -(BigInt::from(1_u8) << 256_usize);
    let positive_large = BigInt::from(1_u8) << 256_usize;
    assert!(matches!(
        evaluator.execute_source(&source(&format!(
            r#"Note.insert({{ id: {negative_large}, text: "negative-large" }}); Note.insert({{ id: -1, text: "negative-one" }}); Note.insert({{ id: 1, text: "positive-one" }}); Note.insert({{ id: {positive_large}, text: "positive-large" }});"#,
        ))),
        StageOutcome::Passed
    ));
    assert!(matches!(
        evaluator.execute_source(&source(
            r#"Note.insert({ id: -2, text: "negative-two" }); Note.insert({ id: 0, text: "zero" }); Note.insert({ id: 2, text: "positive-two" }); assert (Note | count) == 7;"#,
        )),
        StageOutcome::Passed
    ));
}

#[test]
fn signed_integer_relation_cursor_is_numeric_and_reversible() {
    let large = BigInt::from(1_u8) << 256_usize;
    let values = [
        -large.clone(),
        BigInt::from(-65_535),
        BigInt::from(-255),
        BigInt::from(-2),
        BigInt::from(-1),
        BigInt::from(0),
        BigInt::from(1),
        BigInt::from(2),
        BigInt::from(255),
        BigInt::from(65_535),
        large,
    ];
    let schema = super::TransactionTableKey::new(
        String::from("Note"),
        vec![String::from("id")],
        vec![super::TransactionKeyType::Int],
    )
    .expect("valid Note key schema");
    let mut previous = None;

    for value in values {
        let key = TransactionKey::new(Value::int(value).encode().expect("encoded integer"));
        let cursor = transaction_continuation_cursor(&key, &schema);
        assert_ne!(cursor, key.as_ref());
        assert_eq!(
            transaction_cursor_key(&cursor, &schema).expect("decoded cursor"),
            key
        );
        if let Some(previous) = previous {
            assert!(previous < cursor);
        }
        previous = Some(cursor);
    }

    for (kind, length) in [(0, [u8::MAX; 8]), (2, [0; 8])] {
        let mut cursor = Vec::from(TRANSACTION_INT_CURSOR_PREFIX);
        cursor.push(kind);
        cursor.extend_from_slice(&length);
        assert!(transaction_cursor_key(&cursor, &schema).is_err());
    }
}

#[test]
fn table_effect_handler_scan_relation_page_advances_integer_cursors() {
    let mut database = TransactionDatabase::default();
    database
        .activate(|activation| {
            for value in [-255_i64, 0, 255] {
                let row = Value::int(value.into());
                activation
                    .insert(
                        "Note".into(),
                        TransactionKey::new(row.encode().expect("encoded integer")),
                        row,
                    )
                    .expect("unique integer key");
            }

            let key_fields = BTreeMap::from([(
                "Note".into(),
                super::TransactionTableKey::new(
                    "Note".into(),
                    vec!["id".into()],
                    vec![super::TransactionKeyType::Int],
                )
                .expect("valid Note key schema"),
            )]);
            let table_fields = BTreeMap::from([("Note".into(), vec!["id".into()])]);
            let mut mutations = Vec::new();
            let limits = Limits::default();
            let mut handler = TableEffectHandler {
                activation,
                key_fields: &key_fields,
                table_fields: &table_fields,
                mutations: &mut mutations,
                next_mutation: 0,
                limits,
                activation_time: None,
                publication_rows: None,
                mutation_scope: [0; 32],
                sys_host: None,
                explain_catalogue: None,
            };
            let mut budget = StepBudget::new(limits.max_steps);

            let first = handler
                .scan_relation_page("Note", None, 1, &mut budget)
                .expect("first scan succeeds")
                .expect("declared table is scanable");
            let first_cursor = first.next.clone().expect("first cursor");
            assert_eq!(first.rows, vec![Value::int((-255).into())]);

            let second = handler
                .scan_relation_page("Note", Some(&first_cursor), 1, &mut budget)
                .expect("second scan succeeds")
                .expect("declared table is scanable");
            let second_cursor = second.next.clone().expect("second cursor");
            assert_eq!(second.rows, vec![Value::int(0.into())]);

            let third = handler
                .scan_relation_page("Note", Some(&second_cursor), 1, &mut budget)
                .expect("third scan succeeds")
                .expect("declared table is scanable");
            assert_eq!(third.rows, vec![Value::int(255.into())]);
            assert_eq!(third.next, None);
            assert!(first_cursor < second_cursor);

            let schema = super::TransactionTableKey::new(
                "Note".into(),
                vec!["id".into()],
                vec![super::TransactionKeyType::Int],
            )
            .expect("valid Note key schema");
            let third_cursor = transaction_continuation_cursor(
                &TransactionKey::new(Value::int(255.into()).encode().expect("encoded integer")),
                &schema,
            );
            assert!(second_cursor < third_cursor);

            let exhausted = handler
                .scan_relation_page("Note", Some(&third_cursor), 1, &mut budget)
                .expect("exhaustion scan succeeds")
                .expect("declared table is scanable");
            assert!(exhausted.rows.is_empty());
            assert_eq!(exhausted.next, None);
            Ok::<_, orna_evaluator::EvaluationError>(())
        })
        .expect("handler scan activation commits");
}

#[test]
fn controlled_table_entry_effects_are_admitted() {
    let mut evaluator = TransactionalEvaluator::new("main", Limits::default());

    assert!(matches!(
        evaluator.execute_source(&source(r#"Note.insert({ id: 1, text: "admitted" });"#)),
        StageOutcome::Passed
    ));
    assert!(
        evaluator
            .committed_row("Note", &Value::int(1.into()))
            .is_some()
    );
}

#[test]
fn external_entry_effect_is_rejected_before_table_writes() {
    let mut evaluator = TransactionalEvaluator::new("main", Limits::default());
    let unit = SourceUnit {
        fixture_id: "transaction-effect-admission".into(),
        source_id: "transaction-effect-admission.orna".into(),
        parse_as: "module_unit".into(),
        source: "pub table Note(id: Int) { text: Str, } fn main() { Note.insert({ id: 2, text: \"blocked\" }); std.net.http.get(\"https://example.invalid\"); }".into(),
    };

    let outcome = evaluator.execute_source(&unit);
    assert!(
        matches!(outcome, StageOutcome::Failed(_)),
        "external effect must be rejected before table writes: {outcome:?}"
    );
    assert!(
        evaluator
            .committed_row("Note", &Value::int(2.into()))
            .is_none()
    );
}

#[test]
fn transitive_external_helper_effect_is_rejected_before_table_activation() {
    let mut evaluator = TransactionalEvaluator::new("main", Limits::default());
    let unit = SourceUnit {
        fixture_id: "transaction-effect-admission".into(),
        source_id: "transaction-effect-admission.orna".into(),
        parse_as: "module_unit".into(),
        source: "pub table Note(id: Int) { text: Str, } fn fetch_remote() = std.net.http.get(\"https://example.invalid\"); fn main() { Note.insert({ id: 3, text: \"blocked transitively\" }); fetch_remote(); }".into(),
    };

    let outcome = evaluator.execute_source(&unit);
    match outcome {
        // The single-source effect gate in `admit_transaction_source_inner`
        // reads the first `main` symbol in `analysis.modules`, which is
        // ordered by namespace and also holds the catalogue modules. The
        // production core catalogue declares no `main`, so the gate reads
        // the user module's `main`, whose summary carries the `network`
        // effect propagated from `fetch_remote`, and rejects with
        // `ORNA-EVAL-EFFECT` before table activation. Under the fixture
        // catalogue the effectless fixture value `bank.main` sorts before
        // the user namespace, so the gate checked `bank.main` and passed,
        // and the run failed later with `ORNA-EVAL-NAME` instead.
        StageOutcome::Failed(diagnostic) => assert_eq!(
            diagnostic.code(),
            "ORNA-EVAL-EFFECT",
            "admission must reject the user entry's propagated network effect"
        ),
        outcome => panic!(
            "a transitive external effect must be rejected before table activation: {outcome:?}"
        ),
    }
    assert!(
        evaluator
            .committed_row("Note", &Value::int(3.into()))
            .is_none(),
        "rejected transitive effects must not commit table rows"
    );
}

#[test]
fn project_entry_effect_is_rejected_before_retaining_transaction_modules() {
    let project = ProjectUnit {
        fixture_id: "project-effect-admission".into(),
        project_id: "project-effect-admission".into(),
        environment_id: None,
        modules: vec![SourceUnit {
            fixture_id: "project-effect-admission".into(),
            source_id: "project-effect-admission/main.orna".into(),
            parse_as: "module_unit".into(),
            source: "pub table Note(id: Int) { text: Str, } fn seed() { Note.insert({ id: 4, text: \"blocked\" }); std.net.http.get(\"https://example.invalid\"); }".into(),
        }],
        loose_rows: Vec::new(),
        expectations: ProjectExpectations {
            environment: ProjectEnvironment {
                network: false,
                credentials: false,
                intrinsics: "Orna 1.1.0 core".into(),
                stdlib: None,
                initial_tables: "empty".into(),
            },
            steps: Vec::new(),
            negative_cases: Vec::new(),
        },
    };

    match admit_transaction_project(
        &project,
        Limits::default(),
        "main.seed",
        &Catalogue::authoritative_core(),
    ) {
        Err(outcome) => assert!(matches!(
            *outcome,
            StageOutcome::Failed(ref diagnostic) if diagnostic.code() == "ORNA-EVAL-EFFECT"
        )),
        Ok(_) => panic!("external project entry effect reached transaction retention"),
    }
}

#[test]
fn table_assertion_item_limit_rolls_back_the_complete_activation() {
    let unit = SourceUnit {
        fixture_id: "table-assertion-item-limit".into(),
        source_id: "table-assertion-item-limit.orna".into(),
        parse_as: "module_unit".into(),
        source: r#"
            pub table Note(id: Int) {
                text: Str,
                assert every(note => note.text != "");
            }
            fn main() {
                Note.insert({ id: 1, text: "one" });
                Note.insert({ id: 2, text: "two" });
                Note.insert({ id: 3, text: "three" });
            }
        "#
        .into(),
    };
    let limits = Limits {
        max_collection_items: 2,
        ..Default::default()
    };
    let mut evaluator = TransactionalEvaluator::new("main", limits);

    let outcome = evaluator.execute_source(&unit);

    assert!(matches!(
        outcome,
        StageOutcome::Failed(ref diagnostic) if diagnostic.code() == "ORNA-EVAL-LIMIT"
    ));
    for id in 1..=3 {
        assert_eq!(
            evaluator.committed_row("Note", &Value::int(id.into())),
            None,
            "row {id} escaped the assertion-limit rollback"
        );
    }
}

#[test]
fn relation_scan_step_limit_rolls_back_the_complete_activation() {
    let unit = SourceUnit {
        fixture_id: "relation-scan-step-limit".into(),
        source_id: "relation-scan-step-limit.orna".into(),
        parse_as: "module_unit".into(),
        source: r#"
            pub table Note(id: Int) {
                text: Str,
            }
            fn main() {
                Note.insert({ id: 1, text: "one" });
                Note.insert({ id: 2, text: "two" });
                Note.insert({ id: 3, text: "three" });
                Note | take(3) | count();
            }
        "#
        .into(),
    };
    let limits = Limits {
        max_steps: 25,
        ..Default::default()
    };
    let no_scan = SourceUnit {
        source: unit.source.replace("take(3)", "take(0)"),
        ..unit.clone()
    };
    let mut no_scan_evaluator = TransactionalEvaluator::new("main", limits);
    assert!(matches!(
        no_scan_evaluator.execute_source(&no_scan),
        StageOutcome::Passed
    ));
    for id in 1..=3 {
        assert!(
            no_scan_evaluator
                .committed_row("Note", &Value::int(id.into()))
                .is_some(),
            "row {id} did not commit when the relation scan was skipped"
        );
    }
    let mut evaluator = TransactionalEvaluator::new("main", limits);

    let outcome = evaluator.execute_source(&unit);

    assert!(matches!(
        outcome,
        StageOutcome::Failed(ref diagnostic) if diagnostic.code() == "ORNA-EVAL-LIMIT"
    ));
    for id in 1..=3 {
        assert_eq!(
            evaluator.committed_row("Note", &Value::int(id.into())),
            None,
            "row {id} escaped the relation-scan limit rollback"
        );
    }
}

#[test]
fn tagged_tuple_key_components_reject_wrong_arity_and_keep_valid_forms() {
    let components = || vec![OvbRaw::Text("north".into()), OvbRaw::Text("pencil".into())];
    let tagged = Value::new(OvbRaw::Tag(60015, Box::new(OvbRaw::Array(components()))))
        .expect("typed tuple")
        .encode()
        .expect("encoded typed tuple");

    assert!(
        table_key_components(&tagged, 1).is_err(),
        "a two-component tagged tuple cannot satisfy a one-component key"
    );
    let decoded = table_key_components(&tagged, 2).expect("valid tagged tuple");
    assert!(matches!(decoded[0].raw(), OvbRaw::Text(value) if value == "north"));
    assert!(matches!(decoded[1].raw(), OvbRaw::Text(value) if value == "pencil"));

    let plain = Value::new(OvbRaw::Array(components()))
        .expect("plain composite key")
        .encode()
        .expect("encoded plain composite key");
    let decoded = table_key_components(&plain, 2).expect("valid plain array key");
    assert!(matches!(decoded[0].raw(), OvbRaw::Text(value) if value == "north"));
    assert!(matches!(decoded[1].raw(), OvbRaw::Text(value) if value == "pencil"));
}

#[test]
fn typed_composite_scan_orders_components_and_rejects_invalid_cursors() {
    let schema = TransactionTableKey::new(
        "Stock".into(),
        vec!["location".into(), "sku".into()],
        vec![TransactionKeyType::Str, TransactionKeyType::Str],
    )
    .expect("valid composite key schema");
    let key_fields = BTreeMap::from([("Stock".into(), schema.clone())]);
    let table_fields = BTreeMap::from([("Stock".into(), vec!["location".into(), "sku".into()])]);
    let rows = [
        (
            vec![
                Value::new(OvbRaw::Text("z".into())).expect("z"),
                Value::new(OvbRaw::Text("last".into())).expect("last"),
            ],
            "z-last",
        ),
        (
            vec![
                Value::new(OvbRaw::Text("a".into())).expect("a"),
                Value::new(OvbRaw::Text("second".into())).expect("second"),
            ],
            "a-second",
        ),
        (
            vec![
                Value::new(OvbRaw::Text("a".into())).expect("a"),
                Value::new(OvbRaw::Text("first".into())).expect("first"),
            ],
            "a-first",
        ),
    ];
    let mut database = TransactionDatabase::default();
    database
        .activate(|activation| {
            for (values, label) in rows {
                let key = schema.key_from_values(&values).expect("typed key");
                let row = Value::new(OvbRaw::Map(vec![
                    (OvbRaw::Text("sku".into()), values[1].raw().clone()),
                    (OvbRaw::Text("label".into()), OvbRaw::Text(label.into())),
                    (OvbRaw::Text("location".into()), values[0].raw().clone()),
                ]))
                .expect("canonical row");
                activation
                    .insert("Stock".into(), key, row)
                    .expect("unique composite key");
            }

            let mut mutations = Vec::new();
            let limits = Limits::default();
            let mut handler = TableEffectHandler {
                activation,
                key_fields: &key_fields,
                table_fields: &table_fields,
                mutations: &mut mutations,
                next_mutation: 0,
                limits,
                activation_time: None,
                publication_rows: None,
                mutation_scope: [0; 32],
                sys_host: None,
                explain_catalogue: None,
            };
            let mut budget = StepBudget::new(limits.max_steps);
            let first = handler
                .scan_relation_page("Stock", None, 2, &mut budget)
                .expect("first scan succeeds")
                .expect("declared table is scanable");
            assert_eq!(
                record_field(&first.rows[0], "label")
                    .expect("first label")
                    .raw(),
                &OvbRaw::Text("a-first".into())
            );
            assert_eq!(
                record_field(&first.rows[1], "label")
                    .expect("second label")
                    .raw(),
                &OvbRaw::Text("a-second".into())
            );
            let cursor = first.next.clone().expect("non-terminal cursor");

            let second = handler
                .scan_relation_page("Stock", Some(&cursor), 2, &mut budget)
                .expect("continuation succeeds")
                .expect("declared table is scanable");
            assert_eq!(second.rows.len(), 1);
            assert_eq!(
                record_field(&second.rows[0], "label")
                    .expect("terminal label")
                    .raw(),
                &OvbRaw::Text("z-last".into())
            );
            assert_eq!(second.next, None);

            let cursor_for = |payload: Vec<u8>| {
                let mut cursor = Vec::from(TRANSACTION_CURSOR_PREFIX);
                cursor.extend_from_slice(&schema.cursor_schema_digest());
                cursor.extend(payload);
                cursor
            };
            assert!(transaction_cursor_key(&cursor, &schema).is_ok());
            assert!(
                transaction_cursor_key(
                    &cursor_for(
                        Value::new(OvbRaw::Array(vec![OvbRaw::Text("a".into())]))
                            .expect("canonical one-component value")
                            .encode()
                            .expect("encoded one-component value")
                    ),
                    &schema,
                )
                .is_err()
            );
            assert!(
                transaction_cursor_key(
                    &cursor_for(
                        Value::new(OvbRaw::Array(vec![
                            OvbRaw::Int(1.into()),
                            OvbRaw::Int(2.into())
                        ]))
                        .expect("canonical wrong-type value")
                        .encode()
                        .expect("encoded wrong-type value"),
                    ),
                    &schema,
                )
                .is_err()
            );
            assert!(
                transaction_cursor_key(&cursor_for(vec![0x82, 0x18, 0x01, 0x01]), &schema,)
                    .is_err()
            );
            let other_schema = TransactionTableKey::new(
                "Other".into(),
                vec!["location".into(), "sku".into()],
                vec![TransactionKeyType::Str, TransactionKeyType::Str],
            )
            .expect("valid alternate schema");
            assert!(transaction_cursor_key(&cursor, &other_schema).is_err());
            Ok::<_, orna_evaluator::EvaluationError>(())
        })
        .expect("typed composite scan activation commits");
}

#[test]
fn decimal_typed_keys_compare_by_exact_numeric_value() {
    let schema = TransactionTableKey::new(
        "Reading".into(),
        vec!["value".into()],
        vec![TransactionKeyType::Decimal],
    )
    .expect("valid decimal key schema");
    let negative = schema
        .key_from_values(&[Value::decimal((-15).into(), (-1).into()).expect("-1.5")])
        .expect("negative decimal key");
    let zero = schema
        .key_from_values(&[Value::decimal(0.into(), 0.into()).expect("zero")])
        .expect("zero decimal key");
    let one_tenth = schema
        .key_from_values(&[Value::decimal(1.into(), (-1).into()).expect("0.1")])
        .expect("small decimal key");
    let one = schema
        .key_from_values(&[Value::decimal(1.into(), 0.into()).expect("one")])
        .expect("one decimal key");
    let two = schema
        .key_from_values(&[Value::decimal(2.into(), 0.into()).expect("two")])
        .expect("two decimal key");
    assert!(negative < zero);
    assert!(zero < one_tenth);
    assert!(one_tenth < one);
    assert!(one < two);
    let ordered = [&negative, &zero, &one_tenth, &one, &two];
    let cursors = ordered
        .iter()
        .map(|key| transaction_continuation_cursor(key, &schema))
        .collect::<Vec<_>>();
    for (key, cursor) in ordered.iter().zip(&cursors) {
        assert_eq!(
            transaction_cursor_key(cursor, &schema).expect("decimal cursor"),
            **key
        );
    }
    for pair in cursors.windows(2) {
        assert!(pair[0] < pair[1], "decimal cursor does not advance");
    }
    let mut noncanonical = cursors[3].clone();
    noncanonical.insert(noncanonical.len() - 1, 1);
    let error = transaction_cursor_key(&noncanonical, &schema)
        .expect_err("decimal cursor with a leading zero digit must be rejected");
    assert_eq!(error.code(), "ORNA-EVAL-VALUE");
}

#[test]
fn admitted_rehydration_keeps_typed_composite_order_for_staged_scan() {
    let schema = TransactionTableKey::new(
        "Pair".into(),
        vec!["left".into(), "right".into()],
        vec![TransactionKeyType::Int, TransactionKeyType::Int],
    )
    .expect("valid composite key schema");
    let key_fields = BTreeMap::from([("Pair".into(), schema.clone())]);
    let table_fields = BTreeMap::from([(
        "Pair".into(),
        vec!["left".into(), "right".into(), "label".into()],
    )]);
    let row = |left: i64, right: i64, label: &str| {
        Value::new(OvbRaw::Map(vec![
            (OvbRaw::Text("left".into()), OvbRaw::Int(left.into())),
            (OvbRaw::Text("label".into()), OvbRaw::Text(label.into())),
            (OvbRaw::Text("right".into()), OvbRaw::Int(right.into())),
        ]))
        .expect("canonical composite row")
    };
    let mut evaluator = TransactionalEvaluator::new("main", Limits::default());
    for (left, right, label) in [(-1, 0, "committed-negative"), (0, 0, "committed-zero")] {
        let key = schema
            .key_from_values(&[Value::int(left.into()), Value::int(right.into())])
            .expect("typed committed key");
        evaluator
            .seed_committed_from_admission(
                "Pair".into(),
                key.as_ref().to_vec(),
                row(left, right, label),
                &key_fields,
                &BTreeMap::new(),
                &BTreeMap::new(),
            )
            .expect("rehydrated row retains admitted key types");
    }

    evaluator
        .database
        .activate(|activation| {
            let staged_key = schema
                .key_from_values(&[Value::int(0.into()), Value::int(1.into())])
                .expect("typed staged key");
            activation
                .insert("Pair".into(), staged_key, row(0, 1, "staged-zero-one"))
                .expect("staged row");

            let mut mutations = Vec::new();
            let limits = Limits::default();
            let mut handler = TableEffectHandler {
                activation,
                key_fields: &key_fields,
                table_fields: &table_fields,
                mutations: &mut mutations,
                next_mutation: 0,
                limits,
                activation_time: None,
                publication_rows: None,
                mutation_scope: [0; 32],
                sys_host: None,
                explain_catalogue: None,
            };
            let mut budget = StepBudget::new(limits.max_steps);
            let first = handler
                .scan_relation_page("Pair", None, 1, &mut budget)
                .expect("first scan succeeds")
                .expect("declared table is scanable");
            assert_eq!(
                record_field(&first.rows[0], "label")
                    .expect("negative label")
                    .raw(),
                &OvbRaw::Text("committed-negative".into())
            );
            let first_cursor = first.next.expect("committed successor exists");
            let second = handler
                .scan_relation_page("Pair", Some(&first_cursor), 1, &mut budget)
                .expect("continuation succeeds")
                .expect("declared table is scanable");
            assert_eq!(
                record_field(&second.rows[0], "label")
                    .expect("zero label")
                    .raw(),
                &OvbRaw::Text("committed-zero".into())
            );
            let second_cursor = second.next.expect("staged successor exists");
            assert!(first_cursor < second_cursor);
            let third = handler
                .scan_relation_page("Pair", Some(&second_cursor), 1, &mut budget)
                .expect("terminal continuation succeeds")
                .expect("declared table is scanable");
            assert_eq!(
                record_field(&third.rows[0], "label")
                    .expect("staged label")
                    .raw(),
                &OvbRaw::Text("staged-zero-one".into())
            );
            assert_eq!(third.next, None);
            Ok::<_, orna_evaluator::EvaluationError>(())
        })
        .expect("typed committed and staged scan activation commits");
}
