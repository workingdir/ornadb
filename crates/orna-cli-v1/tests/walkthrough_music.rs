use std::{io::Write, path::Path, process::{Command, Output, Stdio}};

use orna_application_v1::{ApplicationAuthority, CommittedTableIdentity};
use orna_evaluator_v1::{Environment, Limits, SysHostBindingRegistry, evaluate_expression_ovb2_with_effects};
use orna_project_v1::{ProjectLoader, admit_source_row};
use orna_repository_v1::{GitCommitRef, Repository};
use orna_runtime_v1::{NoFault, RuntimeState, TableObjectId};
use orna_semantic_v1::Catalogue;
use orna_sys_v1::{EnvironmentProvider, FilesystemProvider};
use sha2::{Digest, Sha256};
use orna_value_v1::{AvailabilityState, BlobResolver, ContentReference, ContextValue, Error as ValueError, Format3Context, Format3Quota, GitHash, NativeOid, OwnerLifetime, Snapshot, ValueFormat};
use tempfile::TempDir;
#[path = "support/format3.rs"]
mod format3;
use format3::*;

const MUSIC_SOURCE: &str = "pub table music(id: Str) { title: Str, artist: Str, content: Blob, cover: Blob?, }\npub fn main() {}\n";

fn run(directory: &Path, arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_orna"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .args(arguments)
        .current_dir(directory)
        .output()
        .expect("orna CLI process")
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn run_repl(directory: &Path, input: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_orna"))
        .args(["--db", directory.to_str().unwrap(), "repl"])
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Orna REPL process");
    child.stdin.take().unwrap().write_all(format!("{input}\n:quit\n").as_bytes()).unwrap();
    child.wait_with_output().unwrap()
}
struct UnusedResolver;

impl BlobResolver for UnusedResolver {
    fn read_range(
        &self,
        _reference: &ContentReference,
        _offset: u64,
        _length: u64,
    ) -> orna_value_v1::Result<Vec<u8>> {
        Err(ValueError::ContentUnavailable)
    }
}

fn format3_context_for_database(commit: &GitCommitRef, database_id: [u8; 16]) -> Format3Context {
    let oid = commit.as_str().as_bytes().chunks_exact(2).map(|digits| {
        u8::from_str_radix(std::str::from_utf8(digits).unwrap(), 16).unwrap()
    }).collect::<Vec<_>>();
    let algorithm = match oid.len() {
        20 => GitHash::Sha1,
        32 => GitHash::Sha256,
        _ => panic!("unexpected Git object identifier width"),
    };
    let root = NativeOid::from_bytes(&vec![0x55; algorithm.oid_width()]).unwrap();
    Format3Context::from_persisted(
        database_id,
        Snapshot::Commit { database: database_id, algorithm, oid },
        algorithm,
        root.clone(),
        root,
        AvailabilityState::Available,
        OwnerLifetime::new([0x66; 16]),
        Format3Quota::new(1, 0).unwrap(),
        std::sync::Arc::new(UnusedResolver),
    ).unwrap()
}
const REPL_UPDATE_SOURCE: &str = "pub table music(id: Str) { title: Str, artist: Str, }\npub fn main() {}\n";

#[tokio::test]
async fn repl_update() {
    let (directory, repository, relation_id) = format3_repository_for_table(REPL_UPDATE_SOURCE, "music");
    let root = directory.path();
    let source_commit = repository.head().unwrap().unwrap();
    let loader = ProjectLoader::default();
    let project = loader.load_committed_snapshot(&repository, &source_commit).unwrap();
    let schema = loader.resolve_committed_source_table_schema(&repository, &source_commit, TableObjectId::new(relation_id)).unwrap();
    let value_context = format3_context_for_database(&source_commit, *schema.database_id());
    let row = admit_source_row(&project, "music/london.orna", r#"{ title: "Old title", artist: "Artist", }"#, &schema, &value_context, vec![], [0x72; 16]).unwrap();
    let (identity, initial_digest) = cli_runtime_identity(&repository);
    let state = RuntimeState::open(&repository, identity, initial_digest).await.unwrap();
    let writer = state.acquire_lease(identity.repository_id).await.unwrap();
    let context = state.begin_activation().await.unwrap();
    let authority = ApplicationAuthority::new(Catalogue::authoritative_core(), Limits::default());
    let application = authority.admit_module_with_committed_table_metadata("main.orna", REPL_UPDATE_SOURCE, "main", [CommittedTableIdentity::new("music", TableObjectId::new(relation_id))]).unwrap();
    let staged_source = authority.evaluate_staged_with_source_rows(&application, &Environment::new(), vec![row], &value_context).unwrap();
    let staged = staged_source.stage(&authority, &context).unwrap();
    state.commit_table_activation(writer, &context, staged.mutations(), staged.next_digest(), &NoFault).await.unwrap();
    drop(state);

    let published = run(root, &["publish"]);
    assert_eq!(published.status.code(), Some(0), "initial publish failed: {}", stderr(&published));
    git(root, &["reset", "--quiet", "--hard", "HEAD"]);
    assert!(!root.join("music/london.orna").exists(), "format-3 publication should keep rows in the store");

    let invalid = run_repl(root, r#"main.music.update("london", { title: 7 })"#);
    let invalid_output = format!("{}{}", String::from_utf8_lossy(&invalid.stdout), stderr(&invalid));
    assert!(invalid_output.contains("E2001"), "invalid update should be rejected: {invalid_output}");
    assert!(!root.join("music/london.orna").exists(), "invalid REPL update should not create a loose row file");
    let invalid_publish = run(root, &["publish"]);
    assert_eq!(invalid_publish.status.code(), Some(0), "invalid update left a publication: {}", stderr(&invalid_publish));
    let rows = committed_store_rows(&repository, relation_id);
    assert_eq!(rows.len(), 1);
    let fields = rows[0].value().encoded_fields().unwrap();
    let decoded = orna_value_v1::decode_canonical_storage_value(fields).unwrap();
    assert!(format!("{decoded:?}").contains("Old title"), "invalid update changed the committed row: {decoded:?}");
    assert!(!root.join("music/london.orna").exists(), "committed row should not need a loose source file");

    let updated = run_repl(root, r#"main.music.update("london", { title: "New title" })"#);
    let stdout = String::from_utf8_lossy(&updated.stdout);
    assert!(!stdout.contains("ORNA-EVAL-TABLE-SNAPSHOT"), "REPL update must resolve the committed row snapshot: {stdout} {}", stderr(&updated));
    assert_eq!(updated.status.code(), Some(0), "REPL update failed: {stdout} {}", stderr(&updated));
    assert!(!root.join("music/london.orna").exists(), "REPL update should remain store-only before publication");
    let published = run(root, &["publish"]);
    assert_eq!(published.status.code(), Some(0), "updated row publish failed: {}", stderr(&published));
    assert!(!root.join("music/london.orna").exists(), "format-3 publication should keep rows in the store");
    let rows = committed_store_rows(&repository, relation_id);
    assert_eq!(rows.len(), 1);
    let fields = rows[0].value().encoded_fields().unwrap();
    let decoded = orna_value_v1::decode_canonical_storage_value(fields).unwrap();
    assert!(format!("{decoded:?}").contains("New title"), "updated title was not persisted: {decoded:?}; REPL stdout={stdout}; stderr={}", stderr(&updated));
}
/// ORNA-ROW-004: committed media rows retain annotated Blob references while bytes remain graph objects.
#[tokio::test]
async fn music_walkthrough_captures_queries_publishes_and_exports_offline() {
    let (directory, repository, relation_id) = format3_repository_for_table(MUSIC_SOURCE, "music");
    let project_dir = directory.path().to_path_buf();
    let song_root = TempDir::new().unwrap();
    let original = include_bytes!("fixtures/media/tone.wav");
    std::fs::write(song_root.path().join("recording.mp3"), original).unwrap();
    let cover_bytes = include_bytes!("fixtures/media/pixel.png");
    std::fs::write(song_root.path().join("cover.png"), cover_bytes).unwrap();
    let commit = repository.head().unwrap().unwrap();
    let loader = ProjectLoader::default();
    let project = loader.load_committed_snapshot(&repository, &commit).unwrap();
    let schema = loader.resolve_committed_source_table_schema(
        &repository,
        &commit,
        TableObjectId::new(relation_id),
    ).unwrap();

    let (identity, initial_digest) = cli_runtime_identity(&repository);
    let state = RuntimeState::open(&repository, identity, initial_digest).await.unwrap();
    let writer = state.acquire_lease(identity.repository_id).await.unwrap();
    let context = state.begin_activation().await.unwrap();
    let capability = repository.capture_capability(relation_id).unwrap();
    let mut filesystem = FilesystemProvider::with_limits(1 << 20, 16).unwrap();
    filesystem.allow_root(song_root.path()).unwrap();
    let mut bindings = SysHostBindingRegistry::new(EnvironmentProvider::default())
        .with_filesystem_provider(filesystem)
        .with_repository_capture_capability(capability);
    let root = format!("{:?}", song_root.path().to_string_lossy().as_ref());
    let capture = format!(
        "sys.blob.annotate(sys.blob.capture_file({root}, \"recording.mp3\", 65536), \"audio/mpeg\")"
    );
    let captured = evaluate_expression_ovb2_with_effects(
        &capture,
        &Default::default(),
        Limits::default(),
        &mut bindings,
    ).unwrap();
    let content = bindings.accept_captured_blob_for_row(&captured).unwrap();
    let cover_capture = format!(
        "sys.blob.annotate(sys.blob.capture_file({root}, \"cover.png\", 65536), \"image/png\")"
    );
    let captured_cover = evaluate_expression_ovb2_with_effects(
        &cover_capture,
        &Default::default(),
        Limits::default(),
        &mut bindings,
    ).unwrap();
    let cover = bindings.accept_captured_blob_for_row(&captured_cover).unwrap();
    let value_context = format3_context_for_database(&commit, *schema.database_id());
    assert_eq!(content.transfer_record().database_id(), schema.database_id());
    ContextValue::decode_in_context(content.encoded_value(), ValueFormat::Rov3, &value_context)
        .expect("captured Blob must decode under the committed database context");
    // The original content-only path admits a typed null cover on its own row.
    let null_cover_row = admit_source_row(
        &project,
        "music/london.orna",
        r#"{ title: "Field recording", artist: "Kieran", cover: null, }"#,
        &schema,
        &value_context,
        vec![("content".to_owned(), content)],
        [0x32; 16],
    )
    .expect("a content-bound Music row admits its explicit null cover");
    assert_eq!(null_cover_row.mutation().key(), b"london");

    let cover_content_capture = evaluate_expression_ovb2_with_effects(
        &capture,
        &Default::default(),
        Limits::default(),
        &mut bindings,
    )
    .unwrap();
    let cover_content = bindings
        .accept_captured_blob_for_row(&cover_content_capture)
        .unwrap();
    let covered_row = admit_source_row(
        &project,
        "music/london-cover.orna",
        r#"{ title: "Live in London", artist: "Kieran", }"#,
        &schema,
        &value_context,
        vec![
            ("content".to_owned(), cover_content),
            ("cover".to_owned(), cover),
        ],
        [0x31; 16],
    )
    .expect("the Music row admits content and cover in their named Blob fields");
    assert_eq!(covered_row.mutation().key(), b"london-cover");

    // The admitted-row path resolves the field name against the committed descriptor,
    // checks its Blob type, and retains the field pin through staging and commit.
    let authority = ApplicationAuthority::new(Catalogue::authoritative_core(), Limits::default());
    let application = authority.admit_module_with_committed_table_metadata(
        "main.orna",
        MUSIC_SOURCE,
        "main",
        [CommittedTableIdentity::new("music", TableObjectId::new(relation_id))],
    ).unwrap();
    let staged_source = authority.evaluate_staged_with_source_rows(
        &application,
        &Environment::new(),
        vec![null_cover_row, covered_row],
        &value_context,
    ).unwrap();
    let staged = staged_source.stage(&authority, &context).unwrap();
    state.commit_table_activation(
        writer,
        &context,
        staged.mutations(),
        staged.next_digest(),
        &NoFault,
    ).await.unwrap();
    drop(state);

    let relation = hex(&relation_id);
    let published = run(&project_dir, &["publish"]);
    assert_eq!(published.status.code(), Some(0), "publish failed: {}", stderr(&published));
    let query = run(&project_dir, &["query", &relation, "--format", "json"]);
    assert_eq!(query.status.code(), Some(0), "query failed: {}", stderr(&query));
    let report: serde_json::Value = serde_json::from_slice(&query.stdout).unwrap();
    assert_eq!(report["media_payload_bytes_read"], 0);
    let listings = report["listings"].as_array().unwrap();
    assert_eq!(listings.len(), 3);
    let listing = listings.iter()
        .find(|item| item["key"] == "london" && item["media_type"] == "audio/mpeg")
        .unwrap_or_else(|| panic!("Music.content is listed with its stored audio annotation: {report}"));
    let cover_listing = listings.iter()
        .find(|item| item["key"] == "london-cover" && item["media_type"] == "image/png")
        .unwrap_or_else(|| panic!("Music.cover is listed with its stored image annotation: {report}"));
    assert_eq!(listing["media_type"], "audio/mpeg");
    assert_eq!(listing["length"], original.len() as u64);
    let expected_sha: [u8; 32] = Sha256::digest(original).into();
    assert_eq!(listing["sha256"], hex(&expected_sha));
    assert_eq!(cover_listing["length"], cover_bytes.len() as u64);
    let expected_cover_sha: [u8; 32] = Sha256::digest(cover_bytes).into();
    assert_eq!(cover_listing["sha256"], hex(&expected_cover_sha));

    // MIME-1 defaults to preferred suffixes; explicit hints alter metadata, not identity.
    let script = b"export const answer = 42;\n";
    std::fs::write(song_root.path().join("module.bin"), script).unwrap();
    let default_script_expr = format!(
        "sys.blob.annotate(sys.blob.capture_file({root}, \"module.bin\", 1024), \"text/javascript\")"
    );
    let default_script = evaluate_expression_ovb2_with_effects(
        &default_script_expr, &Default::default(), Limits::default(), &mut bindings,
    ).unwrap();
    let default_script_blob = default_script.blob().unwrap();
    assert_eq!(default_script_blob.suffix(), None);
    assert_eq!(default_script_blob.media_type(), "text/javascript");
    assert_eq!(orna_value_v1::selected_suffix(default_script_blob.media_type(), default_script_blob.suffix()).unwrap(), "js");
    let expected_script_sha: [u8; 32] = Sha256::digest(script).into();
    assert_eq!(default_script_blob.content_identity().sha256(), expected_script_sha);
    let module_expr = format!(
        "sys.blob.annotate(sys.blob.capture_file({root}, \"module.bin\", 1024), \"text/javascript\", \"mjs\")"
    );
    let module = evaluate_expression_ovb2_with_effects(
        &module_expr, &Default::default(), Limits::default(), &mut bindings,
    ).unwrap();
    let module_blob = module.blob().unwrap();
    assert_eq!(module_blob.media_type(), "text/javascript");
    assert_eq!(module_blob.suffix(), Some("mjs"));
    let expected_script_sha: [u8; 32] = Sha256::digest(script).into();
    assert_eq!(module_blob.content_identity().sha256(), expected_script_sha);
    assert_eq!(module_blob.content_identity(), default_script_blob.content_identity());

    let old_mpeg = evaluate_expression_ovb2_with_effects(
        &format!(
            "sys.blob.annotate(sys.blob.capture_file({root}, \"recording.mp3\", 65536), \"audio/mpeg\", \"mp2\")"
        ),
        &Default::default(),
        Limits::default(),
        &mut bindings,
    ).unwrap();
    let old_mpeg_blob = old_mpeg.blob().unwrap();
    assert_eq!(old_mpeg_blob.media_type(), "audio/mpeg");
    assert_eq!(old_mpeg_blob.suffix(), Some("mp2"));
    assert_eq!(orna_value_v1::selected_suffix(old_mpeg_blob.media_type(), None).unwrap(), "mp3");
    assert_eq!(old_mpeg_blob.content_identity().sha256(), expected_sha);
    drop(bindings);
    // Complete-copy export and restore use only the archive after source removal.
    let outside = TempDir::new().unwrap();
    let archive = outside.path().join("music-copy");
    let archive_s = archive.to_str().unwrap();
    let export = run(&project_dir, &["export", archive_s, "--at", "HEAD"]);
    assert_eq!(export.status.code(), Some(0), "export failed: {}", stderr(&export));
    drop(repository);
    drop(directory);
    assert!(!project_dir.exists());
    let restored = outside.path().join("restored");
    let restored_s = restored.to_str().unwrap();
    let restore = run(outside.path(), &["export", archive_s, "--restore", restored_s, "--worktree"]);
    assert_eq!(restore.status.code(), Some(0), "restore failed: {}", stderr(&restore));
    let restored_query = run(&restored, &["query", &relation, "--format", "json"]);
    assert_eq!(
        restored_query.status.code(),
        Some(0),
        "restored query failed: {}",
        stderr(&restored_query)
    );
    let restored_report: serde_json::Value =
        serde_json::from_slice(&restored_query.stdout).unwrap();
    assert_eq!(restored_report["media_payload_bytes_read"], 0);
    let restored_listings = restored_report["listings"].as_array().unwrap();
    let restored_cover_listing = restored_listings
        .iter()
        .find(|item| item["key"] == "london-cover" && item["media_type"] == "image/png")
        .expect("offline restore retains the Music.cover listing");
    assert_eq!(restored_cover_listing["length"], cover_bytes.len() as u64);
    assert_eq!(restored_cover_listing["sha256"], hex(&expected_cover_sha));
    assert!(String::from_utf8(git_output(&restored, &["remote"], None)).unwrap().trim().is_empty());
    let copied = Repository::discover(&restored).unwrap();
    let rows = committed_store_rows(&copied, relation_id);
    assert_eq!(rows.len(), 2);
    let content_only_row = rows
        .iter()
        .find(|row| row.key() == &orna_repository_v1::TypedKey::Text("london".to_owned()))
        .expect("content-only Music row survives offline restore");
    let content_only_blobs = content_only_row
        .blob_fields()
        .expect("the null-cover row retains its content Blob");
    assert_eq!(content_only_blobs.len(), 1);
    assert_eq!(content_only_blobs[0].1.sha256(), expected_sha);
    let covered_row = rows
        .iter()
        .find(|row| row.key() == &orna_repository_v1::TypedKey::Text("london-cover".to_owned()))
        .expect("Music row with a cover survives offline restore");
    let blob_fields = covered_row.blob_fields().expect("Music row carries Blob metadata");
    assert_eq!(blob_fields.len(), 2);
    assert!(blob_fields.iter().any(|(_, metadata)| metadata.sha256() == expected_sha));
    assert!(blob_fields.iter().any(|(_, metadata)| metadata.sha256() == expected_cover_sha));
    let format = copied.open_format_context().unwrap();
    let row_map = format.load_row_map(relation_id).unwrap();
    let graph = format.open_native_graph(&row_map).unwrap();
    let scope = graph.open_read_scope().unwrap();
    let row = graph.lookup_row(covered_row.key(), &scope).unwrap().unwrap();
    let dependencies = row.value().dependencies();
    let stored = dependencies.iter().find_map(|dependency| {
        if dependency.kind() != orna_repository_v1::NativeObjectKind::Tree {
            return None;
        }
        let stored = graph.read_stored_blob_value(&row, dependency.oid(), &scope).ok()?;
        (stored.annotation().media_type() == "audio/mpeg").then_some(stored)
    }).expect("Music.content retains its stored annotated reference");
    let actual = graph.read_blob_range(stored.reference(), 0..original.len() as u64, &scope).unwrap().bytes().to_vec();
    assert_eq!(actual, original);
    assert_eq!(Sha256::digest(actual).as_slice(), expected_sha);
    let stored_cover = dependencies.iter().find_map(|dependency| {
        if dependency.kind() != orna_repository_v1::NativeObjectKind::Tree {
            return None;
        }
        let stored = graph.read_stored_blob_value(&row, dependency.oid(), &scope).ok()?;
        (stored.annotation().media_type() == "image/png").then_some(stored)
    }).expect("Music.cover retains its stored annotated reference");
    let actual_cover = graph
        .read_blob_range(stored_cover.reference(), 0..cover_bytes.len() as u64, &scope)
        .unwrap()
        .bytes()
        .to_vec();
    assert_eq!(actual_cover, cover_bytes);
    assert_eq!(Sha256::digest(actual_cover).as_slice(), expected_cover_sha);

}

const LONDON_ROW: &str = "{ title: \"Live in London\", artist: \"Kieran\", }\n";

fn git_text(root: &Path, arguments: &[&str]) -> String {
    String::from_utf8(git_output(root, arguments, None)).expect("git prints UTF-8")
}

/// Asserts a refusal of the loose row file `path`: format admission's
/// mixed-format code, and the path itself in the message.
fn assert_loose_row_refused(output: &Output, path: &str) {
    let message = stderr(output);
    assert_eq!(output.status.code(), Some(1), "{message}");
    assert!(
        message.contains("error[ORNA-REPO-CONTEXT-004]"),
        "{message}"
    );
    assert!(
        message.contains(path),
        "the refusal names the row file: {message}"
    );
}

/// Rows have no Git paths in format 3: they live only in `.orna/store`
/// (ORNA-REPO-004, ORNA-META-001), so a loose row file in a format-3 index
/// would make the next commit mix formats (Gate A, "unknown/mixed formats fail
/// early"). In a database `orna init` created, `orna add --row` refuses the
/// row file by path and stages nothing, both before the first commit and after
/// it, and the same file staged with plain Git is refused by `orna commit`
/// without moving `HEAD`, a ref or the index (ORNA-REPO-CONTEXT-004).
#[test]
fn add_row_refuses_loose_row_in_format3_repository() {
    let directory = TempDir::new().unwrap();
    let root = directory.path().join("music");
    let init = run(directory.path(), &["init", root.to_str().unwrap()]);
    assert_eq!(
        init.status.code(),
        Some(0),
        "orna init failed: {}",
        stderr(&init)
    );
    git(&root, &["config", "user.email", "tests@example.invalid"]);
    git(&root, &["config", "user.name", "Orna tests"]);
    git(&root, &["config", "commit.gpgsign", "false"]);
    std::fs::write(root.join("main.orna"), MUSIC_SOURCE).unwrap();
    std::fs::create_dir_all(root.join("music")).unwrap();
    std::fs::write(root.join("music/london.orna"), LONDON_ROW).unwrap();

    // The branch is unborn, so the format is the one `orna init` recorded.
    let refused = run(&root, &["add", "--row", "music", "\"london\""]);
    assert_loose_row_refused(&refused, "music/london.orna");
    assert_eq!(
        git_text(&root, &["ls-files", "--stage"]),
        "",
        "nothing was staged"
    );

    git(&root, &["add", "main.orna", ".orna/database.orna"]);
    let committed = run(&root, &["commit", "-m", "Declare music"]);
    assert_eq!(
        committed.status.code(),
        Some(0),
        "first commit failed: {}",
        stderr(&committed)
    );
    let head = git_text(&root, &["rev-parse", "HEAD"]);
    let refs = git_text(&root, &["for-each-ref"]);
    let staged = git_text(&root, &["ls-files", "--stage"]);

    let refused = run(&root, &["add", "--row", "music", "\"london\""]);
    assert_loose_row_refused(&refused, "music/london.orna");
    assert_eq!(
        git_text(&root, &["ls-files", "--stage"]),
        staged,
        "nothing was staged"
    );

    // Plain Git can still stage the file; the commit refuses it.
    git(&root, &["add", "music/london.orna"]);
    let staged = git_text(&root, &["ls-files", "--stage"]);
    let refused = run(&root, &["commit", "-m", "Commit a loose row"]);
    assert_loose_row_refused(&refused, "music/london.orna");
    assert_eq!(
        git_text(&root, &["rev-parse", "HEAD"]),
        head,
        "no commit was made"
    );
    assert_eq!(git_text(&root, &["for-each-ref"]), refs, "no ref moved");
    assert_eq!(
        git_text(&root, &["ls-files", "--stage"]),
        staged,
        "the index is unchanged"
    );
}

/// A format-3 fixture that still commits a legacy loose row file, which
/// format-3 readers leave unread: changing that file is refused like adding
/// one. `orna add --row` refuses the edited file, and once plain Git stages it
/// `orna commit` refuses it too, so `HEAD` keeps the committed bytes
/// (ORNA-REPO-CONTEXT-004).
#[test]
fn commit_refuses_changed_loose_row_in_format3_repository() {
    let (directory, repository, _relation_id, _context) =
        format3::format3_repository_for_table_with_source_row(
            MUSIC_SOURCE,
            "music",
            "music/london.orna",
            LONDON_ROW,
        );
    let root = directory.path();
    std::fs::write(
        root.join("music/london.orna"),
        "{ title: \"Live in London — remastered\", artist: \"Kieran\", }\n",
    )
    .unwrap();
    let head = repository.head().unwrap().unwrap();
    let staged = git_text(root, &["ls-files", "--stage"]);

    let refused = run(root, &["add", "--row", "music", "\"london\""]);
    assert_loose_row_refused(&refused, "music/london.orna");
    assert_eq!(
        git_text(root, &["ls-files", "--stage"]),
        staged,
        "nothing was staged"
    );

    git(root, &["add", "music/london.orna"]);
    let refused = run(root, &["commit", "-m", "Retitle the recording"]);
    assert_loose_row_refused(&refused, "music/london.orna");
    assert_eq!(
        repository.head().unwrap(),
        Some(head.clone()),
        "no commit was made"
    );
    assert_eq!(
        git_text(
            root,
            &["show", &format!("{}:music/london.orna", head.as_str())]
        ),
        LONDON_ROW
    );
}
