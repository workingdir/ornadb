use orna_conformance_v1::{
    BoundedEvaluator, ConformanceAdapter, Corpus, EvidenceClass, EvidenceStatus, Harness,
    ProjectEnvironment, ProjectExpectations, ProjectUnit, RuntimeAdapter, RuntimeEvaluator,
    Scenario, SemanticAdapter, SourceUnit, StageOutcome,
};
use orna_foundation_v1::{Diagnostic, OvbRaw, Value};
use std::collections::BTreeMap;

#[test]
fn semantic_adapter_executes_the_v1_analyzer_with_logical_fixture_names() {
    let corpus = Corpus::load_default().expect("reference corpus loads");
    let report = Harness::new(corpus).run(&mut SemanticAdapter::default());
    let fixture = report
        .fixtures
        .iter()
        .find(|fixture| fixture.fixture == "valid/minimal-root.orna")
        .expect("minimal fixture");
    // The analyzer really runs, and honestly exposes that its current v1
    // contract has no `std` prelude/catalogue adapter for this corpus.
    assert_eq!(fixture.stages[1].status, EvidenceStatus::Failed);
    assert_eq!(
        fixture.stages[1].diagnostic.as_ref().unwrap()["code"],
        "ORNA-S012-UNRESOLVED"
    );
    assert!(!report.semantic_evidence.is_empty());
    assert!(
        report
            .scenarios
            .iter()
            .all(|scenario| scenario.status == EvidenceStatus::Skipped)
    );
    assert!(report.scenarios.iter().all(|scenario| {
        scenario.detail.contains("runtime-v1") && !scenario.detail.contains("/home/")
    }));
}

#[test]
fn semantic_project_resolution_uses_project_relative_module_names() {
    let mut adapter = SemanticAdapter::default();
    let project = ProjectUnit {
        fixture_id: "project".into(),
        project_id: "examples/reference".into(),
        environment_id: None,
        modules: vec![
            SourceUnit {
                fixture_id: "project".into(),
                source_id: "examples/reference/main.orna".into(),
                parse_as: "module_unit".into(),
                source: "use library;".into(),
            },
            SourceUnit {
                fixture_id: "project".into(),
                source_id: "examples/reference/library.orna".into(),
                parse_as: "module_unit".into(),
                source: "pub fn pick(value: Int): Int = value;".into(),
            },
        ],
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
        },
    };

    assert!(matches!(
        adapter.resolve_project(&project),
        StageOutcome::Passed
    ));
}

#[derive(Default)]
struct RecordingRuntime {
    calls: usize,
}
impl RuntimeEvaluator for RecordingRuntime {
    fn evaluate(&mut self, _: &SourceUnit) -> StageOutcome<Diagnostic> {
        self.calls += 1;
        StageOutcome::Passed
    }
    fn validate_row(&mut self, _: &SourceUnit) -> StageOutcome<Diagnostic> {
        self.calls += 1;
        StageOutcome::Passed
    }
    fn validate_rows(&mut self, _: &ProjectUnit) -> StageOutcome<Diagnostic> {
        self.calls += 1;
        StageOutcome::Passed
    }
    fn run_scenario(&mut self, _: &Scenario) -> StageOutcome<Diagnostic> {
        self.calls += 1;
        StageOutcome::Passed
    }
}

#[test]
fn runtime_adapter_has_an_executable_seam_but_lazy_semantic_failures_precede_it() {
    let mut adapter = RuntimeAdapter::new(RecordingRuntime::default());
    let corpus = Corpus::load_default().expect("reference corpus loads");
    let report = Harness::new(corpus).run(&mut adapter);
    let runtime = adapter.into_runtime();
    assert!(runtime.calls >= report.scenarios.len());
    assert!(
        report
            .scenarios
            .iter()
            .all(|scenario| scenario.status == EvidenceStatus::Passed)
    );
}

#[test]
fn bounded_evaluator_executes_expression_units_and_redacts_failures() {
    let mut evaluator = BoundedEvaluator::default();
    let valid = SourceUnit {
        fixture_id: "test-valid".into(),
        source_id: "logical/test.orna".into(),
        parse_as: "row_unit".into(),
        source: "{ total: std.math.increment(1) }".into(),
    };
    assert_eq!(evaluator.evaluate(&valid), StageOutcome::Passed);

    let invalid = SourceUnit {
        fixture_id: "test-invalid".into(),
        source_id: "logical/test.orna".into(),
        parse_as: "row_unit".into(),
        source: "{ total: missing }".into(),
    };
    let StageOutcome::Failed(diagnostic) = evaluator.evaluate(&invalid) else {
        panic!("unknown name must fail");
    };
    assert_eq!(diagnostic.code(), "ORNA-EVAL-NAME");
    assert_eq!(diagnostic.message(), "<redacted>");
}

