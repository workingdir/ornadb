use super::{BoundedEvaluator, RuntimeEvaluator, SourceUnit, StageOutcome};
use crate::{ProjectEnvironment, ProjectExpectations, ProjectUnit};
use num_bigint::BigInt;
use orna_evaluator::{Environment, Limits, evaluate_expression_with_functions};
use orna_sys::{
    AdmissionError, AdmissionRequest, ArgumentMap, FunctionDescriptor, FunctionId,
    FunctionIdentity, InvocationContext, InvocationMode, InvocationState, InvocationStatus,
    RetainedInvocationResult, RevisionId, RuntimeId, RuntimeSupervisor, SnapshotId,
    TransactionMode, TypeId, TypeWitness,
};
use orna_value::Value;
use std::time::Duration;

fn source_descriptor() -> FunctionDescriptor {
    FunctionDescriptor {
        identity: FunctionIdentity {
            function: FunctionId::new("math.answer"),
            revision: RevisionId::from_bytes([0x62; 32]),
            snapshot: SnapshotId::new("source-s1"),
        },
        parameters: Vec::new(),
        result_type: TypeId::new("Int"),
        visible: true,
        callable: true,
        generics_resolved: true,
    }
}

fn source_request(descriptor: FunctionDescriptor, result: TypeId) -> AdmissionRequest<()> {
    AdmissionRequest {
        explicit_snapshot: Some(descriptor.identity.snapshot.clone()),
        function: descriptor,
        arguments: ArgumentMap::default(),
        mode: InvocationMode::Invoke,
        transaction: TransactionMode::Inherit,
        witness: TypeWitness::new(result),
        idempotency_key: Some("source-answer".into()),
        context: InvocationContext::default(),
    }
}

fn source_start_request(descriptor: FunctionDescriptor, result: TypeId) -> AdmissionRequest<()> {
    let mut request = source_request(descriptor, result);
    request.mode = InvocationMode::Start;
    request.transaction = TransactionMode::Separate;
    request.idempotency_key = Some("source-answer-start".into());
    request
}

#[test]
fn sys_source_bridge_rejects_result_witness_before_evaluation() {
    let mut evaluator = BoundedEvaluator::new(Limits::default());
    let source = SourceUnit {
        fixture_id: "source-bridge".into(),
        source_id: "source-bridge.orna".into(),
        parse_as: "module_unit".into(),
        source: "fn answer(): Int = 42;".into(),
    };
    assert!(matches!(
        evaluator.evaluate_module(&source),
        StageOutcome::Passed
    ));
    let descriptor = source_descriptor();
    let supervisor = RuntimeSupervisor::new(RuntimeId::new("source"));

    assert_eq!(
        evaluator.invoke_through_sys(
            &supervisor,
            source_request(descriptor.clone(), TypeId::new("String")),
            &descriptor,
            "answer",
        ),
        Err(AdmissionError::ReturnType)
    );
}

#[test]
fn sys_source_bridge_rejects_descriptor_mismatch_before_evaluation() {
    let mut evaluator = BoundedEvaluator::new(Limits::default());
    let source = SourceUnit {
        fixture_id: "source-bridge".into(),
        source_id: "source-bridge.orna".into(),
        parse_as: "module_unit".into(),
        source: "fn answer(): Int = 42;".into(),
    };
    assert!(matches!(
        evaluator.evaluate_module(&source),
        StageOutcome::Passed
    ));
    let descriptor = source_descriptor();
    let mut request_descriptor = descriptor.clone();
    request_descriptor.identity.function = FunctionId::new("math.other");
    let supervisor = RuntimeSupervisor::new(RuntimeId::new("source"));

    assert_eq!(
        evaluator.invoke_through_sys(
            &supervisor,
            source_request(request_descriptor, TypeId::new("Int")),
            &descriptor,
            "answer",
        ),
        Err(AdmissionError::SourceMismatch)
    );
}

#[test]
fn sys_source_bridge_replays_identical_request_without_re_evaluation() {
    let mut evaluator = BoundedEvaluator::new(Limits::default());
    let source = SourceUnit {
        fixture_id: "source-bridge".into(),
        source_id: "source-bridge.orna".into(),
        parse_as: "module_unit".into(),
        source: "fn answer(): Int = 42;".into(),
    };
    assert!(matches!(
        evaluator.evaluate_module(&source),
        StageOutcome::Passed
    ));
    let descriptor = source_descriptor();
    let request = source_request(descriptor.clone(), TypeId::new("Int"));
    let supervisor = RuntimeSupervisor::new(RuntimeId::new("source"));

    let first = evaluator
        .invoke_through_sys(&supervisor, request.clone(), &descriptor, "answer")
        .expect("first pinned source invocation succeeds");
    assert!(matches!(
        &first,
        InvocationState::Terminal(RetainedInvocationResult::Success(_))
    ));
    let second = evaluator
        .invoke_through_sys(&supervisor, request, &descriptor, "answer")
        .expect("identical pinned source invocation replays");
    assert_eq!(first, second);
}

