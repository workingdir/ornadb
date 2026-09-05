use orna_semantic_v1::{
    Catalogue, DIAG_AMBIGUOUS, DIAG_ASSERTION, DIAG_ASSERTION_EFFECT, DIAG_ASSERTION_ONE_TABLE,
    DIAG_ASSERTION_SCOPE, DIAG_LEGACY_SYS_RUNTIME, DIAG_LEGACY_TRYFROM, DIAG_RESERVED, DIAG_TYPE,
    DIAG_UNRESOLVED, DIAG_UNSUPPORTED, ModuleInput, Type, analyze, analyze_with_catalogue,
};

fn has(result: &orna_semantic_v1::Analysis, code: &str) -> bool {
    result
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code() == code)
}

#[test]
fn closure_lists_do_not_erase_incompatible_return_types() {
    for source in [
        "pub fn values() = [() => 1, () => true];",
        "pub fn values() = [() => ({}), () => {}];",
    ] {
        let result = analyze(&[ModuleInput::new("closures.orna", source)]);
        assert!(has(&result, DIAG_TYPE), "{:?}", result.diagnostics);
    }
    let compatible = analyze(&[ModuleInput::new(
        "closures.orna",
        "pub fn values() = [(x: Int) => x + 1, (y: Int) => y + 2];",
    )]);
    assert!(compatible.is_ok(), "{:?}", compatible.diagnostics);
}

