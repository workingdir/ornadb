//! PUB-004 keeps publication metadata available through both storage objects
//! and runtime maintenance observations. Source under test lives in fixtures.

use orna_semantic_v1::{ModuleInput, analyze};

#[test]
fn storage_and_maintenance_expose_effective_publication_state() {
    let result = analyze(&[ModuleInput::new(
        "publication-metadata.orna",
        include_str!("fixtures/publication/publication-metadata.orna"),
    )]);

    assert!(result.is_ok(), "{:#?}", result.diagnostics);
}
