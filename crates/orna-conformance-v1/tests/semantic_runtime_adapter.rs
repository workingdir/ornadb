use orna_conformance_v1::{
    BoundedEvaluator, ConformanceAdapter, Corpus, EvidenceClass, EvidenceStatus, Harness,
    ProjectEnvironment, ProjectExpectations, ProjectUnit, RuntimeAdapter, RuntimeEvaluator,
    Scenario, SemanticAdapter, SourceUnit, StageOutcome,
};
use orna_foundation_v1::{Diagnostic, OvbRaw, Value};
use std::collections::BTreeMap;

#[test]
fn semantic_mail_fixture_distinguishes_stored_email_from_provider_messages() {
    let corpus = Corpus::load_default().expect("reference corpus loads");
    let report = Harness::new(corpus).run(&mut SemanticAdapter::default());
    let fixture = report
        .fixtures
        .iter()
        .find(|fixture| fixture.fixture == "valid/unbounded-stream.orna")
        .expect("mail fixture exists");
    assert!(fixture.passed, "{:?}", fixture.stages);
    // This verifies the frozen static contract, not connector execution.
    assert!(
        fixture
            .stages
            .iter()
            .any(|stage| stage.status == EvidenceStatus::Skipped)
    );
}

#[test]
fn semantic_adapter_executes_the_v1_analyzer_with_logical_fixture_names() {
    let corpus = Corpus::load_default().expect("reference corpus loads");
    let report = Harness::new(corpus).run(&mut SemanticAdapter::default());
    let fixture = report
        .fixtures
        .iter()
        .find(|fixture| fixture.fixture == "valid/minimal-root.orna")
        .expect("minimal fixture");
    // The analyzer really runs against the authoritative core catalogue.
    assert_eq!(fixture.stages[1].status, EvidenceStatus::Passed);
    assert_eq!(fixture.stages[2].status, EvidenceStatus::Passed);
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

#[test]
fn semantic_adapter_keeps_type_errors_in_the_typecheck_phase() {
    let unit = SourceUnit {
        fixture_id: "type-error".into(),
        source_id: "logical/type-error.orna".into(),
        parse_as: "module_unit".into(),
        source: "pub table Bad(value: Float) { text: Str, }".into(),
    };
    let mut adapter = SemanticAdapter::default();
    assert!(matches!(adapter.resolve(&unit), StageOutcome::Passed));
    let StageOutcome::Failed(diagnostic) = adapter.typecheck(&unit) else {
        panic!("type errors must be reported by typecheck");
    };
    assert_eq!(diagnostic.code(), "ORNA-S021-TYPE");
}

#[test]
fn semantic_adapter_preserves_published_closed_type_diagnostics() {
    let corpus = Corpus::load_default().expect("reference corpus loads");
    let report = Harness::new(corpus).run(&mut SemanticAdapter::default());
    for fixture_id in [
        "invalid/unknown-field.orna",
        "invalid/wrong-field-type.orna",
        "invalid/affine-addition.orna",
        "invalid/affine-sum.orna",
        "invalid/currency-addition.orna",
        "invalid/currency-static-symbol.orna",
        "invalid/float-money-implicit.orna",
        "invalid/float-key.orna",
        "invalid/relation-equality.orna",
        "invalid/legacy-checkpoint-reset-method.orna",
        "invalid/legacy-failure-replay-method.orna",
        "invalid/legacy-failure-resolve-method.orna",
        "invalid/legacy-stream-retry-method.orna",
        "invalid/legacy-stream-skip-method.orna",
        "invalid/implicit-conversion-chain.orna",
        "invalid/money-float.orna",
        "invalid/module-single-table-assertion.orna",
        "invalid/module-zero-table-assertion.orna",
        "invalid/legacy-result.orna",
        "invalid/legacy-sys-runtime.orna",
        "invalid/legacy-sys-storage-call.orna",
        "invalid/legacy-tryfrom.orna",
        "invalid/legacy-assert-owner-pipe.orna",
        "invalid/legacy-assert-self-pipe.orna",
        "invalid/incompatible-dimensions.orna",
        "invalid/reserved-std.orna",
        "invalid/reserved-sys.orna",
        "invalid/range-key-overlap-magic.orna",
        "invalid/rekey-auto-id.orna",
        "invalid/effectful-display.orna",
        "invalid/secret-display.orna",
        "invalid/assert-effectful-table.orna",
        "invalid/computed-field-effect.orna",
        "invalid/mutate-sys-commit.orna",
    ] {
        let fixture = report
            .fixtures
            .iter()
            .find(|fixture| fixture.fixture == fixture_id)
            .expect("closed type fixture");
        assert!(fixture.passed, "{fixture_id}: {:?}", fixture.stages);
    }
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

#[test]
fn harness_does_not_turn_unexpected_evaluation_into_fixture_failure() {
    let corpus = Corpus::load_default().expect("reference corpus loads");
    let report = Harness::new(corpus).run(&mut RuntimeAdapter::new(BoundedEvaluator::default()));
    for fixture_id in [
        "valid/coalesce-precedence.orna",
        "valid/question-coalesce-parenthesized.orna",
    ] {
        let fixture = report
            .fixtures
            .iter()
            .find(|fixture| fixture.fixture == fixture_id)
            .expect("coalesce fixture");
        assert!(fixture.passed, "{fixture_id}: {:?}", fixture.stages);
        assert_eq!(
            fixture.stages.last().expect("evaluation stage").status,
            EvidenceStatus::Skipped
        );
    }
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
fn module_admission_checks_source_and_zero_limits_before_parsing() {
    let unit = SourceUnit {
        fixture_id: "module-admission".into(),
        source_id: "logical/module-admission.orna".into(),
        parse_as: "module_unit".into(),
        source: "invalid(".into(),
    };
    for limits in [
        orna_evaluator_v1::Limits {
            max_source_bytes: 2,
            ..Default::default()
        },
        orna_evaluator_v1::Limits {
            max_steps: 0,
            ..Default::default()
        },
    ] {
        let mut evaluator = BoundedEvaluator::new(limits);
        let StageOutcome::Failed(diagnostic) = evaluator.evaluate(&unit) else {
            panic!("module admission limits must fail before parsing");
        };
        assert_eq!(diagnostic.code(), "ORNA-EVAL-LIMIT");
        assert_eq!(diagnostic.message(), "<redacted>");
        let StageOutcome::Failed(diagnostic) =
            evaluator.evaluate_project(&pure_project(vec![unit.clone()]))
        else {
            panic!("project preflight must apply source limits before parsing");
        };
        assert_eq!(diagnostic.code(), "ORNA-EVAL-LIMIT");
    }
}

#[test]
fn rejected_project_capacity_does_not_publish_partial_function_updates() {
    let original = SourceUnit {
        fixture_id: "retained-limit".into(),
        source_id: "logical/retained-limit.orna".into(),
        parse_as: "module_unit".into(),
        source: "fn stable() = 1;".into(),
    };
    let mut evaluator = BoundedEvaluator::new(orna_evaluator_v1::Limits {
        max_collection_items: 2,
        ..Default::default()
    });
    assert_eq!(evaluator.evaluate(&original), StageOutcome::Passed);
    let replacement = SourceUnit {
        source: "fn stable() = 99; fn added() = 2;".into(),
        ..original.clone()
    };
    let overflow = SourceUnit {
        source: "fn excess() = 3;".into(),
        ..original.clone()
    };
    let StageOutcome::Failed(diagnostic) =
        evaluator.evaluate_project(&pure_project(vec![replacement, overflow]))
    else {
        panic!("retained function count must be bounded across modules");
    };
    assert_eq!(diagnostic.code(), "ORNA-EVAL-LIMIT");
    assert!(matches!(
        evaluator.invoke("added"),
        StageOutcome::Skipped { .. }
    ));
    let probe = SourceUnit {
        parse_as: "expression_unit".into(),
        source: "if stable() == 1 { 1 } else { 1 / 0 }".into(),
        ..original.clone()
    };
    assert_eq!(evaluator.evaluate(&probe), StageOutcome::Passed);
    // Replacing an existing definition does not consume another retained slot.
    let replacement = SourceUnit {
        source: "fn stable() = 1; fn added() = 2;".into(),
        ..original
    };
    assert_eq!(evaluator.evaluate(&replacement), StageOutcome::Passed);
    assert_eq!(evaluator.invoke("added"), StageOutcome::Passed);
}

#[test]
fn expression_units_use_retained_functions_and_rejected_modules_preserve_them() {
    let module = SourceUnit {
        fixture_id: "retained-functions".into(),
        source_id: "logical/retained-functions.orna".into(),
        parse_as: "module_unit".into(),
        source: "fn increment(value: Int) = value + 1;".into(),
    };
    let mut evaluator = BoundedEvaluator::default();
    assert_eq!(evaluator.evaluate(&module), StageOutcome::Passed);
    let expression = SourceUnit {
        parse_as: "expression_unit".into(),
        source: "if increment(41) == 42 { 1 } else { 1 / 0 }".into(),
        ..module.clone()
    };
    assert_eq!(evaluator.evaluate(&expression), StageOutcome::Passed);
    let failed_module = SourceUnit {
        source: "fn increment(value: Int) = value + 100; let answer = increment(1);".into(),
        ..module
    };
    let StageOutcome::Failed(diagnostic) = evaluator.evaluate(&failed_module) else {
        panic!("module-level let is not part of the module grammar");
    };
    assert_eq!(diagnostic.code(), "ORNA-PARSE-001");
    assert_eq!(evaluator.evaluate(&expression), StageOutcome::Passed);
}

#[test]
fn registry_expression_dispatch_preserves_source_limits() {
    let mut evaluator = BoundedEvaluator::new(orna_evaluator_v1::Limits {
        max_source_bytes: 2,
        ..Default::default()
    });
    let unit = SourceUnit {
        fixture_id: "source-budget".into(),
        source_id: "logical/source-budget.orna".into(),
        parse_as: "expression_unit".into(),
        source: "invalid(".into(),
    };
    let StageOutcome::Failed(diagnostic) = evaluator.evaluate(&unit) else {
        panic!("source size limits apply before parsing");
    };
    assert_eq!(diagnostic.code(), "ORNA-EVAL-LIMIT");
    assert_eq!(diagnostic.message(), "<redacted>");
}

#[test]
fn retained_functions_admit_structured_parameters_and_wildcards() {
    let module = SourceUnit {
        fixture_id: "parameter-patterns".into(),
        source_id: "logical/parameter-patterns.orna".into(),
        parse_as: "module_unit".into(),
        source: "fn add((a, b) = (1, 2)) = a + b; fn ignore(_, _) = 7; fn verify() = if add((10, 20)) == 30 && ignore(1, 2) == 7 { 1 } else { 1 / 0 }; fn reject() = add(1);".into(),
    };
    let mut evaluator = BoundedEvaluator::default();
    assert_eq!(evaluator.evaluate(&module), StageOutcome::Passed);
    assert_eq!(evaluator.invoke("verify"), StageOutcome::Passed);
    assert_eq!(evaluator.invoke("add"), StageOutcome::Passed);
    let StageOutcome::Failed(diagnostic) = evaluator.invoke("reject") else {
        panic!("parameter patterns must match the supplied values");
    };
    assert_eq!(diagnostic.code(), "ORNA-EVAL-TYPE");
}

#[test]
fn retained_functions_execute_closures_without_mutating_captures() {
    let module = SourceUnit {
        fixture_id: "closure-capture".into(),
        source_id: "logical/closure-capture.orna".into(),
        parse_as: "module_unit".into(),
        source: "fn verify() { let seed = 2; let compute = value => value + seed; seed = 9; if (10 | compute) == 12 { 1 } else { 1 / 0 } } fn reject() { let seed = 1; let mutate = () => { seed += 1; seed }; mutate() }".into(),
    };
    let mut evaluator = BoundedEvaluator::default();
    assert_eq!(evaluator.evaluate(&module), StageOutcome::Passed);
    assert_eq!(evaluator.invoke("verify"), StageOutcome::Passed);
    let StageOutcome::Failed(diagnostic) = evaluator.invoke("reject") else {
        panic!("captured values are immutable");
    };
    assert_eq!(diagnostic.code(), "ORNA-EVAL-IMMUTABLE-CAPTURE");
    assert_eq!(diagnostic.message(), "<redacted>");
}

#[test]
fn retained_function_pipeline_executes_and_checks_its_result() {
    let module = SourceUnit {
        fixture_id: "function-pipeline".into(),
        source_id: "logical/function-pipeline.orna".into(),
        parse_as: "module_unit".into(),
        source: "fn add(value: Int, extra = 6) = value + extra; fn verify() = if (10 | add(extra: 6)) == 16 { 1 } else { 1 / 0 }; fn reject() = 10 | add(value: 3);".into(),
    };
    let mut evaluator = BoundedEvaluator::default();
    assert_eq!(evaluator.evaluate(&module), StageOutcome::Passed);
    assert_eq!(evaluator.invoke("verify"), StageOutcome::Passed);
    let StageOutcome::Failed(diagnostic) = evaluator.invoke("reject") else {
        panic!("the pipe input occupies the first parameter");
    };
    assert_eq!(diagnostic.code(), "ORNA-EVAL-ARGUMENT");
}

#[test]
fn retained_module_functions_call_helpers_in_defaults_and_bodies() {
    let module = SourceUnit {
        fixture_id: "nested-functions".into(),
        source_id: "logical/nested-functions.orna".into(),
        parse_as: "module_unit".into(),
        source: "fn entry(value = helper(40)) = helper(value); fn helper(value: Int) = value + 1; fn recurse() = recurse();".into(),
    };
    let mut evaluator = BoundedEvaluator::default();
    assert_eq!(evaluator.evaluate(&module), StageOutcome::Passed);
    assert_eq!(evaluator.invoke("entry"), StageOutcome::Passed);
    let StageOutcome::Failed(diagnostic) = evaluator.invoke("recurse") else {
        panic!("recursive calls must hit the shared invocation limits");
    };
    assert_eq!(diagnostic.code(), "ORNA-EVAL-LIMIT");
    assert_eq!(diagnostic.message(), "<redacted>");
}

#[test]
fn retained_function_defaults_cannot_reset_the_invocation_budget() {
    let module = SourceUnit {
        fixture_id: "default-budget".into(),
        source_id: "logical/default-budget.orna".into(),
        parse_as: "module_unit".into(),
        source: "fn compute(first = 1 + 2, second = 3 + 4) = first + second;".into(),
    };
    let mut evaluator = BoundedEvaluator::new(orna_evaluator_v1::Limits {
        max_steps: 6,
        ..Default::default()
    });
    assert_eq!(evaluator.evaluate(&module), StageOutcome::Passed);
    let StageOutcome::Failed(diagnostic) = evaluator.invoke("compute") else {
        panic!("defaults and body must share one invocation budget");
    };
    assert_eq!(diagnostic.code(), "ORNA-EVAL-LIMIT");
    assert_eq!(diagnostic.message(), "<redacted>");
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
