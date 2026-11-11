//! The `core.bits` engine natives over lowered source, against the old evaluator's outcomes: two's-complement bitwise
//! operations, arithmetic right shifts, and the `ORNA-EVAL-VALUE` / `ORNA-EVAL-LIMIT` refusals of a shift.

use orna_exec::{ExecError, ExecLimits, Value, run_entry};
use orna_hir::{DefId, DefKind, ModulePath, Name, Program, SourceId, SourceUnit, lower};
use orna_syntax::parse_module_with_file;

const LIMITS: ExecLimits = ExecLimits {
    max_steps: 100_000,
    max_depth: 64,
    max_collection_items: 2_048,
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
    let user = ModulePath::new([Name::new("main")]);
    let diagnostics: Vec<String> = program
        .modules
        .iter()
        .filter(|module| module.path == user)
        .flat_map(|module| {
            module
                .diagnostics
                .iter()
                .map(|diagnostic| format!("{diagnostic:?}"))
        })
        .collect();
    assert!(
        diagnostics.is_empty(),
        "lowering diagnostics: {diagnostics:?}"
    );
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
fn bitwise_operations_are_two_complement() {
    let head = "use core.bits;\n";
    assert_eq!(
        run(&format!("{head}pub fn run(): Int = bits.bit_or(5, 2);\n")),
        Ok(int("7"))
    );
    assert_eq!(
        run(&format!("{head}pub fn run(): Int = bits.bit_and(5, 3);\n")),
        Ok(int("1"))
    );
    assert_eq!(
        run(&format!("{head}pub fn run(): Int = bits.bit_xor(5, 3);\n")),
        Ok(int("6"))
    );
    assert_eq!(
        run(&format!("{head}pub fn run(): Int = bits.bit_not(0);\n")),
        Ok(int("-1"))
    );
    assert_eq!(
        run(&format!("{head}pub fn run(): Int = bits.bit_not(-1);\n")),
        Ok(int("0"))
    );
}

#[test]
fn shifts_are_exact_and_right_shift_is_arithmetic() {
    let head = "use core.bits;\n";
    assert_eq!(
        run(&format!(
            "{head}pub fn run(): Int = bits.shift_left(-3, 2);\n"
        )),
        Ok(int("-12"))
    );
    assert_eq!(
        run(&format!(
            "{head}pub fn run(): Int = bits.shift_right(-9, 2);\n"
        )),
        Ok(int("-3"))
    );
    assert_eq!(
        run(&format!(
            "{head}pub fn run(): Int = bits.shift_left(1, 100);\n"
        )),
        Ok(int("1267650600228229401496703205376"))
    );
    assert_eq!(
        run(&format!(
            "{head}pub fn run(): Int = bits.shift_right(5, 100);\n"
        )),
        Ok(int("0"))
    );
    assert_eq!(
        run(&format!(
            "{head}pub fn run(): Int = bits.shift_right(-5, 100);\n"
        )),
        Ok(int("-1"))
    );
}

#[test]
fn negative_shift_count_is_the_old_value_code() {
    let head = "use core.bits;\n";
    assert_eq!(
        run(&format!(
            "{head}pub fn run(): Int = bits.shift_left(1, -1);\n"
        )),
        Err(ExecError::Code("ORNA-EVAL-VALUE"))
    );
    assert_eq!(
        run(&format!(
            "{head}pub fn run(): Int = bits.shift_right(1, -1);\n"
        )),
        Err(ExecError::Code("ORNA-EVAL-VALUE"))
    );
}

#[test]
fn shift_past_the_digit_limit_is_the_old_limit_code() {
    assert_eq!(
        run("use core.bits;\npub fn run(): Int = bits.shift_left(1, 1000000);\n"),
        Err(ExecError::Code("ORNA-EVAL-LIMIT"))
    );
}
