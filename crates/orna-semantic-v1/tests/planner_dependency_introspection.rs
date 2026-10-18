use orna_semantic_v1::{Catalogue, ModuleInput, analyze_with_catalogue};

const SYS_INTROSPECTION_SOURCE: &str =
    include_str!("fixtures/planner-dependency-introspection.orna");

/// ORNA-PLAN-002: `sys.explain` exposes plans through the typed planner API.
/// ORNA-DEP-001/002: dependency queries use typed `sys.Object` references and call-kind filters.
#[test]
fn portable_sys_sources_resolve_planner_and_dependency_introspection() {
    let analysis = analyze_with_catalogue(
        &[ModuleInput::new(
            "planner-dependencies.orna",
            SYS_INTROSPECTION_SOURCE,
        )],
        &Catalogue::authoritative_fixture(),
    );
    assert!(analysis.is_ok(), "{:?}", analysis.diagnostics);
    for name in [
        "outgoing",
        "incoming_calls",
        "explain_query",
        "explain_query_explicit",
        "explain_query_piped",
        "explain_function",
    ] {
        assert!(
            analysis
                .modules
                .values()
                .any(|module| module.exports.contains_key(name)),
            "missing export {name}"
        );
    }
}
