//! Requirement-targeted entry point for production VFS integration coverage.

use std::sync::Arc;

#[path = "support/media_commit.rs"]
mod media_commit;

// The shared media commit harness resolves the format-3 publication helper from
// the integration-test crate root, including when VFS tests are nested here.
#[path = "support/format3.rs"]
mod format3;

// ORNA-VFS-010 is exercised by `cwd_mount_metadata_save_avoids_corrupt_sibling_payload`;
// ORNA-VFS-024 is exercised by `cwd_mount_refuses_unmount_with_dirty_draft_and_allows_clean_unmount`.
// Both are production FUSE tests compiled into this req_vfs target.
#[path = "vfs_mount_integration.rs"]
mod vfs_mount_integration;

#[path = "vfs_editor_compat.rs"]
mod vfs_editor_compat;
#[test]
/// ORNA-VFS-016: escaped key components round-trip without aliasing reserved names.
fn typed_key_components_round_trip_reserved_and_escaped_names() {
    use orna_runtime_v1::vfs::{project_component, unproject_component, VfsComponent};

    for key in ["", "~empty", ".", "..", ".git", ".orna", "data.orna", "a/b", "é"] {
        let projected = project_component(key);
        assert_eq!(
            unproject_component(&projected),
            Ok(VfsComponent::Canonical(key.to_owned())),
            "projected path component did not round-trip: {key:?} -> {projected:?}",
        );
    }
    assert_ne!(project_component(""), project_component("~empty"));
}

#[test]
/// ORNA-VFS-004 and ORNA-VFS-013: field path lookup stays inside the captured row and unlink targets preserve row/blob policy.
fn row_projection_rejects_escapes_and_classifies_unlink_targets() {
    use orna_runtime_v1::vfs::{
        project_content_name, SnapshotPin, VfsRowProjection, VfsStoredField, VfsUnlinkTarget,
    };

    let row = VfsRowProjection::project(
        SnapshotPin::capture(Arc::new(17_u64)),
        &[
            VfsStoredField::document("title", "\"Original\""),
            VfsStoredField::content("content", "audio/mpeg", None, 99),
            VfsStoredField::content_optional("cover", "image/jpeg", None, 4096),
        ],
    )
    .expect("row projection");

    for path in ["../other/data.orna", "/absolute/content.mp3", "content.mp3/../../other"] {
        assert!(row.lookup(path).is_err(), "escaped projection path resolved: {path}");
    }
    assert!(matches!(
        row.resolve_unlink("data.orna").expect("row document target"),
        VfsUnlinkTarget::RowDocument
    ));
    let required =
        project_content_name("content", "audio/mpeg", None).expect("registered annotation");
    assert!(matches!(
        row.resolve_unlink(&required).expect("required blob target"),
        VfsUnlinkTarget::RequiredContent(_)
    ));
    let optional =
        project_content_name("cover", "image/jpeg", None).expect("registered annotation");
    assert!(matches!(
        row.resolve_unlink(&optional).expect("optional blob target"),
        VfsUnlinkTarget::OptionalContent(_)
    ));
}
#[tokio::test]
/// ORNA-VFS-014, ORNA-VFS-017, and ORNA-VFS-022: private reads are copies and open handles/cursors remain pinned to their generation.
async fn read_handles_and_readdir_cursors_keep_their_open_generation() {
    use orna_runtime_v1::vfs::{
        SnapshotReaddirCursor, SnapshotPin, VfsFileSnapshot, VfsRepositoryCache,
    };

    let cache = VfsRepositoryCache::new();
    let image = Arc::new(VfsFileSnapshot::new(
        SnapshotPin::capture(Arc::new(17_u64)),
        Arc::<[u8]>::from(&b"opened generation"[..]),
    ));
    let file = cache.managed_file(image).await.expect("managed file");
    let open = file.open_read().await;
    let cursor = SnapshotReaddirCursor::new(open.pin().clone_pin(), 3_u64);

    assert_eq!(open.pin().snapshot(), &17);
    assert_eq!(open.read_at(0, 32), b"opened generation");
    let mut private_copy = open.read_at(0, 32);
    private_copy[0] = b'X';
    assert_eq!(open.read_at(0, 32), b"opened generation");
    assert_eq!(cursor.pin().snapshot(), &17);
    assert_eq!(*cursor.continuation(), 3);
}

#[tokio::test]
/// ORNA-VFS-015 and ORNA-VFS-020: separate file candidates stay private until explicit validated admissions, without evaluation during writes.
async fn speculative_file_candidates_are_admitted_individually() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use orna_runtime_v1::vfs::{
        ActivationDecision, SnapshotPin, TemporaryRenameOutcome, VfsFileSnapshot,
        VfsRepositoryCache,
    };

    let cache = VfsRepositoryCache::new();
    let first = cache
        .managed_file(Arc::new(VfsFileSnapshot::new(
            SnapshotPin::capture(Arc::new(1_u64)),
            Arc::<[u8]>::from(&b"first source"[..]),
        )))
        .await
        .expect("first managed file");
    let second = cache
        .managed_file(Arc::new(VfsFileSnapshot::new(
            SnapshotPin::capture(Arc::new(2_u64)),
            Arc::<[u8]>::from(&b"second source"[..]),
        )))
        .await
        .expect("second managed file");

    let first_draft = first.open_draft(1024).await;
    first_draft.truncate(0).await.expect("truncate first candidate");
    first_draft.write_at(0, b"first candidate").await.expect("write first candidate");
    let second_draft = second.open_draft(1024).await;
    second_draft.truncate(0).await.expect("truncate second candidate");
    second_draft.write_at(0, b"second candidate").await.expect("write second candidate");

    let validations = Arc::new(AtomicUsize::new(0));
    assert_eq!(validations.load(Ordering::SeqCst), 0);
    assert_eq!(first.open_read().await.read_at(0, 32), b"first source");
    assert_eq!(second.open_read().await.read_at(0, 32), b"second source");

    let first_validations = Arc::clone(&validations);
    let first_result = first
        .commit_draft_with(&first_draft, move |candidate| async move {
            first_validations.fetch_add(1, Ordering::SeqCst);
            assert_eq!(candidate.replacement_bytes(), b"first candidate");
            Ok::<_, ()>(ActivationDecision::Accepted(SnapshotPin::capture(Arc::new(3_u64))))
        })
        .await
        .expect("explicit admission of first candidate");
    assert!(matches!(first_result, TemporaryRenameOutcome::Applied { .. }));
    assert_eq!(validations.load(Ordering::SeqCst), 1);
    assert_eq!(first.open_read().await.read_at(0, 32), b"first candidate");
    assert_eq!(second.open_read().await.read_at(0, 32), b"second source");

    let second_validations = Arc::clone(&validations);
    let second_result = second
        .commit_draft_with(&second_draft, move |candidate| async move {
            second_validations.fetch_add(1, Ordering::SeqCst);
            assert_eq!(candidate.replacement_bytes(), b"second candidate");
            Ok::<_, ()>(ActivationDecision::Accepted(SnapshotPin::capture(Arc::new(4_u64))))
        })
        .await
        .expect("explicit admission of second candidate");
    assert!(matches!(second_result, TemporaryRenameOutcome::Applied { .. }));
    assert_eq!(validations.load(Ordering::SeqCst), 2);
}
