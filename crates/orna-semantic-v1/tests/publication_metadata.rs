//! ORNA-PUB-004 keeps effective publication policy and pending/published state
//! visible through `sys.Storage` and runtime maintenance metadata. In the
//! 1.1.0 API the policy is the opaque `sys.Value` envelope, published state is
//! `last_publication`/`last_snapshot`, and `sys.MaintenanceJob` reports
//! maintenance such as `checkpoint_publish`; there are no published counters or
//! nested policy fields (`api/sys.json`). Source under test lives in fixtures.

use orna_semantic_v1::{ModuleInput, analyze};

#[test]
fn storage_and_maintenance_expose_effective_publication_state() {
    let result = analyze(&[ModuleInput::new(
        "publication-metadata.orna",
        include_str!("fixtures/publication/publication-metadata.orna"),
    )]);

    assert!(
        result.is_ok(),
        "{:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}
