use orna_semantic_v1::{
    Catalogue, DIAG_AMBIGUOUS, DIAG_ASSERTION, DIAG_ASSERTION_EFFECT, DIAG_ASSERTION_SCOPE,
    DIAG_RESERVED, DIAG_TYPE, DIAG_UNRESOLVED, DIAG_UNSUPPORTED, ModuleInput, Type, analyze,
    analyze_with_catalogue,
};

fn has(result: &orna_semantic_v1::Analysis, code: &str) -> bool {
    result
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code() == code)
}

#[test]
fn unicode_nfkc_casefold_sibling_collision_is_rejected() {
    let result = analyze(&[
        ModuleInput::new("ff/left.orna", ""),
        ModuleInput::new("ﬀ/right.orna", ""),
    ]);
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == "ORNA-S002-NAMESPACE")
    );
}

#[test]
fn graph_resolution_keeps_explicit_imports_over_globs_and_rejects_module_assertion_execution() {
    let result = analyze(&[
        ModuleInput::new("left.orna", "pub fn pick(): Int = 1;"),
        ModuleInput::new("right.orna", "pub fn pick(): Int = 2;"),
        ModuleInput::new(
            "consumer.orna",
            "use sys as system; use left.{pick}; use right.*; fn chosen() = pick(); assert true;",
        ),
    ]);

    assert!(!has(&result, DIAG_AMBIGUOUS));
    assert!(has(&result, DIAG_ASSERTION_SCOPE));
}

#[test]
fn qualified_module_member_calls_resolve_only_public_imported_exports() {
    let result = analyze(&[
        ModuleInput::new(
            "library.orna",
            "pub fn seed(): Int = 1; fn hidden(): Int = 2;",
        ),
        ModuleInput::new(
            "warehouse.orna",
            "pub fn transfer(from: Int, to: Int, amount: Int): Int = from + to + amount;",
        ),
        ModuleInput::new(
            "main.orna",
            "use library; use warehouse; fn seed() = library.seed(); fn move_stock() = warehouse.transfer(1, 2, 3);",
        ),
    ]);

    assert!(result.is_ok(), "{:?}", result.diagnostics);

    let private = analyze(&[
        ModuleInput::new("library.orna", "fn hidden(): Int = 2;"),
        ModuleInput::new("main.orna", "use library; fn f() = library.hidden();"),
    ]);
    assert!(has(&private, DIAG_UNRESOLVED));

    let missing = analyze(&[
        ModuleInput::new("library.orna", "pub fn seed(): Int = 1;"),
        ModuleInput::new("main.orna", "use library; fn f() = library.missing();"),
    ]);
    assert!(has(&missing, DIAG_UNRESOLVED));
}

#[test]
fn qualified_table_operations_infer_rows_and_reach_block_expression_statements() {
    let result = analyze(&[
        ModuleInput::new("library.orna", "pub table Book(id: Str) { title: Str, }"),
        ModuleInput::new(
            "main.orna",
            r#"
                use library;
                table Stock(id: Str) { quantity: Int, }
                table Reading(id: Str) { value: Int, }
                fn seed() {
                    library.Book.insert({ id: "book-1", title: "The Night Garden" });
                    Stock.insert({ id: "north", quantity: 12 });
                    Stock.update("north", { quantity: 9 });
                    Reading.delete("sample-1");
                }
                fn read() = Stock.one();
                fn first() = Reading.first();
                fn count(): Int = Reading.count();
            "#,
        ),
    ]);

    assert!(result.is_ok(), "{:?}", result.diagnostics);

    let wrong_arity = analyze(&[ModuleInput::new(
        "books.orna",
        "pub table Book(id: Str) { title: Str, } fn bad() = Book.delete();",
    )]);
    assert!(has(&wrong_arity, "ORNA-S021-TYPE"));

    let non_table = analyze(&[ModuleInput::new(
        "books.orna",
        "type Book; fn bad() = Book.insert({ id: \"book-1\" });",
    )]);
    assert!(has(&non_table, "ORNA-S021-TYPE"));
}