#[test]
fn failure_skip_requires_a_typed_version_precondition() {
    for arguments in [
        "failure.reference, expected_status: failure.status, reason: reason",
        "failure.reference, expected_version: 1, expected_status: failure.status, reason: reason",
    ] {
        let source = format!(
            "pub fn skip(failure: sys.Failure, reason: Str) = sys.admin.skip_failure({arguments});"
        );
        let result = analyze(&[ModuleInput::new("skip.orna", source)]);
        assert!(has(&result, DIAG_TYPE), "{:?}", result.diagnostics);
    }
    let valid = analyze(&[ModuleInput::new(
        "skip.orna",
        "pub fn skip(failure: sys.Failure, reason: Str) = sys.admin.skip_failure(failure.reference, expected_version: failure.version, expected_status: failure.status, reason: reason);",
    )]);
    assert!(valid.is_ok(), "{:?}", valid.diagnostics);
    let symbol = &valid.modules[&orna_semantic_v1::Namespace(vec!["skip".into()])].exports["skip"];
    assert!(symbol.effects.effects.contains("admin"));
    assert!(symbol.effects.may_fail);
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
fn system_checkpoint_history_selectors_resolve_from_intrinsic_surface() {
    let result = analyze(&[ModuleInput::new(
        "sys-checkpoint.orna",
        "pub fn published() = sys.Checkpoint.as_of(HEAD);",
    )]);

    assert!(result.is_ok(), "{:?}", result.diagnostics);
}

#[test]
fn system_run_history_sorting_resolves_system_row_fields() {
    let result = analyze(&[ModuleInput::new(
        "sys-run-history.orna",
        "pub fn committed_runs() = sys.Run.as_of(HEAD) | sort_by(run => -run.started);",
    )]);

    assert!(result.is_ok(), "{:?}", result.diagnostics);
}

#[test]
fn system_storage_relation_filters_typed_status_fields() {
    let result = analyze(&[ModuleInput::new(
        "sys-storage.orna",
        "pub fn pending() = sys.Storage | filter(storage => storage.pending_rows > 0);",
    )]);

    assert!(result.is_ok(), "{:?}", result.diagnostics);
}

#[test]
fn system_file_history_uses_the_file_reference_overload() {
    let result = analyze(&[ModuleInput::new(
        "sys-file-history.orna",
        "pub fn history(file: sys.File) = sys.history(file.reference);",
    )]);

    assert!(result.is_ok(), "{:?}", result.diagnostics);
}

#[test]
fn system_catalogue_relations_support_typed_filter_and_map_queries() {
    let result = analyze(&[
        ModuleInput::new(
            "sys-definition-file.orna",
            "pub fn source_files(function: sys.Function) = sys.catalog.definitions | filter(definition => definition.reference == function.definition) | map(definition => definition.file);",
        ),
        ModuleInput::new(
            "sys-table-query.orna",
            "pub fn inventory_table_objects() = sys.catalog.objects | filter(object => object.kind == sys.ObjectKind.table && object.qualified_name.starts_with(\"inventory.\"));",
        ),
    ]);

    assert!(result.is_ok(), "{:?}", result.diagnostics);
}

#[test]
fn table_projection_stages_type_computed_and_default_fields() {
    let source = r#"
        pub table Contact(id: Str) {
            first: Str,
            last: Str,
            country: Str = "GB",
            full_name: Str => "{first} {last}",
        }

        pub fn names() = Contact | map(Contact.full_name);
    "#;
    let filtered = r#"
        pub table Contact(id: Str) {
            first: Str,
            last: Str,
            country: Str = "GB",
            full_name: Str => "{first} {last}",
        }

        pub fn british_names() =
            Contact
            | filter(contact => contact.country == "GB")
            | map(Contact.full_name);
    "#;
    let result = analyze(&[
        ModuleInput::new("computed-field.orna", source),
        ModuleInput::new("default-and-computed-fields.orna", filtered),
    ]);

    assert!(result.is_ok(), "{:?}", result.diagnostics);
}

#[test]
fn system_dependency_queries_use_object_references() {
    let result = analyze(&[ModuleInput::new(
        "dependency-query.orna",
        "pub fn impact(object: sys.Object) = sys.dependents(object.reference);",
    )]);

    assert!(result.is_ok(), "{:?}", result.diagnostics);
}

#[test]
fn system_snapshot_selectors_accept_revision_strings() {
    let result = analyze(&[ModuleInput::new(
        "historical-query.orna",
        "pub fn before_change() = sys.snapshot(\"HEAD~3\");",
    )]);

    assert!(result.is_ok(), "{:?}", result.diagnostics);
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
    assert!(result.diagnostics.iter().any(|diagnostic| {
        diagnostic.message() == "declaration assertion uses forbidden network effect"
    }));
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
fn legacy_table_assertion_owner_pipes_keep_published_diagnostics() {
    let owner = analyze(&[ModuleInput::new(
        "owner-pipe.orna",
        "pub table User(id: Uuid) { username: Str, assert User | all_unique(user => user.username); }",
    )]);
    assert!(!has(&owner, DIAG_UNRESOLVED));
    assert!(owner.diagnostics.iter().any(|diagnostic| {
        diagnostic.message() == "remove the repeated table owner before the assertion predicate"
    }));

    let self_pipe = analyze(&[ModuleInput::new(
        "self-pipe.orna",
        "pub table User(id: Uuid) { username: Str, assert self | all_unique(user => user.username); }",
    )]);
    assert!(!has(&self_pipe, DIAG_UNRESOLVED));
    assert!(self_pipe.diagnostics.iter().any(|diagnostic| {
        diagnostic.message() == "remove `self |`; the table already supplies its candidate relation"
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
fn reserved_table_names_keep_typecheck_diagnostics() {
    let std_table = analyze(&[ModuleInput::new(
        "reserved-std.orna",
        "pub table std { value: Int, }",
    )]);
    assert!(
        std_table
            .diagnostics
            .iter()
            .any(|diagnostic| { diagnostic.message() == "`std` is reserved" })
    );

    let sys_table = analyze(&[ModuleInput::new(
        "reserved-sys.orna",
        "pub table sys { value: Int, }",
    )]);
    assert!(
        sys_table
            .diagnostics
            .iter()
            .any(|diagnostic| { diagnostic.message() == "`sys` is reserved" })
    );
}

#[test]
fn authoritative_ui_catalogue_checks_page_builder_contextually() {
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "page.orna",
            r#"
                use std.ui.*;
                pub fn values_page(values: [Str]) =
                    Page("/values", _ => List(values));
            "#,
        )],
        &Catalogue::authoritative_core(),
    );

    assert!(result.is_ok(), "{:?}", result.diagnostics);
    let module = result.modules.values().next().expect("page module");
    assert_eq!(
        module.symbols.get("values_page").expect("page function").ty,
        Type::Function {
            parameters: vec![Type::List(Box::new(Type::Text))],
            parameter_names: Some(vec!["values".into()]),
            result: Box::new(Type::Named("std.UI".into())),
        }
    );
}

#[test]
fn generic_ordering_pipeline_keeps_element_and_optional_types() {
    let result = analyze(&[ModuleInput::new(
        "generic.orna",
        r#"
            pub protocol Order {
                fn compare(self, other: Self): Ordering;
            }

            pub fn maximum<T impl Order>(values: [T]): T? =
                values | sort_by(value => value) | last();
        "#,
    )]);

    assert!(result.is_ok(), "{:?}", result.diagnostics);
    let module = result.modules.values().next().expect("generic module");
    assert_eq!(
        module.symbols.get("maximum").expect("maximum function").ty,
        Type::Function {
            parameters: vec![Type::List(Box::new(Type::Named("T".into())))],
            parameter_names: Some(vec!["values".into()]),
            result: Box::new(Type::Optional(Box::new(Type::Named("T".into())))),
        }
    );
}

#[test]
fn omitted_numeric_function_parameters_are_inferred_without_dynamic_fallback() {
    let result = analyze(&[ModuleInput::new(
        "inferred.orna",
        "pub fn square(value) = value * value;",
    )]);

    assert!(result.is_ok(), "{:?}", result.diagnostics);
    let module = result.modules.values().next().expect("inferred module");
    assert_eq!(
        module.symbols.get("square").expect("square function").ty,
        Type::Function {
            parameters: vec![Type::Int],
            parameter_names: Some(vec!["value".into()]),
            result: Box::new(Type::Int),
        }
    );
}

#[test]
fn table_keys_reject_float_and_affine_temperatures_reject_addition() {
    let key = analyze(&[ModuleInput::new(
        "key.orna",
        "pub table Bad(value: Float) { text: Str, }",
    )]);
    assert!(has(&key, DIAG_TYPE));

    let temperature = analyze(&[ModuleInput::new(
        "temperature.orna",
        "pub fn bad() = 20.C + 5.C;",
    )]);
    assert!(has(&temperature, DIAG_TYPE));
}

#[test]
fn table_keys_reject_ranges_with_the_published_primary_key_rule() {
    let result = analyze(&[ModuleInput::new(
        "range-key.orna",
        "pub table Bad(period: Range<Date>) { value: Str, }",
    )]);

    assert!(result.diagnostics.iter().any(|diagnostic| {
        diagnostic.message() == "Range<T> is not a primary-key type in version 1.0"
    }));
}

#[test]
fn automatic_key_tables_reject_explicit_rekey_operations() {
    let result = analyze(&[ModuleInput::new(
        "rekey.orna",
        "pub table Note { text: Str, } pub fn bad() = Note.rekey(1, 2);",
    )]);

    assert!(result.diagnostics.iter().any(|diagnostic| {
        diagnostic.message() == "an automatic-key table cannot be explicitly re-keyed"
    }));
}

#[test]
fn display_implementations_reject_database_writes() {
    let result = analyze(&[ModuleInput::new(
        "display.orna",
        r#"
            pub type Contact {
                pub name: Str,
                impl Display {
                    fn display(self, context: DisplayContext): Str {
                        Audit.insert({ message: "displayed contact" });
                        self.name
                    }
                }
            }
        "#,
    )]);

    assert!(result.diagnostics.iter().any(|diagnostic| {
        diagnostic.message() == "Display and Present implementations must be read-only"
    }));
}

#[test]
fn secret_values_reject_display_after_authoritative_open() {
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "secret.orna",
            "pub fn bad() = std.secret.open(std.secret.ref(\"x\"), as: Str).display();",
        )],
        &Catalogue::authoritative_core(),
    );

    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| { diagnostic.message() == "secret values cannot be displayed" })
    );
}

