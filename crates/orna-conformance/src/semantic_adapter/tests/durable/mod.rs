use super::{
    Catalogue, DurableTransactionalEvaluator, RunningTableRequestDisposition, SourceUnit,
    StageOutcome, TransactionalEvaluator,
};
use orna_evaluator::Limits;
use orna_repository::Repository;
use orna_runtime::{
    FaultInjector, FaultPoint, NoFault, RecoveryDisposition, RequestIdentity, RequestOwner,
    RequestState, RunObservationRegistration, RunObservationStatus, RuntimeError, RuntimeIdentity,
    RuntimeState, TableMutation, TerminalOutcome, WriterLease,
};
use orna_stream::{DiagnosticClass, DiagnosticCode, SafeDiagnostic};
use orna_value::{OvbRaw, Value};
use std::{
    path::Path,
    process::Command,
    sync::{Arc, Mutex},
};
use tempfile::TempDir;

fn git(path: &Path, args: &[&str]) {
    assert!(
        Command::new("git")
            .args(args)
            .current_dir(path)
            .status()
            .expect("git command")
            .success()
    );
}

fn source(body: &str) -> SourceUnit {
    SourceUnit {
        fixture_id: "durable-txn".into(),
        source_id: "durable-txn.orna".into(),
        parse_as: "module_unit".into(),
        source: format!("pub table Note(id: Int) {{ text: Str, }} fn main() {{ {body} }}"),
    }
}

fn instant_source(body: &str) -> SourceUnit {
    SourceUnit {
        fixture_id: "durable-txn-instant".into(),
        source_id: "durable-txn-instant.orna".into(),
        parse_as: "module_unit".into(),
        source: format!(
            "pub table Note(id: Int) {{ first: Instant, second: Instant, text: Str, }} fn main() {{ {body} }}"
        ),
    }
}

fn shadowed_now_source() -> SourceUnit {
    SourceUnit {
        fixture_id: "durable-txn-shadowed-now".into(),
        source_id: "durable-txn-shadowed-now.orna".into(),
        parse_as: "module_unit".into(),
        source: "pub table Note(id: Int) { value: Int, } fn now() = 7; fn main() { Note.insert({ id: 1, value: now() }); }".into(),
    }
}

fn assert_repeated_now(row: &Value, expected: &Value) {
    let OvbRaw::Map(fields) = row.raw() else {
        panic!("table row must be a canonical map")
    };
    let first = fields
        .iter()
        .find(|(key, _)| key == &OvbRaw::Text("first".into()))
        .map(|(_, value)| value)
        .expect("first timestamp");
    let second = fields
        .iter()
        .find(|(key, _)| key == &OvbRaw::Text("second".into()))
        .map(|(_, value)| value)
        .expect("second timestamp");
    assert_eq!(first, second);
    assert!(matches!(first, OvbRaw::Tag(60002, _)));
    assert_eq!(first, expected.raw());
}

struct FailAt(FaultPoint);

impl FaultInjector for FailAt {
    fn check(&self, point: FaultPoint) -> Result<(), RuntimeError> {
        if point == self.0 {
            Err(RuntimeError::FaultInjected(point))
        } else {
            Ok(())
        }
    }
}

async fn admit_running_table_request(
    state: &RuntimeState,
    owner: WriterLease,
    request: RequestIdentity,
    fingerprint: [u8; 32],
) -> orna_runtime::RunningTableRequestContinuation {
    let start = state
        .begin_observed_request(
            RunObservationRegistration {
                request,
                consumer_identity: super::request_consumer_identity(),
                function: "main".into(),
                source_identity: Some("durable-continuation.orna".into()),
                invocation_id: request.request_id,
            },
            fingerprint,
            owner,
        )
        .await
        .expect("running request admission");
    assert!(start.admitted);
    state
        .continue_running_table_request(request, fingerprint, owner)
        .await
        .expect("running request continuation")
}

