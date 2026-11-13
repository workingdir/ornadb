//! Deterministic step-growth proof for the std operations that moved to Orna
//! source in stage S5 (design section 5.2).
//!
//! The evaluator counts steps through `StepBudget`; this harness evaluates each
//! converted linear operation through the pinned `std.collection` bodies with a
//! caller-owned budget and asserts `steps(4n) / steps(n) <= 4.3`. An O(n^2)
//! body would show 16x. `ORNA_STD_SCALE_ROWS` overrides the base size.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Instant;

use num_bigint::BigInt;
use orna_check::{Catalogue, ModuleInput};
use orna_evaluator::{
    Environment, Functions, Limits, PureFunction, StandardLayer, StepBudget,
    evaluate_with_functions_and_budget, reference_standard_sources,
};
use orna_intrinsics::{Dispatch, catalogue};
use orna_project_repl::AdmittedReplSession;
use orna_syntax::parse_module_with_file;
use orna_syntax_ast::Declaration;
use orna_value::{CanonicalValue, Raw};

const RATIO_LIMIT: u64 = 43; // 4.3x, compared as tenths
const DEFAULT_BASE_ROWS: usize = 1024;

const OPERATIONS: &[(&str, &str)] = &[
    ("chunk", "std.collection.chunk(xs, 4)"),
    (
        "flatten",
        "std.collection.flatten(std.collection.chunk(xs, 4))",
    ),
    (
        "partition",
        "std.collection.partition(xs, value => value % 2 == 0)",
    ),
    ("zip", "std.collection.zip(xs, xs)"),
    ("zip_exact", "std.collection.zip_exact(xs, xs)"),
    ("unique", "std.collection.unique(xs)"),
    ("pairs", "std.collection.pairs(xs)"),
    ("window", "std.collection.window(xs, 4)"),
    (
        "split_when",
        "std.collection.split_when(xs, value => value % 2 == 0)",
    ),
];

fn collection_source() -> String {
    reference_standard_sources()
        .into_iter()
        .find(|(path, _)| path == "std/collection.orna")
        .expect("the pinned collection module is published")
        .1
}

fn standard_functions() -> Functions {
    let source = collection_source();
    let parsed = parse_module_with_file(&source, "std/collection.orna");
    assert!(parsed.is_ok(), "{:#?}", parsed.diagnostics);
    let mut functions = BTreeMap::new();
    for item in parsed.value.items {
        if let Declaration::Function { signature, body } = item.declaration {
            functions.insert(
                format!("std.collection.{}", signature.name),
                PureFunction {
                    parameters: signature.parameters,
                    body,
                    environment: Environment::new(),
                },
            );
        }
    }
    let mut intrinsics = BTreeMap::new();
    for intrinsic in catalogue() {
        if matches!(intrinsic.dispatch, Dispatch::Native) && intrinsic.pins.is_current() {
            intrinsics.insert(
                format!("{}.{}", intrinsic.module.path(), intrinsic.name),
                intrinsic.id,
            );
        }
    }
    let layer = StandardLayer::new(functions, BTreeMap::new(), intrinsics);
    Functions::with_standard(Arc::new(layer))
}

fn body_for(expression: &str) -> orna_syntax_ast::Expr {
    let source = format!("pub fn run() = {expression};\n");
    let parsed = parse_module_with_file(&source, "scale-run.orna");
    assert!(parsed.is_ok(), "{:#?}", parsed.diagnostics);
    for item in parsed.value.items {
        if let Declaration::Function { signature, body } = item.declaration
            && signature.name == "run"
        {
            return body;
        }
    }
    panic!("run body missing");
}

fn environment(rows: usize) -> Environment {
    let mut environment = Environment::new();
    let values = (0..rows)
        .map(|value| Raw::Int(BigInt::from(value)))
        .collect();
    environment.insert(
        "xs".into(),
        CanonicalValue::new(Raw::Array(values)).expect("canonical list"),
    );
    environment
}

fn limits(rows: usize) -> Limits {
    Limits {
        max_collection_items: rows.saturating_mul(8).saturating_add(4096),
        max_steps: rows as u64 * 2_000 + 10_000_000,
        ..Limits::default()
    }
}