#[test]
fn computed_fields_reject_effectful_initializers() {
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "contact.orna",
            r#"
                pub table Contact(id: Str) {
                    name: Str,
                    remote: Str => std.net.http.get("https://example.com"),
                }
            "#,
        )],
        &Catalogue::authoritative_core(),
    );

    assert!(result.diagnostics.iter().any(|diagnostic| {
        diagnostic.message() == "computed field must be deterministic and row-local"
    }));
}

#[test]
fn system_commit_rows_reject_mutation() {
    let result = analyze(&[ModuleInput::new(
        "commit.orna",
        "pub fn bad() = sys.Commit.insert({ hash: \"x\" });",
    )]);

    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| { diagnostic.message() == "sys.Commit is read-only" })
    );
}

#[test]
fn published_money_and_affine_diagnostics_are_preserved() {
    let affine_sum = analyze(&[ModuleInput::new(
        "sum.orna",
        "pub fn bad(values: [Float<C>]) = values | sum;",
    )]);
    assert!(
        affine_sum
            .diagnostics
            .iter()
            .any(|diagnostic| { diagnostic.message() == "cannot sum absolute affine quantities" })
    );

    let currency_symbol = analyze(&[ModuleInput::new(
        "currency.orna",
        "pub protocol Currency { static code: Str; static symbol: Str; static minor_digits: Int; }",
    )]);
    assert!(currency_symbol.diagnostics.iter().any(|diagnostic| {
        diagnostic.message()
            == "currency symbols belong to locale-aware formatting, not Currency identity"
    }));

    let float_money = analyze(&[ModuleInput::new(
        "money.orna",
        "pub fn bad(energy: Float<kWh>, rate: Money<GBP> / kWh) = energy * rate;",
    )]);
    assert!(float_money.diagnostics.iter().any(|diagnostic| {
        diagnostic.message() == "binary Float cannot enter an exact Money calculation implicitly"
    }));

    let float_constructor = analyze(&[ModuleInput::new(
        "constructor.orna",
        "pub fn bad(value: Float) = Money<GBP>(value);",
    )]);
    assert!(float_constructor.diagnostics.iter().any(|diagnostic| {
        diagnostic.message()
            == "Money cannot be constructed from an inexact Float without explicit rounding"
    }));
}

