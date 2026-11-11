//! Generic nominal types on the type pass: a constructor's result carries its
//! type arguments, and field types are substituted with them.

use super::tests::{program, result, test_accepted, test_module};
use super::typeck;

#[test]
fn a_generic_constructor_result_carries_its_arguments() {
    let mut program = program(
        "pub type Box<T> {\n    value: T,\n}\n\
         pub fn make(): Box<Int> = Box { value: 1 };\n\
         pub fn read(b: Box<Int>): Int = b.value;\n",
    );
    let summary = typeck(&mut program);
    assert!(test_accepted(&program, &summary), "{summary:?}");
    assert!(
        program.modules[test_module(&program)]
            .diagnostics
            .is_empty()
    );
    assert_eq!(result(&program, "make"), "Box<Int>");
    assert_eq!(result(&program, "read"), "Int");
}
