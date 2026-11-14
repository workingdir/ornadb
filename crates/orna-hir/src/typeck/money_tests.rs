//! Money operator and construction codes (hir-host.md B5): `E5003`, `E5004` and `E5103` are decided by the kernel,
//! and `Money<C> * Money<C>` keeps the generic no-instance diagnostic.

use super::tests::{program, test_module};
use super::typeck;
use crate::Program;
use crate::diag::{INFER_CONFLICT, MONEY_BINARY_FLOAT, MONEY_CURRENCY_MISMATCH, MONEY_FROM_FLOAT};

const DECLARATIONS: &str = "pub type GBP {\n    impl Currency {\n        static code = \"GBP\";\n        static minor_digits = 2;\n    }\n}\n\
pub type EUR {\n    impl Currency {\n        static code = \"EUR\";\n        static minor_digits = 2;\n    }\n}\n";

/// The diagnostic codes of the test module, in report order.
fn codes(program: &Program) -> Vec<String> {
    program.modules[test_module(program)]
        .diagnostics
        .iter()
        .map(|diagnostic| diagnostic.code.to_string())
        .collect()
}

/// Types `body` after the currency declarations and returns its diagnostic codes.
fn check(body: &str) -> Vec<String> {
    let mut program = program(&format!("{DECLARATIONS}{body}"));
    typeck(&mut program);
    codes(&program)
}

#[test]
fn adding_two_currencies_is_e5003() {
    let found = check("pub fn bad(a: Money<GBP>, b: Money<EUR>) = a + b;\n");
    assert_eq!(
        found,
        vec![MONEY_CURRENCY_MISMATCH.to_string()],
        "{found:?}"
    );
}

#[test]
fn adding_currency_literals_of_two_currencies_is_e5003() {
    let found = check("pub fn bad() = 10.GBP + 5.EUR;\n");
    assert_eq!(
        found,
        vec![MONEY_CURRENCY_MISMATCH.to_string()],
        "{found:?}"
    );
}

#[test]
fn ordering_two_currencies_is_e5003() {
    let found = check("pub fn bad(a: Money<GBP>, b: Money<EUR>): Bool = a < b;\n");
    assert_eq!(
        found,
        vec![MONEY_CURRENCY_MISMATCH.to_string()],
        "{found:?}"
    );
}

#[test]
fn adding_money_and_float_is_e5103() {
    let found = check("pub fn bad(a: Money<GBP>, f: Float) = a + f;\n");
    assert_eq!(found, vec![MONEY_BINARY_FLOAT.to_string()], "{found:?}");
}

#[test]
fn multiplying_float_by_money_is_e5103() {
    let found = check("pub fn bad(f: Float, m: Money<GBP>) = f * m;\n");
    assert_eq!(found, vec![MONEY_BINARY_FLOAT.to_string()], "{found:?}");
}

#[test]
fn building_money_from_float_is_e5004() {
    let found = check("pub fn bad(value: Float) = Money<GBP>(value);\n");
    assert_eq!(found, vec![MONEY_FROM_FLOAT.to_string()], "{found:?}");
}

#[test]
fn multiplying_money_by_money_keeps_the_generic_diagnostic() {
    let found = check("pub fn bad(a: Money<GBP>, b: Money<GBP>) = a * b;\n");
    assert_eq!(found, vec![INFER_CONFLICT.to_string()], "{found:?}");
}

#[test]
fn adding_one_currency_is_accepted() {
    let found = check("pub fn ok(a: Money<GBP>, b: Money<GBP>): Money<GBP> = a + b;\n");
    assert_eq!(found, Vec::<String>::new(), "{found:?}");
}
