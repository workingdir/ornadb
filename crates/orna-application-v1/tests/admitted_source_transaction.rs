use futures::executor::block_on;
use orna_application_v1::{ApplicationAuthority, ApplicationLiveAdapter};
use orna_evaluator_v1::Limits as EvaluatorLimits;
use orna_foundation_v1::{CanonicalValue, OvbRaw};
use orna_live_v1::{LiveApplication, LiveApplicationWorkSupervisor, LiveEvalResponse};
use orna_protocol_v1::{Envelope, Message, PresentationContext, ResultStatus};
use orna_repository_v1::Repository;
use orna_runtime_v1::{
    Component, ConsumerIdentity, FaultInjector, FaultPoint, RequestIdentity, RequestState,
    RunObservationRegistration, RuntimeError, RuntimeIdentity, RuntimeState, StagedTableActivation,
    TerminalOutcome, WriterLease,
};
use orna_semantic_v1::Catalogue;
use std::{
    fs,
    path::PathBuf,
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

const SOURCE: &str = include_str!("fixtures/admitted-source-transaction.orna");
static NEXT_REPOSITORY: AtomicU64 = AtomicU64::new(0);

struct TestRuntime {
    root: PathBuf,
    state: RuntimeState,
    lease: WriterLease,
}

impl TestRuntime {
    fn cleanup(self) {
        drop(self.state);
        fs::remove_dir_all(self.root).unwrap();
    }
}

fn runtime() -> TestRuntime {
    let sequence = NEXT_REPOSITORY.fetch_add(1, Ordering::Relaxed);
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "orna-admitted-source-transaction-{}-{timestamp}-{sequence}",
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
    TestRuntime { root, state, lease }
}

fn registration(request: RequestIdentity) -> RunObservationRegistration {
    RunObservationRegistration {
        request,
        consumer_identity: ConsumerIdentity {
            principal: Component::new("admitted-source-test").unwrap(),
            root: Component::new("main").unwrap(),
            function: Component::new("main").unwrap(),
            binding: Component::new("admitted-source-test").unwrap(),
        },
        function: "main".into(),
        source_identity: Some("test:admitted-source-transaction:v1".into()),
        invocation_id: request.request_id,
    }
}

fn eval_envelope(identity: RequestIdentity) -> Envelope {
    Envelope {
        request: Some(identity.request_id),
        watch: None,
        message: Message::Eval {
            source: SOURCE.to_owned(),
            database: orna_protocol_v1::DatabaseContext {
                database: [45; 16],
                snapshot: None,
            },
            presentation: PresentationContext {
                locale: "en".to_owned(),
                timezone: None,
                width: None,
                theme: "light".to_owned(),
                supported_kinds: Vec::new(),
            },
            fingerprint: [0; 32],
        },
        extensions: Default::default(),
    }
}

fn eval_fingerprint(identity: RequestIdentity) -> [u8; 32] {
    orna_protocol_v1::canonical_request_fingerprint(
        identity.session_id,
        &eval_envelope(identity),
        orna_live_v1::Limits::default().protocol,
    )
    .unwrap()
}

fn eval_message(identity: RequestIdentity, fingerprint: [u8; 32]) -> Message {
    let mut message = eval_envelope(identity).message;
    let Message::Eval {
        fingerprint: transmitted,
        ..
    } = &mut message
    else {
        unreachable!("the Eval envelope has an Eval message")
    };
    *transmitted = fingerprint;
    message
}

fn result_bytes(response: &Envelope) -> Vec<u8> {
    response
        .encode(orna_live_v1::Limits::default().protocol)
        .expect("the adapter response is a valid terminal envelope")
}

#[derive(Debug)]
struct FailAfterTableWrite;

impl FaultInjector for FailAfterTableWrite {
    fn check(&self, point: FaultPoint) -> Result<(), RuntimeError> {
        if point == FaultPoint::AfterTableWrite {
            Err(RuntimeError::FaultInjected(point))
        } else {
            Ok(())
        }
    }
}

fn source_adapter() -> ApplicationLiveAdapter {
    let authority =
        ApplicationAuthority::new(Catalogue::authoritative_core(), EvaluatorLimits::default());
    ApplicationLiveAdapter::new(authority).with_module("admitted-source-transaction.orna", "main")
}

async fn evaluate_source_request(
    adapter: &mut ApplicationLiveAdapter,
    supervisor: &LiveApplicationWorkSupervisor,
    evaluations: &AtomicU64,
    identity: RequestIdentity,
    fingerprint: [u8; 32],
    context: &orna_runtime_v1::RuntimeActivationContext,
) -> LiveEvalResponse {
    evaluations.fetch_add(1, Ordering::Relaxed);
    let message = eval_message(identity, fingerprint);
    let mut work = supervisor
        .admit(identity.session_id, identity.request_id)
        .unwrap();
    adapter
        .eval_with_transaction(
            identity.session_id,
            identity.request_id,
            &message,
            Some(context),
            &mut work,
        )
        .await
        .expect("admitted no-argument source evaluates through LiveApplication")
}

fn transaction(response: LiveEvalResponse) -> (Envelope, orna_live_v1::LiveEvalTransaction) {
    let LiveEvalResponse::Transaction {
        response,
        transaction,
    } = response
    else {
        panic!("source insert must return the live transaction")
    };
    (response, transaction)
}

#[test]
fn admitted_source_table_mutation_commits_rolls_back_and_replays_terminally() {
    let runtime = runtime();
    let supervisor = LiveApplicationWorkSupervisor::new();
    let mut adapter = source_adapter();
    let evaluations = AtomicU64::new(0);

    let failed_identity = RequestIdentity {
        session_id: [51; 16],
        request_id: [52; 16],
    };
    let failed_fingerprint = eval_fingerprint(failed_identity);
    let failed_start = block_on(runtime.state.begin_observed_request(
        registration(failed_identity),
        failed_fingerprint,
        runtime.lease,
    ))
    .unwrap();
    assert!(failed_start.admitted);
    let failed_context = block_on(runtime.state.continue_running_table_request(
        failed_identity,
        failed_fingerprint,
        runtime.lease,
    ))
    .unwrap()
    .context()
    .clone();
    let (failed_response, failed_transaction) = transaction(block_on(evaluate_source_request(
        &mut adapter,
        &supervisor,
        &evaluations,
        failed_identity,
        failed_fingerprint,
        &failed_context,
    )));
    assert!(matches!(
        failed_response.message,
        Message::Result {
            status: ResultStatus::Success,
            ..
        }
    ));
    assert_eq!(failed_transaction.mutations.len(), 1);
    assert_eq!(failed_transaction.mutations[0].table(), "Note");
    let failed_mutation = failed_transaction.mutations[0].clone();
    let before_failed_commit = block_on(runtime.state.capture()).unwrap();
    let failed_staged = StagedTableActivation::from_source(
        failed_context,
        failed_transaction.mutations,
        failed_transaction.next_digest,
        Arc::new(FailAfterTableWrite),
    )
    .unwrap();
    let failed_commit = block_on(runtime.state.commit_staged_table_request_activation(
        runtime.lease,
        failed_identity,
        failed_fingerprint,
        &failed_staged,
        TerminalOutcome::new(result_bytes(&failed_response)).unwrap(),
    ));
    assert!(matches!(
        failed_commit,
        Err(RuntimeError::FaultInjected(FaultPoint::AfterTableWrite))
    ));
    assert_eq!(
        block_on(
            runtime
                .state
                .committed_table_row(failed_mutation.table(), failed_mutation.key(),)
        )
        .unwrap(),
        None,
        "the table write is rolled back with the failed transaction"
    );
    assert_eq!(
        block_on(runtime.state.capture()).unwrap(),
        before_failed_commit
    );

    let failed_terminal = Envelope {
        request: Some(failed_identity.request_id),
        watch: None,
        message: Message::Result {
            status: ResultStatus::Failure,
            value: None,
            fingerprint: failed_fingerprint,
            diagnostic: None,
        },
        extensions: Default::default(),
    };
    let failed_terminal_bytes = result_bytes(&failed_terminal);
    block_on(runtime.state.complete_observed_request_with_owner(
        failed_identity,
        failed_fingerprint,
        runtime.lease,
        TerminalOutcome::new(failed_terminal_bytes.clone()).unwrap(),
    ))
    .unwrap();
    let failed_replay = block_on(runtime.state.begin_observed_request(
        registration(failed_identity),
        failed_fingerprint,
        runtime.lease,
    ))
    .unwrap();
    assert!(
        !failed_replay.admitted,
        "a terminal replay must skip evaluation"
    );
    assert_eq!(
        failed_replay.request.terminal_outcome.unwrap().as_bytes(),
        failed_terminal_bytes
    );
    assert_eq!(evaluations.load(Ordering::Relaxed), 1);
    assert_eq!(
        block_on(runtime.state.request_status_for_identity(failed_identity))
            .unwrap()
            .unwrap()
            .state,
        RequestState::Completed
    );

    let committed_identity = RequestIdentity {
        session_id: [51; 16],
        request_id: [54; 16],
    };
    let committed_fingerprint = eval_fingerprint(committed_identity);
    let committed_start = block_on(runtime.state.begin_observed_request(
        registration(committed_identity),
        committed_fingerprint,
        runtime.lease,
    ))
    .unwrap();
    assert!(committed_start.admitted);
    let committed_context = block_on(runtime.state.continue_running_table_request(
        committed_identity,
        committed_fingerprint,
        runtime.lease,
    ))
    .unwrap()
    .context()
    .clone();
    let (committed_response, committed_transaction) =
        transaction(block_on(evaluate_source_request(
            &mut adapter,
            &supervisor,
            &evaluations,
            committed_identity,
            committed_fingerprint,
            &committed_context,
        )));
    let committed_mutation = committed_transaction.mutations[0].clone();
    assert_ne!(
        failed_mutation.id(),
        committed_mutation.id(),
        "the same source write in a separate request gets its own stage identity"
    );
    let committed_staged = StagedTableActivation::from_source(
        committed_context,
        committed_transaction.mutations,
        committed_transaction.next_digest,
        committed_transaction.faults,
    )
    .unwrap();
    let committed_terminal_bytes = result_bytes(&committed_response);
    let committed = block_on(runtime.state.commit_staged_table_request_activation(
        runtime.lease,
        committed_identity,
        committed_fingerprint,
        &committed_staged,
        TerminalOutcome::new(committed_terminal_bytes.clone()).unwrap(),
    ))
    .unwrap();
    assert_eq!(committed.request.state, RequestState::Completed);
    assert_eq!(
        block_on(
            runtime
                .state
                .committed_table_row(committed_mutation.table(), committed_mutation.key(),)
        )
        .unwrap(),
        committed_mutation.value().map(ToOwned::to_owned)
    );
    let replay = block_on(runtime.state.begin_observed_request(
        registration(committed_identity),
        committed_fingerprint,
        runtime.lease,
    ))
    .unwrap();
    assert!(!replay.admitted, "a committed replay must skip evaluation");
    assert_eq!(
        replay.request.terminal_outcome.unwrap().as_bytes(),
        committed_terminal_bytes
    );
    assert_eq!(evaluations.load(Ordering::Relaxed), 2);
    assert_eq!(
        block_on(
            runtime
                .state
                .request_status_for_identity(committed_identity)
        )
        .unwrap()
        .unwrap()
        .state,
        RequestState::Completed
    );
    assert_eq!(
        CanonicalValue::decode(&committed_mutation.key())
            .unwrap()
            .raw(),
        &OvbRaw::Int(7.into())
    );
    assert_eq!(
        CanonicalValue::decode(committed_mutation.value().unwrap())
            .unwrap()
            .raw(),
        &OvbRaw::Map(vec![
            (OvbRaw::Text("id".to_owned()), OvbRaw::Int(7.into()),),
            (
                OvbRaw::Text("text".to_owned()),
                OvbRaw::Text("committed from admitted source".to_owned()),
            ),
        ])
    );

    runtime.cleanup();
}
