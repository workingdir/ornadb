//! Gate H: a restored complete copy answers query, history and a payload read
//! with the source repository gone.
//!
//! One repository holds a song row and an image row. `orna export --at HEAD`
//! freezes that snapshot into an archive, `orna export ARCHIVE --check` reads
//! the archive back with no repository at all, and `orna export ARCHIVE
//! --restore DEST --worktree` reconstructs it into a fresh directory.
//!
//! The source worktree and its repository handle are dropped before the copy is
//! read, so nothing that needed the source could succeed. The copy is then read
//! three ways: `orna query` lists both rows' payload-free metadata, `orna
//! history` lists each row's revisions, and a read through the ordinary
//! repository reader pulls the committed payload bytes back out of the copy.
//! The last one is the point of the test: the descriptor and the payload chunk
//! travel inside the archive closure, so a payload left behind would fail the
//! read instead of quietly returning metadata.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use orna_evaluator_v1::SysHostBindingRegistry;
use orna_repository_v1::{
    AdmittedRow, KeyRange, NativeGraphContext, NativeObjectKind, NativeOid, Repository, TypedKey,
};
use orna_runtime_v1::{RuntimeIdentity, RuntimeState};
use orna_sys_v1::{EnvironmentProvider, FilesystemProvider};
use sha2::{Digest, Sha256};
use tempfile::TempDir;

#[path = "support/format3.rs"]
mod format3;
use format3::*;

#[path = "support/media_commit.rs"]
mod media_commit;
use media_commit::commit_capture;

const MEDIA_FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/media");
const SONG_IMPORT_FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/media/import-song.orna"
);
const IMAGE_IMPORT_FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/media/import-image.orna"
);
const MEDIA_ROOT_PLACEHOLDER: &str = "__MEDIA_ROOT__";

/// Runs `orna-cli-v1 <arguments>` in `directory` with Git routing isolated, the
/// same way the other CLI tests do.
fn run(directory: &Path, arguments: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_orna-cli-v1"));
    command
        .current_dir(directory)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null");
    for name in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_CEILING_DIRECTORIES",
        "GIT_DISCOVERY_ACROSS_FILESYSTEM",
    ] {
        command.env_remove(name);
    }
    command
        .args(arguments)
        .output()
        .expect("CLI process starts")
}

/// An import expression whose media root points at `root`.
fn import_expression(fixture: &str, root: &Path) -> String {
    let fixture = std::fs::read_to_string(fixture).unwrap();
    let root = format!("{:?}", root.to_string_lossy().as_ref());
    fixture.trim_end().replace(MEDIA_ROOT_PLACEHOLDER, &root)
}

/// The 16-byte relation id as the 32-digit hex the CLI takes.
fn relation_hex(relation_id: [u8; 16]) -> String {
    relation_id
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// The committed row whose canonical key is `key`.
fn committed_row<'a>(rows: &'a [AdmittedRow], key: &str) -> &'a AdmittedRow {
    rows.iter()
        .find(|row| {
            row.key() == &TypedKey::Bytes(key.as_bytes().to_vec())
                || row.key() == &TypedKey::Text(key.to_owned())
        })
        .unwrap_or_else(|| panic!("the {key} row is committed"))
}

/// Reads a graph and its relation's committed rows, exactly as a query does.
fn open(repository: &Repository, relation_id: [u8; 16]) -> (NativeGraphContext, Vec<AdmittedRow>) {
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
    let rows = graph
        .range_rows(&KeyRange::new(None, None, 16).expect("key range"), &scope)
        .expect("read the committed rows");
    (graph, rows)
}

/// The descriptor object of one committed row: the native tree its stored value
/// depends on.
fn descriptor_of(row: &AdmittedRow) -> NativeOid {
    row.value()
        .dependencies()
        .into_iter()
        .find(|dependency| dependency.kind() == NativeObjectKind::Tree)
        .expect("the committed row depends on a descriptor tree")
        .oid()
        .clone()
}

