use futures::executor::block_on;
use orna_application_v1::{
    ApplicationAuthority, ApplicationEffectFuture, ApplicationEffectRequest, ApplicationError,
    AsyncApplicationEffectDispatcher,
};
use orna_evaluator_v1::{Environment, Limits};
use orna_foundation_v1::{CanonicalValue, OvbRaw};
use orna_repository_v1::Repository;
use orna_runtime_v1::{RuntimeActivationContext, RuntimeIdentity, RuntimeState};
use orna_semantic_v1::Catalogue;
use std::{
    fs,
    path::PathBuf,
    process::Command,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

const PAUSE_SOURCE: &str = include_str!("fixtures/admin-pause-stream.orna");
const NONTERMINAL_SOURCE: &str = include_str!("fixtures/admin-nonterminal.orna");
const MULTIPLE_SOURCE: &str = include_str!("fixtures/admin-multiple.orna");
const HELPER_SOURCE: &str = include_str!("fixtures/admin-helper.orna");
const REMOVED_SYS_RUNTIME_SOURCE: &str =
    include_str!("../../orna-conformance-v1/tests/fixtures/sys-runtime-removed.orna");
const SYS_RT_INFO_SOURCE: &str =
    include_str!("../../orna-conformance-v1/tests/fixtures/sys-rt-info.orna");
static NEXT_REPOSITORY: AtomicU64 = AtomicU64::new(0);

struct ActivationContext {
    root: PathBuf,
    state: RuntimeState,
    context: RuntimeActivationContext,
}

impl ActivationContext {
    fn cleanup(self) {
        let Self { root, state, .. } = self;
        drop(state);
        fs::remove_dir_all(root).unwrap();
    }
}

fn activation_context() -> ActivationContext {
    let sequence = NEXT_REPOSITORY.fetch_add(1, Ordering::Relaxed);
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "orna-system-admin-conformance-{}-{timestamp}-{sequence}",
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
            database_id: [61; 16],
            repository_id: [62; 16],
        },
        [63; 32],
    ))
    .unwrap();
    let context = block_on(state.begin_activation()).unwrap();
    ActivationContext {
        root,
        state,
        context,
    }
}

fn authority() -> ApplicationAuthority {
    ApplicationAuthority::new(Catalogue::authoritative_core(), Limits::default())
}

fn stream_arguments() -> Environment {
    Environment::from([(
        "stream".to_owned(),
        CanonicalValue::new(OvbRaw::Text("opaque-stream-reference".to_owned())).unwrap(),
    )])
}

#[derive(Clone)]
struct RecordingDispatcher {
    result: bool,
    effects: Arc<Mutex<Vec<ApplicationEffectRequest>>>,
}

impl AsyncApplicationEffectDispatcher for RecordingDispatcher {
    fn dispatch<'a>(
        &'a self,
        effect: ApplicationEffectRequest,
        _context: &'a RuntimeActivationContext,
    ) -> ApplicationEffectFuture<'a> {
        Box::pin(async move {
            self.effects.lock().unwrap().push(effect);
            Ok(CanonicalValue::new(OvbRaw::Bool(self.result)).unwrap())
        })
    }
}

#[test]
fn explicit_admin_capability_returns_host_outcome_for_fixture_source() {
    // Chapter 16's administrative boundary (source/16-administration.md:5-7)
    // requires an explicitly trusted process capability for this operation.
    let authority = authority();
    let application = authority
        .admit_module("admin-pause-stream.orna", PAUSE_SOURCE, "main")
        .expect("the checked-in administrative source fixture is admitted");
    let runtime = activation_context();
    let effects = Arc::new(Mutex::new(Vec::new()));
    let dispatcher = RecordingDispatcher {
        result: false,
        effects: Arc::clone(&effects),
    };

    let staged = block_on(authority.evaluate_staged_with_async_effects(
        &application,
        &stream_arguments(),
        &runtime.context,
        &dispatcher,
    ))
    .expect("trusted host effect should complete");

    assert_eq!(staged.value().raw(), &OvbRaw::Bool(false));
    assert_eq!(staged.mutations().len(), 1);
    assert_eq!(staged.mutations()[0].table(), "Note");
    assert_eq!(
        effects.lock().unwrap().as_slice(),
        &[ApplicationEffectRequest::PauseStream {
            stream: stream_arguments()["stream"].clone(),
            reason: None,
        }]
    );
    runtime.cleanup();
}

