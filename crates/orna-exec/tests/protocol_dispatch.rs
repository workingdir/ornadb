//! Protocol member dispatch on concrete receivers: `a.compare(b)` resolves to the engine-derived `Order` instance for
//! the scalar kind, and to a user `impl Order` for a user type, without a generic call (the HIR records no inferred
//! type arguments on generic calls yet; see tests/range.rs).

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
    let program = lower_typed(&format!("use core.{{Ordering}};\n{body}"));
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

#[test]
#[ignore = "blocked: user-code `Ordering.less` fails typeck (H2); see step 3"]
fn a_scalar_compare_resolves_to_the_engine_order_instance() {
    assert_eq!(
        run("pub fn run(): Bool = 1.compare(2) == Ordering.less;\n"),
        Ok(Value::Bool(true))
    );
    assert_eq!(
        run("pub fn run(): Bool = 3.compare(3) == Ordering.equal;\n"),
        Ok(Value::Bool(true))
    );
    assert_eq!(
        run("pub fn run(): Bool = 5.compare(4) == Ordering.greater;\n"),
        Ok(Value::Bool(true))
    );
}

#[test]
#[ignore = "blocked: user-code `Ordering.less` fails typeck (H2); see step 3"]
fn a_text_compare_orders_by_code_point() {
    assert_eq!(
        run("pub fn run(): Bool = \"apple\".compare(\"banana\") == Ordering.less;\n"),
        Ok(Value::Bool(true))
    );
    assert_eq!(
        run("pub fn run(): Bool = \"b\".compare(\"a\") == Ordering.greater;\n"),
        Ok(Value::Bool(true))
    );
}
