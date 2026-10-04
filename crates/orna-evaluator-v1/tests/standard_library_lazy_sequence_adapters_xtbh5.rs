use std::collections::BTreeMap;

use orna_evaluator_v1::{
    Environment, Functions, Limits, NominalDefinition, NominalDefinitions, NominalField,
    PureFunction, evaluate_with_functions_and_nominals,
};
use orna_foundation_v1::CanonicalValue;
use orna_syntax_v1::{Declaration, parse_expression, parse_module};
use orna_value_v1::Raw;

const SEQUENCE_MODULES: [&str; 5] = [
    "std/collection.orna",
    "std/iterator.orna",
    "std/iterator/adapters.orna",
    "std/lazy.orna",
    "std/lazy/adapters.orna",
];

fn canonical(raw: Raw) -> CanonicalValue {
    CanonicalValue::new(raw).expect("fixture value is canonical")
}

fn pinned_runtime() -> (Functions, NominalDefinitions) {
    let mut functions = BTreeMap::new();
    for (path, source) in orna_standard::reference_standard_sources_v1()
        .into_iter()
        .filter(|(path, _)| SEQUENCE_MODULES.contains(&path.as_str()))
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

    let replay_source = include_str!("fixtures/stdlib-lazy-sequence-replay-xtbh5.orna");
    let replay_module = parse_module(replay_source);
    assert!(replay_module.is_ok(), "{:#?}", replay_module.diagnostics);
    for item in replay_module.value.items {
        if let Declaration::Function { signature, body } = item.declaration {
            let name = format!("lazy_sequence_proof.{}", signature.name);
            functions.insert(
                name,
                PureFunction {
                    parameters: signature.parameters,
                    body,
                    environment: Environment::new(),
                },
            );
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

fn assert_true_fixture(fixture: &str) {
    let (functions, nominal_definitions) = pinned_runtime();
    for expression in fixture.split("&&").map(str::trim) {
        let parsed = parse_expression(expression);
        assert!(parsed.is_ok(), "{expression}: {:#?}", parsed.diagnostics);
        let result = evaluate_with_functions_and_nominals(
            &parsed.value,
            &Environment::new(),
            &functions,
            &nominal_definitions,
            Limits::default(),
        )
        .unwrap_or_else(|error| {
            panic!(
                "lazy sequence proof failed for `{expression}`: {}",
                error.code()
            )
        });
        assert_eq!(result, canonical(Raw::Bool(true)), "{expression}");
    }
}

#[test]
fn lazy_map_and_filter_preserve_values_and_order() {
    assert_true_fixture(include_str!(
        "fixtures/stdlib-lazy-sequence-transform-xtbh5.orna"
    ));
}

#[test]
fn lazy_flat_map_chain_and_zip_compose_in_source_order() {
    assert_true_fixture(include_str!(
        "fixtures/stdlib-lazy-sequence-compose-xtbh5.orna"
    ));
}

#[test]
fn lazy_take_bounds_infinite_sources_and_drop_skips_the_prefix() {
    assert_true_fixture(include_str!(
        "fixtures/stdlib-lazy-sequence-bounds-xtbh5.orna"
    ));
}

#[test]
fn sequence_pipeline_construction_and_zero_take_do_not_pull_elements() {
    assert_true_fixture(include_str!(
        "fixtures/stdlib-lazy-sequence-no-pull-xtbh5.orna"
    ));
}

#[test]
fn forcing_the_same_sequence_factory_replays_its_pure_pipeline() {
    assert_true_fixture(include_str!(
        "fixtures/stdlib-lazy-sequence-replay-call-xtbh5.orna"
    ));
}
