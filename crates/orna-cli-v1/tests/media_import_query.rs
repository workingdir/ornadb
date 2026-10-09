use std::path::Path;

use orna_evaluator_v1::{Limits, SysHostBindingRegistry, evaluate_expression_ovb2_with_effects};
use orna_repository_v1::{KeyRange, Repository, RowRevision, TypedKey};
use orna_runtime_v1::{
    RequestIdentity, RequestState, RuntimeIdentity, RuntimeState, TableMutation, TerminalOutcome,
};
use orna_sys_v1::{EnvironmentProvider, FilesystemProvider};
use orna_value_v1::{BlobMetadataFilter, compare_blob_metadata, decode_rov3_blob_metadata};
use sha2::{Digest, Sha256};
use tempfile::TempDir;

#[path = "support/format3.rs"]
mod format3;
use format3::*;

const MEDIA_FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/media");
const SONG_IMPORT_FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/media/import-song.orna"
);
const PAGED_SONG_IMPORT_FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/media/import-song-paged.orna"
);
const AUTHORED_SONG_IMPORT_FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/media/import-song-authored.orna"
);
const SINCE_SONG_IMPORT_FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/media/import-song-since.orna"
);
const LIMIT_SONG_IMPORT_FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/media/import-song-limit.orna"
);
const IMAGE_IMPORT_FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/media/import-image.orna"
);
const LIMIT_ZERO_SONG_IMPORT_FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/media/import-song-limit-zero.orna"
);
const SORT_IMAGE_IMPORT_FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/media/import-image-sort.orna"
);
const NEGATION_SONG_IMPORT_FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/media/import-song-negation.orna"
);
const ANNOTATED_SONG_IMPORT_FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/media/import-song-annotation.orna"
);
const PROJECTION_SONG_IMPORT_FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/media/import-song-projection.orna"
);
const PINNED_SONG_IMPORT_FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/media/import-song-at.orna"
);
const NEGATIVE_LIMIT_SONG_IMPORT_FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/media/import-song-negative-limit.orna"
);
const EMPTY_CATALOGUE_SONG_FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/media/import-song-empty-catalogue.orna"
);
const IMAGE_SINCE_BOUNDARY_FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/media/import-image-since-boundary.orna"
);
const IMAGE_JSON_IMPORT_FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/media/import-image-json.orna"
);
const MEDIA_ROOT_PLACEHOLDER: &str = "__MEDIA_ROOT__";

/// One committed media row, as its listing query sees it.
#[derive(Debug, PartialEq, Eq)]
struct MediaListing {
    key: Vec<u8>,
    media_type: String,
    suffix: Option<String>,
    length: u64,
    sha256: [u8; 32],
    hydrated: bool,
}

