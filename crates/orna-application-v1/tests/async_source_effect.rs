use futures::executor::block_on;
use orna_application_v1::{
    ApplicationAuthority, ApplicationEffectFuture, ApplicationEffectRequest, ApplicationError,
    ApplicationLiveAdapter, AsyncApplicationEffectDispatcher,
};
use orna_evaluator_v1::{Environment, Limits};
use orna_foundation_v1::{CanonicalValue, OvbRaw};
use orna_live_v1::{
    LiveAdminEffectDispatcher, LiveApplication, LiveApplicationWorkSupervisor, LiveEvalResponse,
};
use orna_protocol_v1::{DatabaseContext, Message, PresentationContext, ResultStatus};
use orna_repository_v1::Repository;
use orna_runtime_v1::{
    CheckpointKey, Component, ConsumerIdentity, Mutation, NoFault, RunObservationRegistration,
    StreamAdministrationOutcome, StreamObservationRegistration, WriterLease,
};
use orna_runtime_v1::{RuntimeIdentity, RuntimeState};
use orna_semantic_v1::Catalogue;
use std::{
    fs,
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

const SOURCE: &str = include_str!("fixtures/admin-pause-stream.orna");
const STAGED_ADMIN_SOURCE: &str = include_str!("fixtures/admin-pause-staged.orna");
const RESUME_SOURCE: &str = include_str!("fixtures/admin-resume-stream.orna");
const CANCEL_SOURCE: &str = include_str!("fixtures/typed-sys-cancel.orna");
static NEXT_REPOSITORY: AtomicU64 = AtomicU64::new(0);

struct SourcePauseDispatcher(bool);

impl AsyncApplicationEffectDispatcher for SourcePauseDispatcher {
    fn dispatch<'a>(
        &'a self,
        _effect: ApplicationEffectRequest,
        _context: &'a orna_runtime_v1::RuntimeActivationContext,
    ) -> ApplicationEffectFuture<'a> {
        Box::pin(async move { Ok(CanonicalValue::new(OvbRaw::Bool(self.0)).unwrap()) })
    }
}

struct SourceResumeDispatcher;

impl AsyncApplicationEffectDispatcher for SourceResumeDispatcher {
    fn dispatch<'a>(
        &'a self,
        effect: ApplicationEffectRequest,
        _context: &'a orna_runtime_v1::RuntimeActivationContext,
    ) -> ApplicationEffectFuture<'a> {
        Box::pin(async move {
            if !matches!(effect, ApplicationEffectRequest::ResumeStream { .. }) {
                return Err("expected sys.admin.resume_stream".to_owned());
            }
            Ok(CanonicalValue::new(OvbRaw::Bool(true)).unwrap())
        })
    }
}

struct RuntimeContext {
    _root: PathBuf,
    context: orna_runtime_v1::RuntimeActivationContext,
    _state: RuntimeState,
    lease: WriterLease,
    stream: CanonicalValue,
}

impl RuntimeContext {
    fn cleanup(self) {
        let Self {
            _root,
            context,
            _state,
            ..
        } = self;
        drop(context);
        drop(_state);
        fs::remove_dir_all(_root).unwrap();
    }
}

fn runtime_context() -> RuntimeContext {
    let sequence = NEXT_REPOSITORY.fetch_add(1, Ordering::Relaxed);
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "orna-admin-effect-{}-{timestamp}-{sequence}",
        std::process::id()
    ));
    fs::create_dir_all(&root).unwrap();
    let status = Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(&root)
        .status()
        .unwrap();
    assert!(status.success(), "git init must create the test repository");
    let repository = Repository::discover(&root).unwrap();
    let state = block_on(RuntimeState::open(
        &repository,
        RuntimeIdentity {
            database_id: [41; 16],
            repository_id: [42; 16],
        },
        [43; 32],
    ))
    .unwrap();
    let lease = block_on(state.acquire_lease([44; 16])).unwrap();
    let request = orna_runtime_v1::RequestIdentity {
        session_id: [45; 16],
        request_id: [46; 16],
    };
    let registration = RunObservationRegistration {
        request,
        consumer_identity: ConsumerIdentity {
            principal: Component::new("admin-test").unwrap(),
            root: Component::new("main").unwrap(),
            function: Component::new("main").unwrap(),
            binding: Component::new("admin-test").unwrap(),
        },
        function: "main".into(),
        source_identity: Some("test:admin:v1".into()),
        invocation_id: [47; 16],
    };
    let started = block_on(state.begin_observed_request(registration, [48; 32], lease)).unwrap();
    let run = started.run.expect("admitted request creates a live run");
    let consumer = run.consumer_identity.clone();
    let checkpoint = CheckpointKey {
        consumer,
        source_format: Component::new("source-format").unwrap(),
        source: Component::new("test:admin:v1").unwrap(),
        partition_format: Component::new("partition-format").unwrap(),
        partition: None,
        position_format: Component::new("position-format").unwrap(),
    };
    let stream = block_on(state.register_stream_observation_with_owner(
        lease,
        StreamObservationRegistration {
            run: run.id,
            producer: "test-producer".into(),
            consumer: Some("main".into()),
            checkpoint,
        },
    ))
    .unwrap();
    let stream = stream.reference(&run).unwrap();
    let stream = CanonicalValue::decode(&stream.as_row_ref().encode().unwrap()).unwrap();
    let context = block_on(state.begin_activation()).unwrap();
    RuntimeContext {
        _root: root,
        context,
        _state: state,
        lease,
        stream,
    }
}

