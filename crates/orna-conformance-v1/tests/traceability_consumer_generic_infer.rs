//! Focused consumer and generic evidence against the frozen 1.0 gap tails.

use orna_semantic_v1::{Catalogue, ModuleInput, Type, analyze_with_catalogue};

const MULTIPLE_ROOTS: &str = include_str!("fixtures/streams-pa0p-multiple-roots.orna");
const GENERIC_INFERRED_ARGUMENT: &str =
    include_str!("fixtures/traceability-generic-inferred-argument.orna");

fn analyze(source: &str) -> orna_semantic_v1::Analysis {
    analyze_with_catalogue(
        &[ModuleInput::new("main.orna", source)],
        &Catalogue::authoritative_fixture(),
    )
}

fn diagnostic_text(result: &orna_semantic_v1::Analysis) -> String {
    result
        .diagnostics
        .iter()
        .map(|diagnostic| format!("{}: {}", diagnostic.code(), diagnostic.message()))
        .collect::<Vec<_>>()
        .join(" | ")
}

// Specified: ORNA-CONSUMER-005. Exists: the checked-in multi-root fixture and
// this semantic test. Passed: only the named root-count diagnostic boundary.
#[test]
fn durable_consumer_multiple_roots_gets_extraction_diagnostic() {
    let result = analyze(MULTIPLE_ROOTS);
    assert!(
        result.diagnostics.iter().any(|diagnostic| {
            diagnostic.message().starts_with(
                "a durable consumer function may own only one checkpointed source root",
            )
        }),
        "ORNA-CONSUMER-005 boundary: {}",
        diagnostic_text(&result)
    );
}

// Specified: ORNA-GENERIC-001. Exists: a real generic-call source fixture.
// Passed: local type-argument inference and the exported result type only.
#[test]
fn local_generic_call_infers_its_type_argument() {
    let result = analyze(GENERIC_INFERRED_ARGUMENT);
    assert!(result.diagnostics.is_empty(), "{}", diagnostic_text(&result));
    let module = result.modules.values().next().expect("main module");
    assert_eq!(
        module.exports.get("answer").map(|function| &function.ty),
        Some(&Type::Function {
            parameters: Vec::new(),
            parameter_names: Some(Vec::new()),
            result: Box::new(Type::Named("Ranked".into())),
            default_parameters: Default::default(),
        })
    );
}

// Specified: ORNA-GENERIC-005. Exists: the fixture's `Display + Order` bound.
// Passed: declaration and local call typecheck with both bounds.
#[test]
fn generic_function_accepts_multiple_protocol_bounds() {
    let result = analyze(GENERIC_INFERRED_ARGUMENT);
    assert!(result.diagnostics.is_empty(), "{}", diagnostic_text(&result));
    assert!(result.modules.values().next().unwrap().exports.contains_key("answer"));
}