#[tokio::test]
async fn song_and_image_import_commit_through_capture_and_list_without_payloads() {
    let (directory, repository, relation_id) = empty_format3_repository();
    // Import from a scratch copy so the source files can be removed afterwards.
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

    let imports = [
        ("song", "tone.wav", 0x70_u8),
        ("image", "pixel.png", 0x80_u8),
    ];
    for (key, file, ordinal) in imports {
        // Both imports run from committed .orna fixtures.
        let fixture = match key {
            "image" => IMAGE_IMPORT_FIXTURE,
            _ => SONG_IMPORT_FIXTURE,
        };
        let expression = import_expression(fixture, source.path());
        import_media(
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

    // Source files are gone; listing must still answer from committed rows.
    std::fs::remove_file(source.path().join("tone.wav")).unwrap();
    std::fs::remove_file(source.path().join("pixel.png")).unwrap();

    let listing = list_media(&state).await;
    // Rows come back ordered by key: image before song.
    let expected = [
        ("image", "image/png", 73_u64, "pixel.png"),
        ("song", "audio/wav", 4044_u64, "tone.wav"),
    ];
    assert_eq!(listing.len(), expected.len());
    for (row, (key, media_type, length, file)) in listing.iter().zip(expected) {
        assert_eq!(row.key, key.as_bytes());
        assert_eq!(row.media_type, media_type);
        assert_eq!(row.length, length);
        assert!(!row.hydrated, "listing must not hydrate {key}");
        let bytes = std::fs::read(Path::new(MEDIA_FIXTURES).join(file)).unwrap();
        assert_eq!(row.sha256, <[u8; 32]>::from(Sha256::digest(&bytes)));
    }

    // Filters select rows from payload-free metadata; nothing is hydrated.
    let song = || vec![b"song".to_vec()];
    let by_type = BlobMetadataFilter::new().with_media_type("audio/wav");
    assert_eq!(filter_media(&state, &by_type).await, song());
    let by_size = BlobMetadataFilter::new().with_min_length(1000);
    assert_eq!(filter_media(&state, &by_size).await, song());
    let small_png = BlobMetadataFilter::new()
        .with_media_type("image/png")
        .with_max_length(100);
    assert_eq!(
        filter_media(&state, &small_png).await,
        vec![b"image".to_vec()]
    );
    let too_small = BlobMetadataFilter::new().with_max_length(10);
    assert!(filter_media(&state, &too_small).await.is_empty());

    // Bytes avoided: every listed payload is committed but never hydrated, so
    // the listing saves the full committed length of each row.
    let on_disk: u64 = ["tone.wav", "pixel.png"]
        .iter()
        .map(|file| std::fs::metadata(Path::new(MEDIA_FIXTURES).join(file)).unwrap().len())
        .sum();
    let avoided: u64 = listing.iter().map(|row| row.length).sum();
    assert_eq!(avoided, on_disk, "listing avoids every committed payload byte");
    assert!(listing.iter().all(|row| !row.hydrated));

    assert_song_revision_history(&repository, &directory, relation_id);

    // Edit: commit the song key again with the pixel payload. The next listing
    // reads the replacement row at once, still without fetching media bytes.
    std::fs::copy(
        Path::new(MEDIA_FIXTURES).join("pixel.png"),
        source.path().join("pixel.png"),
    )
    .unwrap();
    let edit = import_expression(IMAGE_IMPORT_FIXTURE, source.path());
    import_media(&repository, &state, writer, &mut bindings, &edit, "song", 0x90, false).await;
    let edited = list_media(&state).await;
    let song = edited.iter().find(|row| row.key == b"song").unwrap();
    assert_eq!(song.media_type, "image/png");
    assert_eq!(song.length, 73);
    assert!(!song.hydrated, "edited listing must not hydrate the song");

    // Re-import: committing the same .orna fixture again leaves the listing
    // exactly as the edit left it. Only committed rows are compared.
    import_media(&repository, &state, writer, &mut bindings, &edit, "song", 0xa0, false).await;
    assert_eq!(
        list_media(&state).await,
        edited,
        "re-importing the same fixture must not change the listing"
    );

    drop(directory);
}

#[tokio::test]
async fn history_pages_cover_every_revision_once_across_imported_rows() {
    let (directory, repository, relation_id) = empty_format3_repository();
    let source = TempDir::new().unwrap();
    std::fs::copy(
        Path::new(MEDIA_FIXTURES).join("tone.wav"),
        source.path().join("tone.wav"),
    )
    .unwrap();

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

    // Two imported rows, from two fixtures, so the walk spans more than one row.
    let song = import_expression(SONG_IMPORT_FIXTURE, source.path());
    import_media(&repository, &state, writer, &mut bindings, &song, "song", 0x70, true).await;
    let paged = import_expression(PAGED_SONG_IMPORT_FIXTURE, source.path());
    import_media(&repository, &state, writer, &mut bindings, &paged, "song-again", 0x80, true).await;

    let format = repository.open_format_context().unwrap();
    let row_map = format.load_row_map(relation_id).unwrap();
    let graph = format.open_native_graph(&row_map).unwrap();
    let scope = graph.open_read_scope().unwrap();
    let rows = graph
        .range_rows(&KeyRange::new(None, None, 16).unwrap(), &scope)
        .unwrap();
    let head = git_output(directory.path(), &["rev-parse", "HEAD"], None);
    let head = String::from_utf8(head).unwrap().trim().to_owned();

    for key in ["song", "song-again"] {
        let row = rows
            .iter()
            .find(|row| {
                row.key() == &TypedKey::Bytes(key.as_bytes().to_vec())
                    || row.key() == &TypedKey::Text(key.to_owned())
            })
            .unwrap_or_else(|| panic!("the {key} row is committed"));
        let revisions = graph.list_row_revisions(row, "HEAD", 64, &scope).unwrap();
        assert_eq!(revisions[0].commit().to_hex(), head, "{key}: head first");
        // One revision per page: every page is exactly the next commit.
        let pages = revision_pages(
            |max| graph.list_row_revisions(row, "HEAD", max, &scope).unwrap(),
            1,
            revisions.len(),
        );
        assert_eq!(pages.len(), revisions.len(), "{key}: one page per revision");
        assert!(pages.iter().all(|page| page.len() == 1), "{key}: page size");
        assert_eq!(pages.concat(), revisions, "{key}: pages cover the walk once");
    }
    drop(directory);
}

/// Lists the committed song row's revisions through the OGS-1 walk. Only
/// commit headers and tree listings are read; no blob is opened or pinned.
#[tokio::test]
async fn author_filter_keeps_every_revision_of_each_imported_row() {
    let (directory, repository, relation_id) = empty_format3_repository();
    let source = TempDir::new().unwrap();
    std::fs::copy(
        Path::new(MEDIA_FIXTURES).join("tone.wav"),
        source.path().join("tone.wav"),
    )
    .unwrap();

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

    // Two imported rows from two fixtures, so the filter runs across both walks.
    let song = import_expression(SONG_IMPORT_FIXTURE, source.path());
    import_media(&repository, &state, writer, &mut bindings, &song, "song", 0x70, true).await;
    let authored = import_expression(AUTHORED_SONG_IMPORT_FIXTURE, source.path());
    import_media(&repository, &state, writer, &mut bindings, &authored, "song-authored", 0x80, true).await;

    let format = repository.open_format_context().unwrap();
    let row_map = format.load_row_map(relation_id).unwrap();
    let graph = format.open_native_graph(&row_map).unwrap();
    let scope = graph.open_read_scope().unwrap();
    let rows = graph
        .range_rows(&KeyRange::new(None, None, 16).unwrap(), &scope)
        .unwrap();
    let head = git_output(directory.path(), &["rev-parse", "HEAD"], None);
    let head = String::from_utf8(head).unwrap().trim().to_owned();

    for key in ["song", "song-authored"] {
        let row = rows
            .iter()
            .find(|row| {
                row.key() == &TypedKey::Bytes(key.as_bytes().to_vec())
                    || row.key() == &TypedKey::Text(key.to_owned())
            })
            .unwrap_or_else(|| panic!("the {key} row is committed"));
        let revisions = graph.list_row_revisions(row, "HEAD", 64, &scope).unwrap();
        assert_eq!(revisions[0].commit().to_hex(), head, "{key}: head first");
        // Every revision in the walk is stamped with the fixture identity, so the
        // filter keeps all of them and an unknown author keeps none.
        assert!(
            revisions.iter().all(|revision| revision.author().contains("kierandrewett")),
            "{key}: every revision carries the fixture author"
        );
        assert_eq!(filter_by_author(&revisions, "kierandrewett"), revisions, "{key}");
        assert!(filter_by_author(&revisions, "no-such-author").is_empty(), "{key}");
    }
    drop(directory);
}

#[tokio::test]
async fn since_cut_keeps_only_newer_revisions_of_each_imported_row() {
    let (directory, repository, relation_id) = empty_format3_repository();
    let source = TempDir::new().unwrap();
    std::fs::copy(
        Path::new(MEDIA_FIXTURES).join("tone.wav"),
        source.path().join("tone.wav"),
    )
    .unwrap();

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

    let song = import_expression(SONG_IMPORT_FIXTURE, source.path());
    import_media(&repository, &state, writer, &mut bindings, &song, "song", 0x70, true).await;
    let later = import_expression(SINCE_SONG_IMPORT_FIXTURE, source.path());
    import_media(&repository, &state, writer, &mut bindings, &later, "song-later", 0x80, true).await;
    // Reading the committed rows first, as the listing does, makes them visible to the graph.
    state.committed_table_rows("media").await.unwrap();

    let format = repository.open_format_context().unwrap();
    let row_map = format.load_row_map(relation_id).unwrap();
    let graph = format.open_native_graph(&row_map).unwrap();
    let scope = graph.open_read_scope().unwrap();
    let rows = graph
        .range_rows(&KeyRange::new(None, None, 16).unwrap(), &scope)
        .unwrap();
    let head = git_output(directory.path(), &["rev-parse", "HEAD"], None);
    let head = String::from_utf8(head).unwrap().trim().to_owned();

    for key in ["song", "song-later"] {
        let row = rows
            .iter()
            .find(|row| {
                row.key() == &TypedKey::Bytes(key.as_bytes().to_vec())
                    || row.key() == &TypedKey::Text(key.to_owned())
            })
            .unwrap_or_else(|| panic!("the {key} row is committed"));
        let revisions = graph.list_row_revisions(row, "HEAD", 64, &scope).unwrap();
        assert_eq!(revisions[0].commit().to_hex(), head, "{key}: head first");
        // Cutting at the second revision keeps only the head, which is newer.
        let since = revisions[1].commit().to_hex();
        assert_eq!(
            revisions_since(&revisions, &since),
            Some(revisions[..1].to_vec()),
            "{key}: since keeps only newer revisions"
        );
        // A cut at the oldest revision keeps everything except that commit.
        let oldest = revisions.last().unwrap().commit().to_hex();
        assert_eq!(
            revisions_since(&revisions, &oldest),
            Some(revisions[..revisions.len() - 1].to_vec()),
            "{key}: since at the oldest keeps the rest"
        );
    }
    drop(directory);
}

#[tokio::test]
async fn limit_walk_matches_the_truncated_full_walk_for_each_imported_row() {
    let (directory, repository, relation_id) = empty_format3_repository();
    let source = TempDir::new().unwrap();
    std::fs::copy(
        Path::new(MEDIA_FIXTURES).join("tone.wav"),
        source.path().join("tone.wav"),
    )
    .unwrap();

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

    // Three imported rows, so the limit is checked against more than one walk.
    let imports = [
        ("song", SONG_IMPORT_FIXTURE, 0x70_u8),
        ("song-limit", LIMIT_SONG_IMPORT_FIXTURE, 0x80_u8),
        ("song-later", SINCE_SONG_IMPORT_FIXTURE, 0x90_u8),
    ];
    for (key, fixture, ordinal) in imports {
        let expression = import_expression(fixture, source.path());
        import_media(&repository, &state, writer, &mut bindings, &expression, key, ordinal, true).await;
    }
    // Reading the committed rows first makes them visible to the graph.
    state.committed_table_rows("media").await.unwrap();

    let format = repository.open_format_context().unwrap();
    let row_map = format.load_row_map(relation_id).unwrap();
    let graph = format.open_native_graph(&row_map).unwrap();
    let scope = graph.open_read_scope().unwrap();
    let rows = graph
        .range_rows(&KeyRange::new(None, None, 16).unwrap(), &scope)
        .unwrap();

    for (key, _, _) in imports {
        let row = rows
            .iter()
            .find(|row| {
                row.key() == &TypedKey::Bytes(key.as_bytes().to_vec())
                    || row.key() == &TypedKey::Text(key.to_owned())
            })
            .unwrap_or_else(|| panic!("the {key} row is committed"));
        let full = graph.list_row_revisions(row, "HEAD", 64, &scope).unwrap();
        // `orna history --limit N` walks only N commits, so each bounded walk
        // must equal the first N revisions of the full walk.
        for n in 1..=full.len() {
            let bounded = graph.list_row_revisions(row, "HEAD", n, &scope).unwrap();
            assert_eq!(bounded, limited(&full, n), "{key}: limit {n}");
        }
    }
    drop(directory);
}

fn assert_song_revision_history(
    repository: &Repository,
    directory: &TempDir,
    relation_id: [u8; 16],
) {
    let format = repository.open_format_context().unwrap();
    let row_map = format.load_row_map(relation_id).unwrap();
    let graph = format.open_native_graph(&row_map).unwrap();
    let scope = graph.open_read_scope().unwrap();
    let rows = graph
        .range_rows(&KeyRange::new(None, None, 16).unwrap(), &scope)
        .unwrap();
    let song = rows
        .iter()
        .find(|row| {
            row.key() == &TypedKey::Bytes(b"song".to_vec())
                || row.key() == &TypedKey::Text("song".to_owned())
        })
        .expect("the song row is committed");

    let revisions = graph.list_row_revisions(song, "HEAD", 64, &scope).unwrap();
    let head = git_output(directory.path(), &["rev-parse", "HEAD"], None);
    let head = String::from_utf8(head).unwrap().trim().to_owned();
    // Store install plus one commit per imported row, newest first.
    assert!(revisions.len() >= 3, "history lists every reachable commit");
    assert_eq!(revisions[0].commit().to_hex(), head);
    assert!(revisions[0].present(), "the head revision carries the song");
    assert!(
        !revisions.last().unwrap().present(),
        "the oldest revision predates the import"
    );
    // Paging one commit at a time must reproduce the full walk exactly.
    let pages = revision_pages(
        |max| graph.list_row_revisions(song, "HEAD", max, &scope).unwrap(),
        1,
        revisions.len(),
    );
    assert_eq!(pages.concat(), revisions);
    // The author filter keeps revisions whose `Name <email>` contains the text.
    assert_eq!(filter_by_author(&revisions, "kierandrewett"), revisions);
    assert!(filter_by_author(&revisions, "no-such-author").is_empty());
    // `--since <commit>` keeps only the revisions newer than that commit.
    let since = revisions[2].commit().to_hex();
    assert_eq!(revisions_since(&revisions, &since), Some(revisions[..2].to_vec()));
    assert_eq!(revisions_since(&revisions, "no-such-commit"), None);
    // `--limit N` keeps the N newest revisions; a limit past the walk keeps all.
    assert_eq!(limited(&revisions, 2), revisions[..2].to_vec());
    assert_eq!(limited(&revisions, revisions.len() + 5), revisions);
    assert!(limited(&revisions, 0).is_empty());
}

/// Keeps the revisions newer than `since`, the same cut `orna history --since`
/// applies: the named commit and everything older are dropped. `None` when the
/// commit is not in the walk.
fn revisions_since(revisions: &[RowRevision], since: &str) -> Option<Vec<RowRevision>> {
    let position = revisions
        .iter()
        .position(|revision| revision.commit().to_hex() == since)?;
    Some(revisions[..position].to_vec())
}


/// Asserts an `orna history` run was refused: exit 1 and no revisions printed.
fn assert_history_refused(output: &std::process::Output, what: &str) {
    assert_eq!(
        output.status.code(),
        Some(1),
        "{what} must exit 1: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stdout.is_empty(),
        "{what} must list no revisions: {}",
        String::from_utf8_lossy(&output.stdout)
    );
}

/// The 16-byte relation id as the 32-digit hex the `orna history` CLI takes.
fn relation_hex(relation_id: [u8; 16]) -> String {
    relation_id.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Runs `orna history` with `arguments` as a child process inside the repository
/// and returns its raw output, whatever the exit status.
fn run_history(directory: &Path, arguments: &[&str]) -> std::process::Output {
    let mut words = vec!["history"];
    words.extend_from_slice(arguments);
    run_orna_from(directory, &words)
}

/// Runs `orna query` with `arguments` as a child process inside the repository
/// and returns its raw output, whatever the exit status.
fn run_query(directory: &Path, arguments: &[&str]) -> std::process::Output {
    let mut words = vec!["query"];
    words.extend_from_slice(arguments);
    run_orna_from(directory, &words)
}

/// Runs the built CLI from `cwd` with `words` as its whole argument list, so a
/// test can choose the global `--db` endpoint and the process directory
/// independently.
fn run_orna_from(cwd: &Path, words: &[&str]) -> std::process::Output {
    std::process::Command::new(env!("CARGO_BIN_EXE_orna-cli-v1"))
        .current_dir(cwd)
        .args(words)
        .output()
        .unwrap()
}

/// Runs `orna history <relation> <key> --format json` as a child process inside
/// the repository and parses its stdout as the revision array.
fn history_json(directory: &Path, relation_id: [u8; 16], key: &str) -> Vec<serde_json::Value> {
    let output = run_history(directory, &[&relation_hex(relation_id), key, "--format", "json"]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "history failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    match serde_json::from_slice(&output.stdout).unwrap() {
        serde_json::Value::Array(entries) => entries,
        other => panic!("history --format json must print an array, got {other}"),
    }
}

/// Keeps the `limit` newest revisions, the truncation `orna history --limit`
/// applies to the newest-first walk.
fn limited(revisions: &[RowRevision], limit: usize) -> Vec<RowRevision> {
    revisions.iter().take(limit).cloned().collect()
}

/// Keeps the revisions whose commit author contains `needle`, the same
/// substring rule `orna history --author` applies to each listed revision.
fn filter_by_author(revisions: &[RowRevision], needle: &str) -> Vec<RowRevision> {
    revisions
        .iter()
        .filter(|revision| revision.author().contains(needle))
        .cloned()
        .collect()
}

/// Splits a newest-first revision walk into pages of `size`. `list(max)`
/// returns the first `max` revisions, so page `n` is the slice that the walk
/// adds beyond page `n - 1`: concatenated pages must equal the full history.
fn revision_pages(
    list: impl Fn(usize) -> Vec<RowRevision>,
    size: usize,
    total: usize,
) -> Vec<Vec<RowRevision>> {
    (0..total.div_ceil(size))
        .map(|page| {
            let walked = list((page + 1) * size);
            walked
                .get(page * size..)
                .unwrap_or_default()
                .iter()
                .take(size)
                .cloned()
                .collect()
        })
        .collect()
}

/// Loads an import expression from its committed .orna fixture, with the
/// scratch media root written in as a quoted string literal.
fn import_expression(fixture: &str, root: &Path) -> String {
    let fixture = std::fs::read_to_string(fixture).unwrap();
    let root = format!("{:?}", root.to_string_lossy().as_ref());
    fixture.trim_end().replace(MEDIA_ROOT_PLACEHOLDER, &root)
}

/// Captures one file through `sys.blob.capture_file` and commits its row in an
/// admitted request. The row stores the annotated Blob reference.
async fn import_media(
    repository: &Repository,
    state: &RuntimeState,
    writer: orna_runtime_v1::WriterLease,
    bindings: &mut SysHostBindingRegistry,
    expression: &str,
    key: &str,
    ordinal: u8,
    insert_only: bool,
) {
    let request_identity = RequestIdentity {
        session_id: [ordinal; 16],
        request_id: [ordinal + 1; 16],
    };
    let fingerprint = [ordinal + 2; 32];
    let (_, admission) = state
        .reserve_request_with_admission(request_identity, fingerprint)
        .await
        .unwrap();
    state
        .start_request_with_owner_and_admission(
            request_identity,
            fingerprint,
            writer,
            admission.expect("new request returns its admission capability"),
        )
        .await
        .unwrap();
    let context = state.begin_activation().await.unwrap();

    let value = evaluate_expression_ovb2_with_effects(
        expression,
        &Default::default(),
        Limits::default(),
        bindings,
    )
    .unwrap();
    let binding = bindings.accept_captured_blob_for_row(&value).unwrap();
    let key_text = key;
    let (id, key, row) = ([ordinal + 3; 16], key.as_bytes().to_vec(), Vec::new());
    let mutation = if insert_only {
        TableMutation::insert(id, "media", key, row)
    } else {
        TableMutation::new(id, "media", key, Some(row))
    }
    .unwrap()
    .with_orp_blob_binding(binding)
    .unwrap();
    let committed = state
        .commit_table_request_activation(
            writer,
            request_identity,
            fingerprint,
            &context,
            &[mutation],
            [ordinal + 4; 32],
            TerminalOutcome::new(vec![ordinal + 5]).unwrap(),
            &orna_runtime_v1::NoFault,
        )
        .await
        .unwrap();
    assert_eq!(committed.request.state, RequestState::Completed);
    // The runtime only stages durable mutations. Publication is the explicit
    // boundary that freezes that prefix into the store, so the imported row
    // becomes reachable from a published snapshot.
    publish_media_row(repository, state, key_text, ordinal).await;
}

#[tokio::test]
async fn deleting_a_committed_media_row_removes_it_from_listing_and_filters() {
    let (_directory, repository, relation_id) = empty_format3_repository();
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

    let imports = [
        ("song", SONG_IMPORT_FIXTURE, 0x70_u8),
        ("image", IMAGE_IMPORT_FIXTURE, 0x80_u8),
    ];
    for (key, fixture, ordinal) in imports {
        let expression = import_expression(fixture, source.path());
        import_media(&repository, &state, writer, &mut bindings, &expression, key, ordinal, true).await;
    }

    let song_filter = BlobMetadataFilter::new().with_media_type("audio/wav");
    let image_filter = BlobMetadataFilter::new().with_media_type("image/png");
    assert_eq!(list_media(&state).await.len(), 2);
    assert_eq!(filter_media(&state, &song_filter).await, vec![b"song".to_vec()]);

    // Membership follows the edit: the committed delete removes the song row
    // from the listing and from every filter that used to select it.
    delete_media(&state, writer, "song", 0x90).await;

    let keys = list_media(&state)
        .await
        .into_iter()
        .map(|row| row.key)
        .collect::<Vec<_>>();
    assert_eq!(keys, vec![b"image".to_vec()]);
    assert!(filter_media(&state, &song_filter).await.is_empty());
    assert_eq!(filter_media(&state, &image_filter).await, vec![b"image".to_vec()]);
}

#[tokio::test]
async fn history_json_lists_each_revision_with_exactly_the_four_keys() {
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

    let song = import_expression(SONG_IMPORT_FIXTURE, source.path());
    import_media(&repository, &state, writer, &mut bindings, &song, "song", 0x70, true).await;
    let image = import_expression(IMAGE_JSON_IMPORT_FIXTURE, source.path());
    import_media(&repository, &state, writer, &mut bindings, &image, "image", 0x80, true).await;
    drop(bindings);
    drop(state);

    let head = git_output(directory.path(), &["rev-parse", "HEAD"], None);
    let head = String::from_utf8(head).unwrap().trim().to_owned();
    let entries = history_json(directory.path(), relation_id, "image");
    assert!(!entries.is_empty(), "the image row has revisions");
    for entry in &entries {
        let object = entry.as_object().expect("each revision is a JSON object");
        let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, ["author", "commit", "committed", "present", "tree"]);
        for field in ["commit", "tree"] {
            let hex = object[field].as_str().expect("hex id is a string");
            assert_eq!(hex.len(), 40, "{field} is a full object id");
            assert!(hex.bytes().all(|byte| byte.is_ascii_hexdigit()));
        }
        assert!(object["present"].is_boolean(), "present is a JSON boolean");
        assert!(object["author"].as_str().unwrap().contains('<'));
    }
    // Newest first: the head commit carries the image; older revisions do not.
    assert_eq!(entries[0]["commit"], head.as_str());
    assert_eq!(entries[0]["present"], true);
    assert!(entries.iter().any(|entry| entry["present"] == false));
    drop(directory);
}

#[tokio::test]
async fn history_rejects_negative_and_out_of_range_limits_before_listing() {
    let (directory, repository, relation_id) = empty_format3_repository();
    let source = TempDir::new().unwrap();
    std::fs::copy(
        Path::new(MEDIA_FIXTURES).join("tone.wav"),
        source.path().join("tone.wav"),
    )
    .unwrap();

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

    let fixture = import_expression(NEGATIVE_LIMIT_SONG_IMPORT_FIXTURE, source.path());
    import_media(&repository, &state, writer, &mut bindings, &fixture, "song", 0x70, true).await;
    drop(bindings);
    drop(state);

    let relation = relation_hex(relation_id);
    // Each value is outside 1..=4096 or not a number; `-1` is the negative case.
    for value in ["-1", "0", "4097", "x"] {
        let output = run_history(directory.path(), &[&relation, "song", "--limit", value]);
        assert_history_refused(&output, &format!("--limit {value}"));
    }
    drop(directory);
}

#[tokio::test]
async fn history_since_excludes_the_named_boundary_commit_end_to_end() {
    let (directory, repository, relation_id) = empty_format3_repository();
    let source = TempDir::new().unwrap();
    std::fs::copy(
        Path::new(MEDIA_FIXTURES).join("pixel.png"),
        source.path().join("pixel.png"),
    )
    .unwrap();

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

    let image = import_expression(IMAGE_SINCE_BOUNDARY_FIXTURE, source.path());
    import_media(&repository, &state, writer, &mut bindings, &image, "image", 0x70, true).await;
    drop(bindings);
    drop(state);

    let relation = relation_hex(relation_id);
    let full = history_json(directory.path(), relation_id, "image");
    assert!(full.len() >= 2, "the image row has a present and an absent revision");

    // `--since <commit>` drops the named commit and everything older, so the
    // boundary revision itself is excluded and only the newer one remains.
    let boundary = full[1]["commit"].as_str().unwrap();
    let output = run_history(
        directory.path(),
        &[&relation, "image", "--since", boundary, "--format", "json"],
    );
    assert_eq!(output.status.code(), Some(0), "{}", String::from_utf8_lossy(&output.stderr));
    let newer: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(newer, full[..1].to_vec(), "only the revision newer than the boundary");

    // Cutting at the oldest revision keeps every other revision.
    let oldest = full.last().unwrap()["commit"].as_str().unwrap();
    let output = run_history(
        directory.path(),
        &[&relation, "image", "--since", oldest, "--format", "json"],
    );
    assert_eq!(output.status.code(), Some(0));
    let kept: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(kept, full[..full.len() - 1].to_vec());

    // A commit outside the walk is refused with exit 1 and no listing.
    let unknown = "0".repeat(40);
    let output = run_history(
        directory.path(),
        &[&relation, "image", "--since", &unknown, "--format", "json"],
    );
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    drop(directory);
}

#[tokio::test]
async fn history_on_an_emptied_catalogue_is_refused_with_no_listing() {
    let (directory, repository, relation_id) = empty_format3_repository();
    let source = TempDir::new().unwrap();
    std::fs::copy(
        Path::new(MEDIA_FIXTURES).join("tone.wav"),
        source.path().join("tone.wav"),
    )
    .unwrap();

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

    let song = import_expression(EMPTY_CATALOGUE_SONG_FIXTURE, source.path());
    import_media(&repository, &state, writer, &mut bindings, &song, "song", 0x70, true).await;
    // Deleting the only committed row leaves the catalogue empty.
    delete_media(&state, writer, "song", 0x80).await;
    assert!(list_media(&state).await.is_empty(), "the catalogue is empty");
    drop(bindings);
    drop(state);

    let relation = relation_hex(relation_id);
    let output = run_history(directory.path(), &[&relation, "song"]);
    assert_history_refused(&output, "history on an emptied catalogue");
    drop(directory);
}

/// `orna --db PATH history` must read the named repository, so the same
/// listing is available from an unrelated working directory.
#[tokio::test]
async fn history_reads_the_named_db_endpoint_from_an_unrelated_directory() {
    let (directory, repository, relation_id) = empty_format3_repository();
    let source = TempDir::new().unwrap();
    std::fs::copy(
        Path::new(MEDIA_FIXTURES).join("tone.wav"),
        source.path().join("tone.wav"),
    )
    .unwrap();

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

    let song = import_expression(SONG_IMPORT_FIXTURE, source.path());
    import_media(&repository, &state, writer, &mut bindings, &song, "song", 0x70, true).await;
    drop(bindings);
    drop(state);

    let relation = relation_hex(relation_id);
    // A directory that is not inside any repository: only `--db` can answer.
    let elsewhere = TempDir::new().unwrap();
    let database = directory.path().to_string_lossy().into_owned();
    let output = run_orna_from(
        elsewhere.path(),
        &["--db", &database, "history", &relation, "song", "--format", "json"],
    );
    assert_eq!(
        output.status.code(),
        Some(0),
        "history --db failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let routed: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout).unwrap();
    assert!(!routed.is_empty(), "the imported row has revisions");
    assert_eq!(
        routed,
        history_json(directory.path(), relation_id, "song"),
        "--db names the same repository as the worktree directory"
    );

    // Without `--db` the unrelated directory holds no repository, so the same
    // command is refused instead of silently answering from somewhere else.
    let output = run_history(elsewhere.path(), &[&relation, "song"]);
    assert_history_refused(&output, "history outside a repository without --db");
    drop(directory);
}

/// WALKTHROUGH §6: `orna history --at SELECTOR` reads the snapshot the
/// selector named. The selector resolves once, so the answer stays in the past
/// even though HEAD has moved on since.
#[tokio::test]
async fn history_at_pins_the_named_snapshot_instead_of_head() {
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

    // The song lands first, so its commit is the past the pin must return.
    let song = import_expression(SONG_IMPORT_FIXTURE, source.path());
    import_media(&repository, &state, writer, &mut bindings, &song, "song", 0x70, true).await;
    let past = String::from_utf8(git_output(directory.path(), &["rev-parse", "HEAD"], None))
        .unwrap()
        .trim()
        .to_owned();

    // A later image import moves HEAD on; the pin must not follow it.
    let image = import_expression(IMAGE_JSON_IMPORT_FIXTURE, source.path());
    import_media(&repository, &state, writer, &mut bindings, &image, "image", 0x80, true).await;
    drop(bindings);
    drop(state);
    let present = String::from_utf8(git_output(directory.path(), &["rev-parse", "HEAD"], None))
        .unwrap()
        .trim()
        .to_owned();
    assert_ne!(past, present, "the image import advanced HEAD");

    let relation = relation_hex(relation_id);
    let pinned = run_history(
        directory.path(),
        &[&relation, "song", "--at", &past, "--format", "json"],
    );
    assert_eq!(
        pinned.status.code(),
        Some(0),
        "history --at failed: {}",
        String::from_utf8_lossy(&pinned.stderr)
    );
    let pinned: Vec<serde_json::Value> = serde_json::from_slice(&pinned.stdout).unwrap();
    assert!(!pinned.is_empty(), "the pinned snapshot carries the song");
    assert_eq!(
        pinned[0]["commit"], past.as_str(),
        "--at starts the walk at the named snapshot"
    );
    assert!(
        pinned.iter().all(|entry| entry["commit"] != present.as_str()),
        "the pinned walk never reaches the newer HEAD commit"
    );

    // Pinning only shortens the walk: the pinned listing is the tail of the
    // unpinned one, so no revision is reordered or invented.
    let full = history_json(directory.path(), relation_id, "song");
    assert!(
        full.len() > pinned.len(),
        "the newer import adds a revision the pin excludes"
    );
    assert_eq!(full[full.len() - pinned.len()..], pinned[..]);

    // `--at HEAD` resolves once to the same commit the unpinned read uses.
    let at_head = run_history(
        directory.path(),
        &[&relation, "song", "--at", "HEAD", "--format", "json"],
    );
    assert_eq!(at_head.status.code(), Some(0), "--at HEAD must resolve");
    let at_head: Vec<serde_json::Value> = serde_json::from_slice(&at_head.stdout).unwrap();
    assert_eq!(at_head, full);

    // An unresolvable selector is refused instead of silently reading HEAD.
    let missing = run_history(directory.path(), &[&relation, "song", "--at", "no-such-ref"]);
    assert_history_refused(&missing, "history --at with an unknown selector");
    drop(directory);
}

/// A format-1/2 pin is a read-only compatibility input with no native
/// `.orna/store`, so `--at` must refuse it with an accurate reason instead of
/// reporting the format-3 store seam's failure or answering from the workspace.
#[test]
fn history_at_refuses_a_legacy_snapshot_it_cannot_walk() {
    let directory = TempDir::new().unwrap();
    let root = directory.path();
    let setup: &[&[&str]] = &[
        &["init", "--quiet"],
        &["config", "user.email", "kieran@drewett.dev"],
        &["config", "user.name", "kierandrewett"],
        &["config", "commit.gpgsign", "false"],
    ];
    for words in setup {
        git(root, words);
    }
    std::fs::create_dir_all(root.join(".orna")).unwrap();
    std::fs::write(root.join("main.orna"), b"module main;\n").unwrap();
    std::fs::write(root.join(".orna/format.orna"), b"format 1\n").unwrap();
    git(root, &["add", "--all"]);
    git(root, &["commit", "--quiet", "-m", "legacy format fixture"]);
    let legacy = git_output(root, &["rev-parse", "HEAD"], None);
    let legacy = String::from_utf8(legacy).unwrap().trim().to_owned();

    // The format-1 commit is reachable and pinnable, so the refusal is about
    // the snapshot's format and not about resolving the selector.
    let relation = "43434343434343434343434343434343";
    let refused = run_history(root, &[relation, "song", "--at", &legacy]);
    assert_history_refused(&refused, "history --at naming a format-1 commit");
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(
        stderr.contains("legacy format-1/2"),
        "the refusal names the pinned format: {stderr}"
    );
    drop(directory);
}

/// Deletes one committed media row through an admitted request, the same path
/// the import uses for inserts.
async fn delete_media(
    state: &RuntimeState,
    writer: orna_runtime_v1::WriterLease,
    key: &str,
    ordinal: u8,
) {
    let request_identity = RequestIdentity {
        session_id: [ordinal; 16],
        request_id: [ordinal + 1; 16],
    };
    let fingerprint = [ordinal + 2; 32];
    let (_, admission) = state
        .reserve_request_with_admission(request_identity, fingerprint)
        .await
        .unwrap();
    state
        .start_request_with_owner_and_admission(
            request_identity,
            fingerprint,
            writer,
            admission.expect("new request returns its admission capability"),
        )
        .await
        .unwrap();
    let context = state.begin_activation().await.unwrap();
    // A row mutation without a value deletes the committed row.
    let mutation =
        TableMutation::new([ordinal + 3; 16], "media", key.as_bytes().to_vec(), None).unwrap();
    let committed = state
        .commit_table_request_activation(
            writer,
            request_identity,
            fingerprint,
            &context,
            &[mutation],
            [ordinal + 4; 32],
            TerminalOutcome::new(vec![ordinal + 5]).unwrap(),
            &orna_runtime_v1::NoFault,
        )
        .await
        .unwrap();
    assert_eq!(committed.request.state, RequestState::Completed);
}

async fn filter_media(state: &RuntimeState, filter: &BlobMetadataFilter) -> Vec<Vec<u8>> {
    state
        .committed_table_rows("media")
        .await
        .unwrap()
        .into_iter()
        .filter(|(_, row)| filter.matches(&decode_rov3_blob_metadata(row).unwrap()))
        .map(|(key, _)| key)
        .collect()
}

/// Lists every committed media row as payload-free metadata. Rows are decoded
/// as Blob references only; no read or hydration call is made.
async fn list_media(state: &RuntimeState) -> Vec<MediaListing> {
    state
        .committed_table_rows("media")
        .await
        .unwrap()
        .into_iter()
        .map(|(key, row)| {
            let metadata = decode_rov3_blob_metadata(&row).unwrap();
            MediaListing {
                key,
                media_type: metadata.media_type().to_owned(),
                suffix: metadata.suffix().map(str::to_owned),
                length: metadata.length(),
                sha256: metadata.sha256(),
                hydrated: metadata.is_hydrated(),
            }
        })
        .collect()
}

#[tokio::test]
async fn listing_projects_suffix_column_for_song_and_image_rows() {
    let (_directory, repository, relation_id) = empty_format3_repository();
    let source = TempDir::new().unwrap();
    for name in ["tone.wav", "pixel.png"] {
        std::fs::copy(Path::new(MEDIA_FIXTURES).join(name), source.path().join(name)).unwrap();
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

    let song = import_expression(PROJECTION_SONG_IMPORT_FIXTURE, source.path());
    import_media(&repository, &state, writer, &mut bindings, &song, "song", 0x70, true).await;
    let image = import_expression(IMAGE_IMPORT_FIXTURE, source.path());
    import_media(&repository, &state, writer, &mut bindings, &image, "image", 0x80, true).await;

    let listing = list_media(&state).await;
    let suffixes: Vec<(Vec<u8>, Option<String>)> = listing
        .into_iter()
        .map(|row| (row.key, row.suffix))
        .collect();
    assert_eq!(
        suffixes,
        vec![
            (b"image".to_vec(), Some("png".to_owned())),
            (b"song".to_vec(), Some("wav".to_owned())),
        ]
    );
}

#[tokio::test]
async fn negated_media_type_filter_excludes_only_the_named_type() {
    let (_directory, repository, relation_id) = empty_format3_repository();
    let source = TempDir::new().unwrap();
    for name in ["tone.wav", "pixel.png"] {
        std::fs::copy(Path::new(MEDIA_FIXTURES).join(name), source.path().join(name)).unwrap();
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

    let song = import_expression(NEGATION_SONG_IMPORT_FIXTURE, source.path());
    import_media(&repository, &state, writer, &mut bindings, &song, "song", 0x70, true).await;
    let image = import_expression(IMAGE_IMPORT_FIXTURE, source.path());
    import_media(&repository, &state, writer, &mut bindings, &image, "image", 0x80, true).await;

    let not_png = BlobMetadataFilter::new().without_media_type("image/png");
    assert_eq!(filter_media(&state, &not_png).await, vec![b"song".to_vec()]);
    let not_wav = BlobMetadataFilter::new().without_media_type("audio/wav");
    assert_eq!(filter_media(&state, &not_wav).await, vec![b"image".to_vec()]);
    let neither = BlobMetadataFilter::new()
        .without_media_type("image/png")
        .without_media_type("audio/wav");
    assert!(filter_media(&state, &neither).await.is_empty());
}

#[tokio::test]
async fn sort_orders_mixed_media_types_by_type_then_length() {
    let (_directory, repository, relation_id) = empty_format3_repository();
    let source = TempDir::new().unwrap();
    for name in ["tone.wav", "pixel.png"] {
        std::fs::copy(Path::new(MEDIA_FIXTURES).join(name), source.path().join(name)).unwrap();
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

    // Committed in reverse of the expected sort order.
    let image = import_expression(SORT_IMAGE_IMPORT_FIXTURE, source.path());
    import_media(&repository, &state, writer, &mut bindings, &image, "image", 0x80, true).await;
    let song = import_expression(SONG_IMPORT_FIXTURE, source.path());
    import_media(&repository, &state, writer, &mut bindings, &song, "song", 0x70, true).await;

    let mut metadata: Vec<_> = state
        .committed_table_rows("media")
        .await
        .unwrap()
        .into_iter()
        .map(|(_, row)| decode_rov3_blob_metadata(&row).unwrap())
        .collect();
    metadata.sort_by(compare_blob_metadata);
    let order: Vec<(&str, u64)> = metadata
        .iter()
        .map(|row| (row.media_type(), row.length()))
        .collect();
    // audio/wav sorts before image/png; each type's rows sort by length.
    assert_eq!(order, vec![("audio/wav", 4044), ("image/png", 73)]);
}

/// Revision walk for the committed song row, through the OGS-1 graph.
fn song_revisions_limited_to(
    repository: &Repository,
    relation_id: [u8; 16],
    max_commits: usize,
) -> Vec<RowRevision> {
    let format = repository.open_format_context().unwrap();
    let row_map = format.load_row_map(relation_id).unwrap();
    let graph = format.open_native_graph(&row_map).unwrap();
    let scope = graph.open_read_scope().unwrap();
    let rows = graph
        .range_rows(&KeyRange::new(None, None, 16).unwrap(), &scope)
        .unwrap();
    let song = rows
        .iter()
        .find(|row| {
            row.key() == &TypedKey::Bytes(b"song".to_vec())
                || row.key() == &TypedKey::Text("song".to_owned())
        })
        .expect("the song row is committed");
    graph.list_row_revisions(song, "HEAD", max_commits, &scope).unwrap()
}

/// QUERY: `orna query RELATION` answers the Blob metadata of committed media
/// rows through the real CLI over the real graph, and the read seam it reports
/// charges zero media payload bytes for the whole listing.
#[tokio::test]
async fn query_lists_committed_media_metadata_and_charges_no_payload_bytes() {
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

    let song = import_expression(SONG_IMPORT_FIXTURE, source.path());
    import_media(
        &repository,
        &state,
        writer,
        &mut bindings,
        &song,
        "song",
        0x70,
        true,
    )
    .await;
    let image = import_expression(IMAGE_IMPORT_FIXTURE, source.path());
    import_media(
        &repository,
        &state,
        writer,
        &mut bindings,
        &image,
        "image",
        0x80,
        true,
    )
    .await;
    drop(bindings);
    drop(state);

    let relation = relation_hex(relation_id);
    let output = run_query(directory.path(), &[&relation, "--format", "json"]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "query failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        report["media_payload_bytes_read"].as_u64(),
        Some(0),
        "a metadata listing must charge no media payload byte"
    );
    assert!(
        report["native_objects_read"].as_u64().unwrap() > 0,
        "the listing still walked committed graph objects, so the zero above is not vacuous"
    );
    let listings = report["listings"].as_array().unwrap();
    assert_eq!(
        listings.len(),
        2,
        "both committed rows carry one Blob field: {report}"
    );

    // Every committed payload is described without being fetched: the listed
    // length and digest are the real file's, taken from the row itself.
    let mut listed_length = 0_u64;
    for (key, file) in [("image", "pixel.png"), ("song", "tone.wav")] {
        let listing = listings
            .iter()
            .find(|listing| listing["key"] == key)
            .unwrap_or_else(|| panic!("the {key} row is listed"));
        let bytes = std::fs::read(Path::new(MEDIA_FIXTURES).join(file)).unwrap();
        assert_eq!(listing["length"].as_u64(), Some(bytes.len() as u64));
        assert_eq!(
            listing["sha256"].as_str().unwrap(),
            hex(Sha256::digest(&bytes).as_slice())
        );
        assert_eq!(
            listing["hydrated"].as_bool(),
            Some(false),
            "{key}: listing must not hydrate"
        );
        assert_eq!(
            listing["field"].as_u64(),
            Some(0),
            "{key}: the Blob is the row's one field"
        );
        listed_length += listing["length"].as_u64().unwrap();
    }
    let on_disk: u64 = ["tone.wav", "pixel.png"]
        .iter()
        .map(|file| std::fs::metadata(Path::new(MEDIA_FIXTURES).join(file)).unwrap().len())
        .sum();
    assert_eq!(
        listed_length, on_disk,
        "the listing describes every committed payload byte without reading one"
    );

    // The point read answers one named row with the metadata the listing gave.
    let song_json = query_json(directory.path(), &[&relation, "--key", "song"]);
    assert_eq!(song_json["listings"].as_array().unwrap().len(), 1);
    assert_eq!(song_json["listings"][0]["media_type"], "audio/wav");
    assert_eq!(
        song_json["listings"][0]["sha256"],
        listings.iter().find(|listing| listing["key"] == "song").unwrap()["sha256"]
    );
    assert_eq!(song_json["media_payload_bytes_read"], 0);

    // `--field` names one tuple position, so a row whose Blob sits at field 0
    // is not reported at a field it does not have.
    let other_field = query_json(directory.path(), &[&relation, "--field", "1"]);
    assert!(other_field["listings"].as_array().unwrap().is_empty());

    // `--limit` bounds the listed window exactly as the graph range does.
    let limited = query_json(directory.path(), &[&relation, "--limit", "1"]);
    assert_eq!(limited["listings"].as_array().unwrap().len(), 1);

    // The human listing states the same measurement on its summary line.
    let human = run_query(directory.path(), &[&relation]);
    assert_eq!(human.status.code(), Some(0));
    let text = String::from_utf8(human.stdout).unwrap();
    assert!(
        text.contains("media payload bytes read: 0"),
        "the human summary must report the payload it avoided: {text}"
    );

    // A key that is not committed is refused, not answered with an empty page.
    let absent = run_query(directory.path(), &[&relation, "--key", "not-committed"]);
    assert_eq!(absent.status.code(), Some(1));
    assert!(absent.stdout.is_empty());
    drop(directory);
}

/// QUERY `--at`: a listing read at a pinned commit returns that commit's own
/// rows and not the workspace `HEAD`'s, and the pinned read charges no media
/// payload byte.
#[tokio::test]
async fn query_at_pins_the_named_snapshot_and_reads_no_media_payload() {
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

    // The song is published first, so the commit it lands in is the snapshot
    // `--at` has to read. The image import below moves `HEAD` past it.
    let song = import_expression(PINNED_SONG_IMPORT_FIXTURE, source.path());
    import_media(
        &repository,
        &state,
        writer,
        &mut bindings,
        &song,
        "song",
        0x70,
        true,
    )
    .await;
    let past = String::from_utf8(git_output(directory.path(), &["rev-parse", "HEAD"], None))
        .unwrap()
        .trim()
        .to_owned();
    let image = import_expression(IMAGE_IMPORT_FIXTURE, source.path());
    import_media(
        &repository,
        &state,
        writer,
        &mut bindings,
        &image,
        "image",
        0x80,
        true,
    )
    .await;
    drop(bindings);
    drop(state);
    let present = String::from_utf8(git_output(directory.path(), &["rev-parse", "HEAD"], None))
        .unwrap()
        .trim()
        .to_owned();
    assert_ne!(past, present, "the image import advanced HEAD");

    // The unpinned listing is the control: pinning must remove exactly the row
    // the newer commit added, and nothing else.
    let relation = relation_hex(relation_id);
    let head_listing = query_json(directory.path(), &[&relation]);
    assert_eq!(
        listing_keys(&head_listing),
        vec!["image", "song"],
        "both committed rows at HEAD: {head_listing}"
    );

    let pinned = query_json(directory.path(), &[&relation, "--at", &past]);
    assert_eq!(
        listing_keys(&pinned),
        vec!["song"],
        "the pinned commit carries the song and not the later image: {pinned}"
    );
    assert_eq!(
        pinned["media_payload_bytes_read"].as_u64(),
        Some(0),
        "the pinned listing must charge no media payload byte"
    );
    assert!(
        pinned["native_objects_read"].as_u64().unwrap() > 0,
        "the pinned listing still walked that commit's graph, so its zero is not vacuous"
    );
    // The pinned row is the same row, described from the older commit alone.
    let song_at_head = head_listing["listings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|listing| listing["key"] == "song")
        .expect("the song row is listed at HEAD");
    assert_eq!(pinned["listings"][0]["sha256"], song_at_head["sha256"]);
    assert_eq!(pinned["listings"][0]["length"], song_at_head["length"]);

    // `--at HEAD` resolves once to the commit the unpinned read uses, and
    // naming the current commit is the same read again.
    let at_head = query_json(directory.path(), &[&relation, "--at", "HEAD"]);
    assert_eq!(at_head["listings"], head_listing["listings"]);
    let at_present = query_json(directory.path(), &[&relation, "--at", &present]);
    assert_eq!(listing_keys(&at_present), vec!["image", "song"]);

    // Pinning composes with the rest of the query: a predicate over the pinned
    // rows narrows them and still fetches no payload.
    let pinned_kind = query_json(
        directory.path(),
        &[&relation, "--at", &past, "--kind", "audio/wav"],
    );
    assert_eq!(listing_keys(&pinned_kind), vec!["song"]);
    assert_eq!(pinned_kind["media_payload_bytes_read"].as_u64(), Some(0));

    // An unresolvable selector is refused instead of silently reading HEAD.
    let missing = run_query(directory.path(), &[&relation, "--at", "no-such-ref"]);
    assert_eq!(
        missing.status.code(),
        Some(1),
        "an unknown commit is a target error"
    );
    assert!(
        missing.stdout.is_empty(),
        "a refused pin prints no listing: {}",
        String::from_utf8_lossy(&missing.stdout)
    );
    let stderr = String::from_utf8_lossy(&missing.stderr);
    assert!(
        stderr.contains("Snapshot could not be resolved"),
        "the refusal is typed and names the failed resolution: {stderr}"
    );

    // The human listing reports the pinned read's own measurement.
    let human = run_query(directory.path(), &[&relation, "--at", &past]);
    assert_eq!(human.status.code(), Some(0));
    let text = String::from_utf8(human.stdout).unwrap();
    assert!(
        text.contains("media payload bytes read: 0"),
        "the pinned summary must report the payload it avoided: {text}"
    );
    drop(directory);
}

/// A format-1/2 pin is a read-only compatibility input with no native
/// `.orna/store`, so `query --at` must refuse it with an accurate reason
/// instead of reporting the format-3 store seam's failure or answering from
/// the workspace.
#[test]
fn query_at_refuses_a_legacy_snapshot_it_cannot_read() {
    let directory = TempDir::new().unwrap();
    let root = directory.path();
    let setup: &[&[&str]] = &[
        &["init", "--quiet"],
        &["config", "user.email", "kieran@drewett.dev"],
        &["config", "user.name", "kierandrewett"],
        &["config", "commit.gpgsign", "false"],
    ];
    for words in setup {
        git(root, words);
    }
    std::fs::create_dir_all(root.join(".orna")).unwrap();
    std::fs::write(root.join("main.orna"), b"module main;\n").unwrap();
    std::fs::write(root.join(".orna/format.orna"), b"format 1\n").unwrap();
    git(root, &["add", "--all"]);
    git(root, &["commit", "--quiet", "-m", "legacy format fixture"]);
    let legacy = git_output(root, &["rev-parse", "HEAD"], None);
    let legacy = String::from_utf8(legacy).unwrap().trim().to_owned();

    // The format-1 commit is reachable and pinnable, so the refusal is about
    // the snapshot's format and not about resolving the selector.
    let relation = "43434343434343434343434343434343";
    let refused = run_query(root, &[relation, "--at", &legacy]);
    assert_eq!(refused.status.code(), Some(1));
    assert!(refused.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(
        stderr.contains("legacy format-1/2"),
        "the refusal names the pinned format: {stderr}"
    );
    drop(directory);
}

/// The `key` of every listing, in the order the query printed them.
fn listing_keys(report: &serde_json::Value) -> Vec<&str> {
    report["listings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|listing| listing["key"].as_str().unwrap())
        .collect()
}

/// Runs `orna query` and parses its `--format json` report.
fn query_json(directory: &Path, arguments: &[&str]) -> serde_json::Value {
    let mut words = arguments.to_vec();
    words.extend_from_slice(&["--format", "json"]);
    let output = run_query(directory, &words);
    assert_eq!(
        output.status.code(),
        Some(0),
        "query failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[tokio::test]
async fn zero_limit_history_walk_returns_no_revisions() {
    let (directory, repository, relation_id) = empty_format3_repository();
    let source = TempDir::new().unwrap();
    for name in ["tone.wav", "pixel.png"] {
        std::fs::copy(Path::new(MEDIA_FIXTURES).join(name), source.path().join(name)).unwrap();
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

    let song = import_expression(LIMIT_ZERO_SONG_IMPORT_FIXTURE, source.path());
    import_media(&repository, &state, writer, &mut bindings, &song, "song", 0x70, true).await;
    let image = import_expression(IMAGE_IMPORT_FIXTURE, source.path());
    import_media(&repository, &state, writer, &mut bindings, &image, "image", 0x80, true).await;

    // Control: a limit of one returns the head revision, so the walk is live.
    assert_eq!(song_revisions_limited_to(&repository, relation_id, 1).len(), 1);
    // Zero limit returns an empty listing, not an error or the full walk.
    assert!(song_revisions_limited_to(&repository, relation_id, 0).is_empty());
    drop(directory);
}

/// QUERY PREDICATE: an annotation predicate over the stored coordinate
/// (kind/suffix/length) is evaluated from the rows' own annotations, the plan
/// reports the coordinates it used, and the whole evaluation charges zero
/// media payload bytes.
#[tokio::test]
async fn query_predicate_over_annotation_coordinates_reads_no_media_payload() {
    let (directory, repository, relation_id) = empty_format3_repository();
    let source = TempDir::new().unwrap();
    for name in ["tone.wav", "pixel.png"] {
        std::fs::copy(Path::new(MEDIA_FIXTURES).join(name), source.path().join(name)).unwrap();
    }
    // The annotated row carries the same payload bytes as the song row under a
    // different file name. Content identity therefore cannot be what
    // discriminates them: only the authored annotation coordinate can.
    std::fs::copy(
        Path::new(MEDIA_FIXTURES).join("tone.wav"),
        source.path().join("note.wav"),
    )
    .unwrap();

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

    // Three real rows: two WAVs (one of them carrying the authored "note"
    // suffix) and one PNG, so the predicate has to discriminate on more than
    // the media type.
    let song = import_expression(SONG_IMPORT_FIXTURE, source.path());
    import_media(&repository, &state, writer, &mut bindings, &song, "song", 0x70, true).await;
    let annotated = import_expression(ANNOTATED_SONG_IMPORT_FIXTURE, source.path());
    import_media(
        &repository,
        &state,
        writer,
        &mut bindings,
        &annotated,
        "note",
        0x80,
        true,
    )
    .await;
    let image = import_expression(IMAGE_IMPORT_FIXTURE, source.path());
    import_media(&repository, &state, writer, &mut bindings, &image, "image", 0x90, true).await;
    drop(bindings);
    drop(state);

    let relation = relation_hex(relation_id);
    let payload_bytes: u64 = ["tone.wav", "pixel.png", "note.wav"]
        .iter()
        .map(|name| std::fs::metadata(source.path().join(name)).unwrap().len())
        .sum();

    // An unfiltered listing is the control: the predicate below must not add a
    // single payload byte on top of it.
    let all = query_json(directory.path(), &[&relation]);
    assert_eq!(all["blobs"].as_u64(), Some(3), "three committed rows: {all}");
    assert_eq!(all["media_payload_bytes_read"], 0);

    // `--kind` selects on the canonical media type the annotation binds.
    let wavs = query_json(directory.path(), &[&relation, "--kind", "audio/wav"]);
    let keys: Vec<&str> = wavs["listings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|listing| listing["key"].as_str().unwrap())
        .collect();
    assert_eq!(keys, vec!["note", "song"], "both WAV rows and no PNG: {wavs}");
    assert_eq!(
        wavs["media_payload_bytes_read"], 0,
        "the kind predicate fetched media bytes"
    );
    assert!(
        wavs["native_objects_read"].as_u64().unwrap() > 0,
        "the filtered listing still walked the committed graph, so its zero is not vacuous"
    );

    // The plan names the coordinates that answered it, and names the ones a
    // stored annotation does not bind so their absence is explicit.
    let plan = &wavs["plan"];
    assert_eq!(plan["kind"], "annotation-predicate");
    assert_eq!(
        plan["coordinates"],
        serde_json::json!(["kind", "suffix", "length"])
    );
    assert_eq!(
        plan["decode_required_coordinates"],
        serde_json::json!(["duration", "dimensions"])
    );
    assert_eq!(plan["candidates"].as_u64(), Some(3), "all rows: {plan}");
    assert_eq!(plan["matched"].as_u64(), Some(2), "two WAVs: {plan}");
    assert_eq!(plan["payload_fetch"], "none");

    // `--suffix` discriminates the two WAVs by their authored annotation, and
    // an unauthored hint selects nothing rather than falling back to kind.
    let annotated_only = query_json(directory.path(), &[&relation, "--suffix", "note"]);
    let annotated_keys: Vec<&str> = annotated_only["listings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|listing| listing["key"].as_str().unwrap())
        .collect();
    assert_eq!(annotated_keys, vec!["note"], "only the annotated row");
    assert_eq!(annotated_only["listings"][0]["suffix"], "note");
    assert_eq!(annotated_only["media_payload_bytes_read"], 0);

    let absent_suffix = query_json(directory.path(), &[&relation, "--suffix", "absent"]);
    assert!(
        absent_suffix["listings"].as_array().unwrap().is_empty(),
        "an unauthored suffix selects nothing: {absent_suffix}"
    );

    // `--min-length` and `--max-length` bound the length coordinate: the
    // filtered set is exactly the unfiltered set's rows that satisfy the
    // bound, so the predicate neither drops a matching row nor keeps one it
    // should not.
    let all_lengths: Vec<u64> = all["listings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|listing| listing["length"].as_u64().unwrap())
        .collect();
    let bound = *all_lengths.iter().min().unwrap() + 1;
    let at_least = query_json(
        directory.path(),
        &[&relation, "--min-length", &bound.to_string()],
    );
    assert_eq!(
        at_least["blobs"].as_u64(),
        Some(all_lengths.iter().filter(|length| **length >= bound).count() as u64),
        "min-length keeps exactly the rows at or above the bound"
    );
    let at_most = query_json(
        directory.path(),
        &[&relation, "--max-length", &(bound - 1).to_string()],
    );
    assert_eq!(
        at_most["blobs"].as_u64(),
        Some(all_lengths.iter().filter(|length| **length < bound).count() as u64),
        "max-length keeps exactly the rows below the bound"
    );
    assert_eq!(at_least["media_payload_bytes_read"], 0);
    assert_eq!(at_most["media_payload_bytes_read"], 0);

    // `--not-kind` excludes the named annotation from the same coordinate.
    let not_png = query_json(directory.path(), &[&relation, "--not-kind", "image/png"]);
    assert_eq!(not_png["blobs"].as_u64(), Some(2), "the PNG is excluded");

    // Combining coordinates selects on all of them, and a conjunction that no
    // row satisfies is an empty result rather than an unfiltered one.
    let narrow = query_json(
        directory.path(),
        &[&relation, "--kind", "audio/wav", "--suffix", "note"],
    );
    assert_eq!(narrow["blobs"].as_u64(), Some(1), "one row: {narrow}");
    let contradictory = query_json(
        directory.path(),
        &[&relation, "--kind", "audio/wav", "--not-kind", "audio/wav"],
    );
    assert!(contradictory["listings"].as_array().unwrap().is_empty());

    // Every predicate above answered from annotations, so the whole run
    // fetched nothing: the summed payload is the figure the plan avoided.
    assert_eq!(
        wavs["media_payload_bytes_read"], 0,
        "a predicate over {payload_bytes} payload bytes read none of them"
    );

    // The human listing states the plan and the payload it avoided.
    let human = run_query(directory.path(), &[&relation, "--kind", "audio/wav"]);
    assert_eq!(human.status.code(), Some(0));
    let text = String::from_utf8(human.stdout).unwrap();
    assert!(
        text.contains("media payload bytes read: 0"),
        "the human summary must report the payload it avoided: {text}"
    );
    assert!(
        text.contains("plan: annotation predicate")
            && text.contains("coordinates kind,suffix,length")
            && text.contains("decode-required coordinates duration,dimensions"),
        "the human plan must name the coordinates it used: {text}"
    );
    assert!(text.contains("matched 2"), "the plan states the match count: {text}");

    // A flag that names no coordinate the annotation binds is refused rather
    // than silently ignored.
    let unknown = run_query(directory.path(), &[&relation, "--dimensions", "100x100"]);
    assert_eq!(unknown.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&unknown.stderr).contains("--dimensions"),
        "an unknown predicate flag is named in the refusal"
    );
    drop(directory);
}
