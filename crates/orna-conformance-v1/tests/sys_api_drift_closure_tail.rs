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
fn edge_interplay_fixture_projects_nullable_sibling_rows_through_flat_map() {
    let analysis = analyze(&[ModuleInput::new(
        "sys-api-drift-edge-interplay.orna",
        EDGE_INTERPLAY,
    )]);

    // The schema leaves the relationship between docs and default_expression unspecified;
    // filtering a same-table sibling on docs must preserve its nullable expression projection.
    assert!(
        analysis.is_ok(),
        "flat-mapped sibling Columns must retain their nullable expression field: {:?}",
        analysis
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn edge_interplay_fixture_keeps_nullable_sibling_after_outer_filter() {
    let analysis = analyze(&[ModuleInput::new(
        "sys-api-drift-edge-interplay.orna",
        EDGE_INTERPLAY,
    )]);

    // The frozen schema does not connect an outer Column's key_position to nullable fields on
    // same-table siblings; keep the sibling computed_expression nullable through flat_map.
    assert!(
        analysis.is_ok(),
        "outer nullable-field filters must preserve sibling nullable projections: {:?}",
        analysis
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn edge_interplay_fixture_captures_nullable_outer_field_for_sibling_filter() {
    let analysis = analyze(&[ModuleInput::new(
        "sys-api-drift-edge-interplay.orna",
        EDGE_INTERPLAY,
    )]);

    // The frozen schema does not relate an anchor's default_expression to sibling docs or
    // computed_expression; check the two nullable inputs inside the nested closure and retain
    // the sibling's nullable projection.
    assert!(
        analysis.is_ok(),
        "captured nullable Column fields must preserve the sibling expression type: {:?}",
        analysis
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn edge_interplay_fixture_projects_outer_nullable_field_from_sibling_map() {
    let analysis = analyze(&[ModuleInput::new(
        "sys-api-drift-edge-interplay.orna",
        EDGE_INTERPLAY,
    )]);

    // The frozen schema does not say that a documented sibling has any relationship to the
    // anchor's default_expression; preserve that nullable outer field through the nested map.
    assert!(
        analysis.is_ok(),
        "nested sibling maps must retain the captured nullable outer expression: {:?}",
        analysis
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn edge_interplay_fixture_projects_nullable_capture_across_nested_closures() {
    let analysis = analyze(&[ModuleInput::new(
        "sys-api-drift-edge-interplay.orna",
        EDGE_INTERPLAY,
    )]);

    // The frozen schema leaves an anchor's nullable default_expression independent from
    // two-hop sibling docs; retain that captured nullable projection through both flat_maps.
    assert!(
        analysis.is_ok(),
        "nested sibling closures must preserve the captured nullable expression: {:?}",
        analysis
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn edge_interplay_fixture_projects_deepest_nullable_column_field() {
    let analysis = analyze(&[ModuleInput::new(
        "sys-api-drift-edge-interplay.orna",
        EDGE_INTERPLAY,
    )]);

    // The frozen schema does not relate anchor defaults or middle-sibling docs to a candidate
    // Column's computed_expression; preserve the candidate's nullable type at the third row.
    assert!(
        analysis.is_ok(),
        "nested Column closures must retain the deepest nullable projection: {:?}",
        analysis
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn edge_interplay_fixture_keeps_column_projection_under_nullable_table_edge() {
    let analysis = analyze(&[ModuleInput::new(
        "sys-api-drift-edge-interplay.orna",
        EDGE_INTERPLAY,
    )]);

    // The frozen schema makes row_count and default_expression nullable independently and does
    // not say whether unknown-count tables retain columns; preserve the nested nullable result.
    assert!(
        analysis.is_ok(),
        "nullable Table filtering must retain the nullable Column projection: {:?}",
        analysis
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn edge_interplay_fixture_projects_nullable_table_field_through_columns() {
    let analysis = analyze(&[ModuleInput::new(
        "sys-api-drift-edge-interplay.orna",
        EDGE_INTERPLAY,
    )]);

    // The frozen schema does not relate a table's optional row_count to docs on its Columns;
    // preserve the nullable parent field when projecting it through the child relation.
    assert!(
        analysis.is_ok(),
        "Column filtering must retain the parent Table's nullable projection: {:?}",
        analysis
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn edge_interplay_fixture_keeps_nullable_target_count_after_handoff() {
    let analysis = analyze(&[ModuleInput::new(
        "sys-api-drift-edge-interplay.orna",
        EDGE_INTERPLAY,
    )]);

    // The frozen schema makes row_count nullable and does not say row_count_exact guarantees a
    // value; preserve row_count's nullable type after resolving Reference target handles.
    assert!(
        analysis.is_ok(),
        "target Table handoff must retain nullable row_count: {:?}",
        analysis
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn edge_interplay_fixture_composes_unknown_target_count_with_exactness() {
    let analysis = analyze(&[ModuleInput::new(
        "sys-api-drift-edge-interplay.orna",
        EDGE_INTERPLAY,
    )]);

    // The frozen schema leaves row_count and row_count_exact independently specified; checking
    // both the null branch and exactness must retain the nullable target-count projection.
    assert!(
        analysis.is_ok(),
        "nullable target-count and exactness predicates must compose: {:?}",
        analysis
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn edge_interplay_fixture_composes_unknown_target_count_with_inexactness() {
    let analysis = analyze(&[ModuleInput::new(
        "sys-api-drift-edge-interplay.orna",
        EDGE_INTERPLAY,
    )]);

    // The frozen schema does not couple a nullable row_count with row_count_exact; keep the
    // unknown/inexact combination representable after resolving the target through a closure.
    assert!(
        analysis.is_ok(),
        "unknown target-count and inexactness predicates must compose: {:?}",
        analysis
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn edge_interplay_fixture_projects_unknown_inexact_count_through_target_columns() {
    let analysis = analyze(&[ModuleInput::new(
        "sys-api-drift-edge-interplay.orna",
        EDGE_INTERPLAY,
    )]);

    // Keep the optional parent count nullable when the unknown/inexact target is expanded
    // through its Columns relation and captured by the nested projection.
    assert!(
        analysis.is_ok(),
        "unknown/inexact target count must survive the Column chain: {:?}",
        analysis
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn edge_interplay_fixture_retains_unknown_inexact_target_through_column_handoff() {
    let analysis = analyze(&[ModuleInput::new(
        "sys-api-drift-edge-interplay.orna",
        EDGE_INTERPLAY,
    )]);

    // Retain the entire unknown/inexact target across its Columns callback, then project its
    // count as Int?; the frozen API does not define exactness as a non-null guarantee.
    assert!(
        analysis.is_ok(),
        "unknown/inexact target record and nullable count must survive the Column handoff: {:?}",
        analysis
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn edge_interplay_fixture_retains_unknown_inexact_count_through_references() {
    let analysis = analyze(&[ModuleInput::new(
        "sys-api-drift-edge-interplay.orna",
        EDGE_INTERPLAY,
    )]);

    // Retain the nullable parent count while a target's Reference rows are traversed; exactness
    // remains independent, and the frozen schema gives no count guarantee for this callback.
    assert!(
        analysis.is_ok(),
        "unknown/inexact target count must survive the Reference callback: {:?}",
        analysis
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn edge_interplay_fixture_retains_unknown_inexact_target_across_reference_handoff() {
    let analysis = analyze(&[ModuleInput::new(
        "sys-api-drift-edge-interplay.orna",
        EDGE_INTERPLAY,
    )]);

    // Keep the whole unknown/inexact target across its References callback, then project
    // row_count separately to prove the nullable field is retained on the captured record.
    assert!(
        analysis.is_ok(),
        "unknown/inexact target and nullable row_count must survive the Reference handoff: {:?}",
        analysis
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn edge_interplay_fixture_retains_unknown_inexact_count_through_nested_reference_lookup() {
    let analysis = analyze(&[ModuleInput::new(
        "sys-api-drift-edge-interplay.orna",
        EDGE_INTERPLAY,
    )]);

    // Preserve the outer target's nullable row_count while resolving a Reference's to_table
    // through a second catalog lookup; the schema does not promise a count from exactness.
    assert!(
        analysis.is_ok(),
        "unknown/inexact parent count must survive nested Reference lookup: {:?}",
        analysis
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn edge_interplay_fixture_chains_unknown_inexact_target_filters() {
    let analysis = analyze(&[ModuleInput::new(
        "sys-api-drift-edge-interplay.orna",
        EDGE_INTERPLAY,
    )]);

    // The frozen schema specifies count nullability and exactness independently; retaining the
    // count through separate exactness and null filters avoids inventing a coupling invariant.
    assert!(
        analysis.is_ok(),
        "chained unknown/inexact target filters must retain nullable row_count: {:?}",
        analysis
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn edge_interplay_fixture_filters_unknown_inexact_count_after_closure_handoff() {
    let analysis = analyze(&[ModuleInput::new(
        "sys-api-drift-edge-interplay.orna",
        EDGE_INTERPLAY,
    )]);

    // The published API does not couple row_count exactness to count presence; preserve the
    // nullable count when it crosses another closure and is then checked for null.
    assert!(
        analysis.is_ok(),
        "unknown inexact target count must remain nullable across the handoff: {:?}",
        analysis
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn edge_interplay_fixture_composes_present_target_count_with_inexactness() {
    let analysis = analyze(&[ModuleInput::new(
        "sys-api-drift-edge-interplay.orna",
        EDGE_INTERPLAY,
    )]);

    // The frozen schema declares row_count nullable and row_count_exact independently; query
    // the present/inexact branch while retaining row_count's nullable API type.
    assert!(
        analysis.is_ok(),
        "present target-count and inexactness predicates must compose: {:?}",
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
