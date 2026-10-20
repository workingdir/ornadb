use std::{fs, path::Path, process::Command};

use orna_evaluator_v1::SysHostBindingRegistry;
use orna_runtime_v1::RuntimeState;
use orna_storage_v1::{LoosePath, RuntimePublicationCoordinator};
use orna_sys_v1::{EnvironmentProvider, FilesystemProvider};
use tempfile::TempDir;
use orna_repository_v1::{
    migration_copy::{LegacyAnnotationInventory, LEGACY_ANNOTATION_DEFAULT_FORMAT},
    LegacyAnnotationCandidate, MigrationAnnotationDefaults, LEGACY_ANNOTATION_DEFAULT_MEDIA_TYPE,
};
use orna_value_v1::{Blob, MediaAnnotation};

#[path = "../../orna-cli-v1/tests/support/format3.rs"]
mod format3;
#[path = "../../orna-cli-v1/tests/support/media_commit.rs"]
mod media_commit;

const MEDIA_FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../orna-cli-v1/tests/fixtures/media");
const IMPORT_FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../orna-cli-v1/tests/fixtures/media/import-song.orna");

fn git(root: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(root)
        .args(args)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("git output is UTF-8")
        .trim()
        .to_owned()
}

fn import_expression(root: &Path) -> String {
    let source = fs::read_to_string(IMPORT_FIXTURE).expect("read media import fixture");
    let root = format!("{:?}", root.to_string_lossy().as_ref());
    source.trim_end().replace("__MEDIA_ROOT__", &root)
}

#[tokio::test]
async fn req_upgrade_003_writes_rows_only_to_the_canonical_format3_store() {
    // ORNA-UPGRADE-003: new-format writes use one canonical row store; loose,
    // compact, and hybrid dual-writing is forbidden.
    let (repository_dir, repository, relation_id) = format3::empty_format3_repository();
    let media = TempDir::new().expect("temporary media directory");
    fs::copy(
        Path::new(MEDIA_FIXTURES).join("tone.wav"),
        media.path().join("tone.wav"),
    )
    .expect("copy media fixture");
    let key = "req-upgrade-003";
    assert!(LoosePath::for_key("media", &[key.to_owned()]).is_ok());

    let (identity, initial_digest) = format3::cli_runtime_identity(&repository);
    let state = RuntimeState::open(&repository, identity, initial_digest)
        .await
        .expect("open runtime state");
    let writer = state
        .acquire_lease(identity.repository_id)
        .await
        .expect("acquire runtime writer");
    let capability = repository
        .capture_capability(relation_id)
        .expect("issue admitted capture capability");
    let mut filesystem = FilesystemProvider::with_limits(1 << 20, 16)
        .expect("bounded filesystem provider");
    filesystem.allow_root(media.path()).expect("allow media fixture root");
    let mut bindings = SysHostBindingRegistry::new(EnvironmentProvider::default())
        .with_filesystem_provider(filesystem)
        .with_repository_capture_capability(capability);
    media_commit::commit_capture_pending(
        &repository,
        &state,
        writer,
        &mut bindings,
        &import_expression(media.path()),
        key,
        0x71,
        true,
        relation_id,
    )
    .await;
    let checkpoint = state
        .latest_checkpoint()
        .await
        .expect("read latest checkpoint")
        .expect("capture leaves a checkpoint");
    let freeze = state
        .freeze([0x81; 16], &checkpoint)
        .await
        .expect("freeze captured row prefix");
    let head = repository
        .head()
        .expect("read repository head")
        .expect("format-3 fixture has a head");
    let index = repository
        .index_generation()
        .expect("read repository index generation");
    let mut coordinator = RuntimePublicationCoordinator::prepare(
        &repository,
        &state,
        &head,
        index,
        &freeze,
        |_| LoosePath::for_key("media", &[key.to_owned()]),
        "orna: publish format-3 row",
    )
    .await
    .expect("storage writer prepares format-3 publication");
    coordinator
        .publish(&repository)
        .expect("storage writer publishes format-3 row");
    coordinator
        .complete(&repository, &state, &freeze)
        .await
        .expect("complete storage-writer publication");

    let tree = git(
        repository_dir.path(),
        &["ls-tree", "-r", "--name-only", "HEAD"],
    );
    let paths: Vec<_> = tree.lines().collect();
    assert!(paths.iter().any(|path| path.starts_with(".orna/store/")), "native store is committed: {tree}");
    assert!(paths.contains(&".orna/database.orna"), "format metadata remains tracked: {tree}");
    assert!(paths.iter().all(|path| !path.starts_with("media/")), "no loose-row files are written: {tree}");
    assert!(paths.iter().all(|path| !path.starts_with(".orna/rows/")), "no alternate row store is written: {tree}");
    assert!(paths.iter().all(|path| !path.starts_with(".orna/compact/")), "no compact representation is dual-written: {tree}");
    assert!(!paths.contains(&".orna/format.orna"), "no legacy format marker is written: {tree}");
}

