use futures::executor::block_on;
use orna_application_v1::{
    ApplicationAuthority, ApplicationEffectFuture, ApplicationEffectRequest, ApplicationError,
    AsyncApplicationEffectDispatcher,
};
use orna_evaluator_v1::{Environment, Limits};
use orna_foundation_v1::{CanonicalValue, OvbRaw};
use orna_repository_v1::Repository;
use orna_runtime_v1::{
    CheckpointKey, Component, ConsumerIdentity, RunObservationRegistration,
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

fn source_stream_argument() -> Environment {
    Environment::from([(
        "stream".to_owned(),
        CanonicalValue::new(OvbRaw::Text("admitted-stream-argument".to_owned())).unwrap(),
    )])
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
            }
        })
    }
}

#[test]
fn trusted_async_dispatch_preserves_staged_writes_and_uses_host_result() {
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

    assert_eq!(staged.mutations().len(), 1);
    assert_eq!(staged.mutations()[0].table(), "Note");
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
