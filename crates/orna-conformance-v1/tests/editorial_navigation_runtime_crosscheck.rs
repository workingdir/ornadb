use orna_conformance_v1::{ConformanceAdapter, SemanticAdapter, SourceUnit, StageOutcome};

const FIXTURE: &str = include_str!("fixtures/sys-rt-info.orna");

#[test]
fn navigation_model_runtime_symbol_resolves_and_typechecks() {
    let unit = SourceUnit {
        fixture_id: "editorial-navigation-sys-rt-info".into(),
        source_id: "tests/fixtures/sys-rt-info.orna".into(),
        parse_as: "module_unit".into(),
        source: FIXTURE.into(),
    };
    let mut adapter = SemanticAdapter::default();

    assert!(matches!(adapter.parse(&unit), StageOutcome::Passed));
    assert!(matches!(adapter.resolve(&unit), StageOutcome::Passed));
    assert!(matches!(adapter.typecheck(&unit), StageOutcome::Passed));
}
