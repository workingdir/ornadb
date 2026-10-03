use orna_conformance_v1::{SourceUnit, StageOutcome, TransactionalEvaluator};
use orna_evaluator_v1::Limits;
use orna_foundation_v1::Value;
use orna_semantic_v1::{Catalogue, ModuleInput, analyze_with_catalogue};
use orna_value_v1::Raw;

fn fixture_source(source: &str) -> SourceUnit {
    SourceUnit {
        fixture_id: "txn-relation-predicate-identity-usygy".into(),
        source_id: "txn-relation-predicate-identity-usygy.orna".into(),
        parse_as: "module_unit".into(),
        source: source.into(),
    }
}

fn row_field<'a>(row: &'a Value, field: &str) -> &'a Raw {
    let Raw::Map(fields) = row.raw() else {
        panic!("expected a table row record, got {:?}", row.raw());
    };
    fields
        .iter()
        .find_map(|(key, value)| (key == &Raw::Text(field.into())).then_some(value))
        .unwrap_or_else(|| panic!("table row omitted field {field}"))
}

#[test]
fn nested_predicate_continuations_keep_each_lexical_row_identity() {
    let source = include_str!("fixtures/txn-relation-predicate-identity-usygy.orna");
    let analysis = analyze_with_catalogue(
        &[ModuleInput::new("relation-predicate-identity.orna", source)],
        &Catalogue::authoritative_fixture(),
    );
    assert!(
        analysis.is_ok(),
        "nested predicate identity fixture must type-check: {:?}",
        analysis
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );

    let mut runtime = TransactionalEvaluator::new("parent", Limits::default());
    let outcome = runtime.execute_source(&fixture_source(source));
    assert!(matches!(&outcome, StageOutcome::Passed), "{outcome:?}");
    for (id, expected) in [(1, "amber"), (2, "blue")] {
        let row = runtime
            .committed_row("Family", &Value::int(id.into()))
            .expect("matching family row was committed");
        assert_eq!(row_field(&row, "expected"), &Raw::Text(expected.into()));
    }
    for (id, family_id) in [(10, 1), (11, 1), (20, 2)] {
        let row = runtime
            .committed_row("Branch", &Value::int(id.into()))
            .expect("matching branch row was committed");
        assert_eq!(row_field(&row, "family_id"), &Raw::Int(family_id.into()));
    }
    for (id, branch_id) in [(100, 10), (101, 10), (110, 11), (200, 20), (999, 99)] {
        let row = runtime
            .committed_row("Leaf", &Value::int(id.into()))
            .expect("candidate leaf row was committed");
        assert_eq!(row_field(&row, "branch_id"), &Raw::Int(branch_id.into()));
    }
    for (id, family_id, branch_id, leaf_id, token) in [
        (1000, 1, 10, 100, "amber"),
        (1001, 1, 10, 101, "amber"),
        (1010, 1, 11, 110, "amber"),
        (2000, 2, 20, 200, "blue"),
        (2099, 1, 20, 200, "amber"),
        (1099, 2, 10, 100, "blue"),
    ] {
        let row = runtime
            .committed_row("Proof", &Value::int(id.into()))
            .expect("candidate proof row was committed");
        assert_eq!(row_field(&row, "family_id"), &Raw::Int(family_id.into()));
        assert_eq!(row_field(&row, "branch_id"), &Raw::Int(branch_id.into()));
        assert_eq!(row_field(&row, "leaf_id"), &Raw::Int(leaf_id.into()));
        assert_eq!(row_field(&row, "token"), &Raw::Text(token.into()));
    }
}

#[test]
fn deepest_nested_predicate_uses_the_current_family_continuation() {
    let mut runtime = TransactionalEvaluator::new("parent", Limits::default());
    let outcome = runtime.execute_source(&fixture_source(include_str!(
        "fixtures/txn-relation-predicate-identity-mismatch-usygy.orna"
    )));

    assert!(
        matches!(
            &outcome,
            StageOutcome::Failed(diagnostic)
                if diagnostic.code() == "ORNA-EVAL-MODULE-ASSERT"
        ),
        "the blue family must reject an amber deepest proof: {outcome:?}"
    );
    for (table, ids) in [
        ("Family", &[1, 2][..]),
        ("Branch", &[10, 20][..]),
        ("Leaf", &[100, 200][..]),
        ("Proof", &[1000, 2000][..]),
    ] {
        for id in ids {
            assert_eq!(
                runtime.committed_row(table, &Value::int((*id).into())),
                None,
                "failed continuation identity published {table} row {id}"
            );
        }
    }
}