#[tokio::test]
async fn source_activation_commits_rows_and_reopens_for_the_next_activation() {
    let temp = TempDir::new().expect("temporary repository");
    git(temp.path(), &["init"]);
    crate::test_support::configure_fixture_git_identity(temp.path());
    let repository = Repository::discover(temp.path()).expect("repository");
    let identity = RuntimeIdentity {
        database_id: [1; 16],
        repository_id: [2; 16],
    };
    let evaluator = DurableTransactionalEvaluator::new("main", Limits::default());

    assert!(matches!(
        evaluator
            .execute_source(
                &repository,
                identity,
                [3; 16],
                [4; 32],
                &source(r#"Note.insert({ id: 7, text: "first" });"#),
            )
            .await,
        Ok(StageOutcome::Passed)
    ));
    let state = RuntimeState::open(&repository, identity, [4; 32])
        .await
        .expect("reopened runtime");
    let key = Value::int(7.into()).encode().expect("encoded key");
    let expected = Value::new(orna_value::OvbRaw::Map(vec![
        (
            orna_value::OvbRaw::Text("id".into()),
            orna_value::OvbRaw::Int(7.into()),
        ),
        (
            orna_value::OvbRaw::Text("text".into()),
            orna_value::OvbRaw::Text("first".into()),
        ),
    ]))
    .expect("canonical row")
    .encode()
    .expect("encoded row");
    assert_eq!(
        state.committed_table_row("Note", &key).await.unwrap(),
        Some(expected)
    );
    drop(state);

    assert!(matches!(
        evaluator
            .execute_source(
                &repository,
                identity,
                [3; 16],
                [4; 32],
                &source(r#"Note.update(7, { text: "changed" });"#),
            )
            .await,
        Ok(StageOutcome::Passed)
    ));
    let state = RuntimeState::open(&repository, identity, [4; 32])
        .await
        .expect("reopened updated runtime");
    let row = state
        .committed_table_row("Note", &key)
        .await
        .expect("durable row read")
        .expect("updated row");
    let row = Value::decode(&row).expect("canonical durable row");
    assert!(matches!(
        row.raw(),
        orna_value::OvbRaw::Map(fields)
            if fields.iter().any(|(key, value)| key
                == &orna_value::OvbRaw::Text("text".into())
                && value
                    == &orna_value::OvbRaw::Text("changed".into()))
    ));
}
#[tokio::test]
async fn application_eval_resolves_admits_stages_and_commits_digest() {
    let temp = TempDir::new().expect("temporary repository");
    git(temp.path(), &["init"]);
    crate::test_support::configure_fixture_git_identity(temp.path());
    let repository = Repository::discover(temp.path()).expect("repository");
    let unit = SourceUnit {
        fixture_id: "application-eval-proof".into(),
        source_id: "application-eval-proof.orna".into(),
        parse_as: "module_unit".into(),
        source: r#"pub table Note(id: Int) { text: Str, } fn resolved_text() = "resolved"; fn main() { Note.insert({ id: 9, text: resolved_text() }); }"#.into(),
    };
    let initial_digest = [11; 32];
    let admitted = super::admit_transaction_source(
        &unit,
        Limits::default(),
        "main",
        &Catalogue::authoritative_core(),
    )
    .expect("source is parser, resolver, and typecheck admitted");
    let mut preview = TransactionalEvaluator::new("main", Limits::default());
    let mutations = preview
        .execute_admitted(&admitted)
        .expect("admitted source stages one mutation");
    assert_eq!(mutations.len(), 1);
    let expected_digest = super::durable_activation_digest(initial_digest, &mutations);

    let evaluator = DurableTransactionalEvaluator::new("main", Limits::default());
    assert!(matches!(
        evaluator
            .execute_source(
                &repository,
                RuntimeIdentity {
                    database_id: [13; 16],
                    repository_id: [14; 16],
                },
                [12; 16],
                initial_digest,
                &unit,
            )
            .await,
        Ok(StageOutcome::Passed)
    ));

    let state = RuntimeState::open(
        &repository,
        RuntimeIdentity {
            database_id: [13; 16],
            repository_id: [14; 16],
        },
        expected_digest,
    )
    .await
    .expect("reopen committed runtime");
    assert_eq!(
        state
            .capture()
            .await
            .expect("capture committed activation")
            .generation_digest(),
        expected_digest
    );
    let key = Value::int(9.into()).encode().expect("encoded key");
    let row = state
        .committed_table_row("Note", &key)
        .await
        .expect("durable row read")
        .map(|bytes| Value::decode(&bytes).expect("canonical row"))
        .expect("committed application row");
    assert!(matches!(
        row.raw(),
        OvbRaw::Map(fields)
            if fields.iter().any(|(key, value)| key
                == &OvbRaw::Text("text".into())
                && value == &OvbRaw::Text("resolved".into()))
    ));
}

#[tokio::test]
async fn ordinary_activation_reuses_one_captured_now_for_repeated_writes() {
    let temp = TempDir::new().expect("temporary repository");
    git(temp.path(), &["init"]);
    crate::test_support::configure_fixture_git_identity(temp.path());
    let repository = Repository::discover(temp.path()).expect("repository");
    let identity = RuntimeIdentity {
        database_id: [121; 16],
        repository_id: [122; 16],
    };
    let observed = Arc::new(Mutex::new(None));
    let mut evaluator = DurableTransactionalEvaluator::new("main", Limits::default());
    evaluator.activation_time_observer = Some(observed.clone());

    assert!(matches!(
        evaluator
            .execute_source(
                &repository,
                identity,
                [123; 16],
                [124; 32],
                &instant_source(
                    r#"Note.insert({ id: 1, first: now(), second: now(), text: "captured" });"#
                ),
            )
            .await,
        Ok(StageOutcome::Passed)
    ));
    let state = RuntimeState::open(&repository, identity, [124; 32])
        .await
        .expect("reopened runtime");
    let row = state
        .committed_table_row("Note", &Value::int(1.into()).encode().unwrap())
        .await
        .unwrap()
        .map(|bytes| Value::decode(&bytes).expect("canonical row"))
        .expect("committed row");
    let activation_time = observed
        .lock()
        .expect("activation observer lock")
        .expect("captured activation time");
    let expected = super::canonical_instant(activation_time).expect("canonical instant");
    assert_repeated_now(&row, &expected);
}

#[tokio::test]
async fn semantically_admitted_user_now_shadows_activation_intrinsic() {
    let temp = TempDir::new().expect("temporary repository");
    git(temp.path(), &["init"]);
    crate::test_support::configure_fixture_git_identity(temp.path());
    let repository = Repository::discover(temp.path()).expect("repository");
    let identity = RuntimeIdentity {
        database_id: [125; 16],
        repository_id: [126; 16],
    };
    let evaluator = DurableTransactionalEvaluator::new("main", Limits::default());

    assert!(matches!(
        evaluator
            .execute_source(
                &repository,
                identity,
                [127; 16],
                [128; 32],
                &shadowed_now_source(),
            )
            .await,
        Ok(StageOutcome::Passed)
    ));
    let state = RuntimeState::open(&repository, identity, [128; 32])
        .await
        .expect("reopened runtime");
    let row = state
        .committed_table_row("Note", &Value::int(1.into()).encode().unwrap())
        .await
        .unwrap()
        .map(|bytes| Value::decode(&bytes).expect("canonical row"))
        .expect("committed row");
    assert!(matches!(
        row.raw(),
        OvbRaw::Map(fields)
            if fields.iter().any(|(key, value)| key == &OvbRaw::Text("value".into())
                && value == &OvbRaw::Int(7.into()))
    ));
}

#[tokio::test]
async fn request_source_activation_commits_row_and_terminal_together() {
    let temp = TempDir::new().expect("temporary repository");
    git(temp.path(), &["init"]);
    crate::test_support::configure_fixture_git_identity(temp.path());
    let repository = Repository::discover(temp.path()).expect("repository");
    let identity = RuntimeIdentity {
        database_id: [61; 16],
        repository_id: [62; 16],
    };
    let request = RequestIdentity {
        session_id: [63; 16],
        request_id: [64; 16],
    };
    let fingerprint = [65; 32];
    let observed = Arc::new(Mutex::new(None));
    let mut evaluator = DurableTransactionalEvaluator::new("main", Limits::default());
    evaluator.activation_time_observer = Some(observed.clone());

    assert!(matches!(
        evaluator
            .execute_source_request(
                &repository,
                identity,
                [66; 16],
                [67; 32],
                request,
                fingerprint,
                &instant_source(
                    r#"Note.insert({ id: 7, first: now(), second: now(), text: "request" });"#
                ),
            )
            .await,
        Ok(StageOutcome::Passed)
    ));
    let state = RuntimeState::open(&repository, identity, [67; 32])
        .await
        .expect("reopened runtime");
    let status = state
        .request_status_for_identity(request)
        .await
        .expect("request status")
        .expect("completed request");
    assert_eq!(status.state, RequestState::Completed);
    assert_eq!(
        status
            .terminal_outcome
            .as_ref()
            .expect("terminal outcome")
            .as_bytes(),
        b"orna-conformance/request-terminal/v1/passed"
    );
    let runs = state.run_observations().await.expect("run observations");
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].request, request);
    assert_eq!(runs[0].invocation_id, request.request_id);
    assert_eq!(runs[0].status, RunObservationStatus::Completed);
    assert!(!runs[0].live);
    assert!(
        state
            .committed_table_row("Note", &Value::int(7.into()).encode().unwrap())
            .await
            .unwrap()
            .is_some()
    );
    let row = state
        .committed_table_row("Note", &Value::int(7.into()).encode().unwrap())
        .await
        .unwrap()
        .map(|bytes| Value::decode(&bytes).expect("canonical row"))
        .expect("request row");
    let activation_time = observed
        .lock()
        .expect("activation observer lock")
        .expect("captured activation time");
    let expected = super::canonical_instant(activation_time).expect("canonical instant");
    assert_repeated_now(&row, &expected);
    drop(state);

    assert!(matches!(
        evaluator
            .execute_source_request(
                &repository,
                identity,
                // The original writer lease remains held by owner 66.
                // Terminal replay must precede lease acquisition.
                [68; 16],
                [67; 32],
                request,
                fingerprint,
                &source("unknown()"),
            )
            .await,
        Ok(StageOutcome::Passed)
    ));
}

