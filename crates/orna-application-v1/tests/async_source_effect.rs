use futures::executor::block_on;
use orna_application_v1::{
    ApplicationAuthority, ApplicationEffectFuture, ApplicationEffectRequest, ApplicationError,
    AsyncApplicationEffectDispatcher,
};
use orna_evaluator_v1::{Environment, Limits};
use orna_foundation_v1::{CanonicalValue, OvbRaw};
use orna_repository_v1::Repository;
use orna_runtime_v1::{RuntimeIdentity, RuntimeState};
use orna_semantic_v1::Catalogue;
use std::{
    fs,
    path::PathBuf,
    process::Command,
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

const SOURCE: &str = include_str!("fixtures/admin-pause-stream.orna");
static NEXT_REPOSITORY: AtomicU64 = AtomicU64::new(0);

#[derive(Default)]
struct DispatchCapture {
    requests: Mutex<Vec<ApplicationEffectRequest>>,
    result: ResultValue,
}

#[derive(Clone, Copy, Default)]
enum ResultValue {
    #[default]
    Paused,
    Busy,
}

impl AsyncApplicationEffectDispatcher for DispatchCapture {
    fn dispatch<'a>(
        &'a self,
        effect: ApplicationEffectRequest,
        _context: &'a orna_runtime_v1::RuntimeActivationContext,
    ) -> ApplicationEffectFuture<'a> {
        self.requests
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(effect);
        Box::pin(async move {
            match self.result {
                ResultValue::Paused => Ok(CanonicalValue::new(OvbRaw::Bool(true)).unwrap()),
                ResultValue::Busy => Err("sys.admin.busy".to_owned()),
            }
        })
    }
}

struct RuntimeContext {
    _root: PathBuf,
    context: orna_runtime_v1::RuntimeActivationContext,
    _state: RuntimeState,
}

impl RuntimeContext {
    fn cleanup(self) {
        let Self {
            _root,
            context,
            _state,
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
    let context = block_on(state.begin_activation()).unwrap();
    RuntimeContext {
        _root: root,
        context,
        _state: state,
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

fn stream_arguments() -> Environment {
    Environment::from([(
        "stream".to_owned(),
        CanonicalValue::new(OvbRaw::Text("opaque-stream-reference".to_owned())).unwrap(),
    )])
}

#[test]
fn trusted_async_dispatch_preserves_staged_writes_and_uses_host_result() {
    let (authority, application) = admitted();
    let runtime = runtime_context();
    let dispatcher = DispatchCapture::default();
    let staged = block_on(authority.evaluate_staged_with_async_effects(
        &application,
        &stream_arguments(),
        &runtime.context,
        &dispatcher,
    ))
    .expect("the trusted async dispatcher should return the admitted pause result");

    assert_eq!(staged.mutations().len(), 1);
    assert_eq!(staged.mutations()[0].table(), "Note");
    assert_eq!(staged.value().raw(), &OvbRaw::Bool(true));
    assert_eq!(
        *dispatcher
            .requests
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
        vec![ApplicationEffectRequest::PauseStream {
            stream: CanonicalValue::new(OvbRaw::Text("opaque-stream-reference".to_owned()))
                .unwrap(),
            reason: None,
        }]
    );
    runtime.cleanup();
}

#[test]
fn async_dispatch_error_propagates_before_staged_activation_can_be_published() {
    let (authority, application) = admitted();
    let runtime = runtime_context();
    let dispatcher = DispatchCapture {
        requests: Mutex::new(Vec::new()),
        result: ResultValue::Busy,
    };
    let error = block_on(authority.evaluate_staged_with_async_effects(
        &application,
        &stream_arguments(),
        &runtime.context,
        &dispatcher,
    ))
    .expect_err("a busy administrative transition must fail the activation");

    assert!(matches!(
        error,
        ApplicationError::SourceEffectFailed(code) if code == "sys.admin.busy"
    ));
    assert_eq!(
        dispatcher
            .requests
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len(),
        1
    );
    runtime.cleanup();
}

#[test]
fn ordinary_staged_evaluation_does_not_gain_admin_dispatch_authority() {
    let (authority, application) = admitted();
    let error = authority
        .evaluate_staged(&application, &stream_arguments())
        .expect_err("the default evaluator must not dispatch administrative effects");

    assert!(matches!(error, ApplicationError::EffectRejected(_)));
}
