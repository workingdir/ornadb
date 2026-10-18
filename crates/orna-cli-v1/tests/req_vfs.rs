//! Requirement-targeted entry point for production VFS integration coverage.

use std::sync::Arc;

#[path = "support/media_commit.rs"]
mod media_commit;

// The shared media commit harness resolves the format-3 publication helper from
// the integration-test crate root, including when VFS tests are nested here.
#[path = "support/format3.rs"]
mod format3;

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
    let required = project_content_name("content", "audio/mpeg", None);
    assert!(matches!(
        row.resolve_unlink(&required).expect("required blob target"),
        VfsUnlinkTarget::RequiredContent(_)
    ));
    let optional = project_content_name("cover", "image/jpeg", None);
    assert!(matches!(
        row.resolve_unlink(&optional).expect("optional blob target"),
        VfsUnlinkTarget::OptionalContent(_)
    ));
}
