//! Publication churn for a one-row update (ornadb-5il1.382, Gate C).
//!
//! The production writer reports the row-index churn one publication caused,
//! counted apart from the stored payloads it kept. Row boundaries are content
//! defined, so the index spans several pages at this size: a one-row update
//! rewrites the page holding its key and the branches above it, and keeps the
//! other pages by object id.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use orna_evaluator::SysHostBindingRegistry;
use orna_repository::{AdmittedRow, FormatContextError, KeyRange, Repository};
use orna_runtime::{PublicationFreeze, RuntimeState, WriterLease};
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

/// Freezes every row admitted since the last publication.
async fn freeze_admitted(state: &RuntimeState, intent: [u8; 16]) -> PublicationFreeze {
    let checkpoint = state
        .latest_checkpoint()
        .await
        .expect("read latest checkpoint")
        .expect("admitted rows leave a checkpoint");
    state
        .freeze(intent, &checkpoint)
        .await
        .expect("freeze admitted row prefix")
}

/// Prepares the production writer's publication of `freeze` at the repository
/// head without publishing it. The coordinator carries the churn it kept.
async fn prepare_publication(
    repository: &Repository,
    state: &RuntimeState,
    freeze: &PublicationFreeze,
) -> Result<Option<RuntimePublicationCoordinator>, orna_storage::Error> {
    let head = repository
        .head()
        .expect("read repository head")
        .expect("format-3 fixture has a head");
    let index = repository
        .index_generation()
        .expect("read repository index generation");
    RuntimePublicationCoordinator::prepare_allow_noop(
        repository,
        state,
        &head,
        index,
        freeze,
        |_| Err(orna_storage::Error::InvalidKey),
        "orna: publish one-row update",
    )
    .await
}

/// Freezes every row admitted since the last publication, publishes it with
/// the production writer (`prepare_allow_noop` -> `publish` -> `complete`) and
/// returns the churn the coordinator kept for that publication.
async fn publish_pending(
    repository: &Repository,
    state: &RuntimeState,
    intent: [u8; 16],
) -> PublicationChurn {
    let freeze = freeze_admitted(state, intent).await;
    let mut coordinator = prepare_publication(repository, state, &freeze)
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

/// A format-3 repository whose relation is seeded with `ROWS` committed rows,
/// opened with the runtime state and writer lease that publish changes to it.
struct SeededRelation {
    directory: TempDir,
    repository: Repository,
    relation_id: [u8; 16],
    state: RuntimeState,
    writer: WriterLease,
    media: TempDir,
}

/// Seeds rows that each carry their own payload and publishes them. The seed
/// publication must create index nodes and commit every row. The revised
/// payload for the one-row update is written alongside the seeded payloads.
async fn seed_relation() -> SeededRelation {
    let (directory, repository, relation_id) = format3::empty_format3_repository();
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
    SeededRelation {
        directory,
        repository,
        relation_id,
        state,
        writer,
        media,
    }
}

/// Admits one update that replaces `UPDATED_KEY` with the revised payload, at
/// the head the seed published. The update is admitted, not yet frozen.
async fn admit_update(seeded: &SeededRelation) {
    let expression = capture_expression(seeded.media.path(), "tone-v2.wav");
    let mut bindings = media_bindings(seeded.media.path(), &seeded.repository, seeded.relation_id);
    media_commit::commit_capture_pending_wide(
        &seeded.repository,
        &seeded.state,
        seeded.writer,
        &mut bindings,
        &expression,
        UPDATED_KEY,
        200,
        false,
        seeded.relation_id,
    )
    .await;
}

/// The loose object file Git stores `oid` in under the repository directory.
fn loose_object_path(directory: &Path, oid: &str) -> PathBuf {
    directory
        .join(".git")
        .join("objects")
        .join(&oid[..2])
        .join(&oid[2..])
}

/// The kind of every object the repository holds, by object id, as Git lists
/// them. Loose and packed objects are both listed.
fn object_kinds(directory: &Path) -> BTreeMap<String, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(directory)
        .args([
            "cat-file",
            "--batch-all-objects",
            "--batch-check=%(objectname) %(objecttype)",
        ])
        .output()
        .expect("run git cat-file");
    assert!(output.status.success(), "git lists the repository objects");
    String::from_utf8(output.stdout)
        .expect("object listing is UTF-8")
        .lines()
        .map(|line| {
            let (oid, kind) = line.split_once(' ').expect("object id and kind");
            (oid.to_owned(), kind.to_owned())
        })
        .collect()
}