#[test]
fn table_assertion_rejects_authoritative_std_net_effect() {
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "consumer.orna",
            "pub table User(id: Uuid) { name: Str, assert std.net.http.get(\"https://example.com\") == \"ok\"; }",
        )],
        &Catalogue::authoritative_core(),
    );

    assert!(has(&result, DIAG_ASSERTION_EFFECT));
}

#[test]
fn table_assertion_rejects_owner_type_mismatch() {
    let result = analyze(&[ModuleInput::new(
        "books.orna",
        "pub table User(id: Uuid) { name: Str, assert >= 0; }",
    )]);

    assert!(has(&result, DIAG_ASSERTION));
    assert!(result.diagnostics.iter().any(|diagnostic| {
        diagnostic.code() == DIAG_ASSERTION
            && diagnostic.message() == "table assertion must be a predicate over Relation<User>"
    }));
}

#[test]
fn table_assertion_elaborates_reference_relation_predicates_without_an_evaluator() {
    let result = analyze(&[ModuleInput::new(
        "library.orna",
        r#"
            pub table Book(id: Str) {
                title: Str,
                assert every(book => book.title != "");
            }
            pub table Loan(book_id: Str) {
                borrower: Str,
                assert all_unique(loan => loan.borrower);
            }
        "#,
    )]);

    assert!(result.is_ok(), "{:?}", result.diagnostics);
}

#[test]
fn module_assertion_elaborates_the_reference_projects_nested_relation_predicate() {
    let result = analyze(&[ModuleInput::new(
        "library.orna",
        r#"
            pub table Book(id: Str) { title: Str, }
            pub table Loan(book_id: Str) { borrower: Str, }
            assert every(Loan, loan =>
                exists(Book, book => book.id == loan.book_id)
            );
        "#,
    )]);

    assert!(result.is_ok(), "{:?}", result.diagnostics);
}

#[test]
fn authoritative_core_catalogue_resolves_prelude_types_and_common_functions() {
    let profile = Catalogue::authoritative_core();
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "consumer.orna",
            "use std as _; use std.math.{increment, is_zero}; use std.ui.{text}; use std.json.{encode}; fn next(value: INTEGER): INTEGER = increment(value); fn zero(): BOOLEAN = is_zero(0); fn view(): UI = text(\"hello\"); fn bytes(value: JsonValue): ByteStream = encode(value);",
        )],
        &profile,
    );

    assert!(result.is_ok(), "{:?}", result.diagnostics);
}

#[test]
fn authoritative_core_resolves_text_key_helpers() {
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "keys.orna",
            "use std.text.{slug, disambiguate}; fn key(name: Str, keys: JsonValue): Str = disambiguate(slug(name), keys);",
        )],
        &Catalogue::authoritative_core(),
    );

    assert!(result.is_ok(), "{:?}", result.diagnostics);
}

#[test]
fn authoritative_core_resolves_nested_operations_through_an_imported_root() {
    let profile = Catalogue::authoritative_core();
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "consumer.orna",
            "use std as core; fn document(rows: Rows): Document = core.terminal.present_table(rows); fn bytes(value: JsonValue): ByteStream = core.json.encode(value);",
        )],
        &profile,
    );

    assert!(result.is_ok(), "{:?}", result.diagnostics);

    let missing = analyze_with_catalogue(
        &[ModuleInput::new(
            "consumer.orna",
            "use std as core; fn f() = core.terminal.missing();",
        )],
        &profile,
    );
    assert!(has(&missing, DIAG_UNRESOLVED));
}

#[test]
fn refined_aliases_check_owner_assertions_and_expose_static_constructors() {
    let result = analyze(&[ModuleInput::new(
        "ports.orna",
        r#"
            type Port = Int {
                assert >= 1;
                assert <= 65_535;
            }
            pub fn default_port(): Port = Port.from(8080);
        "#,
    )]);

    assert!(result.is_ok(), "{:?}", result.diagnostics);
    let module = result.modules.values().next().expect("module header");
    assert_eq!(
        module.symbols.get("default_port").expect("function").ty,
        Type::Function {
            parameters: vec![],
            parameter_names: Some(vec![]),
            result: Box::new(Type::Named("Port".into())),
        }
    );
    assert_eq!(
        result.assertions.values().next().expect("plans"),
        &vec![
            orna_semantic_v1::AssertionPlan {
                owner: orna_semantic_v1::AssertionOwner::RefinedType("Port".into()),
                dependencies: Default::default(),
                effects: Default::default(),
            },
            orna_semantic_v1::AssertionPlan {
                owner: orna_semantic_v1::AssertionOwner::RefinedType("Port".into()),
                dependencies: Default::default(),
                effects: Default::default(),
            },
        ]
    );
}

