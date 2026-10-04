use std::collections::BTreeMap;

use orna_evaluator_v1::{
    Environment, Functions, Limits, NominalDefinition, NominalDefinitions, NominalField,
    PureFunction, evaluate_with_functions_and_nominals,
};
use orna_foundation_v1::CanonicalValue;
use orna_syntax_v1::{Declaration, parse_expression, parse_module};
use orna_value_v1::Raw;

const STREAM_ADAPTER_MODULES: [&str; 5] = [
    "std/collection.orna",
    "std/iterator.orna",
    "std/iterator/adapters.orna",
    "std/stream.orna",
    "std/stream/adapters.orna",
];

fn canonical(raw: Raw) -> CanonicalValue {
    CanonicalValue::new(raw).expect("expected stream adapter value is canonical")
}

fn pinned_runtime() -> (Functions, NominalDefinitions) {
    let mut functions = BTreeMap::new();
    for (path, source) in orna_standard::reference_standard_sources_v1()
        .into_iter()
        .filter(|(path, _)| STREAM_ADAPTER_MODULES.contains(&path.as_str()))
    {
        let parsed = parse_module(&source);
        assert!(parsed.is_ok(), "{path}: {:#?}", parsed.diagnostics);
        let module = path
            .strip_prefix("std/")
            .and_then(|path| path.strip_suffix(".orna"))
            .expect("pinned std paths have a std/ prefix and .orna suffix")
            .replace('/', ".");
        for item in parsed.value.items {
            if let Declaration::Function { signature, body } = item.declaration {
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

    let nominal_definitions = NominalDefinitions::from([(
        "std.iterator.Iterator".into(),
        NominalDefinition::new(
            [0x71; 16],
            Some("std.iterator".into()),
            vec![NominalField::public([0x72; 16], "pull")],
        ),
    )]);
    (functions, nominal_definitions)
}

fn assert_fixture(fixture: &str, expected: Raw) {
    let (functions, nominal_definitions) = pinned_runtime();
    let parsed = parse_expression(fixture);
    assert!(parsed.is_ok(), "{fixture}: {:#?}", parsed.diagnostics);
    let actual = evaluate_with_functions_and_nominals(
        &parsed.value,
        &Environment::new(),
        &functions,
        &nominal_definitions,
        Limits::default(),
    )
    .unwrap_or_else(|error| {
        panic!("stream adapter fixture failed for `{fixture}`: {}", error.code())
    });
    assert_eq!(actual, canonical(expected), "{fixture}");
}

#[test]
fn iterator_adapter_materializes_transformed_values_in_pull_order() {
    assert_fixture(
        include_str!("fixtures/stdlib-stream-adapters-from-iterator-uttqw.orna"),
        Raw::Array(vec![Raw::Int(6.into()), Raw::Int(12.into())]),
    );
}

#[test]
fn sequence_adapter_calls_its_finite_source_factory() {
    assert_fixture(
        include_str!("fixtures/stdlib-stream-adapters-from-sequence-uttqw.orna"),
        Raw::Array(vec![
            Raw::Int(4.into()),
            Raw::Int(1.into()),
            Raw::Int(6.into()),
        ]),
    );
}

#[test]
fn iterator_batch_adapter_retains_order_and_the_short_final_batch() {
    assert_fixture(
        include_str!("fixtures/stdlib-stream-adapters-iterator-batches-uttqw.orna"),
        Raw::Array(vec![
            Raw::Array(vec![Raw::Int(4.into()), Raw::Int(1.into())]),
            Raw::Array(vec![Raw::Int(6.into()), Raw::Int(2.into())]),
            Raw::Array(vec![Raw::Int(8.into())]),
        ]),
    );
}

#[test]
fn sequence_batch_adapter_handles_an_empty_finite_cursor() {
    assert_fixture(
        include_str!("fixtures/stdlib-stream-adapters-empty-batches-uttqw.orna"),
        Raw::Array(Vec::new()),
    );
}

#[test]
fn buffered_batches_preserve_batch_boundaries_and_all_values() {
    assert_fixture(
        include_str!("fixtures/stdlib-stream-adapters-buffered-batches-uttqw.orna"),
        Raw::Array(vec![
            Raw::Array(vec![Raw::Int(4.into()), Raw::Int(1.into())]),
            Raw::Array(vec![Raw::Int(6.into()), Raw::Int(2.into())]),
            Raw::Array(vec![Raw::Int(8.into())]),
        ]),
    );
}

#[test]
fn iterator_batch_adapter_rejects_nonpositive_batch_sizes() {
    let (functions, nominal_definitions) = pinned_runtime();
    let fixture = include_str!("fixtures/stdlib-stream-adapters-invalid-batch-uttqw.orna");
    let parsed = parse_expression(fixture);
    assert!(parsed.is_ok(), "{fixture}: {:#?}", parsed.diagnostics);
    let error = evaluate_with_functions_and_nominals(
        &parsed.value,
        &Environment::new(),
        &functions,
        &nominal_definitions,
        Limits::default(),
    )
    .expect_err("batch size zero is invalid");
    assert_eq!(error.code(), "ORNA-EVAL-VALUE");
}