fn admitted() -> (
    ApplicationAuthority,
    orna_application_v1::AdmittedApplication,
) {
    let authority = ApplicationAuthority::new(Catalogue::authoritative_core(), Limits::default());
    let application = authority
        .admit_module("admin-pause.orna", SOURCE, "main")
        .expect("the real source fixture must pass parser and semantic admission");
    (authority, application)
}

fn admitted_staged_admin() -> (
    ApplicationAuthority,
    orna_application_v1::AdmittedApplication,
) {
    let authority = ApplicationAuthority::new(Catalogue::authoritative_core(), Limits::default());
    let application = authority
        .admit_module("admin-pause-staged.orna", STAGED_ADMIN_SOURCE, "main")
        .expect("the staged admin fixture must pass parser and semantic admission");
    (authority, application)
}

fn source_stream_argument() -> Environment {
    Environment::from([(
        "stream".to_owned(),
        CanonicalValue::new(OvbRaw::Text("admitted-stream-argument".to_owned())).unwrap(),
    )])
}

fn source_cancel_arguments(invocation: CanonicalValue, reason: &str) -> Environment {
    Environment::from([
        ("job".to_owned(), invocation),
        (
            "reason".to_owned(),
            CanonicalValue::new(OvbRaw::Text(reason.to_owned())).unwrap(),
        ),
    ])
}

struct RuntimePauseDispatcher<'a> {
    state: &'a RuntimeState,
    lease: WriterLease,
}

impl AsyncApplicationEffectDispatcher for RuntimePauseDispatcher<'_> {
    fn dispatch<'a>(
        &'a self,
        effect: ApplicationEffectRequest,
        context: &'a orna_runtime_v1::RuntimeActivationContext,
    ) -> ApplicationEffectFuture<'a> {
        Box::pin(async move {
            match effect {
                ApplicationEffectRequest::PauseStream { stream, reason } => {
                    let outcome = self
                        .state
                        .pause_stream_reference_at_capture(
                            self.lease,
                            stream,
                            reason,
                            context.capture(),
                        )
                        .await
                        .map_err(|error| {
                            error
                                .public_code()
                                .unwrap_or("sys.admin.unavailable")
                                .to_owned()
                        })?;
                    let accepted = matches!(
                        outcome,
                        StreamAdministrationOutcome::Paused { .. }
                            | StreamAdministrationOutcome::PausePending { .. }
                    );
                    Ok(CanonicalValue::new(OvbRaw::Bool(accepted)).unwrap())
                }
                ApplicationEffectRequest::ResumeStream { .. } => {
                    Err("unexpected resume".to_owned())
                }
                ApplicationEffectRequest::CancelInvocation { .. } => {
                    Err("unexpected cancellation".to_owned())
                }
            }
        })
    }
}

struct SourceCancelDispatcher {
    seen: std::sync::Arc<std::sync::Mutex<Option<(CanonicalValue, Option<String>)>>>,
}

impl AsyncApplicationEffectDispatcher for SourceCancelDispatcher {
    fn dispatch<'a>(
        &'a self,
        effect: ApplicationEffectRequest,
        _context: &'a orna_runtime_v1::RuntimeActivationContext,
    ) -> ApplicationEffectFuture<'a> {
        Box::pin(async move {
            let ApplicationEffectRequest::CancelInvocation { invocation, reason } = effect else {
                return Err("unexpected pause".to_owned());
            };
            *self.seen.lock().unwrap() = Some((invocation, reason));
            Ok(CanonicalValue::new(OvbRaw::Bool(true)).unwrap())
        })
    }
}

