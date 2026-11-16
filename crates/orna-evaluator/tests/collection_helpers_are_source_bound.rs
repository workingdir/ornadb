//! Pinned collection helpers run their Orna bodies, never a Rust substitute.
//!
//! The catalogue marks each source-owned collection helper `SourceOnly`. Such
//! a helper has no native arm, so an unresolved `std.collection.<name>` call
//! must not evaluate, and the admitted Orna body is the only route that can
//! produce its value. `native_siblings_still_evaluate` keeps the probe honest:
//! native helpers must evaluate through the same unresolved spelling.

use orna_evaluator::{Environment, Limits, evaluate_expression};
use orna_intrinsics::{Dispatch, Module, lookup};

/// Collection helpers the catalogue dispatches to their Orna body.
const SOURCE_BOUND: [&str; 9] = [
    "chunk",
    "flatten",
    "partition",
    "zip",
    "zip_exact",
    "unique",
    "pairs",
    "window",
    "split_when",
];

/// Collection helpers with a native arm, used as a positive control.
const NATIVE: [&str; 2] = ["map", "count"];

fn program(name: &str) -> String {
    match name {
        "chunk" => "std.collection.chunk([1, 2, 3], 2)".to_owned(),
        "flatten" => "std.collection.flatten([[1], [2]])".to_owned(),
        "partition" => "std.collection.partition([1, 2], value => value > 1)".to_owned(),
        "zip" => "std.collection.zip([1, 2], [3, 4])".to_owned(),
        "zip_exact" => "std.collection.zip_exact([1, 2], [3, 4])".to_owned(),
        "unique" => "std.collection.unique([1, 1, 2])".to_owned(),
        "pairs" => "std.collection.pairs([1, 2, 3])".to_owned(),
        "window" => "std.collection.window([1, 2, 3], 2, 1)".to_owned(),
        "split_when" => "std.collection.split_when([1, 2, 3], value => value > 1)".to_owned(),
        "map" => "std.collection.map([1, 2], value => value + 1)".to_owned(),
        "count" => "std.collection.count([1, 2])".to_owned(),
        other => panic!("no probe program for `{other}`"),
    }
}

#[test]
fn source_bound_helpers_are_catalogue_source_only() {
    for name in SOURCE_BOUND {
        let intrinsic = lookup(Module::Collection, name)
            .unwrap_or_else(|| panic!("`std.collection.{name}` is missing from the catalogue"));
        assert_eq!(
            intrinsic.dispatch,
            Dispatch::SourceOnly,
            "`std.collection.{name}` must run its Orna body"
        );
    }
}

#[test]
fn source_bound_helpers_do_not_evaluate_natively() {
    for name in SOURCE_BOUND {
        let result = evaluate_expression(&program(name), &Environment::new(), Limits::default());
        assert!(
            result.is_err(),
            "`std.collection.{name}` evaluated without its Orna body: {result:?}"
        );
    }
}

#[test]
fn native_siblings_still_evaluate() {
    for name in NATIVE {
        let result = evaluate_expression(&program(name), &Environment::new(), Limits::default());
        assert!(
            result.is_ok(),
            "native `std.collection.{name}` must evaluate through the unresolved spelling: {result:?}"
        );
    }
}

/// The native/source split is decided by the resolved catalogue entry, never
/// by the shape of the admitted body. `map` once evaluated its Orna body when
/// its AST contained a `while` loop (e05e541e7, removed by a741ef33e); that
/// sniff is gone, so `map` must bind to its `Native` entry unconditionally.
#[test]
fn native_siblings_are_catalogue_native() {
    for name in NATIVE {
        let intrinsic = lookup(Module::Collection, name)
            .unwrap_or_else(|| panic!("`std.collection.{name}` is missing from the catalogue"));
        assert_eq!(
            intrinsic.dispatch,
            Dispatch::Native,
            "`std.collection.{name}` must bind its native arm by resolved identity"
        );
    }
}
