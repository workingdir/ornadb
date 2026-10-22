//! Semantic conformance for the 1.1.0 storage placement surface.
//!
//! ORNA-SYS-115 requires `sys.Storage.profile` to identify the one recorded
//! row profile and exposes no new-format future-placement preference.
//! ORNA-SYS-116 requires new-format admission to reject the retired
//! storage-preference functions (`api/retired.json`) rather than select a
//! hidden legacy writer. Keep the source under test in checked-in `.orna`
//! fixtures.

use orna_semantic_v1::{Analysis, DIAG_UNRESOLVED, DIAG_UNSUPPORTED, ModuleInput, analyze};

fn diagnostics(result: &Analysis) -> Vec<(&str, &str)> {
    result
        .diagnostics
        .iter()
        .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
        .collect()
}

#[test]
fn storage_profile_is_the_recorded_row_profile() {
    let result = analyze(&[ModuleInput::new(
        "sys-storage-recorded-profile.orna",
        include_str!("fixtures/publication/sys-storage-recorded-profile.orna"),
    )]);

    assert!(result.is_ok(), "{:?}", diagnostics(&result));
}

#[test]
fn storage_preference_is_not_exposed() {
    let result = analyze(&[ModuleInput::new(
        "sys-storage-preference-not-exposed.orna",
        include_str!("fixtures/publication/sys-storage-preference-not-exposed.orna"),
    )]);

    // One rejection each for the retired member and the retired enum type;
    // neither is admitted and then reported as a type mismatch.
    let mut codes = result
        .diagnostics
        .iter()
        .map(|diagnostic| diagnostic.code())
        .collect::<Vec<_>>();
    codes.sort_unstable();
    assert_eq!(
        codes,
        [DIAG_UNRESOLVED, DIAG_UNSUPPORTED],
        "{:?}",
        diagnostics(&result)
    );
}

#[test]
fn retired_storage_preference_functions_are_rejected() {
    let result = analyze(&[ModuleInput::new(
        "sys-admin-retired-storage-preference.orna",
        include_str!("fixtures/publication/sys-admin-retired-storage-preference.orna"),
    )]);

    let rejected = diagnostics(&result);
    assert_eq!(rejected.len(), 2, "{rejected:?}");
    assert!(
        rejected.iter().all(|(code, message)| {
            *code == DIAG_UNSUPPORTED && message.starts_with("removed portable system name")
        }),
        "{rejected:?}"
    );
}
