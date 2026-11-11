//! The `Some(p)` pattern on the type pass: it matches a present option and
//! types its sub-pattern against the payload.

use super::tests::{program, result, test_accepted, test_module};
use super::typeck;

#[test]
fn a_some_pattern_binds_the_payload_of_an_option() {
    let mut program =
        program("pub fn or_zero(x: Int?): Int = case x {\n    Some(v): v,\n    null: 0,\n};\n");
    let summary = typeck(&mut program);
    assert!(test_accepted(&program, &summary), "{summary:?}");
    assert!(
        program.modules[test_module(&program)]
            .diagnostics
            .is_empty()
    );
    assert_eq!(result(&program, "or_zero"), "Int");
}
