use std::collections::BTreeMap;

use orna_evaluator_v1::{
    Environment, Functions, Limits, NominalDefinition, NominalDefinitions, NominalField,
    PureFunction, evaluate_with_functions_and_nominals,
};
use orna_foundation_v1::CanonicalValue;
use orna_syntax_v1::{Declaration, parse_expression, parse_module};
use orna_value_v1::Raw;

fn canonical(raw: Raw) -> CanonicalValue {
    CanonicalValue::new(raw).expect("fixture value is canonical")
}

fn pinned_functions() -> Functions {
    let mut functions = BTreeMap::new();
    for (path, source) in orna_standard::reference_standard_sources_v1() {
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
    functions
}

fn iterator_nominals() -> NominalDefinitions {
    NominalDefinitions::from([(
        "std.iterator.Iterator".into(),
        NominalDefinition::new(
            [0x71; 16],
            Some("std.iterator".into()),
            vec![NominalField::public([0x72; 16], "pull")],
        ),
    )])
}

fn assert_true_fixture(fixture: &str) {
    let functions = pinned_functions();
    let nominals = iterator_nominals();
    let parsed = parse_expression(fixture);
    assert!(parsed.is_ok(), "{:#?}", parsed.diagnostics);
    let result = evaluate_with_functions_and_nominals(
        &parsed.value,
        &Environment::new(),
        &functions,
        &nominals,
        Limits::default(),
    )
    .unwrap_or_else(|error| panic!("iterator consumer proof failed: {}", error.code()));
    assert_eq!(
        result,
        canonical(Raw::Bool(true)),
        "iterator consumer fixture failed: {fixture}"
    );
}

#[test]
fn predicate_consumers_short_circuit_and_preserve_first_match() {
    assert_true_fixture(include_str!(
        "fixtures/stdlib-iterator-consumers-short-circuit-joccy.orna"
    ));
}

#[test]
fn iterator_consumers_count_reduce_and_map_results() {
    assert_true_fixture(include_str!(
        "fixtures/stdlib-iterator-consumers-count-reduce-joccy.orna"
    ));
}

#[test]
fn iterator_partition_is_stable_and_handles_empty_input() {
    assert_true_fixture(include_str!(
        "fixtures/stdlib-iterator-consumers-partition-joccy.orna"
    ));
}
