//! The `core.decimal.divide` engine native over lowered source: the quotient of two decimals rounded to a scale under each
//! `core.Rounding` member, with the old evaluator's errors for a zero divisor, a negative scale and a scale past the
//! limit. The receiver is a `Decimal`, so the call settles to the native.

use orna_exec::{ExecError, ExecLimits, Value, run_entry};
use orna_hir::{DefId, DefKind, ModulePath, Name, Program, SourceId, SourceUnit, lower, typeck};
use orna_syntax::parse_module_with_file;

const LIMITS: ExecLimits = ExecLimits {
    max_steps: 100_000,
    max_depth: 64,
    max_collection_items: 2_048,
    max_string_bytes: 1 << 20,
    max_integer_digits: 1_024,
};

fn lower_typed(source: &str) -> Program {
    let parsed = parse_module_with_file(source, "test.orna");
    assert!(
        parsed.is_ok(),
        "test source must parse: {:?}",
        parsed.diagnostics
    );
    let mut program = lower(&[SourceUnit {
        source: SourceId::new(0),
        tree: &parsed.value,
        path: ModulePath::new([Name::new("main")]),
    }]);
    typeck(&mut program);
    assert_eq!(program.validate(), Ok(()), "lowered program must validate");
    program
}

fn run(body: &str) -> Result<Value, ExecError> {
    let program = lower_typed(&format!("use core.{{Rounding}};\n{body}"));
    let run_def: DefId = program
        .modules
        .iter()
        .flat_map(|module| module.items.iter().copied())
        .find(|def| {
            matches!(program.defs[*def].kind, DefKind::Fn(_))
                && program.defs[*def].name.as_str() == "run"
        })
        .expect("the source defines `run`");
    run_entry(&program, run_def, LIMITS).result
}

/// The decimal a literal denotes, as the engine evaluates it.
fn decimal(literal: &str) -> Value {
    run(&format!("pub fn run(): Decimal = {literal};\n")).expect("a decimal literal evaluates")
}

/// `dividend.divide(value, scale: scale, rounding: Rounding.mode)`, evaluated.
fn divide(dividend: &str, value: &str, scale: &str, mode: &str) -> Result<Value, ExecError> {
    run(&format!(
        "pub fn run(): Decimal = ({dividend}).divide({value}, scale: {scale}, rounding: Rounding.{mode});\n"
    ))
}

#[test]
fn each_mode_rounds_an_inexact_quotient_its_own_way() {
    // 1 / 3 = 0.333..., 2 / 3 = 0.666..., rounded to two digits.
    let cases = [
        ("1.0", "3.0", "half_even", "0.33"),
        ("1.0", "3.0", "half_up", "0.33"),
        ("1.0", "3.0", "toward_zero", "0.33"),
        ("1.0", "3.0", "away_from_zero", "0.34"),
        ("1.0", "3.0", "floor", "0.33"),
        ("1.0", "3.0", "ceil", "0.34"),
        ("2.0", "3.0", "half_even", "0.67"),
        ("2.0", "3.0", "half_up", "0.67"),
        ("2.0", "3.0", "toward_zero", "0.66"),
        ("2.0", "3.0", "away_from_zero", "0.67"),
        ("2.0", "3.0", "floor", "0.66"),
        ("2.0", "3.0", "ceil", "0.67"),
    ];
    for (dividend, value, mode, expected) in cases {
        assert_eq!(
            divide(dividend, value, "2", mode),
            Ok(decimal(expected)),
            "{dividend} / {value} under {mode}"
        );
    }
}

#[test]
fn ties_break_by_mode_and_half_even_goes_to_the_even_neighbour() {
    // 1 / 8 = 0.125 and 3 / 8 = 0.375 are exact ties at two digits.
    assert_eq!(divide("1.0", "8.0", "2", "half_even"), Ok(decimal("0.12")));
    assert_eq!(divide("1.0", "8.0", "2", "half_up"), Ok(decimal("0.13")));
    assert_eq!(divide("3.0", "8.0", "2", "half_even"), Ok(decimal("0.38")));
    assert_eq!(divide("3.0", "8.0", "2", "half_up"), Ok(decimal("0.38")));
    // 5 / 2 = 2.5 and 7 / 2 = 3.5 at scale zero.
    assert_eq!(divide("5.0", "2.0", "0", "half_even"), Ok(decimal("2.0")));
    assert_eq!(divide("5.0", "2.0", "0", "half_up"), Ok(decimal("3.0")));
    assert_eq!(divide("7.0", "2.0", "0", "half_even"), Ok(decimal("4.0")));
}

#[test]
fn negative_quotients_round_by_their_sign_for_the_directed_modes() {
    // -1 / 3 = -0.333...: floor moves down to -0.34; ceil and toward_zero stay at -0.33.
    assert_eq!(divide("-1.0", "3.0", "2", "floor"), Ok(decimal("-0.34")));
    assert_eq!(divide("-1.0", "3.0", "2", "ceil"), Ok(decimal("-0.33")));
    assert_eq!(
        divide("-1.0", "3.0", "2", "toward_zero"),
        Ok(decimal("-0.33"))
    );
    assert_eq!(
        divide("-1.0", "3.0", "2", "away_from_zero"),
        Ok(decimal("-0.34"))
    );
    // -1 / 8 = -0.125: a negative tie with an even quotient stays, half_up goes away from zero.
    assert_eq!(
        divide("-1.0", "8.0", "2", "half_even"),
        Ok(decimal("-0.12"))
    );
    assert_eq!(divide("-1.0", "8.0", "2", "half_up"), Ok(decimal("-0.13")));
    // A negative divisor flips the sign of the quotient.
    assert_eq!(divide("1.0", "-3.0", "2", "floor"), Ok(decimal("-0.34")));
}

#[test]
fn an_exact_quotient_is_unchanged_by_every_mode() {
    for mode in [
        "half_even",
        "half_up",
        "toward_zero",
        "away_from_zero",
        "floor",
        "ceil",
    ] {
        assert_eq!(
            divide("1.5", "0.5", "4", mode),
            Ok(decimal("3.0")),
            "{mode}"
        );
    }
}

#[test]
fn a_zero_divisor_is_a_divide_by_zero_error() {
    assert_eq!(
        divide("1.0", "0.0", "2", "half_even"),
        Err(ExecError::Code("ORNA-EVAL-DIVIDE-BY-ZERO"))
    );
}

#[test]
fn a_negative_scale_is_a_value_error() {
    assert_eq!(
        divide("1.0", "3.0", "-1", "half_even"),
        Err(ExecError::Code("ORNA-EVAL-VALUE"))
    );
}

#[test]
fn a_scale_past_the_exponent_limit_is_a_limit_error() {
    assert_eq!(
        divide("1.0", "3.0", "2000", "half_even"),
        Err(ExecError::Code("ORNA-EVAL-LIMIT"))
    );
}
