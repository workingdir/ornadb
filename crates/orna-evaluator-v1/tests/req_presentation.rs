use std::collections::BTreeMap;

use orna_evaluator_v1::{
    Environment, Functions, Limits, PureFunction, evaluate_expression_with_functions,
};
use orna_foundation_v1::CanonicalValue;

fn pinned_functions() -> Functions {
    let mut functions = BTreeMap::new();
    for (path, source) in orna_evaluator_v1::reference_standard_sources() {
        let parsed = orna_syntax_v1::parse_module(&source);
        assert!(parsed.is_ok(), "{path}: {:#?}", parsed.diagnostics);
        let module = path
            .strip_prefix("std/")
            .and_then(|path| path.strip_suffix(".orna"))
            .expect("pinned std paths have std/ prefix and .orna suffix")
            .replace('/', ".");
        for item in parsed.value.items {
            if let orna_syntax_v1::Declaration::Function { signature, body } = item.declaration {
                let name = format!("std.{module}.{}", signature.name);
                assert!(
                    functions
                        .insert(
                            name.clone(),
                            PureFunction {
                                parameters: signature.parameters,
                                body,
                                environment: Environment::new(),
                            },
                        )
                        .is_none(),
                    "duplicate pinned standard function {name}"
                );
            }
        }
    }
    functions
}

fn evaluate(source: &str) -> Result<CanonicalValue, orna_evaluator_v1::EvaluationError> {
    evaluate_expression_with_functions(
        source,
        &Environment::new(),
        &pinned_functions(),
        Limits::default(),
    )
}

/// ORNA-CODEC-001: the real evaluator exposes typed `std.encoding.orna.encode` and `decode(..., as: T)` operations.
#[test]
fn orna_codec_round_trips_a_typed_integer_through_the_language_entry_point() {
    assert_eq!(
        evaluate("std.encoding.orna.decode(std.encoding.orna.encode(42), as: Int)"),
        evaluate("42"),
    );
}

/// ORNA-CODEC-003: canonical Orna encoding is deterministic, unambiguous, full-precision, and round-trippable.
#[test]
fn orna_codec_is_deterministic_unambiguous_and_preserves_large_integer_precision() {
    let large_integer = "123456789012345678901234567890123456789";
    let encode_large = format!("std.encoding.orna.encode({large_integer})");
    assert_eq!(evaluate(&encode_large), evaluate(&encode_large));

    let encode_as_string = format!("std.encoding.orna.encode(\"{large_integer}\")");
    assert_ne!(evaluate(&encode_large), evaluate(&encode_as_string));

    let round_trip = format!(
        "std.encoding.orna.decode(std.encoding.orna.encode({large_integer}), as: Int)"
    );
    assert_eq!(evaluate(&round_trip), evaluate(large_integer));
}
