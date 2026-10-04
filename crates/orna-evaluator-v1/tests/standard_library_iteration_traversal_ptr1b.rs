use std::collections::BTreeMap;

use orna_evaluator_v1::{Environment, Functions, Limits, PureFunction, evaluate_with_functions};
use orna_foundation_v1::CanonicalValue;
use orna_syntax_v1::{Declaration, parse_expression, parse_module};
use orna_value_v1::Raw;

const MODULES: [&str; 2] = ["std/collection.orna", "std/iteration.orna"];

fn functions() -> Functions {
    let mut functions = BTreeMap::new();
    for (path, source) in orna_standard::reference_standard_sources_v1()
        .into_iter()
        .filter(|(path, _)| MODULES.contains(&path.as_str()))
    {
        add_module_functions(&mut functions, &path, &source);
    }
    for source in [
        include_str!("fixtures/iteration-neighbors-ptr1b.orna"),
        include_str!("fixtures/iteration-target-edges-ptr1b.orna"),
    ] {
        add_module_functions(&mut functions, "iteration_proof.orna", source);
    }
    functions
}

fn add_module_functions(functions: &mut Functions, path: &str, source: &str) {
    let parsed = parse_module(source);
    assert!(parsed.is_ok(), "{path}: {:#?}", parsed.diagnostics);
    let module = path
        .strip_prefix("std/")
        .and_then(|path| path.strip_suffix(".orna"))
        .map(str::to_owned)
        .unwrap_or_else(|| {
            path.strip_suffix(".orna")
                .expect("proof module path has an .orna suffix")
                .to_owned()
        })
        .replace('/', ".");
    for item in parsed.value.items {
        if let Declaration::Function { signature, body } = item.declaration {
            let name = format!("{module}.{}", signature.name);
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
                "duplicate function {name}"
            );
        }
    }
}

fn assert_true_fixture(fixture: &str) {
    let functions = functions();
    for expression in fixture.split("&&").map(str::trim) {
        let parsed = parse_expression(expression);
        assert!(parsed.is_ok(), "{expression}: {:#?}", parsed.diagnostics);
        let actual = evaluate_with_functions(
            &parsed.value,
            &Environment::new(),
            &functions,
            Limits::default(),
        )
        .unwrap_or_else(|error| {
            panic!(
                "iteration behavior proof failed for `{expression}`: {}",
                error.code()
            )
        });
        assert_eq!(
            actual,
            CanonicalValue::new(Raw::Bool(true)).expect("true is canonical"),
            "iteration behavior fixture failed: {expression}",
        );
    }
}

#[test]
fn finite_folds_preserve_order_and_stop_after_the_boundary() {
    assert_true_fixture(include_str!("fixtures/iteration-fold-ptr1b.orna"));
}

#[test]
fn graph_walks_keep_order_visit_cycles_once_and_short_circuit_reachability() {
    assert_true_fixture(include_str!("fixtures/iteration-traversal-ptr1b.orna"));
}