#[test]
fn typed_sys_cancel_fixture_reaches_the_trusted_source_effect_dispatcher() {
    let runtime = runtime_context();
    let authority = ApplicationAuthority::new(Catalogue::authoritative_core(), Limits::default());
    let application = authority
        .admit_module("typed-sys-cancel.orna", CANCEL_SOURCE, "main")
        .expect("typed cancellation fixture is admitted");
    let invocation = CanonicalValue::new(OvbRaw::Text("opaque-invocation".to_owned())).unwrap();
    let seen = std::sync::Arc::new(std::sync::Mutex::new(None));
    let dispatcher = SourceCancelDispatcher {
        seen: std::sync::Arc::clone(&seen),
    };

    let staged = block_on(authority.evaluate_staged_with_async_effects(
        &application,
        &source_cancel_arguments(invocation.clone(), "requested by operator"),
        &runtime.context,
        &dispatcher,
    ))
    .expect("typed sys.cancel reaches the trusted dispatcher");

    assert!(staged.mutations().is_empty());
    assert_eq!(staged.value().raw(), &OvbRaw::Bool(true));
    assert_eq!(
        *seen.lock().unwrap(),
        Some((invocation, Some("requested by operator".to_owned())))
    );
    runtime.cleanup();
}

#[test]
fn trusted_async_dispatch_returns_admin_outcome_without_staged_writes() {
    let (authority, application) = admitted();
    let runtime = runtime_context();
    let dispatcher = SourcePauseDispatcher(true);
    let staged = block_on(authority.evaluate_staged_with_async_effects(
        &application,
        &source_stream_argument(),
        &runtime.context,
        &dispatcher,
    ))
    .expect("the trusted async dispatcher should return the admitted pause result");

    assert!(staged.mutations().is_empty());
    assert_eq!(staged.value().raw(), &OvbRaw::Bool(true));
    let runtime_dispatch = RuntimePauseDispatcher {
        state: &runtime._state,
        lease: runtime.lease,
    };
    let outcome = block_on(runtime_dispatch.state.pause_stream_reference_at_capture(
        runtime_dispatch.lease,
        runtime.stream.clone(),
        None,
        runtime.context.capture(),
    ))
    .unwrap();
    assert!(matches!(
        outcome,
        StreamAdministrationOutcome::Paused { changed: true }
            | StreamAdministrationOutcome::PausePending { changed: true }
    ));
    let fence = block_on(runtime._state.runtime_observation_fence(runtime.lease)).unwrap();
    let observed = block_on(runtime._state.current_runtime_observations(&fence)).unwrap();
    assert_eq!(observed.streams.len(), 1);
    assert_eq!(
        observed.streams[0].status,
        orna_runtime_v1::StreamObservationStatus::Paused
    );
    runtime.cleanup();
}

#[test]
fn administrative_effect_is_busy_until_staged_table_writes_finish() {
    let (authority, application) = admitted_staged_admin();
    let runtime = runtime_context();
    let dispatcher = SourcePauseDispatcher(true);
    let result = block_on(authority.evaluate_staged_with_async_effects(
        &application,
        &source_stream_argument(),
        &runtime.context,
        &dispatcher,
    ));

    assert!(matches!(
        result,
        Err(ApplicationError::SourceEffectFailed(code)) if code == "sys.admin.busy"
    ));
    runtime.cleanup();
}

#[test]
fn sys_admin_resume_stream_reaches_the_trusted_effect_dispatcher() {
    let authority = ApplicationAuthority::new(Catalogue::authoritative_core(), Limits::default());
    let application = authority
        .admit_module("admin-resume-stream.orna", RESUME_SOURCE, "main")
        .expect("the checked-in resume source fixture is admitted");
    let runtime = runtime_context();
    let staged = block_on(authority.evaluate_staged_with_async_effects(
        &application,
        &source_stream_argument(),
        &runtime.context,
        &SourceResumeDispatcher,
    ))
    .expect("resume_stream reaches the trusted admin dispatcher");

    assert!(staged.mutations().is_empty());
    assert_eq!(staged.value().raw(), &OvbRaw::Bool(true));
    runtime.cleanup();
}

