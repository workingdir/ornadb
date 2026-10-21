//! `Currency` is a core protocol (09-standard-library.md): a currency nominal
//! type implements it with the stable static properties `code: Str` and
//! `minor_digits: Int` (ORNA-MONEY-004) without declaring the protocol.

use orna_semantic_v1::{Analysis, DIAG_TYPE, ModuleInput, Type, analyze};

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
