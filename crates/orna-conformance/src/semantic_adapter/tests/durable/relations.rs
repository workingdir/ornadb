use super::*;

#[test]
fn request_terminal_replays_the_exact_skip_reason() {
    let outcome = StageOutcome::Skipped {
        reason: "exact retained skip reason".into(),
    };
    let terminal = request_terminal(&outcome).expect("bounded terminal");
    assert_eq!(
        replay_request_terminal(terminal.as_bytes()).expect("replayable terminal"),
        outcome
    );
}

#[tokio::test]
async fn project_activation_keeps_modules_qualified_and_rolls_back_failed_roots() {
    let temp = TempDir::new().expect("temporary repository");
    git(temp.path(), &["init"]);
    crate::test_support::configure_fixture_git_identity(temp.path());
    let repository = Repository::discover(temp.path()).expect("repository");
    let identity = RuntimeIdentity {
        database_id: [41; 16],
        repository_id: [42; 16],
    };
    let project = project(vec![
        (
            "inventory.orna",
            "pub table Note(id: Int) { text: Str, assert every(n => n.text != \"bad\"); } pub fn add() { Note.insert({ id: 7, text: \"from-import\" }); } pub fn bad() { Note.insert({ id: 8, text: \"bad\" }); }",
        ),
        (
            "main.orna",
            "use inventory; fn seed() { inventory.add(); } fn exercise() { inventory.bad(); }",
        ),
    ]);
    let evaluator = DurableTransactionalEvaluator::default();

    match evaluator
        .execute_project(
            &repository,
            identity,
            [43; 16],
            [44; 32],
            &project,
            "main.seed",
        )
        .await
    {
        Ok(StageOutcome::Passed) => {}
        Ok(StageOutcome::Failed(diagnostic)) => panic!("seed failed: {}", diagnostic.code()),
        Ok(StageOutcome::Cancelled(diagnostic)) => {
            panic!("seed cancelled: {}", diagnostic.code())
        }
        Ok(StageOutcome::Skipped { reason }) => panic!("seed skipped: {reason}"),
        Err(error) => panic!("seed runtime error: {error}"),
    }
    assert!(matches!(
        evaluator
            .execute_project(
                &repository,
                identity,
                [43; 16],
                [44; 32],
                &project,
                "main.exercise",
            )
            .await,
        Ok(StageOutcome::Failed(_))
    ));
    let state = RuntimeState::open(&repository, identity, [44; 32])
        .await
        .expect("reopened runtime");
    assert!(
        state
            .committed_table_row("inventory.Note", &Value::int(7.into()).encode().unwrap())
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        state
            .committed_table_row("inventory.Note", &Value::int(8.into()).encode().unwrap())
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn authoritative_five_module_project_executes_seed_and_exercise() {
    let temp = TempDir::new().expect("temporary repository");
    git(temp.path(), &["init"]);
    crate::test_support::configure_fixture_git_identity(temp.path());
    let repository = Repository::discover(temp.path()).expect("repository");
    let identity = RuntimeIdentity {
        database_id: [51; 16],
        repository_id: [52; 16],
    };
    let evaluator = DurableTransactionalEvaluator::default();
    let project = authoritative_project();

    for entry in ["main.seed", "main.exercise"] {
        match evaluator
            .execute_project(&repository, identity, [53; 16], [54; 32], &project, entry)
            .await
        {
            Ok(StageOutcome::Passed) => {}
            Ok(StageOutcome::Failed(diagnostic)) => {
                panic!("{entry} failed: {}", diagnostic.code())
            }
            Ok(StageOutcome::Cancelled(diagnostic)) => {
                panic!("{entry} cancelled: {}", diagnostic.code())
            }
            Ok(StageOutcome::Skipped { reason }) => panic!("{entry} skipped: {reason}"),
            Err(error) => panic!("{entry} runtime error: {error}"),
        }
    }
    let state = RuntimeState::open(&repository, identity, [54; 32])
        .await
        .expect("reopened runtime");
    let north = Value::new(orna_value::OvbRaw::Array(vec![
        orna_value::OvbRaw::Text("north".into()),
        orna_value::OvbRaw::Text("pencil".into()),
    ]))
    .unwrap()
    .encode()
    .unwrap();
    assert!(
        state
            .committed_table_row("warehouse.Stock", &north)
            .await
            .unwrap()
            .is_some()
    );
}

#[test]
fn relation_lowering_requires_exact_declared_key_order() {
    let keys = std::collections::BTreeMap::from([(
        String::from("Stock"),
        super::TransactionTableKey::new(
            String::from("Stock"),
            vec![String::from("location"), String::from("sku")],
            vec![
                super::TransactionKeyType::Str,
                super::TransactionKeyType::Str,
            ],
        )
        .expect("valid Stock key schema"),
    )]);
    let float_fields = super::TableFloatFields::new();
    let table_fields = super::TableFields::new();
    let functions = Functions::new();
    let shadowed = std::collections::BTreeSet::new();
    let lowering = super::RelationLowering {
        table_keys: &keys,
        float_fields: &float_fields,
        table_fields: &table_fields,
        functions: &functions,
        namespace: None,
        shadowed: &shadowed,
    };
    let ordered = orna_syntax::parse_expression(
        r#"Stock | filter(stock => stock.location == "north" && stock.sku == "pencil") | one()"#,
    );
    assert!(ordered.is_ok());
    assert!(lowering.relation_lookup(&ordered.value).is_some());

    let reordered = orna_syntax::parse_expression(
        r#"Stock | filter(stock => stock.sku == "pencil" && stock.location == "north") | one()"#,
    );
    assert!(reordered.is_ok());
    assert!(lowering.relation_lookup(&reordered.value).is_none());

    let non_key = orna_syntax::parse_expression(
        r#"Stock | filter(stock => stock.location == "north" && stock.quantity == 12) | one()"#,
    );
    assert!(non_key.is_ok());
    assert!(lowering.relation_lookup(&non_key.value).is_none());

    let duplicate_key = orna_syntax::parse_expression(
        r#"Stock | filter(stock => stock.location == "north" && stock.location == "south") | one()"#,
    );
    assert!(duplicate_key.is_ok());
    assert!(lowering.relation_lookup(&duplicate_key.value).is_none());
}

#[test]
fn relation_lowering_descends_into_range_endpoints() {
    let parsed = parse_expression(
        r#"(Stock | filter(stock => stock.location == "north" && stock.sku == "pencil") | one()) .. 2"#,
    );
    assert!(parsed.is_ok(), "{:?}", parsed.diagnostics);
    let mut expression = parsed.value;
    let keys = std::collections::BTreeMap::from([(
        String::from("Stock"),
        super::TransactionTableKey::new(
            String::from("Stock"),
            vec![String::from("location"), String::from("sku")],
            vec![
                super::TransactionKeyType::Str,
                super::TransactionKeyType::Str,
            ],
        )
        .expect("valid Stock key schema"),
    )]);

    let float_fields = super::TableFloatFields::new();
    let table_fields = super::TableFields::new();
    let functions = Functions::new();
    let shadowed = std::collections::BTreeSet::new();
    let lowering = super::RelationLowering {
        table_keys: &keys,
        float_fields: &float_fields,
        table_fields: &table_fields,
        functions: &functions,
        namespace: None,
        shadowed: &shadowed,
    };

    lowering.lower_relation_expression_with_resolution(&mut expression);

    let Expr::Range {
        lower: Some(lower),
        operator,
        upper: Some(upper),
        ..
    } = expression
    else {
        panic!("expected range expression");
    };
    assert_eq!(operator, "..");
    assert!(matches!(
        lower.as_ref(),
        Expr::Group { inner, .. }
            if matches!(inner.as_ref(), Expr::Call { callee, .. }
                if matches!(callee.as_ref(), Expr::Field { base, name, .. }
                    if name == "lookup"
                        && matches!(base.as_ref(), Expr::Name { text, .. } if text == "Stock")))
    ));
    assert!(matches!(
        upper.as_ref(),
        Expr::Literal { text, .. } if text == "2"
    ));
}

#[test]
fn relation_integer_aggregate_lowering_requires_unshadowed_root_bindings() {
    for operation in ["map", "min", "max", "sum"] {
        let terminal = match operation {
            "min" | "max" => format!("{operation}()"),
            _ => operation.into(),
        };
        let source = format!(
            r#"
                    pub table Reading(id: Int) {{ value: Int, }}
                    fn {operation}(values: Int) = 99;
                    fn total() = Reading | map(reading => reading.value) | {terminal};
            "#
        );
        let parsed = parse_module(&source);
        assert!(parsed.is_ok(), "{operation}: {:?}", parsed.diagnostics);
        let admitted = admitted_transaction_module(&parsed.value.items, None, None, false)
            .expect("valid source");
        let lowered = lower_relation_bindings(
            &admitted.functions,
            &admitted.key_fields,
            &admitted.float_fields,
            &admitted.table_fields,
        );

        assert!(
            matches!(&lowered.get("total").expect("total is lowered").body, Expr::Binary { op, .. } if op == "|"),
            "{operation} shadow was lowered: {:?}",
            lowered.get("total").expect("total is lowered").body
        );
    }
}

#[test]
fn specialized_relation_lowering_respects_declarations_and_bindings() {
    for source in [
        r#"
                pub table Stock(location: Str, sku: Str) { quantity: Int, }
                fn filter(value: Int) = 99;
                fn one(value: Int) = 99;
                fn count(value: Int) = 99;
                fn window(value: Int) = 99;
                fn lookup_case() = Stock | filter(stock => stock.location == "north" && stock.sku == "pencil") | one();
                fn filtered_one_case() = Stock | filter(stock => stock.quantity == 12) | one();
                fn filter_count_case() = Stock | filter(stock => stock.quantity == 12) | count;
                fn count_case() = Stock | count;
                fn window_case() = Stock | window(2) | count;
        "#,
        r#"
                pub table Stock(location: Str, sku: Str) { quantity: Int, }
                fn lookup_case(filter: Int, one: Int) = Stock | filter(stock => stock.location == "north" && stock.sku == "pencil") | one();
                fn filtered_one_case(filter: Int, one: Int) = Stock | filter(stock => stock.quantity == 12) | one();
                fn filter_count_case(filter: Int, count: Int) = Stock | filter(stock => stock.quantity == 12) | count;
                fn count_case(count: Int) = Stock | count;
                fn window_case(window: Int) = Stock | window(2) | count;
        "#,
    ] {
        let parsed = parse_module(source);
        assert!(parsed.is_ok(), "{:?}", parsed.diagnostics);
        let admitted = admitted_transaction_module(&parsed.value.items, None, None, false)
            .expect("valid source");
        let lowered = lower_relation_bindings(
            &admitted.functions,
            &admitted.key_fields,
            &admitted.float_fields,
            &admitted.table_fields,
        );
        for name in [
            "lookup_case",
            "filtered_one_case",
            "filter_count_case",
            "count_case",
            "window_case",
        ] {
            assert!(
                matches!(&lowered.get(name).expect("case is lowered").body, Expr::Binary { op, .. } if op == "|"),
                "{name} was specialized despite lexical shadowing: {:?}",
                lowered.get(name).expect("case is lowered").body
            );
        }
    }
}

#[test]
fn admitted_assertions_retain_parser_metadata_until_validation() {
    let source = r#"
            pub table Note(id: Int) {
                text: Str,
                assert every(note => note.text != "");
            }
            assert every(Note, note => note.id == note.id);
    "#;
    let parsed = parse_module(source);
    assert!(parsed.is_ok(), "{:?}", parsed.diagnostics);

    let admitted =
        admitted_transaction_module(&parsed.value.items, Some("notes"), Some("notes"), false)
            .expect("valid assertions");
    let table_assertion = &admitted.table_assertions["Note"][0];
    let table_expression_span = table_assertion.expression.span();
    assert_eq!(table_assertion.owner_kind, AssertionOwnerKind::Table);
    assert_eq!(table_assertion.owner_name, "Note");
    assert_eq!(table_assertion.source_order, 1);
    assert!(table_assertion.declaration_span.start <= table_expression_span.start);
    assert!(table_assertion.declaration_span.end >= table_expression_span.end);

    let module_assertion = &admitted.module_assertions[0];
    assert_eq!(module_assertion.owner_kind, AssertionOwnerKind::Module);
    assert_eq!(module_assertion.owner_name, "notes");
    assert_eq!(module_assertion.source_order, 1);
    assert_eq!(
        module_assertion.declaration_span,
        module_assertion.expression.span()
    );
}

#[test]
fn relation_float_sum_returns_a_canonical_nan_from_a_canonical_nan_row() {
    let unit = SourceUnit {
        fixture_id: "relation-float-sum-nan".into(),
        source_id: "relation-float-sum-nan.orna".into(),
        parse_as: "module_unit".into(),
        source: r#"
                pub table Reading(id: Int) { value: Float, }
                fn total(): Float = Reading | map(reading => reading.value) | sum;
                fn parent() {
                    assert total() != total();
                    Reading.insert({ id: 2, value: total() });
                }
        "#
        .into(),
    };
    let mut evaluator = TransactionalEvaluator::new("parent", Limits::default());
    let canonical_nan = 0x7ff8_0000_0000_0000;
    let seeded_nan = Value::float_bits(0x7ff8_0000_0000_0001);
    assert_eq!(seeded_nan.raw(), &OvbRaw::Float(canonical_nan));
    let row = Value::new(OvbRaw::Map(vec![
        (OvbRaw::Text("id".into()), OvbRaw::Int(1.into())),
        (OvbRaw::Text("value".into()), seeded_nan.raw().clone()),
    ]))
    .expect("canonical seeded Float row");
    let key_schema = TransactionTableKey::new(
        "Reading".into(),
        vec!["id".into()],
        vec![TransactionKeyType::Int],
    )
    .expect("valid Reading key schema");
    let key_fields = BTreeMap::from([("Reading".into(), key_schema.clone())]);
    evaluator
        .seed_committed_from_admission(
            "Reading".into(),
            key_schema
                .key_from_values(&[Value::int(1.into())])
                .expect("canonical integer key")
                .as_ref()
                .to_vec(),
            row,
            &key_fields,
            &BTreeMap::new(),
            &BTreeMap::new(),
        )
        .expect("seeded relation row");

    let outcome = evaluator.execute_source(&unit);

    assert!(matches!(outcome, StageOutcome::Passed), "{outcome:?}");
    let row = evaluator
        .committed_row("Reading", &Value::int(2.into()))
        .expect("sum result row was committed");
    let OvbRaw::Map(fields) = row.raw() else {
        panic!("committed row is not a record");
    };
    let Some((_, OvbRaw::Float(bits))) = fields
        .iter()
        .find(|(key, _)| matches!(key, OvbRaw::Text(name) if name == "value"))
    else {
        panic!("committed row does not contain a Float value field");
    };
    assert_eq!(*bits, canonical_nan);
}
#[test]
fn source_relation_distinct_uses_lazy_plan_for_decimal_rows() {
    let unit = SourceUnit {
        fixture_id: "txn-relation-distinct".into(),
        source_id: "txn-relation-distinct.orna".into(),
        parse_as: "module_unit".into(),
        source: r#"
                pub table Reading(id: Int) { value: Decimal, }
                fn main() {
                    Reading.insert({ id: 1, value: 18.2500 });
                    Reading.insert({ id: 2, value: 2.0 });
                    Reading.insert({ id: 3, value: 18.25 });
                    assert Reading | map(reading => reading.value) | distinct() | count() == 2;
                }
        "#
        .into(),
    };
    let mut evaluator = TransactionalEvaluator::new("main", Limits::default());

    let outcome = evaluator.execute_source(&unit);

    assert!(matches!(outcome, StageOutcome::Passed), "{outcome:?}");
    for id in 1..=3 {
        assert!(
            evaluator
                .committed_row("Reading", &Value::int(id.into()))
                .is_some(),
            "distinct relation execution did not commit row {id}"
        );
    }
}
