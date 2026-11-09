//! Unit tests for the supervisor, ledger and value model in the crate root.

use super::*;
use std::sync::mpsc;
use std::sync::{
    Arc, Condvar, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;
fn ty(name: &str) -> TypeId {
    TypeId::new(name)
}
fn value(t: &str, data: &str) -> TypedValue {
    TypedValue::public(ty(t), data)
}
fn function(default: Option<TypedValue>) -> FunctionDescriptor {
    FunctionDescriptor {
        identity: FunctionIdentity {
            function: FunctionId::new("f"),
            revision: RevisionId::from_bytes([0x11; 32]),
            snapshot: SnapshotId::new("s1"),
        },
        parameters: vec![Parameter {
            name: "a".into(),
            static_type: ty("Int"),
            default,
        }],
        result_type: ty("Str"),
        visible: true,
        callable: true,
        generics_resolved: true,
    }
}
#[test]
fn sys_meta_returns_catalogue_facts_and_redacts_protected_payloads() {
    struct Resolver(BTreeMap<String, ValueMetadataFacts>);
    impl ValueMetadataResolver for Resolver {
        fn value_metadata_facts(&self, static_type: &TypeId) -> Option<ValueMetadataFacts> {
            self.0.get(static_type.as_str()).cloned()
        }
    }

    let type_id = ty("app.Credential");
    let fixture = include_str!("../tests/fixtures/secret-surface.orna").trim();
    let value = TypedValue::protected(type_id.clone(), fixture.as_bytes().to_vec());
    let facts = ValueMetadataFacts::new(
        Some(ty("app.Credential")),
        [ty("protocol.Z"), ty("protocol.A")],
        ["z-orna-codec".to_owned(), "a-orna-codec".to_owned()],
    )
    .unwrap();
    let resolver = Resolver(BTreeMap::from([(type_id.as_str().to_owned(), facts)]));

    let metadata = system_value_metadata(&value, &resolver).unwrap();
    assert_eq!(metadata.static_type().as_str(), "app.Credential");
    assert_eq!(
        metadata.nominal_type().map(TypeRef::as_str),
        Some("app.Credential")
    );
    assert_eq!(
        metadata
            .protocols()
            .iter()
            .map(TypeRef::as_str)
            .collect::<Vec<_>>(),
        ["protocol.A", "protocol.Z"]
    );
    assert_eq!(metadata.codecs(), ["a-orna-codec", "z-orna-codec"]);
    assert!(metadata.is_redacted());
    assert_eq!(value.canonical(), None);

    let projection = serde_json::to_vec(&metadata).unwrap();
    assert!(!String::from_utf8_lossy(&projection).contains(fixture));

    let public_value = TypedValue::public(type_id, b"canonical-ovb-bytes".to_vec());
    let canonical_before = public_value.canonical().unwrap().to_vec();
    let public_metadata = system_value_metadata(&public_value, &resolver).unwrap();
    assert!(!public_metadata.is_redacted());
    assert_eq!(public_value.canonical(), Some(canonical_before.as_slice()));
}

#[test]
fn sys_meta_fails_closed_without_catalogue_type_facts() {
    struct EmptyResolver;
    impl ValueMetadataResolver for EmptyResolver {
        fn value_metadata_facts(&self, _: &TypeId) -> Option<ValueMetadataFacts> {
            None
        }
    }

    let value = value("Int", "42");
    assert_eq!(
        system_value_metadata(&value, &EmptyResolver),
        Err(ValueMetadataError::TypeUnavailable)
    );
}

#[test]
fn revision_id_is_an_exact_byte_oriented_nominal_value() {
    let bytes = [0x7e; 32];
    let revision = RevisionId::from_bytes(bytes);

    let _: fn([u8; 32]) -> RevisionId = RevisionId::from_bytes;
    assert_eq!(revision.as_bytes(), &bytes);
    assert_eq!(revision.into_bytes(), bytes);
}
#[test]
fn changing_one_revision_byte_changes_the_invocation_identity() {
    let request = request(Some(value("Int", "1")), ArgumentMap::default());
    let bound = bind(&request.function, &request.arguments).unwrap();
    let original = identity(&request, &bound);

    let mut changed = request;
    let mut revision = *changed.function.identity.revision.as_bytes();
    revision[0] ^= 1;
    changed.function.identity.revision = RevisionId::from_bytes(revision);
    let changed_bound = bind(&changed.function, &changed.arguments).unwrap();

    assert_ne!(original, identity(&changed, &changed_bound));
}
#[test]
fn the_first_cancellation_request_keeps_its_reason_and_beats_a_late_success() {
    let mut runtime = Runtime::new(RuntimeId::new("r"));
    let handle = match runtime
        .admit(request(Some(value("Int", "1")), ArgumentMap::default()))
        .unwrap()
    {
        Admission::New { handle, .. } => handle,
        _ => unreachable!(),
    };
    let first = Diagnostic::new("sys.test.first", "first request");
    let second = Diagnostic::new("sys.test.second", "second request");

    assert_eq!(runtime.cancellation_requested(&handle), Ok(false));
    assert_eq!(runtime.cancel(&handle, Some(first.clone())), Ok(true));
    assert_eq!(runtime.cancel(&handle, Some(second)), Ok(true));
    assert_eq!(runtime.cancellation_requested(&handle), Ok(true));
    assert_eq!(
        runtime.invocation_state(&handle),
        Ok(InvocationState::Active)
    );

    runtime
        .retain_terminal(&handle, InvocationResult::Success(value("Str", "late")))
        .unwrap();
    assert_eq!(
        runtime.invocation_state(&handle),
        Ok(InvocationState::Terminal(
            RetainedInvocationResult::Cancelled(Some(first))
        ))
    );
    // Cancelling a terminal invocation only reports whether it was cancelled.
    assert_eq!(runtime.cancel(&handle, None), Ok(true));
}

#[test]
fn a_start_admission_is_queued_until_a_worker_marks_it_running() {
    let mut runtime = Runtime::new(RuntimeId::new("r"));
    let mut start = request(Some(value("Int", "1")), ArgumentMap::default());
    start.mode = InvocationMode::Start;
    start.transaction = TransactionMode::Separate;
    let handle = match runtime.admit(start).unwrap() {
        Admission::New { handle, .. } => handle,
        _ => unreachable!(),
    };
    let status = |runtime: &Runtime| runtime.invocation_metadata(&handle).unwrap().status();
    assert_eq!(status(&runtime), InvocationStatus::Queued);
    runtime.mark_running(&handle).unwrap();
    assert_eq!(status(&runtime), InvocationStatus::Running);
    runtime.restart();
    let orphaned = runtime.id().clone();
    assert_eq!(orphaned.as_str(), "r@2");
    // The old-generation handle is no longer valid; the ledger entry is
    // orphaned, never re-run.
    assert_eq!(
        runtime.invocation_metadata(&handle).map(|_| ()),
        Err(AdmissionError::ForeignRuntime)
    );
}

#[test]
fn an_absent_context_field_and_an_empty_one_have_different_invocation_identities() {
    // ORNA-SYS-079: the idempotency tuple includes the relevant context, so a
    // key bound with no locale must not match a reuse that supplies "".
    let absent = request(Some(value("Int", "1")), ArgumentMap::default());
    let bound = bind(&absent.function, &absent.arguments).unwrap();
    let original = identity(&absent, &bound);

    type Field = fn(&mut InvocationContext) -> &mut Option<String>;
    let fields: [Field; 4] = [
        |context| &mut context.locale,
        |context| &mut context.timezone,
        |context| &mut context.trace_owner,
        |context| &mut context.cancellation_owner,
    ];
    for field in fields {
        let mut empty = absent.clone();
        *field(&mut empty.context) = Some(String::new());
        assert_ne!(original, identity(&empty, &bound));
    }
}
fn request(default: Option<TypedValue>, args: ArgumentMap) -> AdmissionRequest<String> {
    AdmissionRequest {
        function: function(default),
        arguments: args,
        explicit_snapshot: None,
        mode: InvocationMode::Invoke,
        transaction: TransactionMode::Inherit,
        witness: TypeWitness::new(ty("Str")),
        idempotency_key: None,
        context: InvocationContext::default(),
    }
}
fn args(entries: Vec<Argument>) -> ArgumentMap {
    ArgumentMap::new(entries).unwrap()
}
struct Executor {
    calls: usize,
    result: InvocationResult<TypedValue>,
}
impl InvocationExecutor for Executor {
    fn execute(&mut self, _: &ExecutionBoundary) -> InvocationResult<TypedValue> {
        self.calls += 1;
        self.result.clone()
    }
}
struct CountingExecutor {
    calls: Arc<AtomicUsize>,
}
impl InvocationExecutor for CountingExecutor {
    fn execute(&mut self, _: &ExecutionBoundary) -> InvocationResult<TypedValue> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        InvocationResult::Success(value("Str", "unexpected"))
    }
}
struct BlockingExecutor {
    gate: Arc<(Mutex<bool>, Condvar)>,
    calls: Arc<AtomicUsize>,
    result: InvocationResult<TypedValue>,
}
impl InvocationExecutor for BlockingExecutor {
    fn execute(&mut self, _: &ExecutionBoundary) -> InvocationResult<TypedValue> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let (open, wake) = &*self.gate;
        let mut open = open.lock().unwrap();
        while !*open {
            open = wake.wait(open).unwrap();
        }
        self.result.clone()
    }
}
struct AdmissionLifecycleExecutor {
    started: Arc<AtomicBool>,
}
impl InvocationExecutor for AdmissionLifecycleExecutor {
    fn execute(&mut self, _: &ExecutionBoundary) -> InvocationResult<TypedValue> {
        InvocationResult::Success(value("Str", "unexpected"))
    }

    fn execute_controlled(
        &mut self,
        _: &ExecutionBoundary,
        cancellation: &CancellationToken,
    ) -> InvocationResult<TypedValue> {
        self.started.store(true, Ordering::SeqCst);
        while !cancellation.is_cancelled() {
            thread::yield_now();
        }
        InvocationResult::Cancelled(None)
    }
}
struct CancellableExecutor {
    started: Arc<AtomicBool>,
    observed: Arc<AtomicBool>,
}
impl InvocationExecutor for CancellableExecutor {
    fn execute(&mut self, _: &ExecutionBoundary) -> InvocationResult<TypedValue> {
        InvocationResult::Success(value("Str", "uncancelled"))
    }

