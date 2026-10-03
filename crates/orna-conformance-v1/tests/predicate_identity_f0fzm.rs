use orna_conformance_v1::{SourceUnit, StageOutcome, TransactionalEvaluator};
use orna_evaluator_v1::Limits;
use orna_foundation_v1::Value;
use orna_semantic_v1::{Catalogue, ModuleInput, analyze_with_catalogue};
use orna_value_v1::Raw;

fn source_unit(source: &str) -> SourceUnit {
    SourceUnit {
        fixture_id: "txn-paired-depth-continuation-identity-f0fzm".into(),
        source_id: "txn-paired-depth-continuation-identity-f0fzm.orna".into(),
        parse_as: "module_unit".into(),
        source: source.into(),
    }
}

fn field<'a>(row: &'a Value, name: &str) -> &'a Raw {
    let Raw::Map(fields) = row.raw() else {
        panic!("expected row record, got {:?}", row.raw());
    };
    fields
        .iter()
        .find_map(|(key, value)| (key == &Raw::Text(name.into())).then_some(value))
        .unwrap_or_else(|| panic!("row omitted {name}"))
}

fn assert_fixture_typechecks(source: &str) {
    let analysis = analyze_with_catalogue(
        &[ModuleInput::new("paired-depth-continuation.orna", source)],
        &Catalogue::authoritative_fixture(),
    );
    assert!(
        analysis.is_ok(),
        "paired-depth fixture must type-check: {:?}",
        analysis
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn paired_depth_continuations_keep_each_lexical_identity() {
    let source = include_str!("fixtures/txn-paired-depth-continuation-identity-f0fzm.orna");
    assert_fixture_typechecks(source);

    let mut runtime = TransactionalEvaluator::new("parent", Limits::default());
    let outcome = runtime.execute_source(&source_unit(source));
    assert!(matches!(outcome, StageOutcome::Passed), "{outcome:?}");

    for (id, expected) in [(1, "amber"), (2, "blue")] {
        let row = runtime
            .committed_row("Pair", &Value::int(id.into()))
            .expect("computed pair committed");
        assert_eq!(field(&row, "expected"), &Raw::Text(expected.into()));
    }
    for (id, pair_id, expected) in [(10, 1, "west"), (11, 1, "east"), (20, 2, "north")] {
        let row = runtime
            .committed_row("Left", &Value::int(id.into()))
            .expect("computed left row committed");
        assert_eq!(field(&row, "pair_id"), &Raw::Int(pair_id.into()));
        assert_eq!(field(&row, "expected"), &Raw::Text(expected.into()));
    }
    for (id, pair_id, expected) in [(100, 1, "gold"), (101, 1, "silver"), (200, 2, "violet")] {
        let row = runtime
            .committed_row("Right", &Value::int(id.into()))
            .expect("computed right row committed");
        assert_eq!(field(&row, "pair_id"), &Raw::Int(pair_id.into()));
        assert_eq!(field(&row, "expected"), &Raw::Text(expected.into()));
    }
    for (id, pair_id, left_id, right_id, pair, left, right) in [
        (1000, 1, 10, 100, "amber", "west", "gold"),
        (1001, 1, 10, 101, "amber", "west", "silver"),
        (1010, 1, 11, 100, "amber", "east", "gold"),
        (1011, 1, 11, 101, "amber", "east", "silver"),
        (2000, 2, 20, 200, "blue", "north", "violet"),
    ] {
        let row = runtime
            .committed_row("Evidence", &Value::int(id.into()))
            .expect("computed paired evidence committed");
        assert_eq!(field(&row, "pair_id"), &Raw::Int(pair_id.into()));
        assert_eq!(field(&row, "left_id"), &Raw::Int(left_id.into()));
        assert_eq!(field(&row, "right_id"), &Raw::Int(right_id.into()));
        assert_eq!(field(&row, "pair_expected"), &Raw::Text(pair.into()));
        assert_eq!(field(&row, "left_expected"), &Raw::Text(left.into()));
        assert_eq!(field(&row, "right_expected"), &Raw::Text(right.into()));
    }
}
