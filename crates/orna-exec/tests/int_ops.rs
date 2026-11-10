//! Integer `/`, `%` and `^` over lowered source, against the old evaluator's
//! outcomes: truncating division, `ORNA-EVAL-DIVIDE-BY-ZERO`, `ORNA-EVAL-TYPE`
//! for a negative exponent, and the digit quota as `ORNA-EVAL-LIMIT`.

use orna_exec::{ExecError, ExecLimits, Value, run_entry};
use orna_hir::{DefId, DefKind, ModulePath, Name, Program, SourceId, SourceUnit, lower};
use orna_syntax::parse_module_with_file;

const LIMITS: ExecLimits = ExecLimits {
    max_steps: 100_000,
    max_depth: 64,
    max_string_bytes: 1 << 20,
    max_integer_digits: 1_024,
};

fn lower_one(source: &str) -> Program {
    let parsed = parse_module_with_file(source, "test.orna");
    assert!(
        parsed.is_ok(),
        "test source must parse: {:?}",
        parsed.diagnostics
    );
    let program = lower(&[SourceUnit {
        source: SourceId::new(0),
        tree: &parsed.value,
        path: ModulePath::new([Name::new("main")]),
    }]);
    assert_eq!(program.validate(), Ok(()), "lowered program must validate");
    program
}

fn run(source: &str) -> Result<Value, ExecError> {
    let program = lower_one(source);
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

fn int(value: &str) -> Value {
    Value::Int(value.parse().expect("integer literal"))
}

#[test]
fn division_truncates_toward_zero_like_the_old_evaluator() {
    assert_eq!(run("pub fn run(): Int = 7 / 2;\n"), Ok(int("3")));
    assert_eq!(run("pub fn run(): Int = -7 / 2;\n"), Ok(int("-3")));
    assert_eq!(run("pub fn run(): Int = 7 / -2;\n"), Ok(int("-3")));
}

#[test]
fn remainder_takes_the_sign_of_the_dividend() {
    assert_eq!(run("pub fn run(): Int = 7 % 3;\n"), Ok(int("1")));
    assert_eq!(run("pub fn run(): Int = -7 % 3;\n"), Ok(int("-1")));
    assert_eq!(run("pub fn run(): Int = 7 % -3;\n"), Ok(int("1")));
}

#[test]
fn zero_divisor_is_the_old_divide_by_zero_code() {
    assert_eq!(
        run("pub fn run(): Int = 1 / 0;\n"),
        Err(ExecError::Code("ORNA-EVAL-DIVIDE-BY-ZERO"))
    );
    assert_eq!(
        run("pub fn run(): Int = 1 % 0;\n"),
        Err(ExecError::Code("ORNA-EVAL-DIVIDE-BY-ZERO"))
    );
}

#[test]
fn power_is_right_associative_and_exact() {
    assert_eq!(run("pub fn run(): Int = 2 ^ 10;\n"), Ok(int("1024")));
    assert_eq!(run("pub fn run(): Int = 2 ^ 3 ^ 2;\n"), Ok(int("512")));
    assert_eq!(run("pub fn run(): Int = 0 ^ 0;\n"), Ok(int("1")));
}

#[test]
fn negative_exponent_is_the_old_type_code() {
    assert_eq!(
        run("pub fn run(): Int = 2 ^ -1;\n"),
        Err(ExecError::Code("ORNA-EVAL-TYPE"))
    );
}

#[test]
fn oversized_power_hits_the_digit_quota() {
    assert_eq!(
        run("pub fn run(): Int = 10 ^ 2000;\n"),
        Err(ExecError::Code("ORNA-EVAL-LIMIT"))
    );
}
