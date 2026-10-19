use orna_repository_v1::{
    migration_copy::{LegacyAnnotationInventory, LEGACY_ANNOTATION_DEFAULT_FORMAT},
    LegacyAnnotationCandidate, MigrationAnnotationDefaults, LEGACY_ANNOTATION_DEFAULT_MEDIA_TYPE,
};
use orna_value_v1::{Blob, MediaAnnotation};

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