    fn execute_controlled(
        &mut self,
        _: &ExecutionBoundary,
        cancellation: &CancellationToken,
    ) -> InvocationResult<TypedValue> {
        self.started.store(true, Ordering::SeqCst);
        while !cancellation.is_cancelled() {
            thread::yield_now();
        }
        self.observed.store(true, Ordering::SeqCst);
        InvocationResult::Cancelled(None)
    }
}
struct ReentrantExecutor {
    supervisor: RuntimeSupervisor,
    restarted: Arc<AtomicBool>,
}
impl InvocationExecutor for ReentrantExecutor {
    fn execute(&mut self, _: &ExecutionBoundary) -> InvocationResult<TypedValue> {
        self.restarted
            .store(self.supervisor.restart().is_err(), Ordering::SeqCst);
        InvocationResult::Success(value("Str", "stale"))
    }
}
struct PanicExecutor {
    calls: Arc<AtomicUsize>,
}
impl InvocationExecutor for PanicExecutor {
    fn execute(&mut self, _: &ExecutionBoundary) -> InvocationResult<TypedValue> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        panic!("executor panic")
    }
}
fn diagnostic(code: &'static str) -> Diagnostic {
    Diagnostic {
        code,
        message: "invocation terminated",
        fields: BTreeMap::from([(
            "detail".into(),
            DiagnosticField::Redacted {
                static_type: ty("sys.Secret"),
            },
        )]),
        causes: Vec::new(),
    }
}
#[test]
fn canonical_order_and_type_retention() {
    let map = args(vec![
        Argument {
            name: "z".into(),
            value: value("Int", "1"),
        },
        Argument {
            name: "a".into(),
            value: value("Str", "1"),
        },
    ]);
    let names: Vec<_> = map.entries().map(|(name, _)| name).collect();
    assert_eq!(names, ["a", "z"]);
    assert_eq!(map.entries().next().unwrap().1.static_type(), &ty("Str"));
    assert!(matches!(
        ArgumentMap::new(vec![
            Argument {
                name: "a".into(),
                value: value("Str", "first"),
            },
            Argument {
                name: "a".into(),
                value: value("Str", "second"),
            },
        ]),
        Err(AdmissionError::ArgumentType {
            detail: ArgumentTypeDetail::Duplicate,
            ..
        })
    ));
}
#[test]
fn defaults_affect_identity() {
    let empty = ArgumentMap::default();
    let mut one = Runtime::new(RuntimeId::new("runtime"));
    let first = one
        .admit(request(Some(value("Int", "1")), empty.clone()))
        .unwrap();
    let mut two = Runtime::new(RuntimeId::new("runtime"));
    let second = two.admit(request(Some(value("Int", "2")), empty)).unwrap();
    let identity = |admission: Admission<String>| match admission {
        Admission::New { boundary, .. } => boundary.identity,
        _ => unreachable!(),
    };
    assert_ne!(identity(first), identity(second));
}
#[test]
fn rejects_mismatched_default_before_admission() {
    let mut runtime = Runtime::new(RuntimeId::new("r"));
    let result = runtime.admit(request(
        Some(value("Str", "not-an-int")),
        ArgumentMap::default(),
    ));

    assert!(matches!(
        result,
        Err(AdmissionError::ArgumentType {
            name,
            detail: ArgumentTypeDetail::Mismatch,
        }) if name == "a"
    ));
    assert!(runtime.invocations.is_empty());
}
#[test]
fn protected_argument_identity_uses_only_explicit_stable_nonsecret_metadata() {
    let mut runtime = Runtime::new(RuntimeId::new("r"));
    let reference = SecretRef::new("account").unwrap();
    let mut first = request(
        None,
        args(vec![Argument {
            name: "a".into(),
            value: TypedValue::protected_with_secret_ref(ty("Int"), "secret-one", &reference),
        }]),
    );
    first.idempotency_key = Some("key".into());

    let boundary = match runtime.admit(first.clone()).unwrap() {
        Admission::New { boundary, .. } => boundary,
        _ => unreachable!(),
    };
    let rendered = format!("{boundary:?}");
    assert!(rendered.contains("InvocationIdentity(<withheld>)"));
    assert!(!rendered.contains("secret-one"));
    assert!(!rendered.contains(boundary.identity.0.as_str()));

    let mut rotated_secret = first;
    rotated_secret.arguments = args(vec![Argument {
        name: "a".into(),
        value: TypedValue::protected_with_secret_ref(ty("Int"), "rotated-secret", &reference),
    }]);
    assert!(matches!(
        runtime.admit(rotated_secret),
        Ok(Admission::Active { .. })
    ));
}

#[test]
fn protected_argument_without_safe_identity_is_rejected_for_idempotency() {
    let mut runtime = Runtime::new(RuntimeId::new("r"));
    let mut request = request(
        None,
        args(vec![Argument {
            name: "a".into(),
            value: TypedValue::protected(
                ty("Int"),
                include_str!("../tests/fixtures/secret-surface.orna").as_bytes(),
            ),
        }]),
    );
    request.idempotency_key = Some("key".into());

    assert!(matches!(
        runtime.admit(request),
        Err(AdmissionError::ArgumentType {
            detail: ArgumentTypeDetail::ProtectedValueNeedsIdentity,
            ..
        })
    ));
    assert!(runtime.invocations.is_empty());
}

#[test]
fn retained_argument_metadata_is_ordered_and_redaction_safe() {
    let mut runtime = Runtime::new(RuntimeId::new("r"));
    let mut request = request(
        None,
        args(vec![
            Argument {
                name: "b".into(),
                value: TypedValue::protected(
                    ty("Str"),
                    include_str!("../tests/fixtures/secret-surface.orna")
                        .trim()
                        .as_bytes(),
                ),
            },
            Argument {
                name: "a".into(),
                value: value("Int", "42"),
            },
        ]),
    );
    request.function.parameters = vec![
        Parameter {
            name: "b".into(),
            static_type: ty("Str"),
            default: None,
        },
        Parameter {
            name: "a".into(),
            static_type: ty("Int"),
            default: None,
        },
    ];

    let handle = match runtime.admit(request).unwrap() {
        Admission::New { handle, .. } => handle,
        _ => unreachable!("a new request must create one retained observation"),
    };
    let metadata = runtime
        .invocations
        .get(handle.invocation.id())
        .expect("admitted invocation row")
        .arguments
        .clone();

    assert_eq!(metadata.len(), 2);
    assert_eq!(metadata[0].name, "a");
    assert_eq!(metadata[0].position, 0);
    assert_eq!(metadata[0].static_type, ty("Int"));
    assert_eq!(metadata[0].value, Some(b"42".to_vec()));
    assert!(!metadata[0].redacted);

    assert_eq!(metadata[1].name, "b");
    assert_eq!(metadata[1].position, 1);
    assert_eq!(metadata[1].static_type, ty("Str"));
    assert_eq!(metadata[1].value, None);
    assert!(metadata[1].redacted);

    let rendered = format!("{metadata:?}");
    let fixture = include_str!("../tests/fixtures/secret-surface.orna").trim();
    assert!(!rendered.contains(fixture));
    let serialized = serde_json::to_string(&metadata).unwrap();
    assert!(serialized.contains("\"redacted\":true"));
    assert!(!serialized.contains(fixture));
    assert!(!format!("{runtime:?}").contains("digest:"));

    let projection = runtime
        .invocation_metadata(&handle)
        .expect("admitted invocation metadata should project");
    assert_eq!(projection.invocation(), handle.invocation());
    assert_eq!(projection.status(), InvocationStatus::Running);
    assert_eq!(projection.result_type().as_str(), "Str");
    assert_eq!(projection.arguments()[0].name(), "a");
    assert_eq!(
        projection.arguments()[0].canonical(),
        Some(b"42".as_slice())
    );
    assert_eq!(projection.arguments()[1].name(), "b");
    assert_eq!(projection.arguments()[1].canonical(), None);
    assert!(projection.arguments()[1].is_redacted());
    assert!(!format!("{projection:?}").contains(fixture));
}

#[test]
fn secret_surface_fixture_stays_redacted_across_sys_value_projections() {
    let fixture = include_str!("../tests/fixtures/secret-surface.orna").trim();
    let protected = TypedValue::protected(ty("app.Credential"), fixture.as_bytes());

    let arguments = args(vec![Argument {
        name: "credential".into(),
        value: protected.clone(),
    }]);
    let metadata = arguments.metadata();
    assert_eq!(metadata[0].canonical(), None);
    assert!(metadata[0].is_redacted());
    let json = serde_json::to_string(&metadata).unwrap();
    assert!(json.contains("\"redacted\":true"));
    assert!(!json.contains(fixture));

    let retained = RetainedValue::new(protected);
    assert!(retained.is_redacted());
    assert_eq!(retained.canonical(), None);
    assert!(!format!("{retained:?}").contains(fixture));

    let metadata = SecretMetadata::new(
        orna_security::SecretRef::new("fixture.credential").unwrap(),
        "sops",
        true,
    );
    let secret_projection = system_secret_projection(&metadata);
    let json = serde_json::to_string(&secret_projection).unwrap();
    assert!(json.contains("fixture.credential"));
    assert!(json.contains("sops"));
    assert!(json.contains("true"));
    assert!(!json.contains(fixture));
}

#[test]
fn invocation_metadata_projects_failure_without_inventing_diagnostic_identity() {
    let mut runtime = Runtime::new(RuntimeId::new("r"));
    let handle = match runtime
        .admit(request(Some(value("Int", "1")), ArgumentMap::default()))
        .unwrap()
    {
        Admission::New { handle, .. } => handle,
        _ => unreachable!("a new request must create one retained observation"),
    };
    let failure = Diagnostic {
        code: "sys.invoke.target",
        message: "target failed",
        fields: BTreeMap::new(),
        causes: Vec::new(),
    };
    runtime
        .retain_terminal(&handle, InvocationResult::OrdinaryFailure(failure.clone()))
        .unwrap();

    let projection = runtime.invocation_metadata(&handle).unwrap();
    assert_eq!(projection.status(), InvocationStatus::Failed);
    assert_eq!(projection.failure(), Some(&failure));
    assert!(projection.ended().is_some());
}