#[tokio::test]
async fn running_table_continuation_commits_once_with_the_exact_success_terminal() {
    let temp = TempDir::new().expect("temporary repository");
    git(temp.path(), &["init"]);
    crate::test_support::configure_fixture_git_identity(temp.path());
    let repository = Repository::discover(temp.path()).expect("repository");
    let identity = RuntimeIdentity {
        database_id: [111; 16],
        repository_id: [112; 16],
    };
    let request = RequestIdentity {
        session_id: [113; 16],
        request_id: [114; 16],
    };
    let fingerprint = [115; 32];
    let state = RuntimeState::open(&repository, identity, [116; 32])
        .await
        .expect("runtime");
    let owner = state.acquire_lease([117; 16]).await.expect("writer lease");
    let continuation = admit_running_table_request(&state, owner, request, fingerprint).await;
    let evaluator = DurableTransactionalEvaluator::new("main", Limits::default());
    let read_only = evaluator
        .execute_running_table_request_with_success_terminal(
            &repository,
            &state,
            continuation.clone(),
            &source(""),
            TerminalOutcome::new(b"read-only-success".to_vec()).expect("bounded terminal"),
        )
        .await;
    assert!(matches!(
        read_only,
        RunningTableRequestDisposition::NoMutation
    ));
    let read_only_status = state
        .request_status(request, fingerprint)
        .await
        .expect("request status")
        .expect("running request");
    assert_eq!(read_only_status.state, RequestState::Running);
    assert!(read_only_status.terminal_outcome.is_none());
    let retained =
        TerminalOutcome::new(b"exact-protocol-success".to_vec()).expect("bounded terminal");

    let outcome = evaluator
        .execute_running_table_request_with_success_terminal(
            &repository,
            &state,
            continuation.clone(),
            &instant_source(
                r#"Note.insert({ id: 17, first: now(), second: now(), text: "continued" });"#,
            ),
            retained,
        )
        .await;
    assert!(matches!(outcome, RunningTableRequestDisposition::Committed));
    let status = state
        .request_status(request, fingerprint)
        .await
        .expect("request status")
        .expect("completed request");
    assert_eq!(status.state, RequestState::Completed);
    assert_eq!(
        status
            .terminal_outcome
            .expect("retained terminal")
            .as_bytes(),
        b"exact-protocol-success"
    );
    let key = Value::int(17.into()).encode().expect("encoded key");
    assert!(
        state
            .committed_table_row("Note", &key)
            .await
            .expect("committed row")
            .is_some()
    );
    let row = state
        .committed_table_row("Note", &key)
        .await
        .expect("committed row")
        .map(|bytes| Value::decode(&bytes).expect("canonical row"))
        .expect("continuation row");
    let expected = super::canonical_instant(continuation.context().activation_time())
        .expect("continuation canonical instant");
    assert_repeated_now(&row, &expected);
    let OvbRaw::Map(fields) = row.raw() else {
        panic!("continuation row must be a canonical map")
    };
    let first = fields
        .iter()
        .find(|(key, _)| key == &OvbRaw::Text("first".into()))
        .map(|(_, value)| value)
        .expect("continuation timestamp");
    assert_eq!(first, expected.raw());

    // A completed capability is rejected before source evaluation, so a
    // later source cannot create a second row under the retained request.
    assert!(matches!(
        evaluator
            .execute_running_table_request_with_success_terminal(
                &repository,
                &state,
                continuation,
                &source(r#"Note.insert({ id: 18, text: "must not run" });"#),
                TerminalOutcome::new(b"must-not-retain".to_vec()).expect("bounded terminal"),
            )
            .await,
        RunningTableRequestDisposition::Fenced(RuntimeError::RequestStateConflict)
    ));
    assert!(
        state
            .committed_table_row("Note", &Value::int(18.into()).encode().unwrap())
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn running_table_continuation_semantic_terminal_is_absorbing_before_replay() {
    let temp = TempDir::new().expect("temporary repository");
    git(temp.path(), &["init"]);
    crate::test_support::configure_fixture_git_identity(temp.path());
    let repository = Repository::discover(temp.path()).expect("repository");
    let identity = RuntimeIdentity {
        database_id: [191; 16],
        repository_id: [192; 16],
    };
    let request = RequestIdentity {
        session_id: [193; 16],
        request_id: [194; 16],
    };
    let fingerprint = [195; 32];
    let state = RuntimeState::open(&repository, identity, [196; 32])
        .await
        .expect("runtime");
    let owner = state.acquire_lease([197; 16]).await.expect("writer lease");
    let continuation = admit_running_table_request(&state, owner, request, fingerprint).await;
    let evaluator = DurableTransactionalEvaluator::new("main", Limits::default());

    assert!(matches!(
        evaluator
            .execute_running_table_request_with_success_terminal(
                &repository,
                &state,
                continuation.clone(),
                &source("unknown()"),
                TerminalOutcome::new(b"must-not-retain".to_vec()).expect("bounded terminal"),
            )
            .await,
        RunningTableRequestDisposition::Semantic(StageOutcome::Failed(_))
    ));
    let failure =
        TerminalOutcome::new(b"exact-semantic-terminal".to_vec()).expect("bounded terminal");
    state
        .fail_observed_request_with_owner(
            request,
            fingerprint,
            owner,
            failure.clone(),
            SafeDiagnostic {
                code: DiagnosticCode::ExecutionRejected,
                class: DiagnosticClass::Permanent,
            },
        )
        .await
        .expect("owner-fenced semantic terminal");

    assert!(matches!(
        evaluator
            .execute_running_table_request_with_success_terminal(
                &repository,
                &state,
                continuation,
                &source(r#"Note.insert({ id: 24, text: "must not run" });"#),
                TerminalOutcome::new(b"replacement-terminal".to_vec()).expect("bounded terminal"),
            )
            .await,
        RunningTableRequestDisposition::Fenced(RuntimeError::RequestStateConflict)
    ));
    assert!(
        state
            .committed_table_row("Note", &Value::int(24.into()).encode().unwrap())
            .await
            .expect("row lookup")
            .is_none()
    );
    let status = state
        .request_status(request, fingerprint)
        .await
        .expect("request status")
        .expect("semantic terminal");
    assert_eq!(status.state, RequestState::Completed);
    assert_eq!(
        status
            .terminal_outcome
            .expect("retained semantic terminal")
            .as_bytes(),
        failure.as_bytes()
    );
}

#[tokio::test]
async fn running_table_continuation_fault_recovers_as_proven() {
    let temp = TempDir::new().expect("temporary repository");
    git(temp.path(), &["init"]);
    crate::test_support::configure_fixture_git_identity(temp.path());
    let repository = Repository::discover(temp.path()).expect("repository");
    let identity = RuntimeIdentity {
        database_id: [181; 16],
        repository_id: [182; 16],
    };
    let request = RequestIdentity {
        session_id: [183; 16],
        request_id: [184; 16],
    };
    let fingerprint = [185; 32];
    let initial_digest = [186; 32];
    let state = RuntimeState::open(&repository, identity, initial_digest)
        .await
        .expect("runtime");
    let initial_capture = state.capture().await.expect("initial capture");
    let owner = state.acquire_lease([187; 16]).await.expect("writer lease");
    let continuation = admit_running_table_request(&state, owner, request, fingerprint).await;
    let evaluator = DurableTransactionalEvaluator::new("main", Limits::default());
    let success =
        TerminalOutcome::new(b"canonical-success-bytes".to_vec()).expect("bounded terminal");
    let row_key = Value::int(23.into()).encode().expect("encoded row key");

    assert!(matches!(
        evaluator
            .execute_running_table_request_with_success_terminal_with_faults(
                &repository,
                &state,
                continuation,
                &source(r#"Note.insert({ id: 23, text: "faulted" });"#),
                success,
                &FailAt(FaultPoint::AfterTerminalClaim),
            )
            .await,
        RunningTableRequestDisposition::Fenced(RuntimeError::FaultInjected(
            FaultPoint::AfterTerminalClaim
        ))
    ));
    assert!(
        state
            .committed_table_row("Note", &row_key)
            .await
            .expect("row lookup")
            .is_none()
    );
    assert_eq!(
        state.capture().await.expect("unchanged capture"),
        initial_capture
    );
    let running = state
        .request_status(request, fingerprint)
        .await
        .expect("request status")
        .expect("running request");
    assert_eq!(running.state, RequestState::Running);
    assert!(running.terminal_outcome.is_none());
    drop(state);

    let reopened = RuntimeState::open(&repository, identity, initial_digest)
        .await
        .expect("reopened runtime");
    assert!(
        reopened
            .committed_table_row("Note", &row_key)
            .await
            .expect("reopened row lookup")
            .is_none()
    );
    assert_eq!(
        reopened.capture().await.expect("reopened capture"),
        initial_capture
    );
    let running = reopened
        .request_status(request, fingerprint)
        .await
        .expect("reopened request status")
        .expect("running request");
    assert_eq!(running.state, RequestState::Running);
    assert!(running.terminal_outcome.is_none());

    let recovery_owner = reopened
        .takeover_lease(owner, [188; 16])
        .await
        .expect("owner takeover");
    let rollback_proven =
        TerminalOutcome::new(b"rollback-proven-bytes".to_vec()).expect("rollback-proven terminal");
    let uncertain = TerminalOutcome::new(b"uncertain-bytes".to_vec()).expect("uncertain terminal");
    let recovered = reopened
        .recover_running_request_with_outcomes(
            request,
            fingerprint,
            RequestOwner::from(owner),
            recovery_owner,
            rollback_proven.clone(),
            uncertain,
        )
        .await
        .expect("rollback-proven recovery");
    assert_eq!(recovered.disposition, RecoveryDisposition::RollbackProven);
    assert_eq!(recovered.status.state, RequestState::Orphaned);
    assert_eq!(
        recovered
            .status
            .terminal_outcome
            .as_ref()
            .expect("recovered terminal")
            .as_bytes(),
        rollback_proven.as_bytes()
    );
    assert_eq!(
        reopened
            .request_status(request, fingerprint)
            .await
            .expect("recovered request status")
            .expect("orphaned request")
            .terminal_outcome
            .expect("durable terminal")
            .as_bytes(),
        rollback_proven.as_bytes()
    );
    assert_eq!(
        reopened
            .recover_running_request(
                request,
                fingerprint,
                RequestOwner::from(owner),
                recovery_owner,
                TerminalOutcome::new(b"replacement-bytes".to_vec()).expect("replacement terminal"),
            )
            .await,
        Err(RuntimeError::RequestStateConflict)
    );
    assert_eq!(
        reopened
            .reserve_request(request, fingerprint)
            .await
            .expect("terminal replay"),
        recovered.status
    );
    assert!(
        reopened
            .committed_table_row("Note", &row_key)
            .await
            .expect("durable row remains absent")
            .is_none()
    );
}

#[tokio::test]
async fn running_table_continuation_rejects_foreign_and_stale_capabilities_without_writes() {
    let temp = TempDir::new().expect("temporary repository");
    git(temp.path(), &["init"]);
    crate::test_support::configure_fixture_git_identity(temp.path());
    let repository = Repository::discover(temp.path()).expect("repository");
    let identity = RuntimeIdentity {
        database_id: [121; 16],
        repository_id: [122; 16],
    };
    let request = RequestIdentity {
        session_id: [123; 16],
        request_id: [124; 16],
    };
    let fingerprint = [125; 32];
    let state = RuntimeState::open(&repository, identity, [126; 32])
        .await
        .expect("runtime");
    let owner = state.acquire_lease([127; 16]).await.expect("writer lease");
    let continuation = admit_running_table_request(&state, owner, request, fingerprint).await;
    let evaluator = DurableTransactionalEvaluator::new("main", Limits::default());
    let target_key = Value::int(19.into()).encode().expect("encoded target key");

    assert!(matches!(
        evaluator
            .execute_running_table_request_with_success_terminal(
                &repository,
                &state,
                continuation.clone(),
                &source("unknown()"),
                TerminalOutcome::new(b"semantic-failure".to_vec()).expect("bounded terminal"),
            )
            .await,
        RunningTableRequestDisposition::Semantic(StageOutcome::Failed(_))
    ));
    let semantic_status = state
        .request_status(request, fingerprint)
        .await
        .expect("request status")
        .expect("running request");
    assert_eq!(semantic_status.state, RequestState::Running);
    assert!(semantic_status.terminal_outcome.is_none());

    let foreign_temp = TempDir::new().expect("foreign temporary repository");
    git(foreign_temp.path(), &["init"]);
    crate::test_support::configure_fixture_git_identity(foreign_temp.path());
    let foreign_repository = Repository::discover(foreign_temp.path()).expect("foreign repository");
    let foreign = RuntimeState::open(&foreign_repository, identity, [126; 32])
        .await
        .expect("foreign runtime");
    assert!(matches!(
        evaluator
            .execute_running_table_request_with_success_terminal(
                &foreign_repository,
                &foreign,
                continuation.clone(),
                &source(r#"Note.insert({ id: 19, text: "foreign" });"#),
                TerminalOutcome::new(b"foreign".to_vec()).expect("bounded terminal"),
            )
            .await,
        RunningTableRequestDisposition::Fenced(RuntimeError::RequestUnknown)
    ));
    assert!(
        foreign
            .committed_table_row("Note", &target_key)
            .await
            .unwrap()
            .is_none()
    );

    // Advance the durable capture after admission. The old continuation
    // must fail before it can stage the target mutation.
    let snapshot = state
        .begin_table_activation(&["maintenance"])
        .await
        .expect("maintenance snapshot");
    let maintenance = TableMutation::new([131; 16], "maintenance", vec![1], Some(vec![1]))
        .expect("maintenance mutation");
    let next = super::durable_activation_digest(
        snapshot.context().capture().generation_digest(),
        std::slice::from_ref(&maintenance),
    );
    state
        .commit_table_activation(
            owner,
            snapshot.context(),
            std::slice::from_ref(&maintenance),
            next,
            &NoFault,
        )
        .await
        .expect("maintenance commit");
    let after_maintenance = state.capture().await.expect("advanced capture");

    assert!(matches!(
        evaluator
            .execute_running_table_request_with_success_terminal(
                &repository,
                &state,
                continuation,
                &source(r#"Note.insert({ id: 19, text: "stale" });"#),
                TerminalOutcome::new(b"stale".to_vec()).expect("bounded terminal"),
            )
            .await,
        RunningTableRequestDisposition::Fenced(RuntimeError::StaleCapture { .. })
    ));
    assert_eq!(
        state.capture().await.expect("unchanged capture"),
        after_maintenance
    );
    assert!(
        state
            .committed_table_row("Note", &target_key)
            .await
            .unwrap()
            .is_none()
    );
    let status = state
        .request_status(request, fingerprint)
        .await
        .expect("request status")
        .expect("running request");
    assert_eq!(status.state, RequestState::Running);
    assert!(status.terminal_outcome.is_none());
}

#[tokio::test]
async fn request_source_failure_replays_without_executing_new_source() {
    let temp = TempDir::new().expect("temporary repository");
    git(temp.path(), &["init"]);
    crate::test_support::configure_fixture_git_identity(temp.path());
    let repository = Repository::discover(temp.path()).expect("repository");
    let identity = RuntimeIdentity {
        database_id: [71; 16],
        repository_id: [72; 16],
    };
    let request = RequestIdentity {
        session_id: [73; 16],
        request_id: [74; 16],
    };
    let evaluator = DurableTransactionalEvaluator::new("main", Limits::default());
    let first = evaluator
        .execute_source_request(
            &repository,
            identity,
            [75; 16],
            [76; 32],
            request,
            [77; 32],
            &source("unknown()"),
        )
        .await
        .expect("terminal failure");
    let StageOutcome::Failed(first_diagnostic) = first else {
        panic!("unknown source must fail");
    };
    let state = RuntimeState::open(&repository, identity, [76; 32])
        .await
        .expect("reopened runtime");
    let runs = state.run_observations().await.expect("run observations");
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].request, request);
    assert_eq!(runs[0].invocation_id, request.request_id);
    assert_eq!(runs[0].status, RunObservationStatus::Failed);
    assert_eq!(
        runs[0].diagnostic,
        Some(SafeDiagnostic {
            code: DiagnosticCode::ExecutionRejected,
            class: DiagnosticClass::Permanent,
        })
    );
    drop(state);

    let replay = evaluator
        .execute_source_request(
            &repository,
            identity,
            [78; 16],
            [76; 32],
            request,
            [77; 32],
            &source(r#"Note.insert({ id: 9, text: "must not run" });"#),
        )
        .await
        .expect("failed terminal replay");
    assert!(matches!(
        replay,
        StageOutcome::Failed(ref diagnostic) if diagnostic == &first_diagnostic
    ));
    let state = RuntimeState::open(&repository, identity, [76; 32])
        .await
        .expect("reopened runtime");
    assert!(
        state
            .committed_table_row("Note", &Value::int(9.into()).encode().unwrap())
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn request_source_never_executes_an_existing_nonterminal_reservation() {
    let temp = TempDir::new().expect("temporary repository");
    git(temp.path(), &["init"]);
    crate::test_support::configure_fixture_git_identity(temp.path());
    let repository = Repository::discover(temp.path()).expect("repository");
    let identity = RuntimeIdentity {
        database_id: [81; 16],
        repository_id: [82; 16],
    };
    let request = RequestIdentity {
        session_id: [83; 16],
        request_id: [84; 16],
    };
    let state = RuntimeState::open(&repository, identity, [85; 32])
        .await
        .expect("runtime");
    state
        .reserve_request_with_admission(request, [86; 32])
        .await
        .expect("reservation");
    let evaluator = DurableTransactionalEvaluator::new("main", Limits::default());
    assert!(matches!(
        evaluator
            .execute_source_request(
                &repository,
                identity,
                [87; 16],
                [85; 32],
                request,
                [86; 32],
                &source(r#"Note.insert({ id: 10, text: "must not run" });"#),
            )
            .await,
        Err(RuntimeError::RequestStateConflict)
    ));
    assert!(
        state
            .committed_table_row("Note", &Value::int(10.into()).encode().unwrap())
            .await
            .unwrap()
            .is_none()
    );
}
