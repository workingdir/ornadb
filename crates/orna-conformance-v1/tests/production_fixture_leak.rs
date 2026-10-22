use orna_conformance_v1::{
    DurableTransactionalEvaluator, ProjectEnvironment, ProjectExpectations, ProjectUnit,
    SourceUnit,
};

/// Production project admission must not inherit the conformance fixture
/// catalogue: a user module at `vehicle/corsa` must not collide with the
/// fixture `vehicle.corsa` namespace (ORNA-S002-NAMESPACE).
#[test]
fn production_project_admission_accepts_user_module_under_fixture_namespace() {
    let project = ProjectUnit {
        fixture_id: "production-fixture-leak".into(),
        project_id: "production-fixture-leak".into(),
        environment_id: None,
        modules: vec![SourceUnit {
            fixture_id: "production-fixture-leak".into(),
            source_id: "production-fixture-leak/vehicle/corsa.orna".into(),
            parse_as: "module_unit".into(),
            source: include_str!("fixtures/stream-cancellation-sensors.orna").into(),
        }],
        loose_rows: Vec::new(),
        expectations: ProjectExpectations {
            environment: ProjectEnvironment {
                network: false,
                credentials: false,
                intrinsics: "Orna 1.1.0 core".into(),
                stdlib: None,
                initial_tables: "empty".into(),
            },
            steps: Vec::new(),
            negative_cases: Vec::new(),
        },
    };

    assert!(
        DurableTransactionalEvaluator::default()
            .project_stream_root_admitted(&project, "vehicle.corsa.ingest"),
        "user module under a fixture namespace must be admitted by production project admission"
    );
}
