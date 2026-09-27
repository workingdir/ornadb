use orna_conformance_v1::{
    ConformanceAdapter, ProjectEnvironment, ProjectExpectations, ProjectUnit, SemanticAdapter,
    SourceUnit, StageOutcome,
};

// Source-level evidence only; this does not exercise runtime admin dispatch or `sys.admin.busy`.

#[test]
fn authoritative_stream_admin_fixture_passes_semantic_admission() {
    let project = ProjectUnit {
        fixture_id: "stream-admin-repl-project".into(),
        project_id: "logical/stream-admin-repl".into(),
        environment_id: None,
        modules: vec![SourceUnit {
            fixture_id: "stream-admin-repl-module".into(),
            source_id: "logical/stream-admin-repl/main.orna".into(),
            parse_as: "module_unit".into(),
            source: include_str!("fixtures/stream-admin-repl.orna").into(),
        }],
        loose_rows: Vec::new(),
        expectations: ProjectExpectations {
            environment: ProjectEnvironment {
                network: false,
                credentials: false,
                intrinsics: "Orna 1.0.0 core".into(),
                stdlib: None,
                initial_tables: "empty".into(),
            },
            steps: Vec::new(),
            negative_cases: Vec::new(),
        },
    };
    let mut adapter = SemanticAdapter::default();

    assert_eq!(adapter.parse_project(&project), StageOutcome::Passed);
    assert_eq!(adapter.resolve_project(&project), StageOutcome::Passed);
    assert_eq!(adapter.typecheck_project(&project), StageOutcome::Passed);
}
