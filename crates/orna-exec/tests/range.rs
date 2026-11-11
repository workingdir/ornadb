//! `core.range` (hand-written Orna, kernel-and-library.md listing 5) over lowered source: a user program constructs a
//! `Range` and calls `contains` through the embedded `core.range` module, then runs on the executor.
//!
//! BLOCKED (HIR typeck, not exec): a call to a user-generic fn (`contains<T>`) whose type arguments are inferred
//! never has those solved targs written back to `Callee::Source.targs`; typeck/call.rs `source_call_ty` creates the
//! inference variables but `finish` zonks only expression and local types. The executor then sees an empty
//! environment and fails with `NotYet("type argument outside the caller's environment (H5)")`. The executor protocol
//! dispatch is covered by tests/protocol_dispatch.rs. Remove the `ignore` attributes once HIR records targs.

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

const HEAD: &str = "use core.range.{Range, contains};\n";

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

fn run(source: &str) -> Result<Value, ExecError> {
    let program = lower_typed(source);
    let user_diagnostics: Vec<String> = program
        .diagnostics()
        .filter(|(module, _)| program.modules[*module].path.to_string() == "main")
        .map(|(_, diagnostic)| format!("{} {}", diagnostic.code, diagnostic.message))
        .collect();
    assert!(
        user_diagnostics.is_empty(),
        "the user program must type clean: {user_diagnostics:?}"
    );
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

fn boolean(source: &str) -> Result<Value, ExecError> {
    run(&format!("{HEAD}{source}"))
}

#[test]
#[ignore = "blocked: HIR does not record inferred type arguments on generic calls (see module doc)"]
fn closed_range_admits_both_bounds_only_when_inclusive() {
    assert_eq!(
        boolean(
            "pub fn run(): Bool = contains(Range { lower: 1, upper: 5, inclusive: false }, 3);\n"
        ),
        Ok(Value::Bool(true))
    );
    assert_eq!(
        boolean(
            "pub fn run(): Bool = contains(Range { lower: 1, upper: 5, inclusive: false }, 5);\n"
        ),
        Ok(Value::Bool(false))
    );
    assert_eq!(
        boolean(
            "pub fn run(): Bool = contains(Range { lower: 1, upper: 5, inclusive: true }, 5);\n"
        ),
        Ok(Value::Bool(true))
    );
    assert_eq!(
        boolean(
            "pub fn run(): Bool = contains(Range { lower: 1, upper: 5, inclusive: true }, 0);\n"
        ),
        Ok(Value::Bool(false))
    );
}

#[test]
#[ignore = "blocked: HIR does not record inferred type arguments on generic calls (see module doc)"]
fn lower_bound_is_inclusive_and_absent_bounds_are_open() {
    assert_eq!(
        boolean(
            "pub fn run(): Bool = contains(Range { lower: 1, upper: null, inclusive: false }, 1);\n"
        ),
        Ok(Value::Bool(true))
    );
    assert_eq!(
        boolean(
            "pub fn run(): Bool = contains(Range { lower: null, upper: 5, inclusive: false }, -100);\n"
        ),
        Ok(Value::Bool(true))
    );
    assert_eq!(
        boolean(
            "pub fn run(): Bool = contains(Range { lower: null, upper: null, inclusive: false }, 7);\n"
        ),
        Ok(Value::Bool(true))
    );
}
