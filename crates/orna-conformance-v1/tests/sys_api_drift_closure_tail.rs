use orna_semantic_v1::{analyze, ModuleInput};
use serde_json::Value;

const PUBLISHED_SYS_API: &str = include_str!("../../../api/sys.json");
const FROZEN_SYS_API: &str = include_str!("fixtures/reference/api/sys.json");
const PUBLICATION_SURFACE: &str = include_str!("fixtures/sys-api-drift-publication-surface.orna");
const INTERNAL_PUBLICATION_METADATA: &str =
    include_str!("fixtures/sys-api-drift-internal-publication-metadata.orna");
const DEFAULT_ARGUMENTS: &str = include_str!("fixtures/sys-api-drift-default-arguments.orna");
const EDGE_INTERPLAY: &str = include_str!("fixtures/sys-api-drift-edge-interplay.orna");

#[test]
fn published_api_matches_the_frozen_schema_and_keeps_local_provenance() {
    let mut published: Value = serde_json::from_str(PUBLISHED_SYS_API).expect("published sys API");
    let frozen: Value = serde_json::from_str(FROZEN_SYS_API).expect("frozen sys API fixture");

    assert_ne!(
        published["source_of_truth"], frozen["source_of_truth"],
        "the generated artifact records its local annotated-method source"
    );
    published["source_of_truth"] = frozen["source_of_truth"].clone();
    assert_eq!(
        published, frozen,
        "public schema inventories and edges must match the frozen API"
    );
}

#[test]
fn publication_surface_fixture_uses_frozen_sys_fields() {
    let analysis = analyze(&[ModuleInput::new(
        "sys-api-drift-publication-surface.orna",
        PUBLICATION_SURFACE,
    )]);
    assert!(analysis.is_ok(), "{:#?}", analysis.diagnostics);
}

#[test]
fn default_argument_fixture_uses_published_schema_edges() {
    let analysis = analyze(&[ModuleInput::new(
        "sys-api-drift-default-arguments.orna",
        DEFAULT_ARGUMENTS,
    )]);
    assert!(
        analysis.is_ok(),
        "{:?}",
        analysis
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn edge_interplay_fixture_closes_projected_table_reference_rows() {
    let analysis = analyze(&[ModuleInput::new(
        "sys-api-drift-edge-interplay.orna",
        EDGE_INTERPLAY,
    )]);
    assert!(
        analysis.is_ok(),
        "nested SYS projections plus filtered Reference rows and captured Table rows across their Reference relation must resolve through the published schema: {:?}",
        analysis
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn runtime_publication_counters_do_not_leak_into_frozen_sys_types() {
    let analysis = analyze(&[ModuleInput::new(
        "sys-api-drift-internal-publication-metadata.orna",
        INTERNAL_PUBLICATION_METADATA,
    )]);

    assert!(
        !analysis.is_ok(),
        "runtime ledger fields outside the frozen sys types must not be admitted"
    );
    assert_eq!(
        analysis.diagnostics.len(),
        4,
        "expected one rejection per field outside the frozen API"
    );
}
