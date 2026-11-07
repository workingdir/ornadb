//! Publication churn for a one-row update (ornadb-5il1.382, Gate C).
//!
//! The production writer reports the row-index churn one publication caused,
//! counted apart from the stored payloads it kept. Row boundaries are content
//! defined, so the index spans several pages at this size: a one-row update
//! rewrites the page holding its key and the branches above it, and keeps the
//! other pages by object id.

use std::{fs, path::Path};

use orna_evaluator::SysHostBindingRegistry;
use orna_repository::{AdmittedRow, KeyRange, Repository};
use orna_runtime::RuntimeState;
use orna_storage::{PublicationChurn, RuntimePublicationCoordinator};
use orna_sys::{EnvironmentProvider, FilesystemProvider};
use tempfile::TempDir;

#[path = "../../orna-cli/tests/support/format3.rs"]
mod format3;
#[path = "../../orna-cli/tests/support/media_commit.rs"]
mod media_commit;

const MEDIA_FIXTURES: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../orna-cli/tests/fixtures/media"
);
/// Enough rows for the content-defined index to span several pages.
const ROWS: u64 = 256;
const UPDATED_KEY: &str = "production-writer-0000";

/// The media import expression with its Blob source file replaced by `file`.
fn capture_expression(media_root: &Path, file: &str) -> String {
    let source = fs::read_to_string(Path::new(MEDIA_FIXTURES).join("import-song.orna"))
        .expect("read media import fixture");
    let root = format!("{:?}", media_root.to_string_lossy().as_ref());
    source
        .trim_end()
        .replace("__MEDIA_ROOT__", &root)
        .replace("\"tone.wav\"", &format!("{file:?}"))
}

/// Largest row count one committed-rows read returns in this test.
const READ_LIMIT: usize = 256;

/// Reads every committed row of the relation from the published store, the
/// reader the query verbs use. The support module's reader is capped at 16
/// rows, below this fixture's row count.
fn committed_rows(repository: &Repository, relation_id: [u8; 16]) -> Vec<AdmittedRow> {
    let format = repository
        .open_format_context()
        .expect("open the format context");
    let row_map = format
        .load_row_map(relation_id)
        .expect("load the relation row map");
    let graph = format
        .open_native_graph(&row_map)
        .expect("open the native graph");
    let scope = graph.open_read_scope().expect("open a read scope");
    graph
        .range_rows(
            &KeyRange::new(None, None, READ_LIMIT).expect("key range"),
            &scope,
        )
        .expect("read the committed rows")
}

/// Host bindings whose capture capability is issued at the repository's
/// current head, so a capture is admitted against the head it will publish on.
fn media_bindings(
    media_root: &Path,
    repository: &Repository,
    relation_id: [u8; 16],
) -> SysHostBindingRegistry {
    let capability = repository
        .capture_capability(relation_id)
        .expect("issue admitted capture capability");
    let mut filesystem =
        FilesystemProvider::with_limits(1 << 20, 16).expect("bounded filesystem provider");
    filesystem
        .allow_root(media_root)
        .expect("allow media fixture root");
    SysHostBindingRegistry::new(EnvironmentProvider::default())
        .with_filesystem_provider(filesystem)
        .with_repository_capture_capability(capability)
}

/// Freezes every row admitted since the last publication, publishes it with
/// the production writer (`prepare_allow_noop` -> `publish` -> `complete`) and
/// returns the churn the coordinator kept for that publication.
async fn publish_pending(
    repository: &Repository,
    state: &RuntimeState,
    intent: [u8; 16],
) -> PublicationChurn {
    let checkpoint = state
        .latest_checkpoint()
        .await
        .expect("read latest checkpoint")
        .expect("admitted rows leave a checkpoint");
    let freeze = state
        .freeze(intent, &checkpoint)
        .await
        .expect("freeze admitted row prefix");
    let head = repository
        .head()
        .expect("read repository head")
        .expect("format-3 fixture has a head");
    let index = repository
        .index_generation()
        .expect("read repository index generation");
    let mut coordinator = RuntimePublicationCoordinator::prepare_allow_noop(
        repository,
        state,
        &head,
        index,
        &freeze,
        |_| Err(orna_storage::Error::InvalidKey),
        "orna: publish one-row update",
    )
    .await
    .expect("storage writer prepares publication")
    .expect("admitted rows are a visible change");
    let churn = coordinator.churn().clone();
    coordinator
        .publish(repository)
        .expect("storage writer publishes rows");
    coordinator
        .complete(repository, state, &freeze)
        .await
        .expect("complete publication");
    churn
}

