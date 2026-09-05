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