#[test]
fn rejects_before_effect_boundary() {
    let map = args(vec![Argument {
        name: "a".into(),
        value: value("Str", "bad"),
    }]);
    let mut runtime = Runtime::new(RuntimeId::new("r"));
    assert!(matches!(
        runtime.admit(request(None, map)),
        Err(AdmissionError::ArgumentType { .. })
    ));
    assert_eq!(runtime.invocations.len(), 0);
    let missing = runtime.admit(request(None, ArgumentMap::default()));
    assert!(matches!(
        missing,
        Err(AdmissionError::ArgumentMissing { .. })
    ));
    let unknown = args(vec![Argument {
        name: "x".into(),
        value: value("Int", "1"),
    }]);
    assert!(matches!(
        runtime.admit(request(None, unknown)),
        Err(AdmissionError::ArgumentUnknown { .. })
    ));
    let mut hidden = request(Some(value("Int", "1")), ArgumentMap::default());
    hidden.function.visible = false;
    assert!(matches!(
        runtime.admit(hidden),
        Err(AdmissionError::NotVisible)
    ));
    let mut wrong_result = request(Some(value("Int", "1")), ArgumentMap::default());
    wrong_result.witness = TypeWitness::new(ty("Int"));
    assert!(matches!(
        runtime.admit(wrong_result),
        Err(AdmissionError::ReturnType)
    ));
    let mut wrong_argument_and_result = request(
        None,
        args(vec![Argument {
            name: "a".into(),
            value: value("Str", "bad"),
        }]),
    );
    wrong_argument_and_result.witness = TypeWitness::new(ty("Int"));
    assert!(matches!(
        runtime.admit(wrong_argument_and_result),
        Err(AdmissionError::ArgumentType { .. })
    ));
    assert!(runtime.invocations.is_empty());
    let mut snap = request(Some(value("Int", "1")), ArgumentMap::default());
    snap.explicit_snapshot = Some(SnapshotId::new("other"));
    assert!(matches!(
        runtime.admit(snap),
        Err(AdmissionError::SnapshotMismatch)
    ));
}
#[test]
fn idempotency_replays_active_terminal_and_conflicts() {
    let mut runtime = Runtime::new(RuntimeId::new("r"));
    let mut first = request(Some(value("Int", "1")), ArgumentMap::default());
    first.idempotency_key = Some("key".into());
    let accepted = runtime.admit(first.clone()).unwrap();
    let handle = match accepted {
        Admission::New { handle, .. } => handle,
        _ => unreachable!(),
    };
    assert!(matches!(
        runtime.admit(first.clone()).unwrap(),
        Admission::Active { .. }
    ));
    runtime
        .retain_terminal(&handle, InvocationResult::Success(value("Str", "answer")))
        .unwrap();
    let replayed = runtime.admit(first.clone()).unwrap();
    let Admission::Terminal {
        handle: replayed_handle,
        outcome,
        result,
    } = replayed
    else {
        panic!("expected retained terminal replay");
    };
    assert_eq!(replayed_handle.invocation(), handle.invocation());
    assert_eq!(outcome, TerminalClass::Succeeded);
    let RetainedInvocationResult::Success(retained) = result else {
        panic!("expected retained success");
    };
    assert_eq!(retained.static_type(), &ty("Str"));
    assert_eq!(retained.codec_version(), CANONICAL_VALUE_CODEC_V1);
    assert_eq!(retained.canonical(), Some(b"answer".as_slice()));
    assert_eq!(
        runtime.invocation_state(&handle),
        Ok(InvocationState::Terminal(
            RetainedInvocationResult::Success(retained)
        ))
    );
    assert!(matches!(
        runtime.admit(first.clone()).unwrap(),
        Admission::Terminal {
            outcome: TerminalClass::Succeeded,
            result: RetainedInvocationResult::Success(_),
            ..
        }
    ));
    let mut conflict = first;
    conflict.context.locale = Some("cy".into());
    assert!(matches!(
        runtime.admit(conflict),
        Err(AdmissionError::IdempotencyMismatch)
    ));
}
#[test]
fn run_executes_new_idempotent_work_once_and_replays_terminal_state() {
    let mut runtime = Runtime::new(RuntimeId::new("r"));
    let mut request = request(Some(value("Int", "1")), ArgumentMap::default());
    request.idempotency_key = Some("key".into());
    let mut first = Executor {
        calls: 0,
        result: InvocationResult::Success(value("Str", "answer")),
    };
    assert!(matches!(
        runtime.run(request.clone(), &mut first),
        Ok(InvocationState::Terminal(
            RetainedInvocationResult::Success(_)
        ))
    ));
    assert_eq!(first.calls, 1);

    let mut replay = Executor {
        calls: 0,
        result: InvocationResult::Success(value("Str", "wrong")),
    };
    assert!(matches!(
        runtime.run(request, &mut replay),
        Ok(InvocationState::Terminal(
            RetainedInvocationResult::Success(ref retained)
        )) if retained.canonical() == Some(b"answer".as_slice())
    ));
    assert_eq!(replay.calls, 0);
}
#[test]
fn run_rejects_async_mode_before_admission_or_effect() {
    let mut runtime = Runtime::new(RuntimeId::new("r"));
    let mut runtime_request = request(Some(value("Int", "1")), ArgumentMap::default());
    runtime_request.mode = InvocationMode::Start;
    runtime_request.transaction = TransactionMode::Separate;
    let mut executor = Executor {
        calls: 0,
        result: InvocationResult::Success(value("Str", "must-not-run")),
    };

    assert_eq!(
        runtime.run(runtime_request, &mut executor),
        Err(AdmissionError::StartMode)
    );
    assert_eq!(executor.calls, 0);
    assert_eq!(runtime.next_invocation, 0);
    assert!(runtime.invocations.is_empty());

    let supervisor = RuntimeSupervisor::new(RuntimeId::new("owner"));
    let mut supervisor_request = request(Some(value("Int", "1")), ArgumentMap::default());
    supervisor_request.mode = InvocationMode::Start;
    supervisor_request.transaction = TransactionMode::Separate;
    let mut executor = Executor {
        calls: 0,
        result: InvocationResult::Success(value("Str", "must-not-run")),
    };
    assert_eq!(
        supervisor.run(supervisor_request, &mut executor),
        Err(AdmissionError::StartMode)
    );
    assert_eq!(executor.calls, 0);
    assert_eq!(supervisor.generation(), Ok(1));
}
#[test]
fn run_retains_cancellation_as_a_terminal_classification() {
    let mut runtime = Runtime::new(RuntimeId::new("r"));
    let mut executor = Executor {
        calls: 0,
        result: InvocationResult::Cancelled(Some(diagnostic("cancelled"))),
    };
    let state = runtime.run(
        request(Some(value("Int", "1")), ArgumentMap::default()),
        &mut executor,
    );
    assert!(matches!(
        state,
        Ok(InvocationState::Terminal(
            RetainedInvocationResult::Cancelled(Some(_))
        ))
    ));
    assert_eq!(executor.calls, 1);
}
#[test]
fn supervised_run_replays_the_retained_terminal_result_and_obeys_restart_fence() {
    let supervisor = RuntimeSupervisor::new(RuntimeId::new("owner"));
    let mut request = request(Some(value("Int", "1")), ArgumentMap::default());
    request.idempotency_key = Some("key".into());
    let mut first = Executor {
        calls: 0,
        result: InvocationResult::Success(value("Str", "answer")),
    };
    assert!(matches!(
        supervisor.run(request.clone(), &mut first),
        Ok(InvocationState::Terminal(RetainedInvocationResult::Success(ref retained)))
            if retained.canonical() == Some(b"answer".as_slice())
    ));
    assert_eq!(first.calls, 1);

    let mut replay = Executor {
        calls: 0,
        result: InvocationResult::Success(value("Str", "wrong")),
    };
    assert!(matches!(
        supervisor.run(request.clone(), &mut replay),
        Ok(InvocationState::Terminal(RetainedInvocationResult::Success(ref retained)))
            if retained.canonical() == Some(b"answer".as_slice())
    ));
    assert_eq!(replay.calls, 0);

    supervisor.restart().unwrap();
    let mut after_restart = Executor {
        calls: 0,
        result: InvocationResult::Success(value("Str", "after-restart")),
    };
    assert!(matches!(
        supervisor.run(request, &mut after_restart),
        Ok(InvocationState::Terminal(RetainedInvocationResult::Success(ref retained)))
            if retained.canonical() == Some(b"answer".as_slice())
    ));
    assert_eq!(after_restart.calls, 0);
}

#[test]
fn supervised_run_does_not_block_cancel_or_timeout_observation() {
    let supervisor = RuntimeSupervisor::new(RuntimeId::new("owner"));
    let mut active_request = request(Some(value("Int", "1")), ArgumentMap::default());
    active_request.mode = InvocationMode::Start;
    active_request.transaction = TransactionMode::Separate;
    let active_gate = Arc::new((Mutex::new(false), Condvar::new()));
    let active_handle = supervisor
        .start(
            active_request,
            BlockingExecutor {
                gate: Arc::clone(&active_gate),
                calls: Arc::new(AtomicUsize::new(0)),
                result: InvocationResult::Success(value("Str", "active")),
            },
        )
        .unwrap();

    let run_gate = Arc::new((Mutex::new(false), Condvar::new()));
    let run_calls = Arc::new(AtomicUsize::new(0));
    let run_thread = {
        let supervisor = supervisor.clone();
        let run_gate = Arc::clone(&run_gate);
        let run_calls = Arc::clone(&run_calls);
        thread::spawn(move || {
            let mut executor = BlockingExecutor {
                gate: run_gate,
                calls: run_calls,
                result: InvocationResult::Success(value("Str", "run")),
            };
            supervisor.run(
                request(Some(value("Int", "2")), ArgumentMap::default()),
                &mut executor,
            )
        })
    };
    let deadline = Instant::now() + Duration::from_secs(1);
    while run_calls.load(Ordering::SeqCst) == 0 && Instant::now() < deadline {
        thread::yield_now();
    }
    assert_eq!(run_calls.load(Ordering::SeqCst), 1);

    let (await_sender, await_receiver) = mpsc::channel();
    let await_supervisor = supervisor.clone();
    let await_handle = active_handle.clone();
    let await_thread = thread::spawn(move || {
        let result =
            await_supervisor.await_invocation(&await_handle, Some(Duration::from_millis(20)));
        await_sender.send(result).unwrap();
    });
    assert!(matches!(
        await_receiver.recv_timeout(Duration::from_secs(1)),
        Ok(Err(AwaitError::Timeout))
    ));

    let (cancel_sender, cancel_receiver) = mpsc::channel();
    let cancel_supervisor = supervisor.clone();
    let cancel_handle = active_handle.clone();
    let cancel_thread = thread::spawn(move || {
        cancel_sender
            .send(cancel_supervisor.cancel(&cancel_handle, Some(diagnostic("cancelled"))))
            .unwrap();
    });
    assert_eq!(
        cancel_receiver.recv_timeout(Duration::from_secs(1)),
        Ok(Ok(true))
    );

    let (active_open, active_wake) = &*active_gate;
    *active_open.lock().unwrap() = true;
    active_wake.notify_one();
    let (run_open, run_wake) = &*run_gate;
    *run_open.lock().unwrap() = true;
    run_wake.notify_one();
    await_thread.join().unwrap();
    cancel_thread.join().unwrap();
    assert!(matches!(
        supervisor.await_invocation(&active_handle, Some(Duration::from_secs(1))),
        Ok(super::InvocationResult {
            status: InvocationStatus::Cancelled,
            value: None,
            failure: Some(_),
            ..
        })
    ));
    let run_result = run_thread.join().unwrap();
    assert!(matches!(run_result, Ok(InvocationState::Terminal(_))));
}

#[test]
fn supervised_run_rejects_reentrant_restart_without_rotating_owner() {
    let supervisor = RuntimeSupervisor::new(RuntimeId::new("owner"));
    let mut request = request(Some(value("Int", "1")), ArgumentMap::default());
    request.idempotency_key = Some("reentrant".into());
    let restarted = Arc::new(AtomicBool::new(false));

    let result = supervisor.run(
        request.clone(),
        &mut ReentrantExecutor {
            supervisor: supervisor.clone(),
            restarted: Arc::clone(&restarted),
        },
    );

    assert!(matches!(
        result,
        Ok(InvocationState::Terminal(
            RetainedInvocationResult::Success(_)
        ))
    ));
    assert!(restarted.load(Ordering::SeqCst));
    assert_eq!(supervisor.generation(), Ok(1));
}

