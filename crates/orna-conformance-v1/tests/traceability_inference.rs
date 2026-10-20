//! Focused successful and failing inference evidence on checked-in sources.

use orna_semantic_v1::{Catalogue, ModuleInput, Type, analyze_with_catalogue};
use orna_conformance_v1::AdmittedReplSession;
use orna_evaluator_v1::Limits;
use orna_foundation_v1::Value;

const INFER_SUCCESS: &str = include_str!("fixtures/traceability-infer-success.orna");
const INFER_UNDERCONSTRAINED: &str =
    include_str!("fixtures/semantic/underconstrained-lambda-field.orna");
const NUMERIC_CONTEXT: &str = include_str!(
    "fixtures/reference/examples/valid/numeric-literal-context.orna"
);

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

fn increment_type(result: &orna_semantic_v1::Analysis) -> &Type {
    &result
        .modules
        .values()
        .next()
        .expect("main module")
        .exports
        .get("increment")
        .expect("increment export")
        .ty
}

// Specified: ORNA-INFER-001, ORNA-FN-008. Exists: the checked-in unannotated function.
// Passed: analysis resolves it to a single static callable signature.
#[test]
fn omitted_parameter_and_return_annotations_are_inferred() {
    let result = analyze(INFER_SUCCESS);
    assert!(result.diagnostics.is_empty(), "{}", diagnostic_text(&result));
    assert!(matches!(
        increment_type(&result),
        Type::Function { parameters, result, .. }
            if parameters == &[Type::Int] && **result == Type::Int
    ));
}

// Specified: ORNA-INFER-002. Exists: the same real source fixture.
// Passed: exported inferred signature is concrete Int -> Int, with no fallback.
#[test]
fn inferred_export_does_not_fall_back_to_dynamic_any() {
    let result = analyze(INFER_SUCCESS);
    assert!(result.diagnostics.is_empty(), "{}", diagnostic_text(&result));
    assert!(matches!(
        increment_type(&result),
        Type::Function { parameters, result, .. }
            if parameters == &[Type::Int] && **result == Type::Int
    ));
}

// Specified: ORNA-INFER-003, ORNA-INFER-004. Exists: the same real source fixture,
// whose public function omits every annotation. Passed: inference succeeds with no
// diagnostic, so absent annotations are not a syntax or annotation error.
#[test]
fn inferable_function_signature_needs_no_annotations() {
    let result = analyze(INFER_SUCCESS);
    assert!(result.diagnostics.is_empty(), "{}", diagnostic_text(&result));
}

// Specified: ORNA-INFER-007. Exists: a fixture with an underconstrained
// exported lambda field. Passed: one targeted annotation diagnostic only.
#[test]
fn underconstrained_inference_requests_an_annotation() {
    let result = analyze(INFER_UNDERCONSTRAINED);
    let annotations = result
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.code() == "ORNA-S020-ANNOTATION")
        .collect::<Vec<_>>();
    assert_eq!(annotations.len(), 1, "{}", diagnostic_text(&result));
}

// Specified: ORNA-INFER-008. Exists: the frozen numeric-context example.
// Passed: contextual Decimal and explicit Float signatures remain distinct.
#[test]
fn contextual_numeric_inference_preserves_decimal_and_float_types() {
    let result = analyze(NUMERIC_CONTEXT);
    assert!(result.diagnostics.is_empty(), "{}", diagnostic_text(&result));
    let exports = &result.modules.values().next().expect("main module").exports;
    for (name, expected) in [
        ("decimal_value", Type::Decimal),
        ("float_value", Type::Float),
        ("explicit_float", Type::Float),
    ] {
        assert!(matches!(
            exports.get(name).map(|function| &function.ty),
            Some(Type::Function { result, .. }) if **result == expected
        ));
    }
}

// Specified: ORNA-INFER-006. Exists: two sources that differ only in literal data
// values. Passed: each analysis is repeatable, and both exported signatures are the
// same Int -> Int, so the signature does not depend on runtime data values.
#[test]
fn inferred_signature_is_independent_of_literal_data_values() {
    let small = analyze("pub fn increment(value) = value + 1;");
    let large = analyze("pub fn increment(value) = value + 99;");
    let repeated = analyze("pub fn increment(value) = value + 1;");
    for result in [&small, &large, &repeated] {
        assert!(result.diagnostics.is_empty(), "{}", diagnostic_text(result));
        assert!(matches!(
            increment_type(result),
            Type::Function { parameters, result, .. }
                if parameters == &[Type::Int] && **result == Type::Int
        ));
    }
    assert_eq!(increment_type(&repeated), increment_type(&small));
}

// Specified: ORNA-INFER-010. Exists: the same function written with and without a
// redundant parameter and return annotation, run through the admitted REPL path.
// Passed: both sessions produce the same successful result.
#[test]
fn redundant_annotation_does_not_change_successful_behavior() {
    let call = "increment(41)";
    let mut inferred = AdmittedReplSession::new(Limits::default());
    assert_eq!(inferred.submit("fn increment(value) = value + 1;").unwrap(), None);
    let inferred_result = inferred.submit(call).unwrap();

    let mut annotated = AdmittedReplSession::new(Limits::default());
    assert_eq!(
        annotated
            .submit("fn increment(value: Int): Int = value + 1;")
            .unwrap(),
        None
    );
    let annotated_result = annotated.submit(call).unwrap();

    assert_eq!(inferred_result, Some(Value::int(42.into())));
    assert_eq!(inferred_result, annotated_result);
}
