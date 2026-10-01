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
fn edge_interplay_fixture_closes_column_row_nullable_edges() {
    let analysis = analyze(&[ModuleInput::new(
        "sys-api-drift-edge-interplay.orna",
        EDGE_INTERPLAY,
    )]);
    assert!(
        analysis.is_ok(),
        "nested SYS projections plus Column rows handed across helpers and composed nullable field predicates must resolve through the published schema: {:?}",
        analysis
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn edge_interplay_fixture_closes_nullable_column_filter_arms() {
    let analysis = analyze(&[ModuleInput::new(
        "sys-api-drift-edge-interplay.orna",
        EDGE_INTERPLAY,
    )]);

    // The frozen schema marks both fields nullable but does not define a relationship between
    // them; preserve the Column row and type-check the null-key and default-expression arms
    // independently within one disjunctive predicate.
    assert!(
        analysis.is_ok(),
        "nullable Column filter arms must resolve on the same helper-returned row: {:?}",
        analysis
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn edge_interplay_fixture_closes_nullable_expression_projection_edges() {
    let analysis = analyze(&[ModuleInput::new(
        "sys-api-drift-edge-interplay.orna",
        EDGE_INTERPLAY,
    )]);

    // The frozen schema declares these expression and documentation fields nullable, but is
    // silent on relationships between them; compose their checks and retain the nullable docs
    // type in the projected relation without adding a domain invariant.
    assert!(
        analysis.is_ok(),
        "nullable Column expression predicates and docs projection must resolve together: {:?}",
        analysis
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn edge_interplay_fixture_preserves_nullable_sibling_projection() {
    let analysis = analyze(&[ModuleInput::new(
        "sys-api-drift-edge-interplay.orna",
        EDGE_INTERPLAY,
    )]);

    // The frozen schema does not say that documented Columns have a default expression;
    // filtering on docs must leave the sibling nullable field nullable in the projection.
    assert!(
        analysis.is_ok(),
        "filtering a nullable Column field must preserve the nullable sibling projection: {:?}",
        analysis
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn edge_interplay_fixture_keeps_nullable_docs_after_default_filter() {
    let analysis = analyze(&[ModuleInput::new(
        "sys-api-drift-edge-interplay.orna",
        EDGE_INTERPLAY,
    )]);

    // The frozen schema does not say that a defaulted Column has docs; filtering by one
    // nullable sibling must leave the projected sibling nullable.
    assert!(
        analysis.is_ok(),
        "filtering by default_expression must preserve nullable Column.docs in projection: {:?}",
        analysis
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn edge_interplay_fixture_composes_nullable_sibling_null_arms() {
    let analysis = analyze(&[ModuleInput::new(
        "sys-api-drift-edge-interplay.orna",
        EDGE_INTERPLAY,
    )]);

    // The frozen schema makes key_position, docs, and computed_expression independently
    // nullable and gives no relationship between them; compose sibling predicates while
    // preserving the nullable computed-expression projection.
    assert!(
        analysis.is_ok(),
        "nullable Column sibling predicates must preserve the projected nullable expression: {:?}",
        analysis
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn edge_interplay_fixture_preserves_nullable_sibling_across_helpers() {
    let analysis = analyze(&[ModuleInput::new(
        "sys-api-drift-edge-interplay.orna",
        EDGE_INTERPLAY,
    )]);

    // The frozen schema does not say that documented Columns have a default expression;
    // keep that nullable sibling after returning filtered Column rows from a helper.
    assert!(
        analysis.is_ok(),
        "a helper-filtered Column row must retain its nullable sibling projection: {:?}",
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
