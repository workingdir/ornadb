//! The currency predicate on declared types (hir-host.md B3, Q-B1 default): `Money<C>` accepts only a currency.

use super::tests::{program, test_accepted, test_module};
use super::typeck;
use crate::{DefKind, Program, engine_money};

/// The diagnostics of the test module, as messages.
fn test_diagnostics(program: &Program) -> Vec<String> {
    program.modules[test_module(program)]
        .diagnostics
        .iter()
        .map(|diagnostic| diagnostic.message.to_string())
        .collect()
}

const GBP: &str = "pub type GBP {\n    impl Currency {\n        static code = \"GBP\";\n        static minor_digits = 2;\n    }\n}\n";

/// A user declaration of the same protocol shape, with the members the Q-B1 default accepts.
const USER_CURRENCY: &str =
    "pub protocol Currency {\n    static code: Str;\n    static minor_digits: Int;\n}\n";

#[test]
fn valid_currency_with_root_protocol_types_with_zero_diagnostics() {
    let mut program = program(&format!(
        "{GBP}pub fn wrap(amount: Money<GBP>): Money<GBP> = amount;\n"
    ));
    let summary = typeck(&mut program);
    assert!(test_accepted(&program, &summary), "{summary:?}");
    assert_eq!(test_diagnostics(&program), Vec::<String>::new());
}

#[test]
fn valid_currency_with_user_declared_protocol_types_with_zero_diagnostics() {
    let mut program = program(&format!(
        "{USER_CURRENCY}pub type GBP {{\n    impl Currency {{\n        static code = \"GBP\";\n        static minor_digits = 2;\n    }}\n}}\npub fn wrap(amount: Money<GBP>): Money<GBP> = amount;\n"
    ));
    let summary = typeck(&mut program);
    assert!(test_accepted(&program, &summary), "{summary:?}");
    assert_eq!(test_diagnostics(&program), Vec::<String>::new());
}

#[test]
fn a_currency_with_an_extra_symbol_member_is_rejected() {
    let mut program = program(
        "pub protocol Currency { static code: Str; static symbol: Str; static minor_digits: Int; }\n\
         pub type GBP {\n    impl Currency {\n        static code = \"GBP\";\n        static symbol = \"£\";\n        static minor_digits = 2;\n    }\n}\n\
         pub fn wrap(amount: Money<GBP>): Money<GBP> = amount;\n",
    );
    let summary = typeck(&mut program);
    assert!(!test_accepted(&program, &summary), "{summary:?}");
    assert!(
        test_diagnostics(&program)
            .iter()
            .any(|message| message.contains("does not implement the `Currency` protocol")),
        "{:?}",
        test_diagnostics(&program)
    );
}

#[test]
fn a_protocol_named_currency_with_a_different_member_type_is_not_accepted() {
    let mut program = program(
        "pub protocol Currency {\n    static code: Str;\n    static minor_digits: Str;\n}\n\
         pub type GBP {\n    impl Currency {\n        static code = \"GBP\";\n        static minor_digits = \"2\";\n    }\n}\n\
         pub fn wrap(amount: Money<GBP>): Money<GBP> = amount;\n",
    );
    let summary = typeck(&mut program);
    assert!(!test_accepted(&program, &summary), "{summary:?}");
}

#[test]
fn a_type_without_a_currency_implementation_is_rejected_as_money_argument() {
    let mut program = program(
        "pub type EUR {\n    cents: Int,\n}\n\
         pub fn wrap(amount: Money<EUR>): Money<EUR> = amount;\n",
    );
    let summary = typeck(&mut program);
    assert!(!test_accepted(&program, &summary), "{summary:?}");
}

#[test]
fn a_protocol_is_the_currency_protocol_only_under_the_q_b1_predicate() {
    let program = program(&format!("{USER_CURRENCY}{GBP}"));
    let protocols: Vec<_> = program
        .defs
        .iter_enumerated()
        .filter(|(_, definition)| {
            definition.name.as_str() == "Currency"
                && matches!(definition.kind, DefKind::Protocol(_))
        })
        .map(|(id, _)| id)
        .collect();
    // The root `core.money.Currency` and the user declaration of the same shape are both currency protocols.
    assert_eq!(protocols.len(), 2, "{protocols:?}");
    assert!(
        protocols
            .iter()
            .all(|protocol| engine_money::is_currency_protocol(&program, *protocol))
    );
}