#[test]
fn catalogue_is_closed_world_and_diagnostics_remain_redacted_and_stable() {
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "secret.orna",
            "use std as _; fn f() = definitely_not_in_the_catalogue;",
        )],
        &Catalogue::authoritative_core(),
    );

    assert!(has(&result, DIAG_UNRESOLVED));
    let json = serde_json::to_string(&result.diagnostics).unwrap();
    assert!(!json.contains("secret.orna"));
    assert!(!json.contains("definitely_not_in_the_catalogue"));
    assert_eq!(
        result
            .diagnostics
            .iter()
            .map(|d| d.code())
            .collect::<Vec<_>>(),
        vec![DIAG_UNRESOLVED]
    );
}

#[test]
fn catalogue_does_not_relax_reserved_source_roots() {
    let result = analyze_with_catalogue(
        &[ModuleInput::new("std/main.orna", "")],
        &Catalogue::authoritative_core(),
    );

    assert!(has(&result, DIAG_RESERVED));
}

#[test]
fn contextual_numeric_and_exact_money_unit_postfixes_remain_closed() {
    let result = analyze(&[ModuleInput::new(
        "literals.orna",
        r#"
            pub protocol Currency { static code: Str; static minor_digits: Int; }
            pub type GBP { impl Currency { static code = "GBP"; static minor_digits = 2; } }
            pub fn decimal_value() = 3.1415;
            pub fn float_value(): Float = 3.1415;
            pub fn explicit_float() = 3.1415f;
            pub fn amount(): Money<GBP> = 12.34.GBP;
            pub fn converted(speed: Float<mph>) = speed.mph;
            pub fn duration() = 90.min;
            pub table Tariff(effective_from: Date) { rate: Money<GBP> / kWh, }
            pub fn cost(energy: Decimal<kWh>, tariff: Tariff) = energy * tariff.rate;
        "#,
    )]);

    assert!(result.is_ok(), "{:?}", result.diagnostics);

    let non_currency = analyze(&[ModuleInput::new(
        "literals.orna",
        "type NotCurrency; fn bad(): Money<NotCurrency> = 12.34.NotCurrency;",
    )]);
    assert!(has(&non_currency, DIAG_TYPE));

    let unsupported = analyze(&[ModuleInput::new(
        "rates.orna",
        "fn bad(energy: Decimal<kWh>, rate: Money<GBP> / hour) = energy * rate;",
    )]);
    assert!(has(&unsupported, DIAG_UNSUPPORTED));
}

#[test]
fn numeric_methods_and_relation_count_use_closed_intrinsic_shapes() {
    let result = analyze(&[ModuleInput::new(
        "intrinsics.orna",
        r#"
            pub table Note { text: Str, }
            pub fn exact_eighth(): Decimal = 1.decimal / 8.decimal;
            pub fn rounded_third(): Decimal = 1.decimal.divide(
                3.decimal,
                scale: 6,
                rounding: half_even,
            );
            pub fn current(): Instant = now();
            pub fn count_call(): Int = Note | count();
            pub fn count_name(): Int = Note | count;
        "#,
    )]);

    assert!(result.is_ok(), "{:?}", result.diagnostics);
}