#[test]
fn ordinary_application_evaluation_cannot_dispatch_admin_effects() {
    // Chapter 16 (source/16-administration.md:5-7) says accepting Orna source
    // does not itself grant the application endpoint administration authority.
    let authority = authority();
    let application = authority
        .admit_module("admin-pause-stream.orna", PAUSE_SOURCE, "main")
        .expect("the checked-in administrative source fixture is admitted");

    let error = authority
        .evaluate_staged(&application, &stream_arguments())
        .expect_err("ordinary source evaluation must fail closed");

    assert!(matches!(error, ApplicationError::EffectRejected(_)));
}

#[test]
fn removed_sys_runtime_name_keeps_its_normative_diagnostic_at_application_admission() {
    // ORNA-SYS-005 (source/15-system.md:26): sys.runtime is an error, not an
    // alias, and sys.rt is the suggested spelling.
    let error = authority()
        .admit_module(
            "sys-runtime-removed.orna",
            REMOVED_SYS_RUNTIME_SOURCE,
            "runtime_info",
        )
        .expect_err("the removed root member must not be admitted");

    assert!(matches!(
        error,
        ApplicationError::Semantic(diagnostics)
            if diagnostics.iter().any(|code| code == "ORNA100-E-SYS-RUNTIME")
    ));
}

#[test]
fn portable_runtime_info_signature_is_admitted_by_the_application_authority() {
    // ORNA-SYS-006 (source/15-system.md:46): the portable sys.RuntimeInfo
    // function signature is available to application source.
    let application = authority()
        .admit_module("sys-rt-info.orna", SYS_RT_INFO_SOURCE, "runtime_info")
        .expect("the checked-in sys.rt.info fixture resolves against sys.RuntimeInfo");

    assert_eq!(application.entry(), "runtime_info");
}

fn assert_placement_rejected(source: &str, logical_path: &str) {
    let authority = authority();
    let application = authority
        .admit_module(logical_path, source, "main")
        .expect("the source fixture is semantically admitted before effect placement");
    let runtime = activation_context();
    let effects = Arc::new(Mutex::new(Vec::new()));
    let dispatcher = RecordingDispatcher {
        result: true,
        effects: Arc::clone(&effects),
    };

    let error = block_on(authority.evaluate_staged_with_async_effects(
        &application,
        &stream_arguments(),
        &runtime.context,
        &dispatcher,
    ))
    .expect_err("unsafe effect placement must fail before host dispatch");

    assert_eq!(error, ApplicationError::UnsupportedSourceEffectPlacement);
    assert!(effects.lock().unwrap().is_empty());
    runtime.cleanup();
}

#[test]
fn nonterminal_admin_effect_is_rejected_before_dispatch() {
    // This records the application evaluator's explicit terminal-effect
    // constraint; it is an implementation boundary, not a separate ORNA ID.
    assert_placement_rejected(NONTERMINAL_SOURCE, "admin-nonterminal.orna");
}

#[test]
fn repeated_admin_effects_are_rejected_before_dispatch() {
    // Multiple host transitions cannot be represented by this single-effect
    // application API call.
    assert_placement_rejected(MULTIPLE_SOURCE, "admin-multiple.orna");
}

#[test]
fn helper_admin_effect_is_rejected_before_dispatch() {
    // The helper's effect must not bypass the application's entry-point
    // placement validation.
    assert_placement_rejected(HELPER_SOURCE, "admin-helper.orna");
}
