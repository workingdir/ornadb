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
    add_module_functions(
        &mut functions,
        "iteration_proof.orna",
        include_str!("fixtures/iteration-traversal-ptr1b.orna"),
    );
    for name in [
        "std.collection.count",
        "std.iteration.fold",
        "std.iteration.depth_first",
        "iteration_proof.traversal_behavior",
    ] {
        assert!(
            functions.contains_key(name),
            "missing proof function {name}"
        );
    }
    functions
}

fn add_module_functions(functions: &mut Functions, path: &str, source: &str) {
    let parsed = parse_module(source);
    assert!(parsed.is_ok(), "{path}: {:#?}", parsed.diagnostics);
    let module = if let Some(module) = path
        .strip_prefix("std/")
        .and_then(|path| path.strip_suffix(".orna"))
    {
        format!("std.{}", module.replace('/', "."))
    } else {
        path.strip_suffix(".orna")
            .expect("proof module path has an .orna suffix")
            .to_owned()
    };
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
    let parsed = parse_expression(fixture);
    assert!(parsed.is_ok(), "{:#?}", parsed.diagnostics);
    let actual = evaluate_with_functions(
        &parsed.value,
        &Environment::new(),
        &functions,
        Limits::default(),
    )
    .unwrap_or_else(|error| panic!("iteration behavior proof failed: {}", error.code()));
    assert_eq!(
        actual,
        CanonicalValue::new(Raw::Bool(true)).expect("true is canonical"),
        "iteration behavior fixture failed: {fixture}",
    );
}

#[test]
fn finite_folds_preserve_order_and_stop_after_the_boundary() {
    assert_true_fixture(include_str!("fixtures/iteration-fold-ptr1b.orna"));
}

#[test]
fn graph_walks_keep_order_visit_cycles_once_and_short_circuit_reachability() {
    // Nested source calls use enough evaluator frames to exceed the test
    // harness's default worker stack for even this small cyclic graph.
    std::thread::Builder::new()
        .name("iteration traversal proof".into())
        .stack_size(16 * 1024 * 1024)
        .spawn(|| assert_true_fixture(include_str!("fixtures/iteration-traversal-call-ptr1b.orna")))
        .expect("traversal proof thread starts")
        .join()
        .expect("traversal behavior proof succeeds");
}