#[test]
fn bounded_adapter_keeps_the_reference_project_effects_skipped_but_validates_empty_rows() {
    let corpus = Corpus::load_default().expect("reference corpus loads");
    let report = Harness::new(corpus).run(&mut RuntimeAdapter::new(BoundedEvaluator::default()));
    let project = report
        .fixtures
        .iter()
        .find(|fixture| fixture.fixture == "PROJECT-REFERENCE")
        .expect("reference project result");

    assert!(project.passed);
    assert_eq!(
        project
            .stages
            .iter()
            .map(|stage| (
                stage.stage.clone(),
                stage.status.clone(),
                stage.class.clone()
            ))
            .collect::<Vec<_>>(),
        vec![
            (
                Some(orna_conformance_v1::Stage::Parse),
                EvidenceStatus::Passed,
                EvidenceClass::Runtime
            ),
            (
                Some(orna_conformance_v1::Stage::Resolve),
                EvidenceStatus::Passed,
                EvidenceClass::Semantic
            ),
            (
                Some(orna_conformance_v1::Stage::Typecheck),
                EvidenceStatus::Passed,
                EvidenceClass::Semantic
            ),
            (
                Some(orna_conformance_v1::Stage::Evaluate),
                EvidenceStatus::Skipped,
                EvidenceClass::Skipped
            ),
            (
                Some(orna_conformance_v1::Stage::RowValidation),
                EvidenceStatus::Passed,
                EvidenceClass::Semantic
            ),
        ]
    );
}

fn pure_project(modules: Vec<SourceUnit>) -> ProjectUnit {
    ProjectUnit {
        fixture_id: "test-project".into(),
        project_id: "logical/project".into(),
        environment_id: None,
        modules,
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
        },
    }
}

#[test]
fn bounded_evaluator_defers_invalid_function_bodies_until_explicit_invocation() {
    let pure_module = SourceUnit {
        fixture_id: "test-module".into(),
        source_id: "logical/pure.orna".into(),
        parse_as: "module_unit".into(),
        source: "pub fn secret() = missing;".into(),
    };
    let mut evaluator = BoundedEvaluator::default();
    assert_eq!(evaluator.evaluate(&pure_module), StageOutcome::Passed);
    assert_eq!(
        evaluator.evaluate_project(&pure_project(vec![pure_module])),
        StageOutcome::Passed
    );
    let StageOutcome::Failed(diagnostic) = evaluator.invoke("secret") else {
        panic!("invalid retained function body must fail when invoked");
    };
    assert_eq!(diagnostic.code(), "ORNA-EVAL-NAME");
    assert_eq!(diagnostic.message(), "<redacted>");
}

#[test]
fn bounded_evaluator_invokes_a_function_with_its_earlier_immutable_binding() {
    let pure_module = SourceUnit {
        fixture_id: "test-module".into(),
        source_id: "logical/pure.orna".into(),
        parse_as: "module_unit".into(),
        source: "pub fn incremented() = if true { let answer = 41; std.math.increment(answer) } else { 0 };".into(),
    };
    let mut evaluator = BoundedEvaluator::default();
    assert_eq!(evaluator.evaluate(&pure_module), StageOutcome::Passed);
    assert_eq!(evaluator.invoke("incremented"), StageOutcome::Passed);
}

fn value(raw: OvbRaw) -> Value {
    Value::new(raw).expect("test values are canonical")
}

#[test]
fn bounded_evaluator_invokes_retained_functions_with_named_arguments_and_defaults() {
    let pure_module = SourceUnit {
        fixture_id: "test-module".into(),
        source_id: "logical/pure.orna".into(),
        parse_as: "module_unit".into(),
        source: "pub fn increment(number, label) = std.math.increment(number); pub fn add_one(value, increment = 1) = value + increment;".into(),
    };
    let mut evaluator = BoundedEvaluator::default();
    assert_eq!(evaluator.evaluate(&pure_module), StageOutcome::Passed);

    let arguments = BTreeMap::from([
        ("label".into(), value(OvbRaw::Text("named".into()))),
        ("number".into(), value(OvbRaw::Int(41.into()))),
    ]);
    assert_eq!(
        evaluator.invoke_with("increment", &arguments),
        StageOutcome::Passed
    );
    assert_eq!(
        evaluator.invoke_with(
            "add_one",
            &BTreeMap::from([("value".into(), value(OvbRaw::Int(41.into())))]),
        ),
        StageOutcome::Passed
    );
}

#[test]
fn bounded_evaluator_redacts_missing_and_unknown_retained_function_arguments() {
    let pure_module = SourceUnit {
        fixture_id: "test-module".into(),
        source_id: "logical/pure.orna".into(),
        parse_as: "module_unit".into(),
        source: "pub fn increment(value) = std.math.increment(value);".into(),
    };
    let mut evaluator = BoundedEvaluator::default();
    assert_eq!(evaluator.evaluate(&pure_module), StageOutcome::Passed);

    for arguments in [
        BTreeMap::new(),
        BTreeMap::from([("unknown".into(), value(OvbRaw::Int(41.into())))]),
    ] {
        let StageOutcome::Failed(diagnostic) = evaluator.invoke_with("increment", &arguments)
        else {
            panic!("missing and unknown arguments must fail");
        };
        assert_eq!(diagnostic.code(), "ORNA-EVAL-ARGUMENT");
        assert_eq!(diagnostic.message(), "<redacted>");
    }
}
