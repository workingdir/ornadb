//! Operator operand typing, nested return checking and `Unit` statement
//! values (ORNA-TYPE-008, ORNA-FN-009, ORNA-CFLOW-001, ORNA-LIB-006).

use std::collections::BTreeMap;

use orna_semantic_v1::{
    Analysis, DIAG_INCOMPATIBLE_DIMENSIONS, DIAG_TYPE, DIAG_TYPE_MISMATCH, ModuleInput, Type,
    analyze,
};

fn analysis(source: &str) -> Analysis {
    analyze(&[ModuleInput::new("operators.orna", source)])
}

fn diagnostics(analysis: &Analysis) -> Vec<(&str, &str)> {
    analysis
        .diagnostics
        .iter()
        .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
        .collect()
}

fn signature<'a>(analysis: &'a Analysis, name: &str) -> (&'a [Type], &'a Type) {
    let module = analysis
        .modules
        .values()
        .find(|module| module.namespace.display() == "operators")
        .expect("operators module");
    match &module.symbols[name].ty {
        Type::Function {
            parameters, result, ..
        } => (parameters, result),
        other => panic!("`{name}` is not a function: {other:?}"),
    }
}

fn result<'a>(analysis: &'a Analysis, name: &str) -> &'a Type {
    signature(analysis, name).1
}

#[test]
fn text_plus_int_is_rejected_with_the_spec_type_mismatch_code() {
    let analysis = analysis("pub fn label(count: Int) = \"total: \" + count;\n");

    assert_eq!(
        diagnostics(&analysis),
        vec![(
            DIAG_TYPE_MISMATCH,
            "operator `+` is not defined for Str and Int"
        )]
    );
}

#[test]
fn compound_assignment_applies_the_operator_table() {
    let analysis = analysis(
        r#"
pub protocol Currency { static code: Str; static minor_digits: Int; }
pub type GBP { impl Currency { static code = "GBP"; static minor_digits = 2; } }

pub fn later(start: Instant): Instant {
    let at = start;
    at += 1.hour;
    at
}

pub fn doubled(amount: Money<GBP>): Money<GBP> {
    let total = amount;
    total *= 2;
    total
}

pub fn shout(text: Str): Str {
    let loud = text;
    loud += "!";
    loud
}
"#,
    );

    assert_eq!(
        diagnostics(&analysis),
        vec![(
            DIAG_TYPE_MISMATCH,
            "operator `+` is not defined for Str and Str"
        )]
    );
}

#[test]
fn operators_reject_operand_types_outside_the_operator_table() {
    let analysis = analysis(
        r#"
pub fn strings(left: Str, right: Str) = left + right;
pub fn booleans(left: Bool, right: Bool) = left + right;
pub fn dates(left: Date, right: Date) = left + right;
pub fn instants(left: Instant, right: Instant) = left + right;
pub fn mixed(left: Int, right: Decimal) = left * right;
pub fn optional(value: Int?) = value + 1;
pub fn negated(value: Str) = -value;
pub fn energy_ago(at: Instant) = at - 5.kWh;
"#,
    );

    let mut found = diagnostics(&analysis);
    found.sort_unstable();
    let mut expected = vec![
        (
            DIAG_TYPE_MISMATCH,
            "operator `*` is not defined for Int and Decimal",
        ),
        (
            DIAG_TYPE_MISMATCH,
            "operator `+` is not defined for Bool and Bool",
        ),
        (
            DIAG_TYPE_MISMATCH,
            "operator `+` is not defined for Date and Date",
        ),
        (
            DIAG_TYPE_MISMATCH,
            "operator `+` is not defined for Instant and Instant",
        ),
        (
            DIAG_TYPE_MISMATCH,
            "operator `+` is not defined for Int? and Int",
        ),
        (
            DIAG_TYPE_MISMATCH,
            "operator `+` is not defined for Str and Str",
        ),
        (DIAG_TYPE_MISMATCH, "operator `-` is not defined for Str"),
        (
            DIAG_TYPE_MISMATCH,
            "operator `-` is not defined for Instant and Int<kWh>",
        ),
    ];
    expected.sort_unstable();
    assert_eq!(found, expected);
}