/// Steps consumed by one expression at `rows`, read from the caller-owned
/// activation budget. The evaluator restores the budget to its remaining
/// capacity, so the difference is the exact deterministic step count.
fn steps(functions: &Functions, rows: usize, expression: &str) -> u64 {
    let body = body_for(expression);
    let environment = environment(rows);
    let limits = limits(rows);
    let mut budget = StepBudget::new(limits.max_steps);
    evaluate_with_functions_and_budget(&body, &environment, functions, limits, &mut budget)
        .unwrap_or_else(|error| panic!("{expression} at {rows} rows failed: {}", error.code()));
    limits.max_steps - budget.remaining()
}

fn base_rows() -> usize {
    std::env::var("ORNA_STD_SCALE_ROWS")
        .ok()
        .and_then(|value| value.parse().ok())
        .filter(|rows| *rows >= 8)
        .unwrap_or(DEFAULT_BASE_ROWS)
}

#[test]
fn converted_linear_operations_grow_linearly() {
    let functions = standard_functions();
    let base = base_rows();
    let large = base.saturating_mul(4);
    for (name, expression) in OPERATIONS {
        let steps_base = steps(&functions, base, expression);
        let steps_large = steps(&functions, large, expression);
        eprintln!(
            "ORNA-STD-SCALE op={name} n={base} steps={steps_base} n={large} steps={steps_large} ratio={:.2}",
            steps_large as f64 / steps_base as f64
        );
        assert!(
            steps_large * 10 <= steps_base * RATIO_LIMIT,
            "{name}: steps grew {steps_base} -> {steps_large} between {base} and {large} rows, \
             above the 4.3x linear bound"
        );
    }
}

#[test]
#[ignore = "advisory wall clock; run with --ignored --nocapture"]
fn converted_linear_operations_wall_clock() {
    let functions = standard_functions();
    let rows = base_rows().saturating_mul(4);
    for (name, expression) in OPERATIONS {
        let body = body_for(expression);
        let environment = environment(rows);
        let limits = limits(rows);
        let mut budget = StepBudget::new(limits.max_steps);
        let started = Instant::now();
        evaluate_with_functions_and_budget(&body, &environment, &functions, limits, &mut budget)
            .unwrap_or_else(|error| panic!("{expression} at {rows} rows failed: {}", error.code()));
        eprintln!(
            "ORNA-STD-SCALE-WALL op={name} n={rows} ms={}",
            started.elapsed().as_millis()
        );
    }
}

/// S5 acceptance: a marker leaf executed directly fails with
/// `ORNA-EVAL-UNSUPPORTED` instead of returning `[]` or recursing. The body is
/// extracted from the pinned `std/collection.orna` marker so the test cannot
/// drift from the source convention.
#[test]
fn pinned_marker_leaf_fails_with_unsupported() {
    let source = collection_source();
    let start = source
        .find("fn __group_by")
        .expect("the pinned group_by marker leaf");
    let equals = source[start..]
        .find('=')
        .map(|offset| start + offset)
        .expect("marker body assignment");
    let semicolon = source[equals..]
        .find(';')
        .map(|offset| equals + offset)
        .expect("marker body terminator");
    let body = source[equals + 1..semicolon].trim();
    assert!(
        body.contains("ORNA-EVAL-UNSUPPORTED"),
        "the pinned marker body names its failure code: {body}"
    );
    let module =
        format!("pub fn marker_group_by<T, K>(values: [T], key: fn(T): K): [(K, [T])] = {body};\n");
    let mut session = AdmittedReplSession::from_catalogue(
        &[ModuleInput::new("marker.orna", module)],
        Catalogue::authoritative_core(),
        std::iter::empty::<(String, String)>(),
        Limits::default(),
    )
    .expect("marker module admits");
    let error = session
        .submit("marker.marker_group_by([1, 2], value => value)")
        .expect_err("a marker leaf must fail, not return [] or recurse");
    assert_eq!(error.code(), "ORNA-EVAL-UNSUPPORTED", "{error:?}");
}