#[test]
fn supervised_run_fences_restart_before_admission_and_execution() {
    let supervisor = RuntimeSupervisor::new(RuntimeId::new("owner"));
    let hook = Arc::new((Mutex::new((false, false)), Condvar::new()));
    supervisor
        .synchronous
        .set_enter_hook(Some(Arc::clone(&hook)));
    let run_supervisor = supervisor.clone();
    let run_thread = thread::spawn(move || {
        let mut executor = Executor {
            calls: 0,
            result: InvocationResult::Success(value("Str", "done")),
        };
        run_supervisor.run(
            request(Some(value("Int", "1")), ArgumentMap::default()),
            &mut executor,
        )
    });
    {
        let (state, wake) = &*hook;
        let mut state = state.lock().unwrap();
        while !state.0 {
            state = wake.wait(state).unwrap();
        }
    }
    let (restart_sender, restart_receiver) = mpsc::channel();
    let restart_supervisor = supervisor.clone();
    let restart_thread = thread::spawn(move || {
        restart_sender.send(restart_supervisor.restart()).unwrap();
    });
    assert!(
        restart_receiver
            .recv_timeout(Duration::from_millis(20))
            .is_err()
    );
    {
        let (state, wake) = &*hook;
        state.lock().unwrap().1 = true;
        wake.notify_all();
    }
    supervisor.synchronous.set_enter_hook(None);
    // The restart is released the moment admission ends, so its cancel-all
    // can land before the run commits its result. Success and Cancelled
    // are both legal; a lost or orphaned run is not.
    assert!(matches!(
        run_thread.join().unwrap(),
        Ok(InvocationState::Terminal(
            RetainedInvocationResult::Success(_) | RetainedInvocationResult::Cancelled(_)
        ))
    ));
    assert_eq!(
        restart_receiver.recv_timeout(Duration::from_secs(1)),
        Ok(Ok(RuntimeId::new("owner@2")))
    );
    restart_thread.join().unwrap();
}

