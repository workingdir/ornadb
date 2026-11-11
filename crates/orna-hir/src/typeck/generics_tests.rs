//! Generic nominal types on the type pass: a constructor's result carries its
//! type arguments, and field types are substituted with them.

use super::tests::{definition, program, result, test_accepted, test_module};
use super::typeck;
use crate::{Callee, DefKind, ExprKind, FnBody, Program, print_type};

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
fn a_generic_call_records_its_solved_type_arguments() {
    let mut program = program(
        "pub type Span<T> {\n    lo: T,\n    hi: T,\n}\n\
         pub fn contains<T>(s: Span<T>, x: T): Bool = true;\n\
         pub fn run(s: Span<Int>): Bool = contains(s, 1);\n",
    );
    let summary = typeck(&mut program);
    assert!(test_accepted(&program, &summary), "{summary:?}");
    assert!(
        program.modules[test_module(&program)]
            .diagnostics
            .is_empty()
    );
    assert_eq!(recorded_targs(&program, "run"), ["Int"]);
}

/// The type arguments the root call of the named function records on its callee, printed.
fn recorded_targs(program: &Program, name: &str) -> Vec<String> {
    let DefKind::Fn(function) = &program.defs[definition(program, name)].kind else {
        panic!("{name} is not a function");
    };
    let FnBody::Source(body_id) = function.body else {
        panic!("{name} has no source body");
    };
    let body = &program.bodies[body_id];
    let ExprKind::Call(call) = &body.exprs[body.root].kind else {
        panic!("{name} does not have a call at its root");
    };
    let Callee::Source { targs, .. } = &call.callee else {
        panic!("{name} does not call a source function");
    };
    targs.iter().map(|ty| print_type(program, *ty)).collect()
}
