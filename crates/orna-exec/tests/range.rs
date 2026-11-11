//! `core.range` (hand-written Orna, kernel-and-library.md listing 5) over lowered source: a user program constructs a
//! `Range` and calls `contains` through the embedded `core.range` module, then runs on the executor.
//!
//! BLOCKED, in order: (1) the user program does not type clean: `Range { .. }` construction and `Range<Int>` do not
//! agree (G4), and the generic constructor and field types are not substituted (G13). `run` asserts that the user
//! module has no diagnostics, so the un-ignored test fails at that assertion. (2) The executor cannot dispatch the
//! generic protocol member `value.compare` from the embedded `core.range` source (H5). Remove the `ignore` attributes
//! once both are fixed.

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
#[ignore = "blocked: G4 and G13 (user program does not type), then H5 (no generic protocol dispatch in exec)"]
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
#[ignore = "blocked: G4 and G13 (user program does not type), then H5 (no generic protocol dispatch in exec)"]
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
