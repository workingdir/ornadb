//! Dimension and affine codes of `+` and `sum` (hir-host.md B5). `E5001` is reported when a `Duration` meets a
//! quantity that is not a time, and when two quantities of different dimensions meet. `E5002` is reported when two
//! affine points meet under `+`, and `E5003` when a fold sums over them.
//!
//! `kWh` and `C` are the embedded `core.units` units (`C` is `K offset 273.15 affine`). The programs below name them
//! without a `use`.

use super::tests::{program, test_accepted, test_module};
use super::typeck;
use crate::Program;
use crate::diag::{AFFINE_POINT_OPERATOR, AFFINE_POINT_SUM, QUANTITY_DIMENSIONS};

/// The diagnostic codes of the test module, in report order.
fn codes(program: &Program) -> Vec<String> {
    program.modules[test_module(program)]
        .diagnostics
        .iter()
        .map(|diagnostic| diagnostic.code.to_string())
        .collect()
}

/// Types `source` and returns the diagnostic codes of the test module.
fn check(source: &str) -> Vec<String> {
    let mut program = program(source);
    typeck(&mut program);
    codes(&program)
}

#[test]
fn duration_plus_energy_is_e5001() {
    let found = check("pub fn bad() = 90.days + 4.kWh;\n");
    assert_eq!(found, vec![QUANTITY_DIMENSIONS.to_string()], "{found:?}");
}

#[test]
fn an_energy_literal_is_a_quantity_of_its_core_unit() {
    let mut program = program("pub fn run() = 4.kWh;\n");
    let summary = typeck(&mut program);
    assert!(test_accepted(&program, &summary), "{summary:?}");
}

#[test]
fn point_plus_point_of_an_affine_unit_is_e5002() {
    let found = check("pub fn bad() = 20.C + 5.C;\n");
    assert_eq!(found, vec![AFFINE_POINT_OPERATOR.to_string()], "{found:?}");
}

#[test]
fn sum_over_an_affine_unit_is_e5003() {
    let found = check("pub fn bad(values: [Float<C>]) = values | sum;\n");
    assert_eq!(found, vec![AFFINE_POINT_SUM.to_string()], "{found:?}");
}
