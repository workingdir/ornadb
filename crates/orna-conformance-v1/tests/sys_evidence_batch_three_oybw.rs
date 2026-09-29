use orna_semantic_v1::{Catalogue, ModuleInput, analyze_with_catalogue};
use orna_sys_v1::{
    Admission, AdmissionRequest, ArgumentMap, ExecutionBoundary, FunctionDescriptor, FunctionId,
    FunctionIdentity, InvocationContext, InvocationExecutor, InvocationMode, InvocationResult,
    InvocationStatus, InvocationState, RetainedInvocationResult, RevisionId, Runtime, RuntimeId,
    SnapshotId, TransactionMode, TypeId, TypeWitness, TypedValue,
};
use std::{thread, time::Duration};

const SYSTEM_REFERENCE: &str =
    include_str!("fixtures/reference/source/15-system.md");
const START_FIXTURE: &str = include_str!("fixtures/typed-start-valid.orna");

struct Case {
    id: &'static str,
    source_line: usize,
    fixture: &'static str,
    boundary: &'static str,
}

const CASES: [Case; 10] = [
    Case { id: "ORNA-SYS-016", source_line: 88, fixture: "crates/orna-conformance-v1/tests/fixtures/typed-start-valid.orna", boundary: "opaque RuntimeId has stable canonical encoding and distinct Rust type" },
    Case { id: "ORNA-SYS-020", source_line: 96, fixture: "crates/orna-conformance-v1/tests/fixtures/typed-start-valid.orna", boundary: "a handle from another runtime fails with the foreign-runtime code" },
    Case { id: "ORNA-SYS-027", source_line: 70, fixture: "crates/orna-conformance-v1/tests/fixtures/typed-start-valid.orna", boundary: "restart advances the runtime generation and fences the old handle" },
    Case { id: "ORNA-SYS-032", source_line: 130, fixture: "crates/orna-conformance-v1/tests/fixtures/typed-start-valid.orna", boundary: "terminal idempotent invocation replay remains terminal" },
    Case { id: "ORNA-SYS-056", source_line: 214, fixture: "crates/orna-conformance-v1/tests/fixtures/typed-start-valid.orna", boundary: "start creates an invocation identity before executor entry" },
    Case { id: "ORNA-SYS-057", source_line: 216, fixture: "crates/orna-conformance-v1/tests/fixtures/typed-start-valid.orna", boundary: "ordinary executor failure is retained as a terminal Failed result" },
    Case { id: "ORNA-SYS-058", source_line: 218, fixture: "crates/orna-conformance-v1/tests/fixtures/typed-start-valid.orna", boundary: "cancelled completion remains distinct from ordinary failure" },
    Case { id: "ORNA-SYS-081", source_line: 285, fixture: "crates/orna-conformance-v1/tests/fixtures/typed-start-valid.orna", boundary: "start returns a handle bound to its runtime and invocation identities" },
    Case { id: "ORNA-SYS-082", source_line: 287, fixture: "crates/orna-conformance-v1/tests/fixtures/typed-start-valid.orna", boundary: "await timeout leaves the invocation running and awaitable" },
    Case { id: "ORNA-SYS-133", source_line: 299, fixture: "crates/orna-conformance-v1/tests/fixtures/typed-start-valid.orna", boundary: "terminal outcome shape preserves the success/failure/cancellation distinction" },
];

fn request(mode: InvocationMode) -> AdmissionRequest<TypedValue> {
    AdmissionRequest {
        function: FunctionDescriptor {
            identity: FunctionIdentity {
                function: FunctionId::new("fixture.handler"),
                revision: RevisionId::from_bytes([0x23; 32]),
                snapshot: SnapshotId::new("snapshot-1"),
            },
            parameters: Vec::new(),
            result_type: TypeId::new("Int"),
            visible: true,
            callable: true,
            generics_resolved: true,
        },
        arguments: ArgumentMap::default(),
        explicit_snapshot: None,
        mode,
        transaction: match mode {
            InvocationMode::Invoke => TransactionMode::Inherit,
            InvocationMode::Start => TransactionMode::Separate,
        },
        witness: TypeWitness::new(TypeId::new("Int")),
        idempotency_key: None,
        context: InvocationContext::default(),
    }
}

struct IdentityFirst;

impl InvocationExecutor for IdentityFirst {
    fn execute(&mut self, boundary: &ExecutionBoundary) -> InvocationResult<TypedValue> {
        assert!(!boundary.invocation.as_str().is_empty());
        InvocationResult::Success(TypedValue::public(TypeId::new("Int"), [42]))
    }
}

struct OrdinaryFailure;

impl InvocationExecutor for OrdinaryFailure {
    fn execute(&mut self, _: &ExecutionBoundary) -> InvocationResult<TypedValue> {
        InvocationResult::OrdinaryFailure(orna_sys_v1::Diagnostic {
            code: "sys.fixture.failure",
            message: "fixture handler failed",
            fields: Default::default(),
            causes: Vec::new(),
        })
    }
}

struct DelayedSuccess;

