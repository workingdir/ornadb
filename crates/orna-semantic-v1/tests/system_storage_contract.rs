//! Semantic conformance for the observed storage profile and the retired
//! storage preference.
//!
//! ORNA-SYS-115 requires `sys.Storage.profile` to identify the one recorded row
//! profile; no new-format future-placement preference is exposed. ORNA-ROW-014
//! requires removed preference/placement modes to not survive as hidden toggles.
//! Keep the source under test in checked-in `.orna` fixtures.

use orna_semantic_v1::{DIAG_TYPE, DIAG_UNRESOLVED, DIAG_UNSUPPORTED, ModuleInput, analyze};

#[test]
fn retired_storage_preference_is_rejected_alongside_profile() {
    let result = analyze(&[ModuleInput::new(
        "sys-storage-observed-vs-preference.orna",
        include_str!("fixtures/publication/sys-storage-observed-vs-preference.orna"),
    )]);

    assert!(!result.is_ok(), "retired preference unexpectedly type checked");
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == DIAG_UNRESOLVED),
        "sys.StoragePreference must be unresolved: {:#?}",
        result.diagnostics
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == DIAG_UNSUPPORTED),
        "storage.preference must be unsupported: {:#?}",
        result.diagnostics
    );
}

#[test]
fn retired_storage_preference_cannot_be_conflated_with_profile() {
    let result = analyze(&[ModuleInput::new(
        "sys-storage-cross-assignment.orna",
        include_str!("fixtures/publication/sys-storage-cross-assignment.orna"),
    )]);

    assert!(!result.is_ok(), "cross-assignments unexpectedly type checked");
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == DIAG_TYPE),
        "profile assigned to a preference must be a type error: {:#?}",
        result.diagnostics
    );
}