#[test]
fn authoritative_core_exposes_implicit_encoding_and_duration_members() {
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "standard.orna",
            r#"
                pub fn json(value: Contact) = std.encoding.json.encode(value);
                pub fn canonical(value: Contact) = std.encoding.orna.encode(value);
                pub fn read(value: ByteStream) = std.encoding.json.decode(value, as: Contact);
                pub fn formats(duration: Duration) = {
                    compact: std.time.duration.compact.format(duration),
                    clock: std.time.duration.clock.format(duration),
                    words: std.time.duration.words.format(duration),
                    iso: std.time.duration.iso.format(duration),
                };
            "#,
        )],
        &Catalogue::authoritative_core(),
    );

    assert!(result.is_ok(), "{:?}", result.diagnostics);
}

#[test]
fn authoritative_core_types_locale_aware_money_pipeline() {
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "receipt.orna",
            "pub fn receipt_total(total: Money<GBP>, locale: Locale): Str = total | std.money.format(locale: locale);",
        )],
        &Catalogue::authoritative_core(),
    );

    assert!(result.is_ok(), "{:?}", result.diagnostics);

    let wrong_input = analyze_with_catalogue(
        &[ModuleInput::new(
            "receipt.orna",
            "pub fn receipt_total(total: Int, locale: Locale): Str = total | std.money.format(locale: locale);",
        )],
        &Catalogue::authoritative_core(),
    );
    assert!(has(&wrong_input, DIAG_TYPE));
}

#[test]
fn qualified_kwh_units_share_the_closed_cross_database_identity() {
    let result = analyze(&[ModuleInput::new(
        "units.orna",
        "pub fn compatible(a: Float<std.units.si.kWh>, b: Float<work.units.kWh>) = a + b;",
    )]);

    assert!(result.is_ok(), "{:?}", result.diagnostics);

    let incompatible = analyze(&[ModuleInput::new(
        "units.orna",
        "pub fn incompatible(a: Float<std.units.si.kWh>, b: Float<work.units.hour>) = a + b;",
    )]);
    assert!(has(&incompatible, DIAG_TYPE));
}

#[test]
fn closed_enum_case_blocks_accept_the_core_log_intrinsic() {
    let result = analyze(&[ModuleInput::new(
        "inspection.orna",
        r#"
            pub enum Inspection {
                value { value: Int },
                failed { reason: Str },
            }
            pub fn inspect(result: Inspection) =
                case result {
                    Inspection.value { value }: { value: value },
                    Inspection.failed { reason }: {
                        log(reason);
                        { value: 0 }
                    },
                };
        "#,
    )]);

    assert!(result.is_ok(), "{:?}", result.diagnostics);
}

#[test]
fn parenthesized_pipeline_lambdas_receive_the_input_type_context() {
    let result = analyze(&[ModuleInput::new(
        "contacts.orna",
        r#"
            pub table Contact(id: Str) { name: Str, }
            pub fn name(contact: Contact) = contact | (value => value.name);
        "#,
    )]);

    assert!(result.is_ok(), "{:?}", result.diagnostics);
}

#[test]
fn root_relation_and_stream_intrinsics_cover_reference_pipelines_without_execution() {
    let source = r#"
            pub table Book(id: Int) { title: Str, }
            pub table Loan(id: Int) { book_id: Int, }
            pub table Reading(id: Int) { value: Int, }
            fn available() = Book
                | filter(book => !exists(Loan, loan => loan.book_id == book.id))
                | one();
            fn ingest() {
                Stream.from_list([1], source_identity: "fixture") | for_each(value => {
                    Reading.insert({ id: value, value: value });
                });
            }
        "#;
    let result = analyze(&[ModuleInput::new("sensors.orna", source)]);

    assert!(result.is_ok(), "{:?}", result.diagnostics);
    let module = result
        .modules
        .values()
        .find(|module| module.namespace.display() == "sensors")
        .unwrap();
    let ingest = module.symbols.get("ingest").unwrap();
    assert!(ingest.effects.effects.contains("database write"));
    assert!(ingest.effects.may_fail);
}