impl InvocationExecutor for DelayedSuccess {
    fn execute(&mut self, _: &ExecutionBoundary) -> InvocationResult<TypedValue> {
        thread::sleep(Duration::from_millis(40));
        InvocationResult::Success(TypedValue::public(TypeId::new("Int"), [7]))
    }
}

#[test]
fn ten_sys_gap_rows_have_bounded_runtime_and_fixture_evidence() {
    let parsed = analyze_with_catalogue(
        &[ModuleInput::new("typed-start-valid.orna", START_FIXTURE)],
        &Catalogue::authoritative_fixture(),
    );
    assert!(parsed.is_ok(), "typed start fixture: {:?}", parsed.diagnostics);

    let runtime_id = RuntimeId::new("runtime-a");
    let encoded_first = serde_json::to_vec(&runtime_id).unwrap();
    let encoded_second = serde_json::to_vec(&RuntimeId::new("runtime-a")).unwrap();
    assert_eq!(encoded_first, encoded_second);
    let _: RuntimeId = RuntimeId::new("runtime-a");
    // SnapshotId and RuntimeId are separate nominal types; there is no implicit conversion.

    let mut owner = Runtime::new(RuntimeId::new("owner"));
    let handle = match owner.admit(request(InvocationMode::Start)).unwrap() {
        Admission::New { handle, .. } => handle,
        _ => panic!("a fresh start must allocate a new invocation"),
    };
    assert!(!handle.resumable);

    let foreign_error = Runtime::new(RuntimeId::new("other"))
        .check_handle(&handle)
        .unwrap_err();
    assert_eq!(foreign_error.code(), "sys.handle.foreign_runtime");

    let prior_id = owner.id().clone();
    let prior_generation = owner.generation();
    let restarted_id = owner.restart();
    assert_ne!(restarted_id, prior_id);
    assert_eq!(owner.generation(), prior_generation + 1);
    assert_eq!(owner.check_handle(&handle).unwrap_err().code(), "sys.handle.foreign_runtime");

    let mut idempotent = Runtime::new(RuntimeId::new("idempotent"));
    let mut idempotent_request = request(InvocationMode::Invoke);
    idempotent_request.idempotency_key = Some("terminal-key".into());
    let first = match idempotent.admit(idempotent_request.clone()).unwrap() {
        Admission::New { handle, .. } => handle,
        _ => unreachable!(),
    };
    idempotent
        .retain_terminal(
            &first,
            InvocationResult::Success(TypedValue::public(TypeId::new("Int"), [1])),
        )
        .unwrap();
    assert!(matches!(
        idempotent.admit(idempotent_request).unwrap(),
        Admission::Terminal { .. }
    ));

    let supervisor = orna_sys_v1::RuntimeSupervisor::new(RuntimeId::new("supervisor"));
    assert!(matches!(
        supervisor.run(request(InvocationMode::Invoke), &mut IdentityFirst).unwrap(),
        InvocationState::Terminal(RetainedInvocationResult::Success(_))
    ));

    let failed = supervisor.run(request(InvocationMode::Invoke), &mut OrdinaryFailure).unwrap();
    assert!(matches!(
        failed,
        InvocationState::Terminal(RetainedInvocationResult::OrdinaryFailure(ref diagnostic))
            if diagnostic.code == "sys.fixture.failure"
    ));

    let mut cancelled = Runtime::new(RuntimeId::new("cancelled"));
    let cancellation_handle = match cancelled.admit(request(InvocationMode::Start)).unwrap() {
        Admission::New { handle, .. } => handle,
        _ => unreachable!(),
    };
    cancelled
        .retain_terminal(
            &cancellation_handle,
            InvocationResult::Cancelled(None),
        )
        .unwrap();
    assert!(matches!(
        cancelled.invocation_state(&cancellation_handle).unwrap(),
        InvocationState::Terminal(RetainedInvocationResult::Cancelled(None))
    ));

    let started = supervisor.start(request(InvocationMode::Start), DelayedSuccess).unwrap();
    assert_eq!(started.runtime(), &supervisor.id().unwrap());
    assert!(!started.invocation().as_str().is_empty());
    assert!(!started.resumable);
    assert!(matches!(
        supervisor.await_invocation(&started, Some(Duration::from_millis(1))),
        Err(orna_sys_v1::AwaitError::Timeout)
    ));
    assert_eq!(supervisor.invocation_metadata(&started).unwrap().status(), InvocationStatus::Running);
    let awaited = supervisor
        .await_invocation(&started, Some(Duration::from_secs(1)))
        .unwrap();
    assert_eq!(awaited.status, InvocationStatus::Succeeded);

    for row in CASES {
        let anchor = format!("**{}**", row.id);
        assert!(
            SYSTEM_REFERENCE.lines().nth(row.source_line - 1).is_some_and(|line| line.contains(&anchor)),
            "{} missing at source/15-system.md:{}",
            row.id,
            row.source_line
        );
        assert!(row.fixture.ends_with(".orna"));
        println!(
            "{} source/15-system.md:{} Specified=Yes Exists=Yes Passed=Yes boundary={} fixture={} full_requirement=Partial",
            row.id, row.source_line, row.boundary, row.fixture
        );
    }
}