/// Replaces the loose object `oid` with the bytes of another loose object of
/// the same kind. The copy still inflates and parses as that kind, so the
/// object-id digest is the check that refuses it.
fn replace_with_same_kind_sibling(directory: &Path, oid: &str) {
    let kinds = object_kinds(directory);
    let kind = kinds.get(oid).expect("the repository holds the object");
    let sibling = kinds
        .iter()
        .find(|(candidate, candidate_kind)| {
            candidate.as_str() != oid
                && candidate_kind == &kind
                && loose_object_path(directory, candidate).is_file()
        })
        .map(|(candidate, _)| candidate.clone())
        .expect("the store holds another loose object of the same kind");
    let target = loose_object_path(directory, oid);
    fs::remove_file(&target).expect("remove the row index root");
    fs::copy(loose_object_path(directory, &sibling), &target).expect("swap in the sibling object");
}

/// The object id of the row index root the repository head publishes.
fn primary_root_id(repository: &Repository, relation_id: [u8; 16]) -> String {
    let format = repository
        .open_format_context()
        .expect("open the format context");
    let row_map = format
        .load_row_map(relation_id)
        .expect("load the relation row map");
    row_map.version().primary_root().to_hex()
}

/// Seeds rows that each carry their own payload, publishes them, then replaces
/// one row with a different payload under the same key. The update's churn
/// must rewrite a bounded number of index nodes, keep the other index nodes by
/// object id, and keep payload objects that its rewritten page names without
/// writing them.
#[tokio::test]
async fn one_row_update_reports_index_churn_apart_from_payload_reuse() {
    let seeded = seed_relation().await;
    admit_update(&seeded).await;
    let churn = publish_pending(&seeded.repository, &seeded.state, [0x92; 16]).await;

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
    let row_count = usize::try_from(ROWS).expect("row count fits a pointer");
    assert_eq!(
        committed_rows(&seeded.repository, seeded.relation_id).len(),
        row_count,
        "the update replaces a row without adding one"
    );
}

/// A row index root whose bytes no longer match its object id is refused as
/// malformed, and no publication is prepared over it.
#[tokio::test]
async fn publication_reports_a_corrupt_row_index_root_as_malformed() {
    let seeded = seed_relation().await;
    admit_update(&seeded).await;
    let root = primary_root_id(&seeded.repository, seeded.relation_id);
    assert!(
        loose_object_path(seeded.directory.path(), &root).is_file(),
        "the row index root is a loose object"
    );
    replace_with_same_kind_sibling(seeded.directory.path(), &root);

    let freeze = freeze_admitted(&seeded.state, [0x93; 16]).await;
    let error = prepare_publication(&seeded.repository, &seeded.state, &freeze)
        .await
        .expect_err("a corrupt row index root refuses the publication");
    assert!(
        matches!(
            error,
            orna_storage::Error::RepositoryMalformed(FormatContextError::GraphMalformed(_))
        ),
        "a corrupt row index root is malformed, not unavailable: {error}"
    );
}

/// A row index root that is missing from the store when publication reads it
/// is unavailable, not malformed: the store cannot answer, but nothing it
/// holds is corrupt. The head and index are observed first because observing
/// the index writes its tree objects back into the store (`write-tree`), so a
/// root removed before that observation is restored by it.
#[tokio::test]
async fn publication_reports_a_missing_row_index_root_as_unavailable() {
    let seeded = seed_relation().await;
    admit_update(&seeded).await;
    let root = primary_root_id(&seeded.repository, seeded.relation_id);
    let head = seeded
        .repository
        .head()
        .expect("read repository head")
        .expect("format-3 fixture has a head");
    let index = seeded
        .repository
        .index_generation()
        .expect("read repository index generation");
    fs::remove_file(loose_object_path(seeded.directory.path(), &root))
        .expect("remove the row index root");
    assert!(
        !object_kinds(seeded.directory.path()).contains_key(&root),
        "the row index root is absent from the store, loose and packed"
    );

    let freeze = freeze_admitted(&seeded.state, [0x94; 16]).await;
    let error = RuntimePublicationCoordinator::prepare_allow_noop(
        &seeded.repository,
        &seeded.state,
        &head,
        index,
        &freeze,
        |_| Err(orna_storage::Error::InvalidKey),
        "orna: publish one-row update",
    )
    .await
    .expect_err("a missing row index root refuses the publication");
    assert_eq!(
        error,
        orna_storage::Error::RepositoryUnavailable,
        "a missing row index root is unavailable"
    );
}