#[test]
fn admitted_operators_keep_their_static_result_types() {
    let analysis = analysis(
        r#"
pub fn ints(left: Int, right: Int) = left + right * 2 - left / right % 3 ^ 2;
pub fn decimals(left: Decimal, right: Decimal) = left * right - left / right;
pub fn floats(left: Float, right: Float) = -left + right;
pub fn elapsed(left: Instant, right: Instant) = left - right;
pub fn negative(value: Int) = -value;
pub fn inverted(flag: Bool) = !flag;
pub fn latest_first(at: Instant) = -at;
pub fn week_ago(at: Instant) = at - 7.days;
"#,
    );

    assert!(analysis.is_ok(), "{:?}", diagnostics(&analysis));
    assert_eq!(result(&analysis, "ints"), &Type::Int);
    assert_eq!(result(&analysis, "decimals"), &Type::Decimal);
    assert_eq!(result(&analysis, "floats"), &Type::Float);
    assert_eq!(
        result(&analysis, "elapsed"),
        &Type::Named("std.DURATION".into())
    );
    assert_eq!(result(&analysis, "negative"), &Type::Int);
    assert_eq!(result(&analysis, "inverted"), &Type::Bool);
    assert_eq!(result(&analysis, "latest_first"), &Type::Instant);
    assert_eq!(result(&analysis, "week_ago"), &Type::Instant);
}

#[test]
fn money_scales_by_exact_numbers_and_rejects_binary_float() {
    let currency = r#"
pub protocol Currency { static code: Str; static minor_digits: Int; }
pub type GBP { impl Currency { static code = "GBP"; static minor_digits = 2; } }
pub type EUR { impl Currency { static code = "EUR"; static minor_digits = 2; } }
"#;
    let valid = analysis(&format!(
        "{currency}
pub fn scaled(amount: Money<GBP>, count: Int) = amount * count;
pub fn shared(amount: Money<GBP>, parts: Decimal) = amount / parts;
pub fn net(gross: Money<GBP>, fee: Money<GBP>) = gross - fee;
"
    ));
    assert!(valid.is_ok(), "{:?}", diagnostics(&valid));
    for name in ["scaled", "shared", "net"] {
        let (parameters, result) = signature(&valid, name);
        assert_eq!(result, &parameters[0], "`{name}` keeps its Money currency");
    }

    let invalid = analysis(&format!(
        "{currency}
pub fn implicit(amount: Money<GBP>, factor: Float) = amount * factor;
pub fn squared(amount: Money<GBP>) = amount * amount;
pub fn converted(left: Money<GBP>, right: Money<EUR>) = left - right;
"
    ));
    let mut found = diagnostics(&invalid);
    found.sort_unstable();
    assert_eq!(found.len(), 3, "{found:?}");
    assert_eq!(found[0].0, DIAG_TYPE_MISMATCH, "{found:?}");
    assert!(
        found[0]
            .1
            .starts_with("operator `*` is not defined for Money<"),
        "Money times Money is an undefined operand pair: {found:?}"
    );
    assert_eq!(
        found[1],
        (
            DIAG_TYPE,
            "binary Float cannot enter an exact Money calculation implicitly"
        )
    );
    assert_eq!(
        found[2],
        (
            DIAG_TYPE,
            "cannot subtract different currencies without conversion"
        )
    );
}

#[test]
fn anonymous_function_returns_are_checked_and_joined() {
    let analysis = analysis(
        r#"
pub fn checked(): fn(Int): Int = (x: Int) => {
    return "s";
};

pub fn chooser() = x => {
    if x > 0 {
        return 1;
    }
    2
};
"#,
    );

    assert_eq!(
        diagnostics(&analysis),
        vec![(DIAG_TYPE, "static types are incompatible")]
    );
    assert!(matches!(
        result(&analysis, "chooser"),
        Type::Function { result, .. } if result.as_ref() == &Type::Int
    ));
}