/// Reads one row's committed payload bytes out of `repository`.
///
/// The descriptor is taken from the row's own stored dependencies, so the read
/// only succeeds when the row and the descriptor closure both travelled into
/// the copy.
fn read_committed_payload(
    repository: &Repository,
    relation_id: [u8; 16],
    key: &str,
) -> (u64, u64, Vec<u8>) {
    let (graph, rows) = open(repository, relation_id);
    let scope = graph.open_read_scope().expect("open a read scope");
    let row = committed_row(&rows, key);

    // The payload-free projection reads the row's own field tuple: it reports
    // the committed length and charges no payload byte.
    let (_, mut metadata) = graph
        .project_blob_metadata(row.key(), 0, &scope)
        .expect("project the committed row's Blob metadata")
        .expect("the committed row has a Blob field");
    assert_eq!(
        scope.payload_bytes_read(),
        0,
        "{key}: projecting metadata must not read a payload byte"
    );

    let reference = graph
        .admit_blob_reference(row, &descriptor_of(row), &scope)
        .expect("admit the descriptor through its stored row");
    let bytes = graph
        .read_blob_range(&reference, 0..metadata.length(), &scope)
        .expect("read the committed payload out of the copy")
        .bytes()
        .to_vec();
    assert_eq!(
        scope.payload_bytes_read(),
        metadata.length(),
        "{key}: reading the bytes charges every payload byte"
    );
    let mut digest = [0_u8; 32];
    digest.copy_from_slice(&Sha256::digest(&bytes));
    assert_eq!(
        metadata.sha256(),
        digest,
        "{key}: the bytes read back are the payload the row records"
    );
    (scope.payload_bytes_read(), metadata.length(), bytes)
}

/// No fetch can answer a read of this copy: it names no remote and its object
/// store points at no alternate.
fn assert_self_contained(copy: &Path) {
    let remotes = git_output(copy, &["remote"], None);
    assert_eq!(
        String::from_utf8(remotes).unwrap().trim(),
        "",
        "the copy configures no remote"
    );
    let alternates = copy.join(".git/objects/info/alternates");
    assert!(
        !alternates.exists(),
        "the copy borrows no object from another store: {}",
        alternates.display()
    );
}

