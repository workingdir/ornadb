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
