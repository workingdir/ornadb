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
