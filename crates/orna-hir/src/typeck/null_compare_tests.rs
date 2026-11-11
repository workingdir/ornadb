//! `== null` and `!= null` on optional types: null is the only absent value (D3), so comparing
//! an option against null is well typed for any payload.

use super::tests::{program, result, test_accepted, test_module};
use super::typeck;

#[test]
fn equality_with_null_on_an_int_option_is_bool() {
    let mut program = program("pub fn is_absent(x: Int?): Bool = x == null;\n");
    let summary = typeck(&mut program);
    assert!(test_accepted(&program, &summary), "{summary:?}");
    assert!(
        program.modules[test_module(&program)]
            .diagnostics
            .is_empty()
    );
    assert_eq!(result(&program, "is_absent"), "Bool");
}

#[test]
fn inequality_with_null_on_an_int_option_is_bool() {
    let mut program = program("pub fn is_present(x: Int?): Bool = x != null;\n");
    let summary = typeck(&mut program);
    assert!(test_accepted(&program, &summary), "{summary:?}");
    assert!(
        program.modules[test_module(&program)]
            .diagnostics
            .is_empty()
    );
    assert_eq!(result(&program, "is_present"), "Bool");
}