#[test]
fn authoritative_named_pipeline_fixtures_insert_the_input_before_explicit_arguments() {
    let pipe_first_argument = r#"
        pub fn between(value: Int, min: Int, max: Int) =
            value >= min && value <= max;

        pub fn selected(value: Int) =
            value | between(10, 20);
    "#;
    let pipeline_precedence = r#"
        pub fn square(value: Int) = value * value;
        pub fn count_is_positive(values: [Int]) = values | count > 0;
        pub fn square_sum(a: Int, b: Int) = a + b | square;
        pub fn increment_count(values: [Int]) = (values | count) + 1;
    "#;
    let precedence_source = pipeline_precedence.to_owned();

    let result = analyze(&[
        ModuleInput::new("pipe-first-argument.orna", pipe_first_argument),
        ModuleInput::new("pipeline-precedence.orna", precedence_source),
    ]);

    assert!(result.is_ok(), "{:?}", result.diagnostics);
    let precedence = result
        .modules
        .values()
        .find(|module| module.namespace.display() == "pipeline-precedence")
        .unwrap();
    assert!(matches!(
        &precedence.symbols["square_sum"].ty,
        Type::Function { result, .. } if result.as_ref() == &Type::Int
    ));
    assert!(matches!(
        &precedence.symbols["increment_count"].ty,
        Type::Function { result, .. } if result.as_ref() == &Type::Int
    ));
}

#[test]
fn generic_and_table_pipeline_stages_remain_fail_closed() {
    let result = analyze(&[ModuleInput::new(
        "unsupported.orna",
        "table Books(id: Int) { title: Str, } fn use_books() = 1 | Books;",
    )]);

    assert!(has(&result, DIAG_UNSUPPORTED));
}

#[test]
fn stream_from_list_requires_the_closed_named_identity_argument() {
    let result = analyze(&[ModuleInput::new(
        "invalid.orna",
        "fn input() = Stream.from_list([1], identity: \"fixture\");",
    )]);

    assert!(has(&result, DIAG_TYPE));
}

#[test]
fn authoritative_ranges_fixture_accepts_only_numeric_membership_and_list_take() {
    let result = analyze(&[ModuleInput::new(
        "ranges.orna",
        r#"
            pub fn inside(value: Int) = value in 1..=5;
            pub fn first_ten(values: [Int]) = values | take(0..10);
        "#,
    )]);

    assert!(result.is_ok(), "{:?}", result.diagnostics);
    let module = result.modules.values().next().unwrap();
    assert!(matches!(
        &module.symbols["inside"].ty,
        Type::Function { result, .. } if result.as_ref() == &Type::Bool
    ));
    assert!(matches!(
        &module.symbols["first_ten"].ty,
        Type::Function { result, .. }
            if result.as_ref() == &Type::List(Box::new(Type::Int))
    ));

    let invalid = analyze(&[
        ModuleInput::new("text-range.orna", "fn bad() = \"a\"..\"z\";"),
        ModuleInput::new(
            "table-take.orna",
            "table Reading(id: Int) { value: Int, } fn bad() = Reading | take(0..10);",
        ),
    ]);
    assert!(has(&invalid, DIAG_TYPE));
    assert!(has(&invalid, DIAG_UNSUPPORTED));
}

#[test]
fn affine_collection_aggregates_preserve_absolute_values_and_reject_sum() {
    let valid = analyze(&[
        ModuleInput::new(
            "maximum.orna",
            "pub fn hottest(values: [Float<C>]) = values | max;",
        ),
        ModuleInput::new(
            "average.orna",
            "pub fn average_temperature(values: [Float<C>]) = values | mean;",
        ),
    ]);
    assert!(valid.is_ok(), "{:?}", valid.diagnostics);

    let invalid = analyze(&[ModuleInput::new(
        "sum.orna",
        "pub fn bad(values: [Float<C>]) = values | sum;",
    )]);
    assert!(has(&invalid, DIAG_TYPE));
    assert!(!has(&invalid, DIAG_UNSUPPORTED));
}

