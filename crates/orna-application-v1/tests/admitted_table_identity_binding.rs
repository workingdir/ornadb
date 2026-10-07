use futures::executor::block_on;
use orna_application_v1::{ApplicationAuthority, CommittedTableIdentity};
use orna_evaluator_v1::Limits;
use orna_repository_v1::Repository;
use orna_runtime_v1::{
    NoFault, RuntimeError, RuntimeIdentity, RuntimeState, TableMutation, TableObjectId,
    WriterLease,
};
use orna_semantic_v1::Catalogue;
use std::{
    fs,
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

const SOURCE: &str = include_str!("fixtures/table-identity-orp-binding.orna");
const OBJECT_ID: [u8; 16] = [0x91; 16];
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
        "orna-table-identity-binding-{}-{timestamp}-{sequence}",
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
            database_id: [51; 16],
            repository_id: [52; 16],
        },
        [53; 32],
    ))
    .unwrap();
    let lease = block_on(state.acquire_lease([54; 16])).unwrap();
    TestRuntime { root, state, lease }
}

fn admitted_identities() -> Vec<orna_runtime_v1::RuntimeTableIdentity> {
    let authority =
        ApplicationAuthority::new(Catalogue::authoritative_core(), Limits::default());
    let admitted = authority
        .admit_module_with_committed_table_metadata(
            "main.orna",
            SOURCE,
            "main",
            [CommittedTableIdentity::new(
                "Note",
                TableObjectId::new(OBJECT_ID),
            )],
        )
        .expect("the committed Note identity admits the fixture schema");
    admitted.runtime_table_identities().unwrap()
}

fn note_row(runtime: &TestRuntime) -> Vec<(Vec<u8>, Vec<u8>)> {
    let snapshot = block_on(runtime.state.begin_admitted_table_activation(&admitted_identities()))
        .unwrap();
    snapshot
        .table_rows()
        .get("Note")
        .cloned()
        .unwrap_or_default()
}

#[test]
fn admitted_identity_binds_row_and_relation_op_in_one_commit() {
    let runtime = runtime();
    let snapshot = block_on(
        runtime
            .state
            .begin_admitted_table_activation(&admitted_identities()),
    )
    .unwrap();
    assert_eq!(
        snapshot.table_object_id("Note"),
        Some(TableObjectId::new(OBJECT_ID))
    );
    let mutation = TableMutation::insert([61; 16], "Note", vec![7], vec![1])
        .unwrap()
        .with_table_object_id(TableObjectId::new(OBJECT_ID));
    block_on(runtime.state.commit_table_activation(
        runtime.lease,
        snapshot.context(),
        &[mutation],
        [62; 32],
        &NoFault,
    ))
    .unwrap();

    assert_eq!(note_row(&runtime), vec![(vec![7], vec![1])]);
    let retained = block_on(runtime.state.pending()).unwrap();
    assert_eq!(retained.len(), 1, "the relation op is retained with the row");
    runtime.cleanup();
}

#[test]
fn admitted_table_rejects_name_only_and_mismatched_identity_before_any_row() {
    let runtime = runtime();
    let snapshot = block_on(
        runtime
            .state
            .begin_admitted_table_activation(&admitted_identities()),
    )
    .unwrap();

    let name_only = TableMutation::insert([63; 16], "Note", vec![8], vec![2]).unwrap();
    let mismatched = TableMutation::insert([64; 16], "Note", vec![9], vec![3])
        .unwrap()
        .with_table_object_id(TableObjectId::new([0x92; 16]));
    for mutation in [name_only, mismatched] {
        let result = block_on(runtime.state.commit_table_activation(
            runtime.lease,
            snapshot.context(),
            &[mutation],
            [65; 32],
            &NoFault,
        ));
        assert!(matches!(result, Err(RuntimeError::InvalidTableMutation)));
    }

    assert!(
        note_row(&runtime).is_empty(),
        "a rejected identity binding applies no table row"
    );
    assert_eq!(block_on(runtime.state.pending()).unwrap().len(), 0);
    runtime.cleanup();
}