#[test]
fn async_dispatch_error_propagates_before_staged_activation_can_be_published() {
    let (authority, application) = admitted();
    let runtime = runtime_context();
    let dispatcher = RuntimePauseDispatcher {
        state: &runtime._state,
        lease: runtime.lease,
    };
    let error = block_on(orna_runtime_v1::with_activation_scope(
        &runtime._state,
        runtime.lease,
        || async {
            authority
                .evaluate_staged_with_async_effects(
                    &application,
                    &source_stream_argument(),
                    &runtime.context,
                    &dispatcher,
                )
                .await
        },
    ))
    .unwrap()
    .expect_err("a reentrant admin call must fail before staged writes publish");

    assert!(matches!(
        error,
        ApplicationError::SourceEffectFailed(code) if code == "sys.admin.busy"
    ));
    assert!(block_on(runtime._state.pending()).unwrap().is_empty());
    assert_eq!(block_on(runtime._state.latest_checkpoint()).unwrap(), None);
    runtime.cleanup();
}

#[test]
fn ordinary_staged_evaluation_does_not_gain_admin_dispatch_authority() {
    let (authority, application) = admitted();
    let runtime = runtime_context();
    let error = authority
        .evaluate_staged(&application, &source_stream_argument())
        .expect_err("the default evaluator must not dispatch administrative effects");

    assert!(matches!(error, ApplicationError::EffectRejected(_)));
    runtime.cleanup();
}

#[test]
fn live_application_reads_publication_relations_from_durable_runtime_rows() {
    let runtime = runtime_context();
    let capture = block_on(runtime._state.capture()).unwrap();
    block_on(runtime._state.commit(
        runtime.lease,
        &capture,
        &Mutation {
            id: [55; 16],
            payload: b"fixture".to_vec(),
            digest: [56; 32],
        },
        [57; 32],
        &NoFault,
    ))
    .expect("seed one durable unpublished mutation for relation reads");
    let activation_context = block_on(runtime._state.begin_activation()).unwrap();
    let authority = ApplicationAuthority::new(Catalogue::authoritative_core(), Limits::default());
    let source = include_str!("../../orna-runtime-v1/tests/fixtures/publication_metadata.orna");
    authority
        .admit_module("publication_metadata.orna", source, "main")
        .expect("the real relation source fixture must pass application admission");
    let mut application =
        ApplicationLiveAdapter::new(authority).with_module("publication_metadata.orna", "main");
    let dispatcher = PublicationRowsDispatcher {
        state: &runtime._state,
    };
    let supervisor = LiveApplicationWorkSupervisor::new();
    let session = [51; 16];
    let request = [52; 16];
    let mut work = supervisor.admit(session, request).unwrap();
    let message = Message::Eval {
        source: source.to_owned(),
        database: DatabaseContext {
            database: [53; 16],
            snapshot: None,
        },
        presentation: PresentationContext {
            locale: "en".to_owned(),
            timezone: None,
            width: None,
            theme: "light".to_owned(),
            supported_kinds: Vec::new(),
        },
        fingerprint: [54; 32],
    };

    let response = block_on(application.dispatch_eval_with_effects(
        session,
        request,
        &message,
        Some(&activation_context),
        &mut work,
        Some(&dispatcher),
    ))
    .expect("live dispatch evaluates the .orna system relation reads");
    let LiveEvalResponse::Pure(envelope) = response else {
        panic!("read-only system relation evaluation must not stage a transaction");
    };
    let Message::Result {
        status,
        value: Some(value),
        ..
    } = envelope.message
    else {
        panic!("live dispatch must return its evaluated module result");
    };
    assert_eq!(status, ResultStatus::Success);
    assert_eq!(value.raw(), &OvbRaw::Bool(true));
    runtime.cleanup();
}

struct PublicationRowsDispatcher<'a> {
    state: &'a RuntimeState,
}

impl LiveAdminEffectDispatcher for PublicationRowsDispatcher<'_> {
    fn publication_metadata_rows<'a>(
        &'a self,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<
                        Option<orna_runtime_v1::RuntimePublicationMetadataRows>,
                        String,
                    >,
                > + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            self.state
                .publication_metadata_rows()
                .await
                .map(Some)
                .map_err(|_| "metadata read failed".to_owned())
        })
    }

    fn pause_stream<'a>(
        &'a self,
        _stream: CanonicalValue,
        _reason: Option<String>,
        _context: &'a orna_runtime_v1::RuntimeActivationContext,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<CanonicalValue, String>> + Send + 'a>,
    > {
        Box::pin(async { Err("unexpected pause".to_owned()) })
    }
}
