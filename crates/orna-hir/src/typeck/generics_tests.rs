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

#[test]
fn a_user_declared_range_is_the_nominal_range_type() {
    let mut program = program(
        "pub type Range<T> {\n    lo: T,\n    hi: T,\n}\n\
         pub fn make(): Range<Int> = Range { lo: 1, hi: 2 };\n\
         pub fn low(r: Range<Int>): Int = r.lo;\n",
    );
    let summary = typeck(&mut program);
    assert!(test_accepted(&program, &summary), "{summary:?}");
    assert!(
        program.modules[test_module(&program)]
            .diagnostics
            .is_empty()
    );
    assert_eq!(result(&program, "make"), "Range<Int>");
    assert_eq!(result(&program, "low"), "Int");
}

#[test]
fn a_generic_instance_protocol_argument_follows_the_receiver() {
    let mut program = program(
        "use core.ops.*;\n\
         pub type Span<T> {\n    lo: T,\n    hi: T,\n    impl Contains<T> {\n        fn contains(self, value: T): Bool = true;\n    }\n}\n\
         pub fn has(s: Span<Int>, v: Int): Bool = v in s;\n\
         pub fn has_method(s: Span<Int>, v: Int): Bool = s.contains(v);\n",
    );
    let summary = typeck(&mut program);
    assert!(test_accepted(&program, &summary), "{summary:?}");
    assert!(
        program.modules[test_module(&program)]
            .diagnostics
            .is_empty()
    );
    assert_eq!(result(&program, "has"), "Bool");
    assert_eq!(result(&program, "has_method"), "Bool");
}