#[test]
fn sys_source_bridge_starts_and_awaits_a_pure_function() {
    let mut evaluator = BoundedEvaluator::new(Limits::default());
    let source = SourceUnit {
        fixture_id: "source-bridge".into(),
        source_id: "source-bridge.orna".into(),
        parse_as: "module_unit".into(),
        source: "fn answer(): Int = 42;".into(),
    };
    assert!(matches!(
        evaluator.evaluate_module(&source),
        StageOutcome::Passed
    ));
    let descriptor = source_descriptor();
    let supervisor = RuntimeSupervisor::new(RuntimeId::new("source"));
    let handle = evaluator
        .start_through_sys(
            &supervisor,
            source_start_request(descriptor.clone(), TypeId::new("Int")),
            &descriptor,
            "answer",
        )
        .expect("source start is admitted");
    let result = supervisor
        .await_invocation(&handle, Some(Duration::from_secs(1)))
        .expect("source start completes");
    assert!(result.status.is_terminal());
    assert_eq!(result.status, InvocationStatus::Succeeded);
    assert!(result.failure.is_none());
    let value = result
        .value
        .expect("a succeeded source start retains a value");
    assert_eq!(
        Value::decode(value.canonical().expect("success retains canonical value")).unwrap(),
        Value::int(BigInt::from(42)),
    );
}

#[test]
fn project_modules_retain_qualified_pure_functions() {
    let mut evaluator = BoundedEvaluator::new(Limits::default());
    let library = SourceUnit {
        fixture_id: "library".into(),
        source_id: "library.orna".into(),
        parse_as: "module_unit".into(),
        source: "pub fn add(left: Int, right: Int): Int = left + right; pub fn twice(value: Int): Int = add(value, value);".into(),
    };
    let main = SourceUnit {
        fixture_id: "main".into(),
        source_id: "main.orna".into(),
        parse_as: "module_unit".into(),
        source: "pub fn answer(): Int = library.twice(21);".into(),
    };

    assert!(matches!(
        evaluator.evaluate_module_with_namespace(&library, Some("library")),
        StageOutcome::Passed
    ));
    assert!(matches!(
        evaluator.evaluate_module_with_namespace(&main, None),
        StageOutcome::Passed
    ));
    let result = evaluate_expression_with_functions(
        "answer()",
        &Environment::new(),
        &evaluator.functions,
        Limits::default(),
    )
    .expect("qualified module call");
    assert_eq!(result, Value::int(42.into()));

    let result = evaluate_expression_with_functions(
        "library.twice(21)",
        &Environment::new(),
        &evaluator.functions,
        Limits::default(),
    )
    .expect("qualified module call");
    assert_eq!(result, Value::int(42.into()));
}

#[test]
fn repl_sessions_import_admitted_functions_without_mutating_the_project() {
    let mut evaluator = BoundedEvaluator::new(Limits::default());
    let library = SourceUnit {
        fixture_id: "session-library".into(),
        source_id: "library.orna".into(),
        parse_as: "module_unit".into(),
        source: "pub fn add(left: Int, right: Int): Int = left + right; pub fn twice(value: Int): Int = add(value, value);".into(),
    };
    assert!(matches!(
        evaluator.evaluate_module_with_namespace(&library, Some("library")),
        StageOutcome::Passed
    ));
    let mut session = evaluator.repl_session().expect("admitted session");
    assert_eq!(session.submit("use library.twice as double;"), Ok(None));
    assert_eq!(session.submit("let n = 21;"), Ok(None));
    assert_eq!(session.submit("double(n)"), Ok(Some(Value::int(42.into()))));
    assert!(session.submit("missing()").is_err());
    assert_eq!(session.submit("$_"), Ok(Some(Value::int(42.into()))));

    let mut fresh = evaluator.repl_session().expect("fresh admitted session");
    for expression in ["n", "double(21)", "$_"] {
        assert!(fresh.submit(expression).is_err(), "session state escaped");
    }
    assert_eq!(fresh.submit("use library;"), Ok(None));
    assert_eq!(
        fresh.submit("library.twice(21)"),
        Ok(Some(Value::int(42.into())))
    );
    assert_eq!(
        fresh.submit("library.twice($_)"),
        Ok(Some(Value::int(84.into())))
    );
}

#[test]
fn pure_project_evaluation_accepts_ordinary_import_declarations() {
    let mut evaluator = BoundedEvaluator::new(Limits::default());
    let project = ProjectUnit {
        fixture_id: "project".into(),
        project_id: "project".into(),
        environment_id: None,
        modules: vec![
            SourceUnit {
                fixture_id: "project".into(),
                source_id: "main.orna".into(),
                parse_as: "module_unit".into(),
                source: "use library; pub fn answer(): Int = library.value();".into(),
            },
            SourceUnit {
                fixture_id: "project".into(),
                source_id: "library.orna".into(),
                parse_as: "module_unit".into(),
                source: "pub fn value(): Int = 42;".into(),
            },
        ],
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

    assert!(matches!(
        evaluator.evaluate_project(&project),
        StageOutcome::Passed
    ));
    assert!(matches!(evaluator.invoke("answer"), StageOutcome::Passed));
}