#[test]
fn inferred_function_summaries_propagate_through_project_calls_independent_of_input_order() {
    let result = analyze(&[
        ModuleInput::new(
            "main.orna",
            "use sensors; pub fn run() { sensors.ingest(); }",
        ),
        ModuleInput::new(
            "sensors.orna",
            r#"
                pub type Sample {
                    pub sensor: Str,
                    pub sequence: Int,
                    pub value: Decimal,
                }
                pub table Reading(sensor: Str, sequence: Int) { value: Decimal, }
                pub fn input() = Stream.from_list([
                    Sample { sensor: "greenhouse", sequence: 0, value: 18.25 },
                ], source_identity: "example:sensors:v1");
                pub fn ingest() {
                    input() | for_each(sample => {
                        Reading.insert({
                            sensor: sample.sensor,
                            sequence: sample.sequence,
                            value: sample.value,
                        });
                    });
                }
            "#,
        ),
    ]);

    assert!(result.is_ok(), "{:?}", result.diagnostics);
    let sensors = result
        .modules
        .values()
        .find(|module| module.namespace.display() == "sensors")
        .unwrap();
    assert!(matches!(
        &sensors.symbols["input"].ty,
        Type::Function { result, .. } if matches!(result.as_ref(), Type::Stream(_))
    ));
    assert!(
        sensors.symbols["ingest"]
            .effects
            .effects
            .contains("database write")
    );
    assert!(sensors.symbols["ingest"].effects.may_fail);

    let main = result
        .modules
        .values()
        .find(|module| module.namespace.display().is_empty())
        .unwrap();
    assert!(
        main.symbols["run"]
            .effects
            .effects
            .contains("database write")
    );
    assert!(main.symbols["run"].effects.may_fail);
}

#[test]
fn reference_values_module_infers_closed_enum_optional_and_interpolation_cases() {
    let result = analyze(&[ModuleInput::new(
        "values.orna",
        r#"
            pub type Score = Int {
                assert >= 0;
                assert <= 100;
            }

            pub enum Availability {
                ready,
                waiting { reason: Str },
            }

            pub fn describe(value: Availability): Str = case value {
                Availability.ready: "ready",
                Availability.waiting { reason }: "waiting: {reason}",
            };

            pub fn optional_name(value: Str?): Str = case value {
                Some(name): name,
                null: "anonymous",
            };

            pub fn add(left: Int, right: Int): Int = left + right;
        "#,
    )]);

    assert!(result.is_ok(), "{:?}", result.diagnostics);
    let values = result.modules.values().next().unwrap();
    assert!(matches!(
        &values.symbols["describe"].ty,
        Type::Function { result, .. } if result.as_ref() == &Type::Text
    ));
    assert!(matches!(
        &values.symbols["optional_name"].ty,
        Type::Function { parameters, result, .. }
            if parameters == &[Type::Optional(Box::new(Type::Text))]
                && result.as_ref() == &Type::Text
    ));
}

#[test]
fn case_inference_rejects_non_exhaustive_and_malformed_reference_patterns() {
    let non_exhaustive = analyze(&[ModuleInput::new(
        "values.orna",
        r#"
            enum Availability { ready, waiting { reason: Str }, }
            fn describe(value: Availability): Str = case value {
                Availability.ready: "ready",
            };
        "#,
    )]);
    assert!(has(&non_exhaustive, DIAG_TYPE));

    let malformed_enum = analyze(&[ModuleInput::new(
        "values.orna",
        r#"
            enum Availability { ready, waiting { reason: Str }, }
            fn describe(value: Availability): Str = case value {
                Availability.ready: "ready",
                Availability.waiting(reason): reason,
            };
        "#,
    )]);
    assert!(has(&malformed_enum, DIAG_UNSUPPORTED));

    let malformed_optional = analyze(&[ModuleInput::new(
        "values.orna",
        r#"
            fn optional_name(value: Str?): Str = case value {
                Some(): "missing",
                null: "anonymous",
            };
        "#,
    )]);
    assert!(has(&malformed_optional, DIAG_TYPE));

    let non_text_interpolation = analyze(&[ModuleInput::new(
        "values.orna",
        "fn label(): Str = \"value: {1}\";",
    )]);
    assert!(has(&non_text_interpolation, DIAG_TYPE));
}

