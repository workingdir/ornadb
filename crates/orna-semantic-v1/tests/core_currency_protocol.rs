//! `Currency` is a core protocol (09-standard-library.md): a currency nominal
//! type implements it with the stable static properties `code: Str` and
//! `minor_digits: Int` (ORNA-MONEY-004) without declaring the protocol. Only
//! such a type parameterizes `Money` or selects a currency literal
//! (ORNA-LIT-007), and a binary `Float` never becomes money (ORNA-NUM-003).

use orna_semantic_v1::{Analysis, DIAG_TYPE, DIAG_UNRESOLVED, ModuleInput, Type, analyze};

const NOT_A_CURRENCY: &str = "Money currency must be a nominal type that implements Currency";
const SUFFIX_NOT_A_CURRENCY: &str = "currency suffix names a type that does not implement Currency";
const FLOAT_MONEY: &str =
    "Money cannot be constructed from an inexact Float without explicit rounding";

fn analysis(source: &str) -> Analysis {
    analyze(&[ModuleInput::new("main.orna", source)])
}

fn has_message(analysis: &Analysis, message: &str) -> bool {
    analysis
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code() == DIAG_TYPE && diagnostic.message() == message)
}

// 05-types.md "Money and currencies": `pub type GBP { impl Currency { ... } }`.
const GBP: &str = "pub type GBP {
    impl Currency {
        static code = \"GBP\";
        static minor_digits = 2;
    }
}

pub fn amount(): Money<GBP> = 12.34.GBP;
";

#[test]
fn core_currency_is_implemented_without_declaring_the_protocol() {
    let analysis = analysis(GBP);
    assert!(analysis.is_ok(), "{:?}", analysis.diagnostics);
    let amount = analysis
        .modules
        .values()
        .find_map(|module| module.exports.get("amount"))
        .expect("amount is exported");
    assert!(
        matches!(&amount.ty, Type::Function { result, .. }
            if matches!(result.as_ref(), Type::Applied { base, .. } if base == "Money")),
        "{:?}",
        amount.ty
    );
}

#[test]
fn core_currency_surface_requires_both_static_properties() {
    let missing = analysis(
        "pub type GBP {
    impl Currency {
        static code = \"GBP\";
    }
}
",
    );
    assert!(has_message(
        &missing,
        "missing protocol implementation member"
    ));

    let unknown = analysis(
        "pub type GBP {
    impl Currency {
        static code = \"GBP\";
        static minor_digits = 2;
        static symbol = \"GBP sign\";
    }
}
",
    );
    assert!(has_message(
        &unknown,
        "unknown protocol implementation member"
    ));

    let mistyped = analysis(
        "pub type GBP {
    impl Currency {
        static code = 1;
        static minor_digits = 2;
    }
}
",
    );
    assert!(!mistyped.is_ok());
    assert!(
        mistyped
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == DIAG_TYPE),
        "{:?}",
        mistyped.diagnostics
    );
}

#[test]
fn undeclared_non_core_protocols_remain_unresolved() {
    let analysis = analysis(
        "pub type Point {
    x: Int,

    impl Shape {
        static sides = 0;
    }
}
",
    );
    assert!(has_message(
        &analysis,
        "unresolved protocol implementation identity"
    ));
}

#[test]
fn a_local_currency_declaration_takes_precedence_over_the_core_surface() {
    // Local declarations take precedence over other names (ORNA-IMPORT-007),
    // so a module-local protocol named `Currency` supplies its own surface.
    let analysis = analysis(
        "pub protocol Currency {
    static code: Str;
    static minor_digits: Int;
    static region: Str;
}

pub type GBP {
    impl Currency {
        static code = \"GBP\";
        static minor_digits = 2;
    }
}
",
    );
    assert!(has_message(
        &analysis,
        "missing protocol implementation member"
    ));
}

/// The `Money` argument of an exported function's result type.
fn money_result_currency(analysis: &Analysis, function: &str) -> Type {
    let symbol = analysis
        .modules
        .values()
        .find_map(|module| module.exports.get(function))
        .unwrap_or_else(|| panic!("{function} is exported"));
    match &symbol.ty {
        Type::Function { result, .. } => match result.as_ref() {
            Type::Applied { base, arguments } if base == "Money" && arguments.len() == 1 => {
                arguments[0].clone()
            }
            other => panic!("{function} returns {other:?}, not Money"),
        },
        other => panic!("{function} has type {other:?}"),
    }
}

// W16-03 (CUR-07, CUR-21): a name nothing declares is not a currency.
#[test]
fn money_over_an_undeclared_name_is_unresolved() {
    for source in [
        "pub fn f(x: Money<XYZ>): Money<XYZ> = x;\n",
        "pub fn f(x: Money<GBP>): Money<GBP> = x;\n",
    ] {
        let analysis = analysis(source);
        assert!(!analysis.is_ok(), "{source}");
        assert!(
            analysis
                .diagnostics
                .iter()
                .all(|diagnostic| diagnostic.code() == DIAG_UNRESOLVED),
            "{source}: {:?}",
            analysis.diagnostics
        );
    }
}

