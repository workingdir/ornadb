use orna_semantic_v1::{
    Catalogue, DIAG_AMBIGUOUS, DIAG_ASSERTION, DIAG_ASSERTION_EFFECT, DIAG_ASSERTION_SCOPE,
    DIAG_RESERVED, DIAG_UNRESOLVED, ModuleInput, analyze, analyze_with_catalogue,
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
