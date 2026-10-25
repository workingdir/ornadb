use futures::executor::block_on;
use orna_application_v1::{ApplicationAuthority, CommittedTableIdentity};
use orna_evaluator_v1::{Environment, Limits};
use orna_foundation_v1::OvbRaw;
use orna_repository_v1::Repository;
use orna_runtime_v1::{RuntimeIdentity, RuntimeState, TableObjectId};
use orna_semantic_v1::Catalogue;
use std::{
    fs,
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

const SOURCE: &str = "pub table Note(id: Int) { text: Str, }\n\npub fn main(): Int {\n  sys.catalog.tables | count()\n}\n";
const OBJECT_ID: [u8; 16] = [0x93; 16];
static NEXT_REPOSITORY: AtomicU64 = AtomicU64::new(0);

struct TestRuntime {
    root: PathBuf,
    state: RuntimeState,
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
        "orna-sys-catalog-tables-{}-{timestamp}-{sequence}",
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
    TestRuntime { root, state }
}

#[test]
fn committed_table_identity_yields_one_sys_table_row() {
    let runtime = runtime();
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
        .expect("the committed Note identity admits the source");
    let identities = admitted.runtime_table_identities().unwrap();
    let snapshot = block_on(runtime.state.begin_admitted_table_activation(&identities)).unwrap();

    let staged = authority
        .evaluate_staged_with_table_snapshot(&admitted, &Environment::new(), &snapshot, None)
        .expect("sys.catalog.tables is served from the activation snapshot");
    assert_eq!(staged.value().raw(), &OvbRaw::Int(1.into()));
    assert!(staged.mutations().is_empty(), "the catalogue read stages no writes");
    runtime.cleanup();
}

#[test]
fn table_without_committed_identity_is_absent_from_sys_table() {
    let runtime = runtime();
    let authority =
        ApplicationAuthority::new(Catalogue::authoritative_core(), Limits::default());
    let admitted = authority
        .admit_module("main.orna", SOURCE, "main")
        .expect("the source admits without committed identity");
    let snapshot = block_on(runtime.state.begin_table_activation(&["Note"])).unwrap();

    let staged = authority
        .evaluate_staged_with_table_snapshot(&admitted, &Environment::new(), &snapshot, None)
        .expect("sys.catalog.tables is served even with no committed identity");
    assert_eq!(staged.value().raw(), &OvbRaw::Int(0.into()));
    runtime.cleanup();
}