// W16-03 (X-CUR-c, X-CUR-b, CUR-08): only a type implementing `Currency`
// parameterizes `Money` (ORNA-MONEY-004).
#[test]
fn money_rejects_declared_types_that_are_not_currencies() {
    for source in [
        "pub fn f(x: Money<Int>): Money<Int> = x;\n",
        "pub table Item(id: Str) {\n    name: Str,\n}\n\npub fn f(x: Money<Item>): Money<Item> = x;\n",
        "pub type P {\n    pub v: Int,\n}\n\npub fn f(x: Money<P>): Money<P> = x;\n",
        "pub type P {\n    pub v: Int,\n}\n\npub fn f(value: Decimal) = Money<P>(value);\n",
    ] {
        let analysis = analysis(source);
        assert!(
            has_message(&analysis, NOT_A_CURRENCY),
            "{source}: {:?}",
            analysis.diagnostics
        );
    }
}

// W16-03 (CUR-13): `decimal_expression.Type` constructs Money only when the
// type implements `Currency` (ORNA-LIT-007).
#[test]
fn a_currency_suffix_naming_another_type_is_rejected() {
    let analysis = analysis("pub type P {\n    pub v: Int,\n}\n\npub fn bad() = 12.34.P;\n");
    assert!(
        has_message(&analysis, SUFFIX_NOT_A_CURRENCY),
        "{:?}",
        analysis.diagnostics
    );
}

// W16-03 (X-CUR-a): a local protocol named `Currency` takes precedence, so a
// type implementing a different surface is not a currency.
#[test]
fn a_local_currency_protocol_with_another_surface_grants_no_money() {
    let analysis = analysis(
        "pub protocol Currency {
    static code: Int;
}

pub type GBP {
    impl Currency {
        static code = 1;
    }
}

pub fn p(): Money<GBP> = 12.34.GBP;
",
    );
    assert!(
        has_message(&analysis, NOT_A_CURRENCY),
        "{:?}",
        analysis.diagnostics
    );
    assert!(
        has_message(&analysis, SUFFIX_NOT_A_CURRENCY),
        "{:?}",
        analysis.diagnostics
    );
}

// W16-03 (CUR-20, ISO-UNIT-i): `12.34f` is a binary Float, which never
// becomes money (ORNA-NUM-003, ORNA-MONEY-010).
#[test]
fn a_float_currency_literal_is_rejected() {
    let analysis = analysis(&format!("{GBP}\npub fn bad() = 12.34f.GBP;\n"));
    assert!(
        has_message(&analysis, FLOAT_MONEY),
        "{:?}",
        analysis.diagnostics
    );
}

// W16-03 (CUR-12, ISO-CUR-c): an imported currency keeps its canonical
// identity in `Money<GBP>` and in the `12.34.GBP` literal. Currency status is
// exported metadata, so neither input order matters.
#[test]
fn imported_currencies_construct_money() {
    let money = ModuleInput::new(
        "money.orna",
        "pub type GBP {\n    impl Currency {\n        static code = \"GBP\";\n        static minor_digits = 2;\n    }\n}\n",
    );
    for import in ["use money.{GBP};", "use money.*;"] {
        let main = ModuleInput::new(
            "main.orna",
            format!("{import}\n\npub fn price(): Money<GBP> = 12.34.GBP;\n"),
        );
        for inputs in [[money.clone(), main.clone()], [main, money.clone()]] {
            let analysis = analyze(&inputs);
            assert!(analysis.is_ok(), "{import}: {:?}", analysis.diagnostics);
            assert_eq!(
                money_result_currency(&analysis, "price"),
                Type::Named("money.GBP".into()),
                "{import}"
            );
        }
    }
}

// ORNA-MONEY-004: a generic `Money` currency must be bounded by `Currency`.
#[test]
fn a_generic_money_currency_needs_a_currency_bound() {
    let protocol =
        "pub protocol Currency {\n    static code: Str;\n    static minor_digits: Int;\n}\n\n";
    let bounded = analysis(&format!(
        "{protocol}pub fn keep<C impl Currency>(amount: Money<C>): Money<C> = amount;\n"
    ));
    assert!(bounded.is_ok(), "{:?}", bounded.diagnostics);
    let unbounded = analysis(&format!(
        "{protocol}pub fn keep<C>(amount: Money<C>): Money<C> = amount;\n"
    ));
    assert!(
        has_message(&unbounded, NOT_A_CURRENCY),
        "{:?}",
        unbounded.diagnostics
    );
}
