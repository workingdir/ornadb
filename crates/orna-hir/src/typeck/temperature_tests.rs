//! The `core.temperature` point/difference algebra (kernel-and-library.md listing 10). A point plus a difference is a
//! point; a point minus a point is a difference; a point plus a point has no instance and is rejected.

use super::tests::{program, test_accepted};
use super::typeck;

const USE: &str = "use core.temperature.{Temperature, Delta};\n";

#[test]
fn point_plus_difference_is_a_point() {
    let source = format!("{USE}pub fn warm(t: Temperature, d: Delta): Temperature = t + d;\n");
    let mut program = program(&source);
    let summary = typeck(&mut program);
    assert!(test_accepted(&program, &summary), "{summary:?}");
}

#[test]
fn point_minus_point_is_a_difference() {
    let source = format!("{USE}pub fn gap(a: Temperature, b: Temperature): Delta = a - b;\n");
    let mut program = program(&source);
    let summary = typeck(&mut program);
    assert!(test_accepted(&program, &summary), "{summary:?}");
}

#[test]
fn point_plus_point_is_rejected() {
    let source = format!("{USE}pub fn bad(a: Temperature, b: Temperature): Temperature = a + b;\n");
    let mut program = program(&source);
    let summary = typeck(&mut program);
    assert!(
        !test_accepted(&program, &summary),
        "a point plus a point must not type"
    );
}