#[tokio::test]
async fn a_restored_copy_answers_query_history_and_a_payload_read_offline() {
    let (directory, repository, relation_id) = empty_format3_repository();
    let source = TempDir::new().unwrap();
    for name in ["tone.wav", "pixel.png"] {
        std::fs::copy(
            Path::new(MEDIA_FIXTURES).join(name),
            source.path().join(name),
        )
        .unwrap();
    }

    let runtime_identity = RuntimeIdentity {
        database_id: [0, 0, 0, 0, 0, 0, 0x40, 0, 0x80, 0, 0, 0, 0, 0, 0, 1],
        repository_id: [0x61; 16],
    };
    let state = RuntimeState::open(&repository, runtime_identity, [0x62; 32])
        .await
        .unwrap();
    let writer = state.acquire_lease([0x63; 16]).await.unwrap();
    let capability = repository.capture_capability(relation_id).unwrap();
    let mut filesystem = FilesystemProvider::with_limits(1 << 20, 16).unwrap();
    filesystem.allow_root(source.path()).unwrap();
    let mut bindings = SysHostBindingRegistry::new(EnvironmentProvider::default())
        .with_filesystem_provider(filesystem)
        .with_repository_capture_capability(capability);

    for (key, fixture, ordinal) in [
        ("song", SONG_IMPORT_FIXTURE, 0x70_u8),
        ("image", IMAGE_IMPORT_FIXTURE, 0x80_u8),
    ] {
        let expression = import_expression(fixture, source.path());
        commit_capture(
            &repository,
            &state,
            writer,
            &mut bindings,
            &expression,
            key,
            ordinal,
            true,
        )
        .await;
    }
    drop(bindings);
    drop(state);

    // The payload each row must still serve, read from the source before it is
    // frozen, so the copy is compared against something real.
    let expected = [("song", "tone.wav"), ("image", "pixel.png")].map(|(key, file)| {
        (
            key,
            std::fs::read(Path::new(MEDIA_FIXTURES).join(file)).unwrap(),
        )
    });

    let project = directory.path().to_path_buf();
    let snapshot = String::from_utf8(git_output(&project, &["rev-parse", "HEAD"], None))
        .unwrap()
        .trim()
        .to_owned();

    // Freeze the snapshot, read the archive back from an unrelated directory,
    // and reconstruct it into a copy that is not the source.
    let outside = TempDir::new().unwrap();
    let archive = outside.path().join("archive");
    let archive_path = archive.to_str().unwrap();
    let exported = run(&project, &["export", archive_path, "--at", "HEAD"]);
    assert_eq!(
        exported.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&exported.stderr)
    );

    let unrelated = TempDir::new().unwrap();
    let checked = run(unrelated.path(), &["export", archive_path, "--check"]);
    assert_eq!(
        checked.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&checked.stderr)
    );
    assert!(
        String::from_utf8_lossy(&checked.stdout).contains(&snapshot),
        "check reports the recorded snapshot"
    );

    let restored: PathBuf = outside.path().join("restored");
    let restored_path = restored.to_str().unwrap().to_owned();
    let reconstructed = run(
        &project,
        &[
            "export",
            archive_path,
            "--restore",
            restored_path.as_str(),
            "--worktree",
        ],
    );
    assert_eq!(
        reconstructed.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&reconstructed.stderr)
    );

    // The source repository and its worktree are gone before the copy is read.
    drop(repository);
    drop(directory);
    drop(source);
    assert!(!project.exists(), "the source worktree is removed");

    assert_eq!(
        String::from_utf8(git_output(&restored, &["rev-parse", "HEAD"], None))
            .unwrap()
            .trim(),
        snapshot,
        "the copy's HEAD is the pinned commit"
    );
    assert_self_contained(&restored);

    // Query the copy: both rows are listed from their committed metadata, and
    // the listing charges no payload byte.
    let relation = relation_hex(relation_id);
    let query = run(&restored, &["query", &relation, "--format", "json"]);
    assert_eq!(
        query.status.code(),
        Some(0),
        "query failed: {}",
        String::from_utf8_lossy(&query.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&query.stdout).unwrap();
    assert_eq!(
        report["media_payload_bytes_read"].as_u64(),
        Some(0),
        "a metadata listing must charge no media payload byte"
    );
    let listings = report["listings"].as_array().unwrap();
    assert_eq!(
        listings.len(),
        2,
        "both committed rows are listed: {report}"
    );
    for (key, bytes) in &expected {
        let listing = listings
            .iter()
            .find(|listing| listing["key"] == *key)
            .unwrap_or_else(|| panic!("the {key} row is listed from the copy"));
        assert_eq!(listing["length"].as_u64(), Some(bytes.len() as u64));
        assert_eq!(
            listing["sha256"].as_str().unwrap(),
            hex(sha2::Sha256::digest(bytes).as_slice())
        );
        assert_eq!(listing["hydrated"].as_bool(), Some(false));
    }

    // History of the copy: each row's revisions, newest first, with the head
    // commit carrying the row.
    for key in ["song", "image"] {
        let history = run(&restored, &["history", &relation, key, "--format", "json"]);
        assert_eq!(
            history.status.code(),
            Some(0),
            "history failed for {key}: {}",
            String::from_utf8_lossy(&history.stderr)
        );
        let entries: serde_json::Value = serde_json::from_slice(&history.stdout).unwrap();
        let entries = entries.as_array().unwrap();
        assert!(!entries.is_empty(), "{key}: the copy carries its revisions");
        assert_eq!(entries[0]["commit"], snapshot.as_str(), "{key}: head first");
        assert_eq!(entries[0]["present"], true, "{key}: head carries the row");
    }

    // Payload read of the copy: the committed bytes come back out of the copy.
    let copy = Repository::discover(&restored).expect("the copy is a discoverable repository");
    for (key, bytes) in &expected {
        let (charged, length, restored_bytes) = read_committed_payload(&copy, relation_id, key);
        assert_eq!(length, bytes.len() as u64, "{key}: the recorded length");
        assert_eq!(
            charged,
            bytes.len() as u64,
            "{key}: only the payload's own bytes are charged"
        );
        assert_eq!(
            &restored_bytes, bytes,
            "{key}: the payload read out of the copy"
        );
    }
}