#[test]
fn named_predicate_continuations_keep_their_captured_fold_identity() {
    let source = include_str!("fixtures/txn-relation-predicate-fold-values-m7ffi.orna");
    let analysis = analyze_with_catalogue(
        &[ModuleInput::new("relation-predicate-fold-values.orna", source)],
        &Catalogue::authoritative_fixture(),
    );
    assert!(
        analysis.is_ok(),
        "named predicate continuation fixture must type-check: {:?}",
        analysis
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );

    let mut runtime = TransactionalEvaluator::new("parent", Limits::default());
    let outcome = runtime.execute_source(&fixture_source(source));
    assert!(
        matches!(&outcome, StageOutcome::Passed),
        "named continuation fold failed: {:?}",
        match &outcome {
            StageOutcome::Passed => None,
            StageOutcome::Failed(diagnostic) | StageOutcome::Cancelled(diagnostic) => {
                Some(diagnostic.code().to_owned())
            }
            StageOutcome::Skipped { reason } => Some(reason.clone()),
        }
    );
    for (id, expected) in [(1, "amber"), (2, "blue")] {
        let family = runtime
            .committed_row("Family", &Value::int(id.into()))
            .expect("family value was committed");
        assert_eq!(row_field(&family, "expected"), &Raw::Text(expected.into()));
    }
    for (id, family_id) in [(10, 1), (11, 1), (20, 2)] {
        let branch = runtime
            .committed_row("Branch", &Value::int(id.into()))
            .expect("branch value was committed");
        assert_eq!(row_field(&branch, "family_id"), &Raw::Int(family_id.into()));
    }
    for (id, branch_id) in [(100, 10), (101, 10), (110, 11), (200, 20), (999, 99)] {
        let leaf = runtime
            .committed_row("Leaf", &Value::int(id.into()))
            .expect("leaf value was committed");
        assert_eq!(row_field(&leaf, "branch_id"), &Raw::Int(branch_id.into()));
    }
    for (id, family_id, branch_id, leaf_id, token) in [
        (1000, 1, 10, 100, "amber"),
        (1001, 1, 10, 101, "amber"),
        (1010, 1, 11, 110, "amber"),
        (2000, 2, 20, 200, "blue"),
        (2099, 1, 20, 200, "amber"),
        (1099, 2, 10, 100, "blue"),
    ] {
        let proof = runtime
            .committed_row("Proof", &Value::int(id.into()))
            .expect("proof value was committed");
        assert_eq!(row_field(&proof, "family_id"), &Raw::Int(family_id.into()));
        assert_eq!(row_field(&proof, "branch_id"), &Raw::Int(branch_id.into()));
        assert_eq!(row_field(&proof, "leaf_id"), &Raw::Int(leaf_id.into()));
        assert_eq!(row_field(&proof, "token"), &Raw::Text(token.into()));
    }
}

#[test]
fn reused_named_predicate_continuations_reject_a_stale_family_capture() {
    let mut runtime = TransactionalEvaluator::new("parent", Limits::default());
    let outcome = runtime.execute_source(&fixture_source(include_str!(
        "fixtures/txn-relation-predicate-fold-mismatch-m7ffi.orna"
    )));

    assert!(
        matches!(
            &outcome,
            StageOutcome::Failed(diagnostic)
                if diagnostic.code() == "ORNA-EVAL-MODULE-ASSERT"
        ),
        "blue must reject the proof accepted by the amber continuation: {:?}",
        match &outcome {
            StageOutcome::Passed => None,
            StageOutcome::Failed(diagnostic) | StageOutcome::Cancelled(diagnostic) => {
                Some(diagnostic.code().to_owned())
            }
            StageOutcome::Skipped { reason } => Some(reason.clone()),
        }
    );
    for (table, id) in [
        ("Family", 1),
        ("Family", 2),
        ("Branch", 10),
        ("Branch", 20),
        ("Leaf", 100),
        ("Leaf", 200),
        ("Proof", 1000),
    ] {
        assert_eq!(
            runtime.committed_row(table, &Value::int(id.into())),
            None,
            "failed captured continuation published {table} row {id}"
        );
    }
}
