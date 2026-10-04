use std::collections::BTreeMap;

use orna_evaluator_v1::{
    Environment, Functions, Limits, NominalDefinitions, PureFunction,
    evaluate_with_functions_and_nominals,
};
use orna_foundation_v1::CanonicalValue;
use orna_syntax_v1::{Declaration, parse_expression, parse_module};
use orna_value_v1::Raw;

const MEMO_MODULES: [&str; 3] = ["std/collection.orna", "std/map.orna", "std/memo.orna"];

fn pinned_runtime() -> (Functions, NominalDefinitions) {
    let mut functions = BTreeMap::new();
    for (path, source) in orna_standard::reference_standard_sources_v1()
        .into_iter()
        .filter(|(path, _)| MEMO_MODULES.contains(&path.as_str()))
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

    let proof_source = include_str!("fixtures/stdlib-memoized-functions-82yit.orna");
    let proof_module = parse_module(proof_source);
    assert!(proof_module.is_ok(), "{:#?}", proof_module.diagnostics);
    for item in proof_module.value.items {
        if let Declaration::Function { signature, body } = item.declaration {
            let name = format!("memoized_proof.{}", signature.name);
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
                "duplicate proof function {name}"
            );
        }
    }

    (functions, NominalDefinitions::new())
}

fn assert_true_fixture(expression: &str) {
    let (functions, nominal_definitions) = pinned_runtime();
    let parsed = parse_expression(expression);
    assert!(parsed.is_ok(), "{expression}: {:#?}", parsed.diagnostics);
    let actual = evaluate_with_functions_and_nominals(
        &parsed.value,
        &Environment::new(),
        &functions,
        &nominal_definitions,
        Limits::default(),
    )
    .unwrap_or_else(|error| {
        panic!(
            "memoized function proof failed for `{expression}`: {}",
            error.code()
        )
    });
    assert_eq!(
        actual,
        CanonicalValue::new(Raw::Bool(true)).expect("true is canonical"),
        "{expression}"
    );
}

#[test]
fn keyed_memoization_reuses_a_cached_result() {
    assert_true_fixture("memoized_proof.keyed_hit()");
}

#[test]
fn memoized_null_values_are_distinct_from_cache_misses() {
    assert_true_fixture("memoized_proof.cached_null()");
}

#[test]
fn memoized_thunks_return_the_cached_value_without_forcing_again() {
    assert_true_fixture("memoized_proof.thunk_hit()");
}

#[test]
fn cache_misses_keys_and_invalidation_are_explicitly_threaded() {
    assert_true_fixture("memoized_proof.key_isolation_and_invalidation()");
}
