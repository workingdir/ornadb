//! Semantic conformance for the storage profile surface after ORNA-SYS-115.
//!
//! ORNA-SYS-115 requires `sys.Storage.profile` to identify the one recorded row
//! profile; no new-format future-placement preference is exposed. ORNA-SYS-116
//! requires New-format admission to reject retired storage-preference functions
//! (see the retired API list in the sys API contract). Keep the source under test
//! in checked-in `.orna` fixtures.

use orna_semantic_v1::{ModuleInput, analyze};

#[test]
fn storage_preference_is_not_exposed_in_new_format() {
    let result = analyze(&[ModuleInput::new(
        "sys-storage-observed-vs-preference.orna",
        include_str!("fixtures/publication/sys-storage-observed-vs-preference.orna"),
    )]);

    assert!(
        !result.is_ok(),
        "ORNA-SYS-115/116: a preference read must not type check in the new format"
    );
    assert!(
        !result.diagnostics.is_empty(),
        "ORNA-SYS-115/116: the preference read must produce a rejection diagnostic"
    );
}

#[test]
fn storage_preference_cannot_be_conflated_with_profile() {
    let result = analyze(&[ModuleInput::new(
        "sys-storage-cross-assignment.orna",
        include_str!("fixtures/publication/sys-storage-cross-assignment.orna"),
    )]);

    assert!(
        !result.is_ok(),
        "ORNA-SYS-115/116: cross-assignments involving the retired preference must be rejected"
    );
    assert!(
        !result.diagnostics.is_empty(),
        "ORNA-SYS-115/116: cross-assignment and cross-comparison must be rejected"
    );
}