#[test]
fn legacy_system_admin_methods_are_rejected_with_published_messages() {
    let result = analyze(&[ModuleInput::new(
        "legacy.orna",
        r#"
            pub fn reset(checkpoint: sys.Checkpoint, position: sys.CheckpointPosition) =
                checkpoint.reset(to: position);
            pub fn replay(failure: sys.Failure) = failure.replay();
            pub fn resolve(failure: sys.Failure) = failure.resolve();
            pub fn retry(stream: sys.Stream) = stream.retry();
            pub fn skip(stream: sys.Stream) = stream.skip(reason: "drop it");
        "#,
    )]);
    let messages = result
        .diagnostics
        .iter()
        .map(|diagnostic| diagnostic.message())
        .collect::<Vec<_>>();
    assert!(messages.contains(
        &"system rows are read-only; use `sys.admin.reset_checkpoint` with compare-and-set arguments"
    ));
    assert!(messages.contains(
        &"system rows are read-only; use `sys.admin.replay_failure(failure.reference, ...)`"
    ));
    assert!(messages.contains(
        &"system rows are read-only; use `sys.admin.resolve_failure(failure.reference, ...)`"
    ));
    assert!(messages.contains(
        &"system rows are read-only; use `sys.admin.retry_failure` on a `sys.FailureRef`"
    ));
    assert!(messages.contains(
        &"system rows are read-only; use `sys.admin.skip_failure` on a `sys.FailureRef`"
    ));
}

#[test]
fn distinct_nominal_types_require_named_conversions() {
    let result = analyze(&[ModuleInput::new(
        "conversion.orna",
        "pub fn bad(value: A): C = value;",
    )]);
    assert!(result.diagnostics.iter().any(|diagnostic| {
        diagnostic.message()
            == "implicit conversion chains are not searched; name each conversion explicitly"
    }));
}