#[test]
fn declared_unit_quantities_follow_dimension_and_affine_algebra() {
    let units = r#"
pub dim Length;
pub dim Time;
pub dim Temperature;
pub unit m : Length base;
pub unit s : Time base;
pub unit K : Temperature base;
pub unit C : Temperature = K offset 273.15 affine;
pub unit deltaC : Temperature = K;
"#;
    let valid = analysis(&format!(
        "{units}
pub fn speed(distance: Float<m>, time: Float<s>) = distance / time;
pub fn scaled(distance: Float<m>, factor: Float) = factor * distance;
pub fn ratio(left: Float<m>, right: Float<m>) = left / right;
pub fn difference(left: Float<C>, right: Float<C>) = left - right;
pub fn warmed(start: Float<C>, delta: Float<deltaC>) = start + delta;
"
    ));
    assert!(valid.is_ok(), "{:?}", diagnostics(&valid));

    // Exported parameters keep their surface spelling; inferred results carry
    // the structural quantity, so compare dimensions and affinity.
    let shape = |name: &str| match result(&valid, name) {
        Type::Quantity {
            numeric,
            dimension_basis,
            affine,
            ..
        } => (
            numeric.as_ref().clone(),
            dimension_basis
                .iter()
                .map(|(dimension, exponent)| (dimension.name.clone(), *exponent))
                .collect::<BTreeMap<_, _>>(),
            *affine,
        ),
        other => panic!("`{name}` is not a quantity: {other:?}"),
    };
    let dimensions = |pairs: &[(&str, i64)]| {
        pairs
            .iter()
            .map(|(name, exponent)| ((*name).to_owned(), *exponent))
            .collect::<BTreeMap<_, _>>()
    };
    assert_eq!(
        shape("speed"),
        (
            Type::Float,
            dimensions(&[("Length", 1), ("Time", -1)]),
            false
        )
    );
    assert_eq!(
        shape("scaled"),
        (Type::Float, dimensions(&[("Length", 1)]), false)
    );
    assert_eq!(result(&valid, "ratio"), &Type::Float);
    assert_eq!(
        shape("warmed"),
        (Type::Float, dimensions(&[("Temperature", 1)]), true),
        "absolute + delta is absolute"
    );
    assert_eq!(
        shape("difference"),
        (Type::Float, dimensions(&[("Temperature", 1)]), false),
        "absolute - absolute is the delta quantity"
    );

    let invalid = analysis(&format!(
        "{units}
pub fn mismatched(distance: Float<m>, time: Float<s>) = distance + time;
pub fn absolute(left: Float<C>, right: Float<C>) = left + right;
"
    ));
    let mut found = diagnostics(&invalid);
    found.sort_unstable();
    assert_eq!(
        found,
        vec![
            (
                DIAG_INCOMPATIBLE_DIMENSIONS,
                "cannot add quantities with incompatible dimensions"
            ),
            (DIAG_TYPE, "cannot add two absolute affine temperatures"),
        ]
    );
}

#[test]
fn nested_return_values_are_checked_against_the_declared_result() {
    let analysis = analysis(
        r#"
pub fn pick(value: Int): Int {
    if value > 0 {
        return "early";
    }
    1
}

pub fn bare(value: Int): Int {
    if value > 0 {
        return;
    }
    1
}
"#,
    );

    assert_eq!(
        diagnostics(&analysis),
        vec![
            (DIAG_TYPE, "static types are incompatible"),
            (DIAG_TYPE, "static types are incompatible"),
        ]
    );
}

#[test]
fn unannotated_return_paths_join_into_the_exported_result() {
    let valid = analysis(
        r#"
pub fn choose(value: Int) {
    if value > 0 {
        return 1;
    }
    2
}

pub fn maybe(value: Int) {
    if value > 0 {
        return null;
    }
    2
}

pub fn early(value: Int) {
    return value;
}

pub fn outer(values: [Int]): Str {
    let double = (value: Int) => {
        return value * 2;
    };
    "done"
}
"#,
    );
    assert!(valid.is_ok(), "{:?}", diagnostics(&valid));
    assert_eq!(result(&valid, "choose"), &Type::Int);
    assert_eq!(
        result(&valid, "maybe"),
        &Type::Optional(Box::new(Type::Int))
    );
    assert_eq!(result(&valid, "early"), &Type::Int);

    let invalid = analysis(
        r#"
pub fn mixed(value: Int) {
    if value > 0 {
        return "early";
    }
    2
}
"#,
    );
    assert_eq!(
        diagnostics(&invalid),
        vec![(
            DIAG_TYPE,
            "return paths have incompatible static types: Int and Str"
        )]
    );
}

#[test]
fn statement_blocks_and_table_delete_complete_with_unit() {
    let valid = analysis(
        r#"
pub table Reading(id: Int) { value: Int, }

pub fn remove(id: Int) = Reading.delete(id);

pub fn tidy(id: Int) {
    Reading.delete(id);
}

pub fn stop(flag: Bool) {
    if flag {
        return;
    }
}

pub fn declared(id: Int): Unit {
    Reading.delete(id);
}
"#,
    );
    assert!(valid.is_ok(), "{:?}", diagnostics(&valid));
    for name in ["remove", "tidy", "stop", "declared"] {
        assert_eq!(
            result(&valid, name),
            &Type::Unit,
            "`{name}` completes with Unit"
        );
    }

    let invalid = analysis(
        r#"
pub table Reading(id: Int) { value: Int, }

pub fn missing(id: Int): Int? {
    Reading.delete(id);
}
"#,
    );
    assert_eq!(
        diagnostics(&invalid),
        vec![(DIAG_TYPE, "static types are incompatible")],
        "a statement block is Unit and cannot stand in for an optional value"
    );
}