/// Seeds rows that each carry their own payload, publishes them, then replaces
/// one row with a different payload under the same key. The update's churn
/// must rewrite a bounded number of index nodes, keep the other index nodes by
/// object id, and keep payload objects that its rewritten page names without
/// writing them.
#[tokio::test]
async fn one_row_update_reports_index_churn_apart_from_payload_reuse() {
    let (_repository_dir, repository, relation_id) = format3::empty_format3_repository();
    let media = TempDir::new().expect("temporary media directory");
    let tone = fs::read(Path::new(MEDIA_FIXTURES).join("tone.wav")).expect("read tone fixture");
    // Each seeded row carries distinct bytes, so each row owns a payload object
    // rather than sharing one with the others.
    for index in 0..ROWS {
        let mut bytes = tone.clone();
        bytes.extend_from_slice(format!("row {index:04}").as_bytes());
        fs::write(media.path().join(format!("tone-{index:04}.wav")), &bytes)
            .expect("write seeded tone");
    }
    let mut revised = tone.clone();
    revised.extend_from_slice(b"revision two");
    fs::write(media.path().join("tone-v2.wav"), &revised).expect("write revised tone");
    let update_expression = capture_expression(media.path(), "tone-v2.wav");

    let (identity, initial_digest) = format3::cli_runtime_identity(&repository);
    let state = RuntimeState::open(&repository, identity, initial_digest)
        .await
        .expect("open runtime state");
    let writer = state
        .acquire_lease(identity.repository_id)
        .await
        .expect("acquire runtime writer");

    let mut bindings = media_bindings(media.path(), &repository, relation_id);
    for index in 0..ROWS {
        // Each request gets its own wide ordinal, as the production writer does.
        let ordinal = u16::try_from(index * 6 + 1).expect("ordinal fits two bytes");
        let expression = capture_expression(media.path(), &format!("tone-{index:04}.wav"));
        media_commit::commit_capture_pending_wide(
            &repository,
            &state,
            writer,
            &mut bindings,
            &expression,
            &format!("production-writer-{index:04}"),
            ordinal,
            true,
            relation_id,
        )
        .await;
    }
    drop(bindings);
    let seed_churn = publish_pending(&repository, &state, [0x91; 16]).await;
    assert!(
        seed_churn.index_objects_created() > 0,
        "seeding rows creates index nodes"
    );
    let row_count = usize::try_from(ROWS).expect("row count fits a pointer");
    assert_eq!(
        committed_rows(&repository, relation_id).len(),
        row_count,
        "the seed publication commits every row"
    );

    // The update is admitted against the head the seed published, so its
    // capture capability is issued at that head.
    let mut bindings = media_bindings(media.path(), &repository, relation_id);
    media_commit::commit_capture_pending_wide(
        &repository,
        &state,
        writer,
        &mut bindings,
        &update_expression,
        UPDATED_KEY,
        200,
        false,
        relation_id,
    )
    .await;
    drop(bindings);
    let churn = publish_pending(&repository, &state, [0x92; 16]).await;

    assert!(
        churn.index_objects_created() > 0,
        "a one-row update rewrites index nodes on the changed path"
    );
    assert!(
        churn.index_objects_reused() > 0,
        "a one-row update keeps the index nodes off the changed path"
    );
    assert!(
        churn.payload_objects_reused() > 0,
        "the rewritten page keeps the payload objects of its other rows"
    );
    assert_eq!(
        committed_rows(&repository, relation_id).len(),
        row_count,
        "the update replaces a row without adding one"
    );
}