#[test]
fn module_assertion_scope_distinguishes_zero_and_one_table_invariants() {
    let one_table = analyze(&[ModuleInput::new(
        "one.orna",
        "pub table User(id: Uuid) { name: Str, } assert every(User, user => user.name != \"\");",
    )]);
    assert!(has(&one_table, DIAG_ASSERTION_ONE_TABLE));
    assert!(one_table.diagnostics.iter().any(|diagnostic| {
        diagnostic.message() == "a one-table invariant belongs inside that table"
    }));

    let zero_table = analyze(&[ModuleInput::new("zero.orna", "assert 1 + 1 == 2;")]);
    assert!(has(&zero_table, DIAG_ASSERTION_SCOPE));
    assert!(zero_table.diagnostics.iter().any(|diagnostic| {
        diagnostic.message() == "module assertions must depend on at least two distinct tables"
    }));
}

#[test]
fn legacy_system_and_result_forms_keep_phase_specific_diagnostics() {
    let runtime = analyze(&[ModuleInput::new(
        "runtime.orna",
        "fn active_streams() { sys.runtime.streams }",
    )]);
    assert!(has(&runtime, DIAG_LEGACY_SYS_RUNTIME));
    assert!(
        runtime
            .diagnostics
            .iter()
            .any(|diagnostic| { diagnostic.message() == "`sys.runtime` was renamed to `sys.rt`" })
    );

    let storage = analyze(&[ModuleInput::new(
        "storage.orna",
        "pub fn bad() = sys.storage(contacts.Contact);",
    )]);
    assert!(storage.diagnostics.iter().any(|diagnostic| {
        diagnostic.message()
            == "`sys.storage` is a grouping namespace; use `sys.Storage` or `sys.admin` storage functions"
    }));

    let result = analyze(&[ModuleInput::new(
        "result.orna",
        "pub fn bad(raw: Str): Result<Message, DecodeError> = Ok(decode(raw));",
    )]);
    assert!(result.diagnostics.iter().any(|diagnostic| {
        diagnostic.message()
            == "Result/Ok/Err control plumbing was removed; return the success type directly"
    }));

    let try_from = analyze(&[ModuleInput::new(
        "try-from.orna",
        "pub type Port { impl TryFrom<Int> { fn from(value) = Port { value: value }; } }",
    )]);
    assert!(has(&try_from, DIAG_LEGACY_TRYFROM));
    assert!(
        try_from.diagnostics.iter().any(|diagnostic| {
            diagnostic.message() == "use From<Source>; From may fail in Orna"
        })
    );
}

