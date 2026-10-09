//! ORNA-STATE-001/003: admitted relations read published `HEAD` through the
//! durable row store, and the unpublished CWD tail stays invisible.

use std::{fs, path::Path, process::Command};

use orna_repository_v1::Repository;
use orna_runtime_v1::{
    ActivationError, ActivationWork, Checkpoint, Component, ConsumerIdentity, FaultInjector,
    FaultPoint, NoFault, PublicationCommitId, RequestIdentity, RequestState,
    RunObservationRegistration, RuntimeError, RuntimeIdentity, RuntimeState, RuntimeTableIdentity,
    TableMutation, TableObjectId, TerminalOutcome, WriterLease,
    run_admitted_table_request_activation,
};
use orna_syntax_v1::{Declaration, parse_module};
use tempfile::TempDir;

const SOURCE: &str = include_str!("fixtures/admitted-relation-transactions.orna");

struct Fail(FaultPoint);

impl FaultInjector for Fail {
    fn check(&self, point: FaultPoint) -> Result<(), RuntimeError> {
        if point == self.0 {
            Err(RuntimeError::FaultInjected(point))
        } else {
            Ok(())
        }
    }
}

fn git(directory: &Path, arguments: &[&str]) {
    let output = Command::new("git")
        .current_dir(directory)
        .args(arguments)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn repository() -> (TempDir, Repository) {
    let directory = TempDir::new().expect("create temporary repository");
    git(
        directory.path(),
        &["init", "--quiet", "--initial-branch=main", "--template="],
    );
    fs::write(directory.path().join("main.orna"), SOURCE).expect("write fixture root module");
    let repository = Repository::discover(directory.path()).expect("discover repository");
    (directory, repository)
}

/// The admitted table name comes from the checked-in Orna module rather than a
/// Rust literal, so the fixture is the source of the relation under test.
fn fixture_table_name() -> String {
    let parsed = parse_module(SOURCE);
    assert!(
        parsed.diagnostics.is_empty(),
        "fixture parses: {:?}",
        parsed.diagnostics
    );
    parsed
        .value
        .items
        .iter()
        .find_map(|item| match &item.declaration {
            Declaration::Table { name, .. } => Some(name.clone()),
            _ => None,
        })
        .expect("fixture declares one admitted table")
}

fn table_mutation(id: u8, table: &str, key: u8, value: &[u8]) -> TableMutation {
    TableMutation::new([id; 16], table, vec![key], Some(value.to_vec()))
        .expect("valid typed table mutation")
}

async fn commit(
    state: &RuntimeState,
    writer: WriterLease,
    mutations: &[TableMutation],
    digest: u8,
) -> Checkpoint {
    let context = state.begin_activation().await.expect("capture activation");
    state
        .commit_table_activation(writer, &context, mutations, [digest; 32], &NoFault)
        .await
        .expect("commit table generation");
    state
        .latest_checkpoint()
        .await
        .expect("read checkpoint")
        .expect("checkpoint retained")
}

async fn publish(state: &RuntimeState, checkpoint: &Checkpoint, intent: u8, commit_id: &str) {
    let freeze = state
        .freeze([intent; 16], checkpoint)
        .await
        .expect("freeze published checkpoint");
    state
        .complete_publication(
            &freeze,
            &PublicationCommitId::new(commit_id).expect("commit id"),
        )
        .await
        .expect("complete publication");
}

#[tokio::test]
async fn admitted_relation_reads_published_head_and_excludes_the_unpublished_tail() {
    let (_directory, repository) = repository();
    let table = fixture_table_name();
    assert_eq!(table, "Note");
    let state = RuntimeState::open(
        &repository,
        RuntimeIdentity {
            database_id: [1; 16],
            repository_id: [2; 16],
        },
        [3; 32],
    )
    .await
    .expect("open runtime");
    let writer = state.acquire_lease([4; 16]).await.expect("acquire writer");
    let object_id = TableObjectId::new([0x91; 16]);
    let identity = RuntimeTableIdentity::new(table.clone(), object_id).expect("admitted identity");
    let admitted = |id: u8, key: u8, value: &[u8]| {
        table_mutation(id, &table, key, value).with_table_object_id(object_id)
    };

    // With nothing published there is no HEAD for an admitted relation read.
    assert_eq!(
        state.select_published_snapshot().await,
        Err(RuntimeError::SnapshotNotFound)
    );
    assert_eq!(
        state.read_admitted_table_at_head(&identity).await,
        Err(RuntimeError::SnapshotNotFound)
    );

    let published = commit(&state, writer, &[admitted(21, 1, b"published")], 5).await;
    publish(
        &state,
        &published,
        6,
        "aa11bb22cc33dd44ee55ff66aa77bb88cc99dd00",
    )
    .await;

    // Committed but unpublished tail: one new row plus one edited row.
    commit(
        &state,
        writer,
        &[
            admitted(22, 2, b"tail"),
            admitted(23, 1, b"published edited"),
        ],
        7,
    )
    .await;

    let head = state
        .select_published_snapshot()
        .await
        .expect("published HEAD resolves");
    assert_eq!(head.generation(), 1);
    let rows = state
        .read_admitted_table_at_head(&identity)
        .await
        .expect("read admitted relation at HEAD");
    assert_eq!(rows.rows(), &[(vec![1], b"published".to_vec())]);
    assert_eq!(rows.capture(), head.capture());

    // The live CWD relation still holds both the committed tail row and the
    // edited row, so HEAD is genuinely excluding unpublished state rather than
    // the tail having failed to commit.
    let cwd = state
        .begin_admitted_table_activation(std::slice::from_ref(&identity))
        .await
        .expect("admit live relation");
    assert_eq!(
        cwd.table_rows()[&table],
        vec![
            (vec![1], b"published edited".to_vec()),
            (vec![2], b"tail".to_vec())
        ]
    );
}

#[tokio::test]
async fn admitted_relation_head_ignores_a_freeze_that_never_completed() {
    let (_directory, repository) = repository();
    let table = fixture_table_name();
    let state = RuntimeState::open(
        &repository,
        RuntimeIdentity {
            database_id: [1; 16],
            repository_id: [2; 16],
        },
        [3; 32],
    )
    .await
    .expect("open runtime");
    let writer = state.acquire_lease([4; 16]).await.expect("acquire writer");
    let object_id = TableObjectId::new([0x92; 16]);
    let identity = RuntimeTableIdentity::new(table.clone(), object_id).expect("admitted identity");

    let first = commit(
        &state,
        writer,
        &[admitted_mutation(&table, object_id, 1, b"one")],
        5,
    )
    .await;
    publish(
        &state,
        &first,
        6,
        "0011223344556677889900112233445566778899",
    )
    .await;

    // A frozen intent that never reached `complete_publication` is not HEAD.
    let second = commit(
        &state,
        writer,
        &[admitted_mutation(&table, object_id, 2, b"two")],
        7,
    )
    .await;
    state.freeze([8; 16], &second).await.expect("freeze tail");

    let head = state
        .select_published_snapshot()
        .await
        .expect("published HEAD resolves");
    assert_eq!(head.generation(), 1);
    let rows = state
        .read_admitted_table_at_head(&identity)
        .await
        .expect("read admitted relation at HEAD");
    assert_eq!(rows.rows(), &[(vec![1], b"one".to_vec())]);
}

fn admitted_mutation(
    table: &str,
    object_id: TableObjectId,
    key: u8,
    value: &[u8],
) -> TableMutation {
    table_mutation(20 + key, table, key, value).with_table_object_id(object_id)
}

/// Admits one observed request through the durable live admission boundary so
/// the admitted relation runner works against a real running request.
async fn admitted_request(
    state: &RuntimeState,
    writer: WriterLease,
    identity: RequestIdentity,
    fingerprint: [u8; 32],
) -> orna_runtime_v1::RunningTableRequestContinuation {
    let (_, capability) = state
        .reserve_request_with_admission(identity, fingerprint)
        .await
        .expect("reserve request");
    let capability = capability.expect("fresh owner-bound capability");
    state
        .begin_observed_request_with_admission(
            RunObservationRegistration {
                request: identity,
                consumer_identity: ConsumerIdentity {
                    principal: Component::new("admitted-relation-test").expect("component"),
                    root: Component::new("main").expect("component"),
                    function: Component::new("main").expect("component"),
                    binding: Component::new("admitted-relation-test").expect("component"),
                },
                function: "main".into(),
                source_identity: Some("test:admitted-relation-transactions:v1".into()),
                invocation_id: identity.request_id,
            },
            fingerprint,
            writer,
            capability,
        )
        .await
        .expect("admit observed request");
    state
        .continue_running_table_request(identity, fingerprint, writer)
        .await
        .expect("continue running request")
}

#[tokio::test]
async fn admitted_relation_writes_commit_rows_request_and_checkpoint_atomically() {
    let (_directory, repository) = repository();
    let table = fixture_table_name();
    let state = RuntimeState::open(
        &repository,
        RuntimeIdentity {
            database_id: [1; 16],
            repository_id: [2; 16],
        },
        [3; 32],
    )
    .await
    .expect("open runtime");
    let writer = state.acquire_lease([4; 16]).await.expect("acquire writer");
    let object_id = TableObjectId::new([0x93; 16]);
    let identity = RuntimeTableIdentity::new(table.clone(), object_id).expect("admitted identity");
    let request = RequestIdentity {
        session_id: [5; 16],
        request_id: [6; 16],
    };
    let fingerprint = [7; 32];
    let continuation = admitted_request(&state, writer, request, fingerprint).await;
    let before = state.capture().await.expect("capture before activation");

    let read = run_admitted_table_request_activation(
        &state,
        continuation,
        std::slice::from_ref(&identity),
        &NoFault,
        TerminalOutcome::new(b"accepted".to_vec()).expect("terminal outcome"),
        |snapshot| {
            assert!(snapshot.table_rows()[&table].is_empty());
            let insert =
                table_mutation(21, &table, 1, b"committed").with_table_object_id(object_id);
            async move { Ok::<_, ()>(ActivationWork::new(vec![insert], [8; 32], "committed")) }
        },
    )
    .await
    .expect("admitted request activation commits");
    assert_eq!(read, "committed");

    // Rows, the request terminal claim, and the checkpoint advanced together.
    let status = state
        .request_status(request, fingerprint)
        .await
        .expect("read request status")
        .expect("request retained");
    assert_eq!(status.state, RequestState::Completed);
    assert_eq!(
        status
            .terminal_outcome
            .as_ref()
            .map(|value| value.as_bytes()),
        Some(&b"accepted"[..])
    );
    assert_eq!(
        state
            .begin_admitted_table_activation(std::slice::from_ref(&identity))
            .await
            .expect("admit live relation")
            .table_rows()[&table],
        vec![(vec![1], b"committed".to_vec())]
    );
    let committed_capture = state.capture().await.expect("capture after activation");
    assert_ne!(committed_capture, before);
    assert_eq!(
        state
            .latest_checkpoint()
            .await
            .expect("read checkpoint")
            .expect("checkpoint retained")
            .digest,
        [8; 32]
    );
}

#[tokio::test]
async fn admitted_relation_request_failure_leaves_no_partially_acknowledged_row() {
    for (index, point) in [
        FaultPoint::BeforeTableWrite,
        FaultPoint::AfterTableWrite,
        FaultPoint::AfterMutation,
        FaultPoint::AfterCheckpoint,
        FaultPoint::AfterCapture,
        FaultPoint::BeforeTerminalClaim,
        FaultPoint::AfterTerminalClaim,
    ]
    .into_iter()
    .enumerate()
    {
        let (_directory, repository) = repository();
        let table = fixture_table_name();
        let state = RuntimeState::open(
            &repository,
            RuntimeIdentity {
                database_id: [1; 16],
                repository_id: [2; 16],
            },
            [3; 32],
        )
        .await
        .expect("open runtime");
        let writer = state.acquire_lease([4; 16]).await.expect("acquire writer");
        let object_id = TableObjectId::new([0x94; 16]);
        let identity =
            RuntimeTableIdentity::new(table.clone(), object_id).expect("admitted identity");
        let request = RequestIdentity {
            session_id: [10; 16],
            request_id: [index as u8 + 1; 16],
        };
        let fingerprint = [11; 32];
        let continuation = admitted_request(&state, writer, request, fingerprint).await;
        let before = state.capture().await.expect("capture before activation");
        let key = index as u8 + 1;
        let insert =
            table_mutation(30 + key, &table, key, b"rolled back").with_table_object_id(object_id);

        let failed =
            run_admitted_table_request_activation(
                &state,
                continuation,
                std::slice::from_ref(&identity),
                &Fail(point),
                TerminalOutcome::new(b"accepted".to_vec()).expect("terminal outcome"),
                move |_snapshot| async move {
                    Ok::<_, ()>(ActivationWork::new(vec![insert], [12; 32], ()))
                },
            )
            .await;
        assert!(
            matches!(
                failed,
                Err(ActivationError::Runtime(RuntimeError::FaultInjected(injected))) if injected == point
            ),
            "{point:?} must fail the activation"
        );

        // No row, no terminal claim, no advanced checkpoint, no pending tail.
        assert!(
            state
                .begin_admitted_table_activation(std::slice::from_ref(&identity))
                .await
                .expect("admit live relation")
                .table_rows()[&table]
                .is_empty(),
            "{point:?} must publish no row"
        );
        let status = state
            .request_status(request, fingerprint)
            .await
            .expect("read request status")
            .expect("request retained");
        assert_eq!(status.state, RequestState::Running, "{point:?}");
        assert_eq!(status.terminal_outcome, None, "{point:?}");
        assert_eq!(
            state.latest_checkpoint().await.expect("read checkpoint"),
            None,
            "{point:?}"
        );
        assert_eq!(
            state.capture().await.expect("capture after failure"),
            before,
            "{point:?}"
        );
        assert!(
            state.pending().await.expect("pending tail").is_empty(),
            "{point:?}"
        );
    }
}

#[tokio::test]
async fn admitted_relation_request_rejects_writes_that_disagree_with_admitted_identity() {
    let (_directory, repository) = repository();
    let table = fixture_table_name();
    let state = RuntimeState::open(
        &repository,
        RuntimeIdentity {
            database_id: [1; 16],
            repository_id: [2; 16],
        },
        [3; 32],
    )
    .await
    .expect("open runtime");
    let writer = state.acquire_lease([4; 16]).await.expect("acquire writer");
    let admitted = TableObjectId::new([0x95; 16]);
    let other = TableObjectId::new([0x96; 16]);
    let identity = RuntimeTableIdentity::new(table.clone(), admitted).expect("admitted identity");
    let request = RequestIdentity {
        session_id: [20; 16],
        request_id: [21; 16],
    };
    let fingerprint = [22; 32];
    let continuation = admitted_request(&state, writer, request, fingerprint).await;
    let wrong = table_mutation(31, &table, 1, b"wrong identity").with_table_object_id(other);

    let failed = run_admitted_table_request_activation(
        &state,
        continuation,
        std::slice::from_ref(&identity),
        &NoFault,
        TerminalOutcome::new(b"accepted".to_vec()).expect("terminal outcome"),
        move |_snapshot| async move { Ok::<_, ()>(ActivationWork::new(vec![wrong], [23; 32], ())) },
    )
    .await;
    assert!(matches!(
        failed,
        Err(ActivationError::Runtime(RuntimeError::InvalidTableMutation))
    ));
    assert_eq!(
        state
            .request_status(request, fingerprint)
            .await
            .expect("read request status")
            .expect("request retained")
            .state,
        RequestState::Running
    );
    assert!(
        state
            .begin_admitted_table_activation(std::slice::from_ref(&identity))
            .await
            .expect("admit live relation")
            .table_rows()[&table]
            .is_empty()
    );
}

#[tokio::test]
async fn admitted_relations_read_together_share_one_published_head_pin() {
    let (_directory, repository) = repository();
    let notes = fixture_table_name();
    let state = RuntimeState::open(
        &repository,
        RuntimeIdentity {
            database_id: [1; 16],
            repository_id: [2; 16],
        },
        [3; 32],
    )
    .await
    .expect("open runtime");
    let writer = state.acquire_lease([4; 16]).await.expect("acquire writer");
    let note_id = TableObjectId::new([0x97; 16]);
    let tag_id = TableObjectId::new([0x98; 16]);

    let published = commit(
        &state,
        writer,
        &[
            table_mutation(21, &notes, 1, b"published").with_table_object_id(note_id),
            table_mutation(22, "Tag", 1, b"tag").with_table_object_id(tag_id),
        ],
        5,
    )
    .await;
    publish(
        &state,
        &published,
        6,
        "bb11bb22cc33dd44ee55ff66aa77bb88cc99dd00",
    )
    .await;

    // Both relations advance in the unpublished tail. Neither must leak into
    // the joint HEAD read, and both must come back on one shared pin.
    commit(
        &state,
        writer,
        &[
            table_mutation(23, &notes, 2, b"tail").with_table_object_id(note_id),
            table_mutation(24, "Tag", 2, b"tail tag").with_table_object_id(tag_id),
        ],
        7,
    )
    .await;

    let identities = [
        RuntimeTableIdentity::new(notes.clone(), note_id).expect("admitted identity"),
        RuntimeTableIdentity::new("Tag", tag_id).expect("admitted identity"),
    ];
    let head = state
        .read_admitted_tables_at_head(&identities)
        .await
        .expect("read both admitted relations at HEAD");
    assert_eq!(head.snapshot().generation(), 1);
    assert_eq!(
        head.relation(&notes).expect("note relation").rows(),
        &[(vec![1], b"published".to_vec())]
    );
    assert_eq!(
        head.relation("Tag").expect("tag relation").rows(),
        &[(vec![1], b"tag".to_vec())]
    );
    // Every relation in the result shares the requested HEAD pin, so a join
    // over them cannot mix generations.
    for rows in head.rows() {
        rows.require_same_context(head.relation(&notes).expect("note relation"))
            .expect("relations share one pin");
    }

    // One capture at HEAD feeds both relations even after the tail advanced.
    let later = state
        .read_admitted_table_at_head(&identities[0])
        .await
        .expect("single admitted HEAD read");
    assert_eq!(later.capture(), head.snapshot().capture());
}

/// Admits one observed request whose relations are addressed by committed
/// identity, so the request stages only under that admitted identity.
async fn admitted_request_with_identities(
    state: &RuntimeState,
    writer: WriterLease,
    identity: RequestIdentity,
    fingerprint: [u8; 32],
    tables: &[RuntimeTableIdentity],
) -> orna_runtime_v1::RunningTableRequestContinuation {
    let (_, capability) = state
        .reserve_request_with_admission(identity, fingerprint)
        .await
        .expect("reserve request");
    let capability = capability.expect("fresh owner-bound capability");
    state
        .begin_observed_request_with_admission(
            RunObservationRegistration {
                request: identity,
                consumer_identity: ConsumerIdentity {
                    principal: Component::new("admitted-relation-test").expect("component"),
                    root: Component::new("main").expect("component"),
                    function: Component::new("main").expect("component"),
                    binding: Component::new("admitted-relation-test").expect("component"),
                },
                function: "main".into(),
                source_identity: Some("test:admitted-relation-transactions:v1".into()),
                invocation_id: identity.request_id,
            },
            fingerprint,
            writer,
            capability,
        )
        .await
        .expect("admit observed request");
    state
        .continue_running_admitted_table_request(identity, fingerprint, writer, tables)
        .await
        .expect("continue admitted running request")
}

#[tokio::test]
async fn admitted_relation_request_reads_and_stages_under_committed_identity() {
    let (_directory, repository) = repository();
    let table = fixture_table_name();
    let state = RuntimeState::open(
        &repository,
        RuntimeIdentity {
            database_id: [1; 16],
            repository_id: [2; 16],
        },
        [3; 32],
    )
    .await
    .expect("open runtime");
    let writer = state.acquire_lease([4; 16]).await.expect("acquire writer");
    let object_id = TableObjectId::new([0x99; 16]);
    let identity = RuntimeTableIdentity::new(table.clone(), object_id).expect("admitted identity");

    // Baseline row already stored under the committed identity.
    commit(
        &state,
        writer,
        &[admitted_mutation(&table, object_id, 1, b"baseline")],
        30,
    )
    .await;

    let request = RequestIdentity {
        session_id: [30; 16],
        request_id: [31; 16],
    };
    let fingerprint = [32; 32];
    let continuation = admitted_request_with_identities(
        &state,
        writer,
        request,
        fingerprint,
        std::slice::from_ref(&identity),
    )
    .await;
    assert_eq!(
        continuation.context().table_object_ids().get(&table),
        Some(&object_id),
        "continuation carries the admitted identity"
    );

    let staged = admitted_mutation(&table, object_id, 2, b"staged");
    let seen = std::sync::Arc::new(std::sync::Mutex::new(None));
    let recorder = seen.clone();
    let outcome = TerminalOutcome::new(b"accepted".to_vec()).expect("terminal outcome");
    run_admitted_table_request_activation(
        &state,
        continuation,
        std::slice::from_ref(&identity),
        &NoFault,
        outcome,
        move |snapshot| {
            let recorder = recorder.clone();
            let staged = staged.clone();
            async move {
                // The activation reads rows by the admitted identity, so the
                // baseline row is visible under the committed key.
                let rows = snapshot
                    .table_rows()
                    .get(&table)
                    .cloned()
                    .unwrap_or_default();
                *recorder.lock().expect("lock seen rows") = Some(rows);
                Ok::<_, ()>(ActivationWork::new(vec![staged], [33; 32], ()))
            }
        },
    )
    .await
    .expect("commit admitted request activation");

    let rows = seen
        .lock()
        .expect("lock seen rows")
        .clone()
        .expect("evaluator observed rows");
    assert_eq!(rows, vec![(vec![1], b"baseline".to_vec())]);

    // The staged row landed under the admitted identity, so a later admitted
    // read sees both versions of the same relation.
    let admitted = state
        .begin_admitted_table_activation(std::slice::from_ref(&identity))
        .await
        .expect("admit live relation");
    assert_eq!(
        admitted.table_rows()[&table],
        vec![
            (vec![1], b"baseline".to_vec()),
            (vec![2], b"staged".to_vec())
        ]
    );

    let status = state
        .request_status(request, fingerprint)
        .await
        .expect("read request status")
        .expect("request retained");
    assert_eq!(status.state, RequestState::Completed);
}

#[tokio::test]
async fn admitted_relation_request_continuation_rejects_duplicate_identities() {
    let (_directory, repository) = repository();
    let table = fixture_table_name();
    let state = RuntimeState::open(
        &repository,
        RuntimeIdentity {
            database_id: [1; 16],
            repository_id: [2; 16],
        },
        [3; 32],
    )
    .await
    .expect("open runtime");
    let writer = state.acquire_lease([4; 16]).await.expect("acquire writer");
    let object_id = TableObjectId::new([0x9a; 16]);
    let identity = RuntimeTableIdentity::new(table.clone(), object_id).expect("admitted identity");
    let request = RequestIdentity {
        session_id: [40; 16],
        request_id: [41; 16],
    };
    let fingerprint = [42; 32];
    let (_, capability) = state
        .reserve_request_with_admission(request, fingerprint)
        .await
        .expect("reserve request");
    let capability = capability.expect("fresh owner-bound capability");
    state
        .begin_observed_request_with_admission(
            RunObservationRegistration {
                request,
                consumer_identity: ConsumerIdentity {
                    principal: Component::new("admitted-relation-test").expect("component"),
                    root: Component::new("main").expect("component"),
                    function: Component::new("main").expect("component"),
                    binding: Component::new("admitted-relation-test").expect("component"),
                },
                function: "main".into(),
                source_identity: Some("test:admitted-relation-transactions:v1".into()),
                invocation_id: request.request_id,
            },
            fingerprint,
            writer,
            capability,
        )
        .await
        .expect("admit observed request");

    let rejected = state
        .continue_running_admitted_table_request(
            request,
            fingerprint,
            writer,
            &[identity.clone(), identity],
        )
        .await;
    assert!(
        matches!(rejected, Err(RuntimeError::InvalidTableMutation)),
        "duplicate admitted identities must fail closed, got {rejected:?}"
    );
}
