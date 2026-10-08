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
const SORT_IMAGE_IMPORT_FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/media/import-image-sort.orna"
);
const NEGATION_SONG_IMPORT_FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/media/import-song-negation.orna"
);
const PROJECTION_SONG_IMPORT_FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/media/import-song-projection.orna"
);
const NEGATIVE_LIMIT_SONG_IMPORT_FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/media/import-song-negative-limit.orna"
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
    import_media(&state, writer, &mut bindings, &edit, "song", 0x90, false).await;
    let edited = list_media(&state).await;
    let song = edited.iter().find(|row| row.key == b"song").unwrap();
    assert_eq!(song.media_type, "image/png");
    assert_eq!(song.length, 73);
    assert!(!song.hydrated, "edited listing must not hydrate the song");

    // Re-import: committing the same .orna fixture again leaves the listing
    // exactly as the edit left it. Only committed rows are compared.
    import_media(&state, writer, &mut bindings, &edit, "song", 0xa0, false).await;
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
    import_media(&state, writer, &mut bindings, &song, "song", 0x70, true).await;
    let paged = import_expression(PAGED_SONG_IMPORT_FIXTURE, source.path());
    import_media(&state, writer, &mut bindings, &paged, "song-again", 0x80, true).await;

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
        let revisions = graph.list_row_revisions(row, 64, &scope).unwrap();
        assert_eq!(revisions[0].commit().to_hex(), head, "{key}: head first");
        // One revision per page: every page is exactly the next commit.
        let pages = revision_pages(
            |max| graph.list_row_revisions(row, max, &scope).unwrap(),
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
    import_media(&state, writer, &mut bindings, &song, "song", 0x70, true).await;
    let authored = import_expression(AUTHORED_SONG_IMPORT_FIXTURE, source.path());
    import_media(&state, writer, &mut bindings, &authored, "song-authored", 0x80, true).await;

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
        let revisions = graph.list_row_revisions(row, 64, &scope).unwrap();
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
    import_media(&state, writer, &mut bindings, &song, "song", 0x70, true).await;
    let later = import_expression(SINCE_SONG_IMPORT_FIXTURE, source.path());
    import_media(&state, writer, &mut bindings, &later, "song-later", 0x80, true).await;
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
        let revisions = graph.list_row_revisions(row, 64, &scope).unwrap();
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
        import_media(&state, writer, &mut bindings, &expression, key, ordinal, true).await;
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
        let full = graph.list_row_revisions(row, 64, &scope).unwrap();
        // `orna history --limit N` walks only N commits, so each bounded walk
        // must equal the first N revisions of the full walk.
        for n in 1..=full.len() {
            let bounded = graph.list_row_revisions(row, n, &scope).unwrap();
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

    let revisions = graph.list_row_revisions(song, 64, &scope).unwrap();
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
        |max| graph.list_row_revisions(song, max, &scope).unwrap(),
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


/// The 16-byte relation id as the 32-digit hex the `orna history` CLI takes.
fn relation_hex(relation_id: [u8; 16]) -> String {
    relation_id.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Runs `orna history` with `arguments` as a child process inside the repository
/// and returns its raw output, whatever the exit status.
fn run_history(directory: &Path, arguments: &[&str]) -> std::process::Output {
    std::process::Command::new(env!("CARGO_BIN_EXE_orna-cli-v1"))
        .current_dir(directory)
        .arg("history")
        .args(arguments)
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
        import_media(&state, writer, &mut bindings, &expression, key, ordinal, true).await;
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
    import_media(&state, writer, &mut bindings, &song, "song", 0x70, true).await;
    let image = import_expression(IMAGE_JSON_IMPORT_FIXTURE, source.path());
    import_media(&state, writer, &mut bindings, &image, "image", 0x80, true).await;
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
        assert_eq!(keys, ["author", "commit", "present", "tree"]);
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
    import_media(&state, writer, &mut bindings, &fixture, "song", 0x70, true).await;
    drop(bindings);
    drop(state);

    let relation = relation_hex(relation_id);
    // Each value is outside 1..=4096 or not a number; `-1` is the negative case.
    for value in ["-1", "0", "4097", "x"] {
        let output = run_history(directory.path(), &[&relation, "song", "--limit", value]);
        assert_eq!(
            output.status.code(),
            Some(1),
            "--limit {value} must exit 1: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            output.stdout.is_empty(),
            "--limit {value} must list no revisions: {}",
            String::from_utf8_lossy(&output.stdout)
        );
    }
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
    import_media(&state, writer, &mut bindings, &song, "song", 0x70, true).await;
    let image = import_expression(IMAGE_IMPORT_FIXTURE, source.path());
    import_media(&state, writer, &mut bindings, &image, "image", 0x80, true).await;

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
    import_media(&state, writer, &mut bindings, &song, "song", 0x70, true).await;
    let image = import_expression(IMAGE_IMPORT_FIXTURE, source.path());
    import_media(&state, writer, &mut bindings, &image, "image", 0x80, true).await;

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
    import_media(&state, writer, &mut bindings, &image, "image", 0x80, true).await;
    let song = import_expression(SONG_IMPORT_FIXTURE, source.path());
    import_media(&state, writer, &mut bindings, &song, "song", 0x70, true).await;

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