#[test]
fn case_arms_preserve_call_effects_and_named_calls_use_declared_parameter_names() {
    let result = analyze(&[ModuleInput::new(
        "values.orna",
        r#"
            table Reading(id: Str) { value: Int, }
            enum Availability { ready, waiting { reason: Str }, }

            fn touch(value: Str): Str {
                Reading.count();
                value
            }
            fn describe(value: Availability): Str = case value {
                Availability.ready: touch(value: "ready"),
                Availability.waiting { reason }: touch(value: reason),
            };
            fn add(left: Int, right: Int): Int = left + right;
            fn total(): Int = add(right: 2, left: 1);
        "#,
    )]);

    assert!(result.is_ok(), "{:?}", result.diagnostics);
    let values = result.modules.values().next().unwrap();
    assert!(
        values.symbols["describe"]
            .effects
            .effects
            .contains("database read")
    );
    assert!(values.symbols["describe"].effects.may_fail);

    let malformed = analyze(&[ModuleInput::new(
        "values.orna",
        r#"
            fn add(left: Int, right: Int): Int = left + right;
            fn bad(): Int = add(left: 1, left: 2);
        "#,
    )]);
    assert!(has(&malformed, DIAG_TYPE));
}

#[test]
fn control_flow_infers_list_for_and_local_assignment_while_other_shapes_fail_closed() {
    let fixture = analyze(&[ModuleInput::new(
        "control-flow.orna",
        r#"
            pub fn describe(values: [Int]): Str {
                let total = 0;

                for value in values {
                    total = total + value;
                }

                if total > 100 {
                    "large"
                } else {
                    "small"
                }
            }
        "#,
    )]);
    let module = fixture.modules.values().next().unwrap();
    assert!(matches!(
        &module.symbols["describe"].ty,
        Type::Function { result, .. } if result.as_ref() == &Type::Text
    ));
    assert!(fixture.is_ok(), "{:?}", fixture.diagnostics);

    let mismatched_branches = analyze(&[ModuleInput::new(
        "mismatched-if.orna",
        "fn choose(value: Int): Str { if value > 0 { \"positive\" } else { 0 } }",
    )]);
    assert!(has(&mismatched_branches, DIAG_TYPE));

    let missing_else = analyze(&[ModuleInput::new(
        "missing-else.orna",
        "fn choose(value: Int): Str { if value > 0 { \"positive\" } }",
    )]);
    assert!(has(&missing_else, DIAG_UNSUPPORTED));

    let compound_assignment = analyze(&[ModuleInput::new(
        "compound-assignment.orna",
        "fn update() { let value = 0; value += 1; }",
    )]);
    assert!(
        compound_assignment.is_ok(),
        "{:?}",
        compound_assignment.diagnostics
    );

    let mismatched_compound = analyze(&[ModuleInput::new(
        "mismatched-compound-assignment.orna",
        "fn update() { let value = 0; value += true; }",
    )]);
    assert!(has(&mismatched_compound, DIAG_TYPE));

    let field_assignment = analyze(&[ModuleInput::new(
        "field-assignment.orna",
        "fn update() { let value = { count: 0 }; value.count = 1; }",
    )]);
    assert!(has(&field_assignment, DIAG_UNSUPPORTED));

    let non_list_for = analyze(&[ModuleInput::new(
        "non-list-for.orna",
        "fn update() { for value in 1 { value } }",
    )]);
    assert!(has(&non_list_for, DIAG_UNSUPPORTED));
}

#[test]
fn coalesce_types_optional_values_with_precedence_and_grouping() {
    let valid = analyze(&[
        ModuleInput::new(
            "coalesce-precedence.orna",
            r#"
                pub fn threshold(value: Int?, days: Int?) = {
                    above: value ?? 0 > 5,
                    default_days: days ?? 90 == 90,
                };
            "#,
        ),
        ModuleInput::new(
            "grouped-coalesce.orna",
            "pub fn value(input: Int?): Int = (input ?? 0);",
        ),
    ]);
    assert!(valid.is_ok(), "{:?}", valid.diagnostics);

    let incompatible = analyze(&[ModuleInput::new(
        "incompatible-coalesce.orna",
        "pub fn bad(input: Int?): Int = input ?? \"fallback\";",
    )]);
    assert!(has(&incompatible, DIAG_TYPE));
}
