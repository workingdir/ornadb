use orna_semantic_v1::{Catalogue, DIAG_NAMESPACE, ModuleInput, SystemProviders, analyze_with_providers};

/// Production admission uses the authoritative core catalogue only. The
/// conformance fixture namespaces (`vehicle`, `vehicle.corsa`, `mail.google`, ...)
/// must not be reserved for user modules.
#[test]
fn user_module_under_fixture_namespace_is_admitted_by_production_catalogue() {
    let analysis = analyze_with_providers(
        &[
            ModuleInput::new("vehicle/corsa.orna", "pub fn speed() = 1;"),
            ModuleInput::new("mail/google.orna", "pub fn label() = 1;"),
        ],
        &Catalogue::authoritative_core(),
        SystemProviders::Production,
    );
    assert!(
        analysis.is_ok(),
        "user modules under fixture namespaces must not collide: {:?}",
        analysis.diagnostics
    );
    assert!(
        !analysis
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == DIAG_NAMESPACE),
        "unexpected namespace collision: {:?}",
        analysis.diagnostics
    );
}
