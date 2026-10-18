#![cfg(target_os = "linux")]

use std::{fs, os::unix::fs::PermissionsExt, path::Path, process::{Command, Output}};

use orna_evaluator_v1::SysHostBindingRegistry;
use orna_repository_v1::{GitHashAlgorithm, NativeOid, Repository, TypedKey};
use orna_runtime_v1::{RuntimeState, RuntimeTableIdentity, TableObjectId};
use orna_sys_v1::{EnvironmentProvider, FilesystemProvider};
use orna_value_v1::decode_rov3_blob_metadata;
use sha2::Digest;
use tempfile::TempDir;

#[path = "support/format3.rs"]
mod format3;
#[path = "support/media_commit.rs"]
mod media_commit;

const MEDIA_FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/media");
const NOTES_SOURCE: &str = "pub table Music(id: Str) { title: Str, }\n";
const ROW_PATH: &str = "Music/london/data.orna";

fn editor(directory: &TempDir, name: &str, body: &str) -> std::path::PathBuf {
    let path = directory.path().join(name);
    fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn edit(repository: &TempDir, editor: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_orna-cli-v1"))
        .current_dir(repository.path())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("EDITOR", editor)
        .args(args)
        .output()
        .expect("production orna edit entrypoint runs")
}

async fn current_row(
    repository: &Repository,
    relation_id: [u8; 16],
    table: &str,
    key: &[u8],
) -> Option<Vec<u8>> {
    let (identity, digest) = format3::cli_runtime_identity(repository);
    let state = RuntimeState::open(repository, identity, digest).await.unwrap();
    let table_identity = RuntimeTableIdentity::new(table, TableObjectId::new(relation_id)).unwrap();
    let snapshot = state.begin_admitted_table_activation(std::slice::from_ref(&table_identity)).await.unwrap();
    snapshot.query_exact(table_identity.table(), key).unwrap().map(<[u8]>::to_vec)
}


#[tokio::test]
async fn edit_entrypoint_admits_valid_row_and_retains_invalid_candidate() {
    let initial = "{ title: \"before\", }\n";
    let (repository_directory, repository, relation_id, format_context) =
        format3::format3_repository_for_table_with_source_row(
            NOTES_SOURCE, "Music", "Music/london.orna", initial,
        );
    let (identity, digest) = format3::cli_runtime_identity(&repository);
    let state = RuntimeState::open(&repository, identity, digest).await.unwrap();
    let writer = state.acquire_lease(identity.repository_id).await.unwrap();
    let mut bindings = SysHostBindingRegistry::new(EnvironmentProvider::default());
    media_commit::commit_source_captures_for_table(
        &repository,
        &state,
        writer,
        &mut bindings,
        relation_id,
        &format_context,
        "Music",
        &[media_commit::SourceCaptureRow {
            key: "london",
            blob_fields: &[],
            source_row: Some(initial),
            ordinal: 0x90,
            insert_only: true,
        }],
    ).await;
    drop(state);
    let before = current_row(&repository, relation_id, "Music", b"london")
        .await.expect("the source row is admitted before editing");

    let scripts = TempDir::new().unwrap();
    let valid = editor(&scripts, "valid-editor", r#"printf '%s\n' '{ title: "after", }' > "$1""#);
    let accepted = edit(&repository_directory, &valid, &["edit", "--row", ROW_PATH]);
    assert!(accepted.status.success(), "edit stderr: {}", String::from_utf8_lossy(&accepted.stderr));
    let after = current_row(&repository, relation_id, "Music", b"london")
        .await.expect("accepted row remains admitted");
    assert_ne!(after, before, "valid edit replaces the complete stored row");

    let invalid = editor(&scripts, "invalid-editor", r#"printf '%s\n' '{ title: }' > "$1""#);
    let rejected = edit(&repository_directory, &invalid, &["--debug", "edit", "--row", ROW_PATH]);
    assert!(!rejected.status.success(), "invalid candidate must be rejected");
    let retained = String::from_utf8_lossy(&rejected.stderr);
    let draft = retained.split("draft retained at ").nth(1)
        .unwrap_or_else(|| panic!("invalid editor did not report retained draft: {retained}"))
        .lines().next().expect("draft path line").trim();
    assert_eq!(fs::read(draft).unwrap(), b"{ title: }\n");
    let rejected_row = current_row(&repository, relation_id, "Music", b"london")
        .await.expect("invalid candidate leaves accepted row");
    assert_eq!(rejected_row, after, "rejection leaves accepted row unchanged");
}
#[tokio::test]
async fn edit_blob_replacement_updates_the_row_without_changing_its_pinned_snapshot() {
    const ORIGINAL_PIXEL: &[u8] = include_bytes!("fixtures/media/pixel.png");

    let (repository_directory, repository, relation_id) = format3::empty_format3_repository();
    let source = TempDir::new().unwrap();
    let original_path = Path::new(MEDIA_FIXTURES).join("pixel.png");
    let replacement_path = Path::new(MEDIA_FIXTURES).join("pixel2.png");
    fs::copy(&original_path, source.path().join("pixel.png")).unwrap();
    fs::copy(&replacement_path, source.path().join("pixel2.png")).unwrap();

    let (identity, digest) = format3::cli_runtime_identity(&repository);
    let state = RuntimeState::open(&repository, identity, digest).await.unwrap();
    let writer = state.acquire_lease(identity.repository_id).await.unwrap();
    let capability = repository.capture_capability(relation_id).unwrap();
    let mut filesystem = FilesystemProvider::with_limits(1 << 20, 16).unwrap();
    filesystem.allow_root(source.path()).unwrap();
    let mut bindings = SysHostBindingRegistry::new(EnvironmentProvider::default())
        .with_filesystem_provider(filesystem)
        .with_repository_capture_capability(capability);
    let root = format!("{:?}", source.path().to_string_lossy().as_ref());
    let capture = format!("sys.blob.capture_file({root}, \"pixel.png\", 65536)");
    media_commit::commit_capture(
        &repository, &state, writer, &mut bindings, &capture, "image", 0xb0, true, relation_id,
    ).await;

    let before_value = current_row(&repository, relation_id, "media", b"image").await
        .expect("published image row exists");
    let before_metadata = decode_rov3_blob_metadata(&before_value).unwrap();
    assert_eq!(before_metadata.length(), ORIGINAL_PIXEL.len() as u64);
    assert_eq!(before_metadata.sha256(), <[u8; 32]>::from(sha2::Sha256::digest(ORIGINAL_PIXEL)));

    let pinned_context = repository.open_format_context().unwrap();
    let pinned_rows = pinned_context.load_row_map(relation_id).unwrap();
    let pinned_graph = pinned_context.open_native_graph(&pinned_rows).unwrap();
    let pinned_scope = pinned_graph.open_read_scope().unwrap();
    let pinned_row = pinned_graph.lookup_row(&TypedKey::Text("image".to_owned()), &pinned_scope)
        .unwrap().expect("pinned snapshot contains image row");
    let descriptor_bytes = before_metadata.descriptor_oid().unwrap();
    let algorithm = match descriptor_bytes.len() {
        20 => GitHashAlgorithm::Sha1,
        32 => GitHashAlgorithm::Sha256,
        _ => panic!("Blob descriptor has an unsupported OID width"),
    };
    let descriptor = NativeOid::new(algorithm, descriptor_bytes.as_bytes()).unwrap();
    let pinned_blob = pinned_graph.read_stored_blob_value(&pinned_row, &descriptor, &pinned_scope).unwrap();
    let pinned_reference = pinned_blob.reference().clone();

    let scripts = TempDir::new().unwrap();
    let replacement = editor(&scripts, "replace-blob", &format!("cp '{}' \"$1\"", replacement_path.display()));
    let accepted = edit(&repository_directory, &replacement, &["edit", "media/image/content.png"]);
    assert!(accepted.status.success(), "edit stderr: {}", String::from_utf8_lossy(&accepted.stderr));

    let after_value = current_row(&repository, relation_id, "media", b"image").await
        .expect("replacement image row exists");
    assert_ne!(after_value, before_value, "replacement updates the accepted Blob row");
    let old_bytes = pinned_graph.read_blob_range(&pinned_reference, 0..before_metadata.length(), &pinned_scope).unwrap();
    assert_eq!(old_bytes.bytes(), ORIGINAL_PIXEL, "the pinned old snapshot still reads its original Blob");
}