#[test]
/// ORNA-UPGRADE-004: legacy Blob defaults are explicit MIME-1 metadata and are
/// derived from the row inventory without inspecting payload bytes.
fn req_upgrade_004_legacy_blob_defaults_are_explicit_and_payload_free() {
    let inventory = LegacyAnnotationInventory::Present(vec![
        LegacyAnnotationCandidate::new(b"unannotated-row".to_vec(), None, None),
        LegacyAnnotationCandidate::new(
            b"annotated-row".to_vec(),
            Some("audio/mpeg".to_owned()),
            Some("mp2".to_owned()),
        ),
    ]);

    let defaults = inventory
        .annotation_defaults()
        .expect("available legacy metadata yields annotation defaults");
    assert_eq!(LEGACY_ANNOTATION_DEFAULT_FORMAT, "MIME-1");
    assert_eq!(
        LEGACY_ANNOTATION_DEFAULT_MEDIA_TYPE,
        "application/octet-stream"
    );
    assert_eq!(inventory.rows()[0].annotation_media_type(), None);
    assert_eq!(inventory.rows()[0].suffix(), None);
    assert_eq!(inventory.rows()[1].annotation_media_type(), Some("audio/mpeg"));
    assert_eq!(inventory.rows()[1].suffix(), Some("mp2"));
    assert_eq!(defaults.entry_count(), 1);
    assert!(!defaults.payload_inspected());

    let default_annotation = MediaAnnotation::new(LEGACY_ANNOTATION_DEFAULT_MEDIA_TYPE, None)
        .expect("the fixed legacy default is canonical MIME-1");
    assert_eq!(default_annotation.media_type(), "application/octet-stream");
    assert_eq!(default_annotation.suffix(), None);
}

#[test]
/// ORNA-UPGRADE-005: a migration reports changed annotated value identity even
/// when the source and migrated values preserve the same raw content identity.
fn req_upgrade_005_reports_annotation_transition_beyond_content_identity() {
    let inventory = LegacyAnnotationInventory::Present(vec![LegacyAnnotationCandidate::new(
        b"same-content-row".to_vec(),
        None,
        None,
    )]);
    let defaults = MigrationAnnotationDefaults::from_legacy_inventory(inventory.rows())
        .expect("metadata-only inventory yields a semantic transition report");
    assert_eq!(defaults.entry_count(), 1);
    assert!(!defaults.payload_inspected());

    let source_value = Blob::from_bytes_with_annotation(b"same bytes".to_vec(), "text/plain", None)
        .expect("source annotation is canonical MIME-1");
    let migrated_value = source_value
        .annotate(LEGACY_ANNOTATION_DEFAULT_MEDIA_TYPE, None)
        .expect("migration default annotation is canonical MIME-1");

    assert!(source_value
        .same_content(&migrated_value)
        .expect("inline content identity is available"));
    assert!(!source_value
        .value_eq(&migrated_value)
        .expect("annotation identity is available"));
    assert_eq!(migrated_value.media_type(), "application/octet-stream");
    assert_eq!(migrated_value.suffix(), None);
}
