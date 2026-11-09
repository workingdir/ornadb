//! The deleted string-table machinery must not reappear.
//!
//! S2 replaces the module/name tables and AST-shape heuristics with the typed
//! `IntrinsicId` bound at admission. This test fails if any deleted symbol is
//! reintroduced into the evaluator source.

const EVALUATOR_SOURCE: &str = include_str!("../src/lib.rs");

const DELETED_SYMBOLS: &[&str] = &[
    "STANDARD_BINDING_MODULES",
    "StandardBindingKind",
    "StandardBindingModule",
    "RegisteredStandardBinding",
    "registered_standard_binding",
    "captured_timezone_snapshot_matches",
    "captured_collection_helper_is_loop",
    "captured_collection_binding_is_native",
    "legacy_root_wrapper_is_pinned",
    "LEGACY_ROOT_WRAPPER_OPERATIONS",
    "is_native_collection_binding",
    "math_name",
    "bits_name",
    "text_name",
    "stats_name",
    "time_name",
    "root_collection_name",
    "root_stream_name",
    "portable_collection_operation",
    "portable_statistics_operation",
    "statistics_function_operation",
    "relation_named_arguments",
    "named_arguments",
];

#[test]
fn deleted_string_table_symbols_are_absent() {
    for symbol in DELETED_SYMBOLS {
        assert!(
            !EVALUATOR_SOURCE.contains(symbol),
            "deleted string-table symbol `{symbol}` reappeared in the evaluator"
        );
    }
}

#[test]
fn dispatch_is_an_exhaustive_intrinsic_id_match() {
    assert!(
        EVALUATOR_SOURCE.contains("use orna_intrinsics::IntrinsicId as I;"),
        "dispatch must match on IntrinsicId"
    );
    assert!(
        EVALUATOR_SOURCE.contains("fn dispatch("),
        "the evaluator must keep one dispatch entry point"
    );
    assert!(
        EVALUATOR_SOURCE.contains("self.dispatch(id, name, values, callee, scope, depth)"),
        "call_native must dispatch on the bound id"
    );
}
