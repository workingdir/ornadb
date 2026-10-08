use std::collections::BTreeSet;

use orna_semantic_v1::{DIAG_DUPLICATE, ModuleInput, analyze};

const COLLISION_SOURCE: &str = include_str!("fixtures/identity-collision-v1.orna");

#[test]
fn second_declaration_under_one_name_is_rejected_as_an_identity_collision() {
    let analysis = analyze(&[ModuleInput {
        logical_path: "app/collision.orna".to_owned(),
        source: COLLISION_SOURCE.to_owned(),
        prelude_exports: BTreeSet::new(),
    }]);
    assert!(!analysis.is_ok(), "two declarations named `total` must not pass");
    assert!(
        analysis
            .diagnostics
            .iter()
            .any(|diagnostic| format!("{diagnostic:?}").contains(DIAG_DUPLICATE)),
        "expected {DIAG_DUPLICATE} in {:?}",
        analysis.diagnostics
    );
}