#[test]
fn supervised_run_replays_one_orphaned_result_after_executor_panic() {
    let supervisor = RuntimeSupervisor::new(RuntimeId::new("owner"));
    let mut request = request(Some(value("Int", "1")), ArgumentMap::default());
    request.idempotency_key = Some("panic".into());
    let calls = Arc::new(AtomicUsize::new(0));
    let mut first = PanicExecutor {
        calls: Arc::clone(&calls),
    };
    assert!(matches!(
        supervisor.run(request.clone(), &mut first),
        Ok(InvocationState::Terminal(
            RetainedInvocationResult::Orphaned(Some(_))
        ))
    ));
    let mut replay = PanicExecutor {
        calls: Arc::clone(&calls),
    };
    assert!(matches!(
        supervisor.run(request, &mut replay),
        Ok(InvocationState::Terminal(
            RetainedInvocationResult::Orphaned(Some(_))
        ))
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn restart_waits_for_a_running_synchronous_callback() {
    let supervisor = RuntimeSupervisor::new(RuntimeId::new("owner"));
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let calls = Arc::new(AtomicUsize::new(0));
    let run_thread = {
        let supervisor = supervisor.clone();
        let gate = Arc::clone(&gate);
        let calls = Arc::clone(&calls);
        thread::spawn(move || {
            let mut executor = BlockingExecutor {
                gate,
                calls,
                result: InvocationResult::Success(value("Str", "done")),
            };
            supervisor.run(
                request(Some(value("Int", "1")), ArgumentMap::default()),
                &mut executor,
            )
        })
    };
    while calls.load(Ordering::SeqCst) == 0 {
        thread::yield_now();
    }

    let (restart_sender, restart_receiver) = mpsc::channel();
    let restart_supervisor = supervisor.clone();
    let restart_thread = thread::spawn(move || {
        restart_sender.send(restart_supervisor.restart()).unwrap();
    });
    assert!(
        restart_receiver
            .recv_timeout(Duration::from_millis(20))
            .is_err()
    );

    let (open, wake) = &*gate;
    *open.lock().unwrap() = true;
    wake.notify_one();
    assert!(matches!(
        run_thread.join().unwrap(),
        Ok(InvocationState::Terminal(
            RetainedInvocationResult::Cancelled(_)
        ))
    ));
    assert_eq!(
        restart_receiver.recv_timeout(Duration::from_secs(1)),
        Ok(Ok(RuntimeId::new("owner@2")))
    );
    restart_thread.join().unwrap();
}

#[test]
fn restart_pending_does_not_block_callback_cancel_or_await() {
    let supervisor = RuntimeSupervisor::new(RuntimeId::new("owner"));
    let mut request = request(Some(value("Int", "1")), ArgumentMap::default());
    request.mode = InvocationMode::Start;
    request.transaction = TransactionMode::Separate;
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let calls = Arc::new(AtomicUsize::new(0));
    let handle = supervisor
        .start(
            request,
            BlockingExecutor {
                gate: Arc::clone(&gate),
                calls: Arc::clone(&calls),
                result: InvocationResult::Success(value("Str", "done")),
            },
        )
        .unwrap();
    while calls.load(Ordering::SeqCst) == 0 {
        thread::yield_now();
    }

    let (restart_sender, restart_receiver) = mpsc::channel();
    let restart_supervisor = supervisor.clone();
    let restart_thread = thread::spawn(move || {
        restart_sender.send(restart_supervisor.restart()).unwrap();
    });
    assert!(
        restart_receiver
            .recv_timeout(Duration::from_millis(20))
            .is_err()
    );

    assert_eq!(
        supervisor.await_invocation(&handle, Some(Duration::ZERO)),
        Err(AwaitError::Timeout)
    );
    assert_eq!(
        supervisor.cancel(&handle, Some(diagnostic("cancelled"))),
        Ok(true)
    );
    let (open, wake) = &*gate;
    *open.lock().unwrap() = true;
    wake.notify_one();

    assert_eq!(
        restart_receiver.recv_timeout(Duration::from_secs(1)),
        Ok(Ok(RuntimeId::new("owner@2")))
    );
    assert_eq!(
        supervisor.state(&handle),
        Err(AdmissionError::ForeignRuntime)
    );
    restart_thread.join().unwrap();
}

#[test]
fn synchronous_reservation_rolls_back_when_worker_is_not_started() {
    let executions = Arc::new(SynchronousExecutions::default());
    {
        let _reservation = executions.reserve().unwrap();
        assert_eq!(executions.pending(), 1);
    }
    assert_eq!(executions.pending(), 0);
    executions.wait_empty().unwrap();
}

#[test]
fn active_worker_terminal_drop_recovers_poisoned_execution_state() {
    let supervisor = RuntimeSupervisor::new(RuntimeId::new("r"));
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let calls = Arc::new(AtomicUsize::new(0));
    let mut request = request(Some(value("Int", "1")), ArgumentMap::default());
    request.mode = InvocationMode::Start;
    request.transaction = TransactionMode::Separate;
    let handle = supervisor
        .start(
            request,
            BlockingExecutor {
                gate: Arc::clone(&gate),
                calls: Arc::clone(&calls),
                result: InvocationResult::Success(value("Str", "done")),
            },
        )
        .unwrap();
    while calls.load(Ordering::SeqCst) == 0 {
        thread::yield_now();
    }

    let synchronous = Arc::clone(&supervisor.synchronous);
    let poisoner = thread::spawn(move || {
        let _state = synchronous.state.lock().unwrap();
        panic!("poison active execution state");
    });
    assert!(poisoner.join().is_err());

    assert_eq!(
        supervisor.cancel(&handle, Some(diagnostic("cancelled"))),
        Ok(true)
    );
    let (open, wake) = &*gate;
    *open.lock().unwrap() = true;
    wake.notify_one();

    let result = supervisor
        .await_invocation(&handle, Some(Duration::from_secs(1)))
        .unwrap();
    assert_eq!(result.status, InvocationStatus::Cancelled);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(supervisor.restart(), Ok(RuntimeId::new("r@2")));
}

#[test]
fn restart_fences_admission_before_a_new_callback_can_begin() {
    let supervisor = RuntimeSupervisor::new(RuntimeId::new("owner"));
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let calls = Arc::new(AtomicUsize::new(0));
    let hook = Arc::new((Mutex::new((false, false)), Condvar::new()));
    supervisor
        .synchronous
        .set_enter_hook(Some(Arc::clone(&hook)));

    let run_supervisor = supervisor.clone();
    let run_gate = Arc::clone(&gate);
    let run_calls = Arc::clone(&calls);
    let run_thread = thread::spawn(move || {
        let mut executor = BlockingExecutor {
            gate: run_gate,
            calls: run_calls,
            result: InvocationResult::Success(value("Str", "unexpected")),
        };
        run_supervisor.run(
            request(Some(value("Int", "1")), ArgumentMap::default()),
            &mut executor,
        )
    });

    {
        let (entered, wake) = &*hook;
        let mut state = entered.lock().unwrap();
        while !state.0 {
            state = wake.wait(state).unwrap();
        }
    }
    let (restart_sender, restart_receiver) = mpsc::channel();
    let restart_supervisor = supervisor.clone();
    let restart_thread = thread::spawn(move || {
        restart_sender.send(restart_supervisor.restart()).unwrap();
    });
    assert!(
        restart_receiver
            .recv_timeout(Duration::from_millis(20))
            .is_err()
    );

    {
        let (entered, wake) = &*hook;
        entered.lock().unwrap().1 = true;
        wake.notify_all();
    }
    supervisor.synchronous.set_enter_hook(None);
    let (open, wake) = &*gate;
    *open.lock().unwrap() = true;
    wake.notify_one();
    assert!(run_thread.join().unwrap().is_ok());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        restart_receiver.recv_timeout(Duration::from_secs(1)),
        Ok(Ok(RuntimeId::new("owner@2")))
    );
    restart_thread.join().unwrap();
}

#[test]
fn supervised_start_timeout_does_not_cancel_or_duplicate_work() {
    let supervisor = RuntimeSupervisor::new(RuntimeId::new("r"));
    let mut request = request(Some(value("Int", "1")), ArgumentMap::default());
    request.mode = InvocationMode::Start;
    request.transaction = TransactionMode::Separate;
    request.idempotency_key = Some("key".into());
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let calls = Arc::new(AtomicUsize::new(0));
    let handle = supervisor
        .start(
            request.clone(),
            BlockingExecutor {
                gate: Arc::clone(&gate),
                calls: Arc::clone(&calls),
                result: InvocationResult::Success(value("Str", "answer")),
            },
        )
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(1);
    while calls.load(Ordering::SeqCst) == 0 && Instant::now() < deadline {
        thread::yield_now();
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    assert_eq!(
        supervisor.await_invocation(&handle, Some(Duration::ZERO)),
        Err(AwaitError::Timeout)
    );
    assert_eq!(supervisor.state(&handle), Ok(InvocationState::Active));
    assert_eq!(
        supervisor.cancel(&handle, Some(diagnostic("cancelled"))),
        Ok(true)
    );
    let (open, wake) = &*gate;
    *open.lock().unwrap() = true;
    wake.notify_one();

    let result = supervisor
        .await_invocation(&handle, Some(Duration::from_secs(1)))
        .unwrap();
    assert_eq!(result.status, InvocationStatus::Cancelled);
    assert_eq!(result.value, None);
    assert_eq!(result.failure, Some(diagnostic("cancelled")));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(supervisor.cancel(&handle, None), Ok(true));

    let replay_calls = Arc::new(AtomicUsize::new(0));
    let replay = supervisor
        .start(
            request,
            BlockingExecutor {
                gate,
                calls: Arc::clone(&replay_calls),
                result: InvocationResult::Success(value("Str", "wrong")),
            },
        )
        .unwrap();
    assert_eq!(replay.invocation(), handle.invocation());
    assert_eq!(replay_calls.load(Ordering::SeqCst), 0);
}
#[test]
fn supervised_start_metadata_transitions_from_queued_to_running_then_cancelled() {
    let supervisor = RuntimeSupervisor::new(RuntimeId::new("r"));
    supervisor.hold_next_worker_start();
    let mut request = request(Some(value("Int", "1")), ArgumentMap::default());
    request.mode = InvocationMode::Start;
    request.transaction = TransactionMode::Separate;
    let started = Arc::new(AtomicBool::new(false));
    let handle = supervisor
        .start(
            request,
            AdmissionLifecycleExecutor {
                started: Arc::clone(&started),
            },
        )
        .unwrap();

    assert_eq!(
        supervisor.invocation_metadata(&handle).unwrap().status(),
        InvocationStatus::Queued
    );
    supervisor.release_held_worker_start();
    let deadline = Instant::now() + Duration::from_secs(1);
    while !started.load(Ordering::SeqCst) && Instant::now() < deadline {
        thread::yield_now();
    }
    assert!(started.load(Ordering::SeqCst));
    assert_eq!(
        supervisor.invocation_metadata(&handle).unwrap().status(),
        InvocationStatus::Running
    );
    assert_eq!(
        supervisor.await_invocation(&handle, Some(Duration::ZERO)),
        Err(AwaitError::Timeout)
    );
    assert_eq!(
        supervisor.invocation_metadata(&handle).unwrap().status(),
        InvocationStatus::Running
    );
    assert_eq!(
        supervisor.cancel(&handle, Some(diagnostic("cancelled"))),
        Ok(true)
    );
    let result = supervisor
        .await_invocation(&handle, Some(Duration::from_secs(1)))
        .unwrap();
    assert_eq!(result.status, InvocationStatus::Cancelled);
    assert_eq!(
        supervisor.invocation_metadata(&handle).unwrap().status(),
        InvocationStatus::Cancelled
    );
    assert_eq!(supervisor.cancel(&handle, None), Ok(true));
}
#[test]
fn supervised_start_cancelled_while_queued_never_enters_callback() {
    let supervisor = RuntimeSupervisor::new(RuntimeId::new("r"));
    supervisor.hold_next_worker_start();
    let mut request = request(Some(value("Int", "1")), ArgumentMap::default());
    request.mode = InvocationMode::Start;
    request.transaction = TransactionMode::Separate;
    let calls = Arc::new(AtomicUsize::new(0));
    let handle = supervisor
        .start(
            request,
            CountingExecutor {
                calls: Arc::clone(&calls),
            },
        )
        .unwrap();

    assert_eq!(
        supervisor.invocation_metadata(&handle).unwrap().status(),
        InvocationStatus::Queued
    );
    let reason = diagnostic("queued-cancelled");
    assert_eq!(supervisor.cancel(&handle, Some(reason.clone())), Ok(true));
    supervisor.release_held_worker_start();

    let result = supervisor
        .await_invocation(&handle, Some(Duration::from_secs(1)))
        .unwrap();
    assert_eq!(result.status, InvocationStatus::Cancelled);
    assert_eq!(result.value, None);
    assert_eq!(result.failure, Some(reason));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(supervisor.cancel(&handle, None), Ok(true));
}

#[test]
fn supervised_start_registration_failure_retains_orphaned_identity() {
    let supervisor = RuntimeSupervisor::new(RuntimeId::new("r"));
    let workers = Arc::clone(&supervisor.workers);
    let poisoner = thread::spawn(move || {
        let _workers = workers.lock().unwrap();
        panic!("poison worker registry");
    });
    assert!(poisoner.join().is_err());

    let mut initial_request = request(Some(value("Int", "1")), ArgumentMap::default());
    initial_request.mode = InvocationMode::Start;
    initial_request.transaction = TransactionMode::Separate;
    initial_request.idempotency_key = Some("registration-failure".into());
    let calls = Arc::new(AtomicUsize::new(0));
    assert_eq!(
        supervisor.start(
            initial_request.clone(),
            CountingExecutor {
                calls: Arc::clone(&calls),
            },
        ),
        Err(AdmissionError::RuntimeUnavailable)
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(supervisor.synchronous.pending(), 0);

    let replay_calls = Arc::new(AtomicUsize::new(0));
    let replay = supervisor
        .start(
            initial_request,
            CountingExecutor {
                calls: Arc::clone(&replay_calls),
            },
        )
        .unwrap();
    assert!(matches!(
        supervisor.state(&replay),
        Ok(InvocationState::Terminal(
            RetainedInvocationResult::Orphaned(None)
        ))
    ));
    assert!(matches!(
        supervisor.await_invocation(&replay, Some(Duration::ZERO)),
        Ok(super::InvocationResult {
            status: InvocationStatus::Orphaned,
            value: None,
            failure: None,
            ..
        })
    ));
    assert!(supervisor.restart().is_ok());
    assert_eq!(replay_calls.load(Ordering::SeqCst), 0);

    let mut fresh_request = request(Some(value("Int", "2")), ArgumentMap::default());
    fresh_request.mode = InvocationMode::Start;
    fresh_request.transaction = TransactionMode::Separate;
    let fresh_calls = Arc::new(AtomicUsize::new(0));
    let fresh_handle = supervisor
        .start(
            fresh_request,
            CountingExecutor {
                calls: Arc::clone(&fresh_calls),
            },
        )
        .unwrap();
    assert!(matches!(
        supervisor.await_invocation(&fresh_handle, Some(Duration::from_secs(1))),
        Ok(super::InvocationResult {
            status: InvocationStatus::Succeeded,
            value: Some(_),
            failure: None,
            ..
        })
    ));
    assert_eq!(fresh_calls.load(Ordering::SeqCst), 1);
}

#[test]
fn supervised_start_reservation_failure_retains_orphaned_identity() {
    let supervisor = RuntimeSupervisor::new(RuntimeId::new("r"));
    let synchronous = Arc::clone(&supervisor.synchronous);
    let poisoner = thread::spawn(move || {
        let _state = synchronous.state.lock().unwrap();
        panic!("poison synchronous execution state");
    });
    assert!(poisoner.join().is_err());

    let mut request = request(Some(value("Int", "1")), ArgumentMap::default());
    request.mode = InvocationMode::Start;
    request.transaction = TransactionMode::Separate;
    request.idempotency_key = Some("reservation-failure".into());
    let calls = Arc::new(AtomicUsize::new(0));
    let handle = supervisor
        .start(
            request,
            CountingExecutor {
                calls: Arc::clone(&calls),
            },
        )
        .unwrap();

    assert!(matches!(
        supervisor.state(&handle),
        Ok(InvocationState::Terminal(
            RetainedInvocationResult::Orphaned(None)
        ))
    ));
    assert!(matches!(
        supervisor.await_invocation(&handle, Some(Duration::ZERO)),
        Ok(super::InvocationResult {
            status: InvocationStatus::Orphaned,
            value: None,
            failure: None,
            ..
        })
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(supervisor.synchronous.pending(), 0);
}

#[test]
fn supervised_start_begin_failure_retains_orphaned_identity() {
    let supervisor = RuntimeSupervisor::new(RuntimeId::new("r"));
    let hook = Arc::new((Mutex::new((false, false)), Condvar::new()));
    supervisor
        .synchronous
        .set_enter_hook(Some(Arc::clone(&hook)));
    let mut request = request(Some(value("Int", "1")), ArgumentMap::default());
    request.mode = InvocationMode::Start;
    request.transaction = TransactionMode::Separate;
    request.idempotency_key = Some("begin-failure".into());
    let calls = Arc::new(AtomicUsize::new(0));
    let handle = supervisor
        .start(
            request,
            CountingExecutor {
                calls: Arc::clone(&calls),
            },
        )
        .unwrap();
    {
        let (state, wake) = &*hook;
        let mut state = state.lock().unwrap();
        while !state.0 {
            state = wake.wait(state).unwrap();
        }
    }

    let synchronous = Arc::clone(&supervisor.synchronous);
    let poisoner = thread::spawn(move || {
        let _state = synchronous.state.lock().unwrap();
        panic!("poison synchronous execution state");
    });
    assert!(poisoner.join().is_err());
    {
        let (state, wake) = &*hook;
        state.lock().unwrap().1 = true;
        wake.notify_all();
    }

    let result = supervisor
        .await_invocation(&handle, Some(Duration::from_secs(1)))
        .unwrap();
    assert_eq!(result.status, InvocationStatus::Orphaned);
    assert_eq!(result.value, None);
    assert_eq!(result.failure, None);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(supervisor.synchronous.pending(), 0);
    supervisor.synchronous.set_enter_hook(None);
}

#[test]
fn supervised_start_recovers_workers_mutex_poison_for_later_admission() {
    let supervisor = RuntimeSupervisor::new(RuntimeId::new("r"));
    let workers = Arc::clone(&supervisor.workers);
    let poisoner = thread::spawn(move || {
        let _workers = workers.lock().unwrap();
        panic!("poison worker registry");
    });
    assert!(poisoner.join().is_err());

    let mut initial_request = request(Some(value("Int", "1")), ArgumentMap::default());
    initial_request.mode = InvocationMode::Start;
    initial_request.transaction = TransactionMode::Separate;
    assert_eq!(
        supervisor.start(
            initial_request,
            Executor {
                calls: 0,
                result: InvocationResult::Success(value("Str", "must-not-run")),
            },
        ),
        Err(AdmissionError::RuntimeUnavailable)
    );

    let mut fresh_request = request(Some(value("Int", "2")), ArgumentMap::default());
    fresh_request.mode = InvocationMode::Start;
    fresh_request.transaction = TransactionMode::Separate;
    let fresh_handle = supervisor
        .start(
            fresh_request,
            Executor {
                calls: 0,
                result: InvocationResult::Success(value("Str", "recovered")),
            },
        )
        .unwrap();
    let result = supervisor
        .await_invocation(&fresh_handle, Some(Duration::from_secs(1)))
        .unwrap();
    assert_eq!(result.status, InvocationStatus::Succeeded);
    assert_eq!(
        result.value.as_ref().and_then(RetainedValue::canonical),
        Some(b"recovered".as_slice())
    );
}

#[test]
fn supervised_await_reaps_the_completed_worker() {
    let supervisor = RuntimeSupervisor::new(RuntimeId::new("r"));
    let mut request = request(Some(value("Int", "1")), ArgumentMap::default());
    request.mode = InvocationMode::Start;
    request.transaction = TransactionMode::Separate;
    let handle = supervisor
        .start(
            request,
            Executor {
                calls: 0,
                result: InvocationResult::Success(value("Str", "answer")),
            },
        )
        .unwrap();

    assert!(matches!(
        supervisor.await_invocation(&handle, Some(Duration::from_secs(1))),
        Ok(super::InvocationResult {
            status: InvocationStatus::Succeeded,
            value: Some(ref retained),
            failure: None,
            ..
        }) if retained.canonical() == Some(b"answer".as_slice())
    ));
    assert!(supervisor.workers.lock().unwrap().is_empty());
}
#[test]
fn supervised_await_reaps_completed_worker_after_workers_mutex_poison() {
    let supervisor = RuntimeSupervisor::new(RuntimeId::new("r"));
    let mut request = request(Some(value("Int", "1")), ArgumentMap::default());
    request.mode = InvocationMode::Start;
    request.transaction = TransactionMode::Separate;
    let handle = supervisor
        .start(
            request,
            Executor {
                calls: 0,
                result: InvocationResult::Success(value("Str", "done")),
            },
        )
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        if matches!(
            supervisor.state(&handle),
            Ok(InvocationState::Terminal(
                RetainedInvocationResult::Success(_)
            ))
        ) {
            break;
        }
        assert!(Instant::now() < deadline, "worker did not complete");
        thread::yield_now();
    }

    let workers = Arc::clone(&supervisor.workers);
    let poisoner = thread::spawn(move || {
        let _workers = workers.lock().unwrap();
        panic!("poison workers mutex");
    });
    assert!(poisoner.join().is_err());

    let result = supervisor
        .await_invocation(&handle, Some(Duration::from_secs(1)))
        .unwrap();
    assert_eq!(result.status, InvocationStatus::Succeeded);
    assert!(supervisor.workers.lock().unwrap().is_empty());
}

#[test]
fn public_handle_and_await_result_have_the_portable_terminal_shape() {
    let supervisor = RuntimeSupervisor::new(RuntimeId::new("r"));
    let mut request = request(Some(value("Int", "1")), ArgumentMap::default());
    request.mode = InvocationMode::Start;
    request.transaction = TransactionMode::Separate;
    let handle = supervisor
        .start(
            request,
            Executor {
                calls: 0,
                result: InvocationResult::Success(value("Str", "answer")),
            },
        )
        .unwrap();

    assert_eq!(handle.invocation, handle.invocation().clone());
    assert_eq!(handle.runtime, handle.runtime().clone());
    assert_eq!(handle.result_type, handle.result_type().clone());
    assert!(!handle.resumable);

    let result = supervisor
        .await_invocation(&handle, Some(Duration::from_secs(1)))
        .unwrap();
    assert_eq!(result.invocation, handle.invocation);
    assert_eq!(result.status, InvocationStatus::Succeeded);
    assert!(result.status.is_terminal());
    assert!(result.value.is_some());
    assert_eq!(result.failure, None);
    assert!(result.started.is_some());
    assert!(result.ended >= result.started.unwrap());
    assert!(result.duration.is_some());
}

#[test]
fn descriptive_refs_are_nominal_runtime_issued_tokens() {
    let mut runtime = Runtime::new(RuntimeId::new("r"));
    let handle = match runtime
        .admit(request(Some(value("Int", "1")), ArgumentMap::default()))
        .unwrap()
    {
        Admission::New { handle, .. } => handle,
        _ => unreachable!(),
    };

    let _: InvocationRef = handle.invocation.clone();
    let _: TypeRef = handle.result_type.clone();
    assert_eq!(handle.invocation.as_str(), "invocation-1");
    assert_eq!(handle.result_type.as_str(), "Str");
}

#[test]
fn resumable_handles_are_rejected_even_when_the_public_field_is_mutated() {
    let mut runtime = Runtime::new(RuntimeId::new("r"));
    let mut handle = match runtime
        .admit(request(Some(value("Int", "1")), ArgumentMap::default()))
        .unwrap()
    {
        Admission::New { handle, .. } => handle,
        _ => unreachable!(),
    };

    handle.resumable = true;
    assert_eq!(
        runtime.check_handle(&handle),
        Err(AdmissionError::ExpiredHandle)
    );
    assert_eq!(
        runtime.invocation_state(&handle),
        Err(AdmissionError::ExpiredHandle)
    );
}

#[test]
fn supervised_start_retains_explicit_cancellation() {
    let supervisor = RuntimeSupervisor::new(RuntimeId::new("r"));
    let mut request = request(Some(value("Int", "1")), ArgumentMap::default());
    request.mode = InvocationMode::Start;
    request.transaction = TransactionMode::Separate;
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let handle = supervisor
        .start(
            request,
            BlockingExecutor {
                gate: Arc::clone(&gate),
                calls: Arc::new(AtomicUsize::new(0)),
                result: InvocationResult::Cancelled(None),
            },
        )
        .unwrap();
    let reason = diagnostic("cancelled");
    assert_eq!(supervisor.cancel(&handle, Some(reason.clone())), Ok(true));
    let (open, wake) = &*gate;
    *open.lock().unwrap() = true;
    wake.notify_one();

    let result = supervisor
        .await_invocation(&handle, Some(Duration::from_secs(1)))
        .unwrap();
    assert_eq!(result.status, InvocationStatus::Cancelled);
    assert_eq!(result.value, None);
    assert_eq!(result.failure, Some(reason));
}
#[test]
fn supervised_cancellation_token_reaches_running_executor() {
    let supervisor = RuntimeSupervisor::new(RuntimeId::new("r"));
    let mut request = request(Some(value("Int", "1")), ArgumentMap::default());
    request.mode = InvocationMode::Start;
    request.transaction = TransactionMode::Separate;
    let started = Arc::new(AtomicBool::new(false));
    let observed = Arc::new(AtomicBool::new(false));
    let handle = supervisor
        .start(
            request,
            CancellableExecutor {
                started: Arc::clone(&started),
                observed: Arc::clone(&observed),
            },
        )
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(1);
    while !started.load(Ordering::SeqCst) && Instant::now() < deadline {
        thread::yield_now();
    }
    assert!(started.load(Ordering::SeqCst));

    assert_eq!(supervisor.cancel(&handle, None), Ok(true));
    let result = supervisor
        .await_invocation(&handle, Some(Duration::from_secs(1)))
        .unwrap();
    assert_eq!(result.status, InvocationStatus::Cancelled);
    assert_eq!(result.value, None);
    assert_eq!(result.failure, None);
    assert!(observed.load(Ordering::SeqCst));
}
#[test]
fn fences_foreign_and_stale_handles() {
    let mut left = Runtime::new(RuntimeId::new("left"));
    let accepted = left
        .admit(request(Some(value("Int", "1")), ArgumentMap::default()))
        .unwrap();
    let handle = match accepted {
        Admission::New { handle, .. } => handle,
        _ => unreachable!(),
    };
    let mut right = Runtime::new(RuntimeId::new("right"));
    assert_eq!(
        right.check_handle(&handle),
        Err(AdmissionError::ForeignRuntime)
    );
    assert_eq!(
        right.invocation_state(&handle),
        Err(AdmissionError::ForeignRuntime)
    );
    assert_eq!(
        right.retain_terminal(
            &handle,
            InvocationResult::OrdinaryFailure(diagnostic("failed")),
        ),
        Err(AdmissionError::ForeignRuntime)
    );
    left.retain_terminal(
        &handle,
        InvocationResult::OrdinaryFailure(diagnostic("failed")),
    )
    .unwrap();
    left.expire(&handle).unwrap();
    assert_eq!(
        left.check_handle(&handle),
        Err(AdmissionError::ExpiredHandle)
    );
    assert_eq!(
        left.invocation_state(&handle),
        Err(AdmissionError::ExpiredHandle)
    );
    assert_eq!(
        left.retain_terminal(
            &handle,
            InvocationResult::OrdinaryFailure(diagnostic("failed")),
        ),
        Err(AdmissionError::ExpiredHandle)
    );
}

#[test]
fn restart_changes_runtime_generation_and_retains_active_as_orphaned() {
    let mut runtime = Runtime::new(RuntimeId::new("owner"));
    let mut active_request = request(Some(value("Int", "1")), ArgumentMap::default());
    active_request.idempotency_key = Some("key".into());
    let old_id = runtime.id().clone();
    let old_generation = runtime.generation();
    let old_handle = match runtime.admit(active_request.clone()).unwrap() {
        Admission::New { handle, .. } => handle,
        _ => unreachable!(),
    };

    let new_id = runtime.restart();

    assert_ne!(new_id, old_id);
    assert_eq!(runtime.id(), &new_id);
    assert_eq!(runtime.generation(), old_generation + 1);
    assert_eq!(
        runtime.check_handle(&old_handle),
        Err(AdmissionError::ForeignRuntime)
    );
    assert_eq!(
        runtime.invocation_state(&old_handle),
        Err(AdmissionError::ForeignRuntime)
    );
    let new_handle = match runtime.admit(active_request.clone()).unwrap() {
        Admission::Terminal {
            handle,
            outcome: TerminalClass::Orphaned,
            result: RetainedInvocationResult::Orphaned(None),
        } => handle,
        _ => panic!("a restarted owner must retain the orphaned operation"),
    };
    assert_ne!(new_handle.runtime(), old_handle.runtime());
    assert_eq!(new_handle.invocation(), old_handle.invocation());

    let mut executor = Executor {
        calls: 0,
        result: InvocationResult::Success(value("Str", "must-not-run")),
    };
    assert!(matches!(
        runtime.run(active_request, &mut executor),
        Ok(InvocationState::Terminal(
            RetainedInvocationResult::Orphaned(None)
        ))
    ));
    assert_eq!(executor.calls, 0);

    let mut next_request = request(Some(value("Int", "1")), ArgumentMap::default());
    next_request.idempotency_key = Some("next-key".into());
    let next_handle = match runtime.admit(next_request).unwrap() {
        Admission::New { handle, .. } => handle,
        _ => unreachable!(),
    };
    assert_ne!(next_handle.invocation(), old_handle.invocation());
}

#[test]
fn restart_preserves_terminal_result_and_fences_old_handle() {
    let mut runtime = Runtime::new(RuntimeId::new("owner"));
    let mut request = request(Some(value("Int", "1")), ArgumentMap::default());
    request.idempotency_key = Some("key".into());
    let old_handle = match runtime.admit(request.clone()).unwrap() {
        Admission::New { handle, .. } => handle,
        _ => unreachable!(),
    };
    runtime
        .retain_terminal(
            &old_handle,
            InvocationResult::Success(value("Str", "answer")),
        )
        .unwrap();

    runtime.restart();

    assert_eq!(
        runtime.check_handle(&old_handle),
        Err(AdmissionError::ForeignRuntime)
    );
    let mut executor = Executor {
        calls: 0,
        result: InvocationResult::Success(value("Str", "must-not-run")),
    };
    assert!(matches!(
        runtime.run(request, &mut executor),
        Ok(InvocationState::Terminal(
            RetainedInvocationResult::Success(ref value)
        )) if value.canonical() == Some(b"answer".as_slice())
    ));
    assert_eq!(executor.calls, 0);
}

#[test]
fn supervisor_exposes_the_current_runtime_generation() {
    let supervisor = RuntimeSupervisor::new(RuntimeId::new("owner"));
    let old_id = supervisor.id().unwrap();
    assert_eq!(supervisor.generation(), Ok(1));

    let new_id = supervisor.restart().unwrap();

    assert_ne!(new_id, old_id);
    assert_eq!(supervisor.id(), Ok(new_id));
    assert_eq!(supervisor.generation(), Ok(2));
}

#[test]
fn restart_cancels_and_joins_started_workers_before_rotating_owner() {
    let supervisor = RuntimeSupervisor::new(RuntimeId::new("owner"));
    let mut request = request(Some(value("Int", "1")), ArgumentMap::default());
    request.mode = InvocationMode::Start;
    request.transaction = TransactionMode::Separate;
    let started = Arc::new(AtomicBool::new(false));
    let observed = Arc::new(AtomicBool::new(false));
    let handle = supervisor
        .start(
            request,
            CancellableExecutor {
                started: Arc::clone(&started),
                observed: Arc::clone(&observed),
            },
        )
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(1);
    while !started.load(Ordering::SeqCst) && Instant::now() < deadline {
        thread::yield_now();
    }
    assert!(started.load(Ordering::SeqCst));

    let new_id = supervisor.restart().unwrap();

    assert!(observed.load(Ordering::SeqCst));
    assert_ne!(handle.runtime(), &new_id);
    assert_eq!(
        supervisor.state(&handle),
        Err(AdmissionError::ForeignRuntime)
    );
    assert_eq!(
        supervisor.cancel(&handle, Some(diagnostic("late cancellation"))),
        Err(AdmissionError::ForeignRuntime)
    );
}

#[test]
fn active_expiration_never_allocates_a_second_executable_operation() {
    let mut runtime = Runtime::new(RuntimeId::new("r"));
    let mut request = request(Some(value("Int", "1")), ArgumentMap::default());
    request.idempotency_key = Some("key".into());
    let first = match runtime.admit(request.clone()).unwrap() {
        Admission::New { handle, .. } => handle,
        _ => unreachable!(),
    };

    assert_eq!(runtime.next_invocation, 1);
    assert_eq!(runtime.invocations.len(), 1);
    assert_eq!(runtime.idempotency.len(), 1);
    runtime.expire(&first).unwrap();
    assert_eq!(runtime.next_invocation, 1);
    assert_eq!(runtime.invocations.len(), 1);
    assert_eq!(runtime.idempotency.len(), 1);

    let second = match runtime.admit(request).unwrap() {
        Admission::Active { handle } => handle,
        _ => panic!("active expiry must retain the original operation"),
    };
    assert_eq!(second.invocation(), first.invocation());
    assert_eq!(runtime.next_invocation, 1);
    assert_eq!(runtime.invocations.len(), 1);
    assert_eq!(runtime.idempotency.len(), 1);
    assert_eq!(runtime.check_handle(&first), Ok(()));
    assert_eq!(runtime.check_handle(&second), Ok(()));
}

#[test]
fn active_expiration_rejects_mismatched_idempotency_reuse() {
    let mut runtime = Runtime::new(RuntimeId::new("r"));
    let mut request = request(Some(value("Int", "1")), ArgumentMap::default());
    request.idempotency_key = Some("key".into());
    let first = match runtime.admit(request.clone()).unwrap() {
        Admission::New { handle, .. } => handle,
        _ => unreachable!(),
    };

    runtime.expire(&first).unwrap();

    let mut mismatched = request;
    mismatched.context.locale = Some("en-GB".into());
    assert!(matches!(
        runtime.admit(mismatched),
        Err(AdmissionError::IdempotencyMismatch)
    ));
}
#[test]
fn terminal_expiration_retains_complete_result_without_operational_handle() {
    let mut runtime = Runtime::new(RuntimeId::new("r"));
    let mut request = request(Some(value("Int", "1")), ArgumentMap::default());
    request.idempotency_key = Some("key".into());
    let handle = match runtime.admit(request.clone()).unwrap() {
        Admission::New { handle, .. } => handle,
        _ => unreachable!(),
    };

    runtime
        .retain_terminal(&handle, InvocationResult::Success(value("Str", "answer")))
        .unwrap();
    runtime.expire(&handle).unwrap();

    assert_eq!(
        runtime.check_handle(&handle),
        Err(AdmissionError::ExpiredHandle)
    );
    let replay = runtime.admit(request.clone()).unwrap();
    assert!(matches!(
        replay,
        Admission::Terminal {
            outcome: TerminalClass::Succeeded,
            result: RetainedInvocationResult::Success(_),
            ..
        }
    ));

    let mut executor = Executor {
        calls: 0,
        result: InvocationResult::Success(value("Str", "rerun")),
    };
    assert!(matches!(
        runtime.run(request.clone(), &mut executor),
        Ok(InvocationState::Terminal(
            RetainedInvocationResult::Success(_)
        ))
    ));
    assert_eq!(executor.calls, 0);

    let mut mismatched = request;
    mismatched.context.locale = Some("en-GB".into());
    assert!(matches!(
        runtime.admit(mismatched),
        Err(AdmissionError::IdempotencyMismatch)
    ));
}
#[test]
fn terminal_expiration_retains_complete_result_across_restart() {
    let mut runtime = Runtime::new(RuntimeId::new("r"));
    let mut request = request(Some(value("Int", "1")), ArgumentMap::default());
    request.idempotency_key = Some("key".into());
    let handle = match runtime.admit(request.clone()).unwrap() {
        Admission::New { handle, .. } => handle,
        _ => unreachable!(),
    };
    runtime
        .retain_terminal(&handle, InvocationResult::Success(value("Str", "before")))
        .unwrap();
    runtime.expire(&handle).unwrap();
    runtime.restart();

    let mut executor = Executor {
        calls: 0,
        result: InvocationResult::Success(value("Str", "after")),
    };
    assert!(matches!(
        runtime.run(request, &mut executor),
        Ok(InvocationState::Terminal(
            RetainedInvocationResult::Success(_)
        ))
    ));
    assert_eq!(executor.calls, 0);
}

#[test]
fn invocation_id_exhaustion_fails_closed_without_overwriting_retained_state() {
    let mut runtime = Runtime::new(RuntimeId::new("r"));
    let mut retained_request = request(Some(value("Int", "1")), ArgumentMap::default());
    retained_request.idempotency_key = Some("retained".into());
    let retained_handle = match runtime.admit(retained_request.clone()).unwrap() {
        Admission::New { handle, .. } => handle,
        _ => unreachable!(),
    };
    runtime
        .retain_terminal(
            &retained_handle,
            InvocationResult::Success(value("Str", "retained")),
        )
        .unwrap();

    runtime.next_invocation = u64::MAX;
    let mut fresh_request = request(Some(value("Int", "1")), ArgumentMap::default());
    fresh_request.idempotency_key = Some("fresh".into());

    assert!(matches!(
        runtime.admit(fresh_request),
        Err(AdmissionError::RuntimeUnavailable)
    ));
    assert_eq!(runtime.next_invocation, u64::MAX);
    assert_eq!(runtime.invocations.len(), 1);
    assert_eq!(runtime.idempotency.len(), 1);
    assert!(matches!(
        runtime.admit(retained_request),
        Ok(Admission::Terminal {
            result: RetainedInvocationResult::Success(ref result),
            ..
        }) if result.canonical() == Some(b"retained".as_slice())
    ));
}
#[test]
fn cancellation_is_not_ordinary_failure() {
    let cancelled: InvocationResult<()> = InvocationResult::Cancelled(Some(Diagnostic {
        code: "cancelled",
        message: "cancelled",
        fields: BTreeMap::new(),
        causes: Vec::new(),
    }));
    assert_eq!(cancelled.ordinary_failure(), None);
    assert_eq!(cancelled.terminal_class(), TerminalClass::Cancelled);
}
#[test]
fn cancellation_request_is_idempotent_and_wins_over_late_completion() {
    let mut runtime = Runtime::new(RuntimeId::new("r"));
    let handle = match runtime
        .admit(request(Some(value("Int", "1")), ArgumentMap::default()))
        .unwrap()
    {
        Admission::New { handle, .. } => handle,
        _ => unreachable!(),
    };
    let reason = diagnostic("cancelled");

    assert!(runtime.cancel(&handle, Some(reason.clone())).unwrap());
    assert!(runtime.cancel(&handle, Some(diagnostic("other"))).unwrap());
    assert!(runtime.cancellation_requested(&handle).unwrap());
    assert_eq!(
        runtime.invocation_state(&handle),
        Ok(InvocationState::Active)
    );

    runtime
        .retain_terminal(&handle, InvocationResult::Cancelled(None))
        .unwrap();
    assert!(runtime.cancel(&handle, None).unwrap());
    assert_eq!(
        runtime.invocation_state(&handle),
        Ok(InvocationState::Terminal(
            RetainedInvocationResult::Cancelled(Some(reason))
        ))
    );

    let mut completed = Runtime::new(RuntimeId::new("completed"));
    let completed_handle = match completed
        .admit(request(Some(value("Int", "1")), ArgumentMap::default()))
        .unwrap()
    {
        Admission::New { handle, .. } => handle,
        _ => unreachable!(),
    };
    assert!(completed.cancel(&completed_handle, None).unwrap());
    completed
        .retain_terminal(
            &completed_handle,
            InvocationResult::Success(value("Str", "done")),
        )
        .unwrap();
    assert!(completed.cancel(&completed_handle, None).unwrap());
    assert_eq!(
        completed.invocation_state(&completed_handle),
        Ok(InvocationState::Terminal(
            RetainedInvocationResult::Cancelled(None)
        ))
    );
}

#[test]
fn cancellation_wins_over_late_terminal_classification() {
    let mut runtime = Runtime::new(RuntimeId::new("r"));
    let handle = match runtime
        .admit(request(Some(value("Int", "1")), ArgumentMap::default()))
        .unwrap()
    {
        Admission::New { handle, .. } => handle,
        _ => unreachable!(),
    };

    runtime
        .cancel(&handle, Some(diagnostic("cancelled")))
        .unwrap();
    runtime
        .retain_terminal(&handle, InvocationResult::Success(value("Str", "late")))
        .unwrap();

    assert_eq!(
        runtime.invocation_state(&handle),
        Ok(InvocationState::Terminal(
            RetainedInvocationResult::Cancelled(Some(diagnostic("cancelled")))
        ))
    );
}
#[test]
fn committed_terminal_classification_wins_over_late_cancellation() {
    let mut runtime = Runtime::new(RuntimeId::new("r"));
    let mut first = request(Some(value("Int", "1")), ArgumentMap::default());
    first.idempotency_key = Some("key".into());
    let handle = match runtime.admit(first.clone()).unwrap() {
        Admission::New { handle, .. } => handle,
        _ => unreachable!(),
    };

    runtime
        .retain_terminal(&handle, InvocationResult::Success(value("Str", "done")))
        .unwrap();
    runtime
        .classify_terminal(&handle, TerminalClass::Cancelled)
        .unwrap();

    assert!(matches!(
        runtime.admit(first).unwrap(),
        Admission::Terminal {
            outcome: TerminalClass::Succeeded,
            result: RetainedInvocationResult::Success(_),
            ..
        }
    ));
}

#[test]
fn incomplete_terminal_classification_cannot_publish_success_or_failure() {
    let mut runtime = Runtime::new(RuntimeId::new("r"));
    let handle = match runtime
        .admit(request(Some(value("Int", "1")), ArgumentMap::default()))
        .unwrap()
    {
        Admission::New { handle, .. } => handle,
        _ => unreachable!(),
    };

    for outcome in [TerminalClass::Succeeded, TerminalClass::Failed] {
        assert_eq!(
            runtime.classify_terminal(&handle, outcome),
            Err(AdmissionError::TerminalInvariant)
        );
        assert_eq!(
            runtime.invocation_state(&handle),
            Ok(InvocationState::Active)
        );
    }
}
#[test]
fn retained_terminal_result_is_absorbing() {
    let mut runtime = Runtime::new(RuntimeId::new("r"));
    let mut first = request(Some(value("Int", "1")), ArgumentMap::default());
    first.idempotency_key = Some("key".into());
    let handle = match runtime.admit(first.clone()).unwrap() {
        Admission::New { handle, .. } => handle,
        _ => unreachable!(),
    };

    runtime
        .retain_terminal(&handle, InvocationResult::Success(value("Str", "answer")))
        .unwrap();
    runtime
        .retain_terminal(
            &handle,
            InvocationResult::Cancelled(Some(diagnostic("cancelled"))),
        )
        .unwrap();

    assert!(matches!(
        runtime.admit(first).unwrap(),
        Admission::Terminal {
            outcome: TerminalClass::Succeeded,
            result: RetainedInvocationResult::Success(ref value),
            ..
        } if value.canonical() == Some(b"answer".as_slice())
    ));
}
#[test]
fn terminal_result_type_is_validated_before_retention() {
    let mut runtime = Runtime::new(RuntimeId::new("r"));
    let handle = match runtime
        .admit(request(Some(value("Int", "1")), ArgumentMap::default()))
        .unwrap()
    {
        Admission::New { handle, .. } => handle,
        _ => unreachable!(),
    };

    assert_eq!(
        runtime.retain_terminal(
            &handle,
            InvocationResult::Success(value("Int", "not-a-string")),
        ),
        Err(AdmissionError::ReturnType)
    );
    assert!(matches!(
        runtime.invocation_state(&handle),
        Ok(InvocationState::Terminal(
            RetainedInvocationResult::OrdinaryFailure(_)
        ))
    ));
}

#[test]
fn malformed_synchronous_completion_is_redacted_terminal_failure_before_error() {
    let mut runtime = Runtime::new(RuntimeId::new("r"));
    let mut request = request(Some(value("Int", "1")), ArgumentMap::default());
    request.idempotency_key = Some("malformed".into());
    let mut executor = Executor {
        calls: 0,
        result: InvocationResult {
            invocation: InvocationRef::from_id(InvocationId::new("forged")),
            status: InvocationStatus::Running,
            value: Some(value("Str", "must-not-retain")),
            failure: Some(diagnostic("must-not-retain")),
            started: Some(Instant::now()),
            ended: Instant::now(),
            duration: Some(Duration::from_secs(1)),
        },
    };

    assert_eq!(
        runtime.run(request.clone(), &mut executor),
        Err(AdmissionError::MalformedCompletion)
    );
    assert_eq!(executor.calls, 1);
    assert!(matches!(
        runtime.admit(request),
        Ok(Admission::Terminal {
            outcome: TerminalClass::Failed,
            result: RetainedInvocationResult::OrdinaryFailure(Diagnostic {
                code: "sys.invoke.malformed_completion",
                fields,
                ..
            }),
            ..
        }) if fields.is_empty()
    ));
}

#[test]
fn failure_cancellation_and_orphan_are_complete_terminal_results() {
    let outcomes = [
        InvocationResult::OrdinaryFailure(diagnostic("failed")),
        InvocationResult::Cancelled(Some(diagnostic("cancelled"))),
        InvocationResult::Orphaned(Some(diagnostic("orphaned"))),
    ];
    let expected = [
        TerminalClass::Failed,
        TerminalClass::Cancelled,
        TerminalClass::Orphaned,
    ];
    let retained_outcomes = [
        RetainedInvocationResult::OrdinaryFailure(diagnostic("failed")),
        RetainedInvocationResult::Cancelled(Some(diagnostic("cancelled"))),
        RetainedInvocationResult::Orphaned(Some(diagnostic("orphaned"))),
    ];

    for (index, ((result, terminal_class), expected_result)) in outcomes
        .into_iter()
        .zip(expected)
        .zip(retained_outcomes)
        .enumerate()
    {
        let mut runtime = Runtime::new(RuntimeId::new(format!("r-{index}")));
        let mut request = request(Some(value("Int", "1")), ArgumentMap::default());
        request.idempotency_key = Some(format!("key-{index}"));
        let handle = match runtime.admit(request.clone()).unwrap() {
            Admission::New { handle, .. } => handle,
            _ => unreachable!(),
        };
        runtime.retain_terminal(&handle, result).unwrap();

        let InvocationState::Terminal(retained) = runtime.invocation_state(&handle).unwrap() else {
            panic!("expected terminal state");
        };
        assert_eq!(retained.terminal_class(), terminal_class);
        assert_eq!(retained, expected_result);
        assert!(matches!(
            runtime.admit(request).unwrap(),
            Admission::Terminal {
                outcome,
                result,
                ..
            } if outcome == terminal_class && result == expected_result
        ));
    }
}
#[test]
fn protected_success_discards_canonical_bytes_before_retention() {
    let mut runtime = Runtime::new(RuntimeId::new("r"));
    let handle = match runtime
        .admit(request(Some(value("Int", "1")), ArgumentMap::default()))
        .unwrap()
    {
        Admission::New { handle, .. } => handle,
        _ => unreachable!(),
    };

    runtime
        .retain_terminal(
            &handle,
            InvocationResult::Success(TypedValue::protected(
                ty("Str"),
                include_str!("../tests/fixtures/secret-surface.orna").as_bytes(),
            )),
        )
        .unwrap();
    let state = runtime.invocation_state(&handle).unwrap();
    let InvocationState::Terminal(RetainedInvocationResult::Success(value)) = &state else {
        panic!("expected retained success");
    };
    assert!(value.is_redacted());
    assert_eq!(value.canonical(), None);
    assert!(!format!("{state:?}").contains(include_str!("../tests/fixtures/secret-surface.orna")));
}
#[test]
fn serialized_diagnostics_redact_secrets() {
    let mut fields = BTreeMap::new();
    fields.insert(
        "token".into(),
        DiagnosticField::Redacted {
            static_type: ty("sys.Secret"),
        },
    );
    let json = serde_json::to_string(&Diagnostic {
        code: "sys.invoke.argument_type",
        message: "reflective invocation admission rejected",
        fields,
        causes: Vec::new(),
    })
    .unwrap();
    assert!(json.contains("redacted"));
    assert!(!json.contains("super-secret"));
}

#[test]
fn normative_sys_descriptor_metadata_is_present_and_runtime_neutral() {
    // This is descriptor evidence only: the compile-time generated
    // normative JSON is parsed here, but no sys function is invoked.
    let document: serde_json::Value =
        serde_json::from_str(&system_api_json()).expect("generated sys API must remain valid JSON");

    assert_eq!(
        document["status"],
        "Final 1.1.0 specification contract; engine execution not claimed"
    );
    let functions = document["functions"]
        .as_array()
        .expect("sys API must declare function descriptors");
    assert!(!functions.is_empty());

    let mut names = std::collections::BTreeSet::new();
    for function in functions {
        let name = function["name"]
            .as_str()
            .filter(|name| !name.trim().is_empty())
            .expect("every sys function descriptor needs a name");
        assert!(
            names.insert(name),
            "duplicate sys function descriptor: {name}"
        );
        assert!(matches!(
            function["effect"].as_str(),
            Some("read" | "invoke" | "admin")
        ));
        for field in ["signature", "purpose"] {
            assert!(
                function[field]
                    .as_str()
                    .is_some_and(|value| !value.trim().is_empty()),
                "{name} needs non-blank {field} metadata"
            );
        }

        if name.starts_with("sys.admin.") {
            assert!(
                function["contract"]
                    .as_str()
                    .is_some_and(|value| !value.trim().is_empty()),
                "{name} needs its administrative contract metadata"
            );
        }
        if name.starts_with("sys.invoke") || name.starts_with("sys.start") {
            assert!(
                function["snapshot_rule"]
                    .as_str()
                    .is_some_and(|value| !value.trim().is_empty()),
                "{name} needs its snapshot rule metadata"
            );
        }
        if name.starts_with("sys.start") {
            assert!(
                function["ownership"]
                    .as_str()
                    .is_some_and(|value| !value.trim().is_empty()),
                "{name} needs its ownership metadata"
            );
        }
    }

    for required in [
        "sys.meta",
        "sys.describe",
        "sys.invoke(Value)",
        "sys.invoke<T>",
        "sys.start(Value)",
        "sys.start<T>",
        "sys.await",
        "sys.cancel",
    ] {
        assert!(names.contains(required), "missing descriptor: {required}");
    }

    let value_metadata = document["value_types"]
        .as_array()
        .and_then(|types| {
            types
                .iter()
                .find(|value| value["name"] == "sys.ValueMetadata<T>")
        })
        .expect("sys.ValueMetadata<T> descriptor must be present");
    assert_eq!(value_metadata["kind"], "record-generic");
    assert!(
        value_metadata["purpose"]
            .as_str()
            .is_some_and(|purpose| !purpose.trim().is_empty())
    );
    assert_eq!(
        value_metadata["fields"]
            .as_array()
            .expect("sys.ValueMetadata<T> fields must be declared")
            .iter()
            .map(|field| field["name"].as_str().expect("field name"))
            .collect::<Vec<_>>(),
        [
            "static_type",
            "nominal_type",
            "protocols",
            "codecs",
            "redacted"
        ]
    );
}
