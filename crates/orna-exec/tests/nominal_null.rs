//! Nominal struct construction, field access by definition, `null`, `??` and `case` on a null arm over lowered source,
//! without the type pass: the executor reads the lowered definitions directly.

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

const PAIR: &str = "type Pair {\n    pub a: Int?,\n    pub b: Int,\n}\n";

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

fn run(body: &str) -> Result<Value, ExecError> {
    let program = lower_one(&format!("{PAIR}{body}"));
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
fn a_field_of_a_constructed_struct_reads_back_its_value() {
    assert_eq!(
        run("pub fn run(): Int = Pair { a: null, b: 9 }.b;\n"),
        Ok(int("9"))
    );
}

#[test]
fn coalesce_takes_the_left_value_when_present_and_the_right_when_absent() {
    assert_eq!(
        run("pub fn run(): Int = Pair { a: 4, b: 9 }.a ?? 7;\n"),
        Ok(int("4"))
    );
    assert_eq!(
        run("pub fn run(): Int = Pair { a: null, b: 9 }.a ?? 7;\n"),
        Ok(int("7"))
    );
}

#[test]
fn null_equals_only_null() {
    assert_eq!(
        run("pub fn run(): Bool = null == null;\n"),
        Ok(Value::Bool(true))
    );
    assert_eq!(
        run("pub fn run(): Bool = null != null;\n"),
        Ok(Value::Bool(false))
    );
    assert_eq!(
        run("pub fn run(): Bool = Pair { a: 1, b: 2 }.a == null;\n"),
        Ok(Value::Bool(false))
    );
    assert_eq!(
        run("pub fn run(): Bool = Pair { a: null, b: 2 }.a == null;\n"),
        Ok(Value::Bool(true))
    );
}

#[test]
fn a_case_with_a_null_arm_selects_by_absence() {
    let source = "fn pick(p: Pair): Int = case p.a {\n    null: 1,\n    _: 2,\n};\n";
    assert_eq!(
        run(&format!(
            "{source}pub fn run(): Int = pick(Pair {{ a: null, b: 9 }});\n"
        )),
        Ok(int("1"))
    );
    assert_eq!(
        run(&format!(
            "{source}pub fn run(): Int = pick(Pair {{ a: 4, b: 9 }});\n"
        )),
        Ok(int("2"))
    );
}
