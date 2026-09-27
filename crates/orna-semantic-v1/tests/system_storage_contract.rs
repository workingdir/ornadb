//! Semantic conformance for the distinct observed and desired storage fields.
//!
//! ORNA-SYS-115 requires `sys.Storage.profile` to report observed placement
//! and `sys.Storage.preference` to report future automatic placement policy.
//! Keep the source under test in checked-in `.orna` fixtures.

use orna_semantic_v1::{DIAG_TYPE, ModuleInput, analyze};

#[test]
fn storage_profile_and_preference_are_independently_typed() {
    let result = analyze(&[ModuleInput::new(
        "sys-storage-observed-vs-preference.orna",
        include_str!("fixtures/publication/sys-storage-observed-vs-preference.orna"),
    )]);

    assert!(result.is_ok(), "{:#?}", result.diagnostics);
}

#[test]
fn storage_profile_and_preference_cannot_be_conflated() {
    let result = analyze(&[ModuleInput::new(
        "sys-storage-cross-assignment.orna",
        include_str!("fixtures/publication/sys-storage-cross-assignment.orna"),
    )]);

    assert!(!result.is_ok(), "cross-assignments unexpectedly type checked");
    assert!(
        !result.diagnostics.is_empty(),
        "cross-assignment and cross-comparison must be rejected"
    );
    assert!(
        result
            .diagnostics
            .iter()
            .all(|diagnostic| diagnostic.code() == DIAG_TYPE),
        "expected only type diagnostics, got {:#?}",
        result.diagnostics
    );
    assert_eq!(
        result.diagnostics.len(),
        2,
        "expected one type error for the cross-assignment and one for the cross-comparison"
    );
}