#[test]
fn closed_literal_addition_diagnostics_preserve_published_meaning() {
    let currencies = analyze(&[ModuleInput::new(
        "currency.orna",
        "pub fn bad() = 10.GBP + 5.EUR;",
    )]);
    assert!(currencies.diagnostics.iter().any(|diagnostic| {
        diagnostic.message() == "cannot add different currencies without conversion"
    }));

    let dimensions = analyze(&[ModuleInput::new(
        "dimensions.orna",
        "pub fn bad() = 90.days + 4.kWh;",
    )]);
    assert!(
        dimensions
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message() == "cannot add Time and Energy")
    );
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
fn authoritative_fixture_resolves_attached_tables_connectors_and_modules() {
    let sources = [
        (
            "attached tables",
            ModuleInput::new(
                "attached_tables.orna",
                r#"
                    pub fn names() =
                        contacts.Contact
                        | filter(contact => (contact.emails | count) > 0)
                        | map(Contact.full_name)
                        | sort_by(value => value);
                    pub fn days() = energy.Reading | bucket_by(1.day, zone: Europe.London);
                "#,
            ),
        ),
        (
            "attached connectors",
            ModuleInput::new(
                "attached_connectors.orna",
                r#"
                    use vehicle.corsa.*;
                    pub fn recent() = Reading | filter(reading => reading.time > now() - 1.min) | last();
                    pub fn parallel() = std.concurrent.parallel([
                        mail.google.sync,
                        finance.openbanking.sync,
                        vehicle.corsa.freematics.sync,
                    ]);
                    pub fn source() = google.mail(credential: std.secret.ref("google.personal"));
                "#,
            ),
        ),
        (
            "attached csv",
            ModuleInput::new(
                "attached_csv.orna",
                r#"
                    pub fn import(path: Str) =
                        std.encoding.csv.rows(std.io.fs.read(path))
                        | for_each(row => Contact.insert(row));
                "#,
            ),
        ),
        (
            "named attached row",
            ModuleInput::new(
                "named_row.orna",
                r#"
                    pub table Vehicle(id: Uuid) {
                        registration: Str,
                        owner: contacts.Contact,
                    }
                    pub fn owner_name(vehicle: Vehicle) = vehicle.owner.name;
                "#,
            ),
        ),
        (
            "snapshot attached table",
            ModuleInput::new(
                "snapshot_table.orna",
                "pub fn counts() = { cwd: contacts.Contact.as_of(CWD) | count, head: contacts.Contact.as_of(HEAD) | count, };",
            ),
        ),
        (
            "system failure replay",
            ModuleInput::new(
                "system_runtime.orna",
                r#"
                    pub fn replay_mail_failure(source_identity: Str, partition: Str?, position_format: Str, position) {
                        let failure = sys.Failure | one(failure =>
                            failure.consumer == mail.google.sync
                            && failure.source_identity == source_identity
                            && failure.partition == partition
                            && failure.position_format == position_format
                            && failure.position == position
                        );
                        sys.admin.replay_failure(failure.reference, expected_status: failure.status)
                    }
                "#,
            ),
        ),
        (
            "system failure skip",
            ModuleInput::new(
                "system_runtime_skip.orna",
                r#"
                    pub fn skip_blocked_mail(reason: Str) {
                        let stream = sys.rt.streams | one(stream => stream.consumer == mail.google.sync);
                        let failure = sys.Failure | one(failure => failure.reference == stream.last_failure);
                        sys.admin.skip_failure(failure.reference, expected_version: failure.version, expected_status: failure.status, reason: reason)
                    }
                "#,
            ),
        ),
        (
            "system runtime presentation",
            ModuleInput::new(
                "system_runtime_presentation.orna",
                "pub fn dashboard() = std.ui.Page(\"/\", _ => std.ui.Table(sys.rt.streams));",
            ),
        ),
        (
            "historical database view",
            ModuleInput::new(
                "system_database.orna",
                r#"
                    pub fn compare_old_and_current() {
                        let old = sys.database.as_of(sys.snapshot("HEAD~10"));
                        { old: old.energy.daily(), current: energy.daily(energy.Reading.as_of(sys.snapshot("HEAD~10"))) }
                    }
                "#,
            ),
        ),
        (
            "inferred relation parameter",
            ModuleInput::new(
                "inferred_relation.orna",
                "pub fn recent(rows, duration = 7.days) = rows | filter(row => row.time >= now() - duration);",
            ),
        ),
        (
            "recovery lambda",
            ModuleInput::new(
                "recovery.orna",
                "pub fn decode_or_default(raw: Str): Message = std.encoding.json.decode(raw, as: Message) |? (failure => Message.default());",
            ),
        ),
    ];
    for (name, source) in sources {
        let result = analyze_with_catalogue(&[source], &Catalogue::authoritative_fixture());
        assert!(result.is_ok(), "{name}: {:?}", result.diagnostics);
    }
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
fn numeric_nested_lambdas_infer_omitted_parameters_without_dynamic_fallback() {
    let result = analyze(&[ModuleInput::new(
        "lambda.orna",
        "pub fn curried_add() = x => y => x + y;",
    )]);

    assert!(result.is_ok(), "{:?}", result.diagnostics);
    assert_eq!(
        result
            .modules
            .values()
            .next()
            .expect("module")
            .symbols
            .get("curried_add")
            .expect("function")
            .ty,
        Type::Function {
            parameters: vec![],
            parameter_names: Some(vec![]),
            result: Box::new(Type::Function {
                parameters: vec![Type::Int],
                parameter_names: Some(vec!["x".into()]),
                result: Box::new(Type::Function {
                    parameters: vec![Type::Int],
                    parameter_names: Some(vec!["y".into()]),
                    result: Box::new(Type::Int),
                }),
            }),
        }
    );

    let underconstrained = analyze(&[ModuleInput::new(
        "lambda.orna",
        "fn identity() = value => value;",
    )]);
    assert!(has(&underconstrained, "ORNA-S020-ANNOTATION"));
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
