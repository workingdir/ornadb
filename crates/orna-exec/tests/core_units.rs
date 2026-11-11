//! The built-in time literals and the `core.units` data-size family over lowered source.
//!
//! `1.hour` and `30.minute` are built-in postfix literals (lower/literals.rs `builtin_postfix`), not `core.units`
//! declarations: the time family is not declared in `core.units` yet (see interface/core/units.orna). The data-size
//! family is declared there, and a unit literal types through `resolve_unit`. The executor does not run unit
//! constructors yet (eval.rs `construct` answers `NotYet` for a non-variant callee), so the data-size case checks
//! typing only.

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

fn user_diagnostics(program: &Program) -> Vec<String> {
    program
        .diagnostics()
        .filter(|(module, _)| program.modules[*module].path.to_string() == "main")
        .map(|(_, diagnostic)| format!("{} {}", diagnostic.code, diagnostic.message))
        .collect()
}

fn run(source: &str) -> Result<Value, ExecError> {
    let program = lower_typed(source);
    let diagnostics = user_diagnostics(&program);
    assert!(
        diagnostics.is_empty(),
        "the user program must type clean: {diagnostics:?}"
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

#[test]
fn hour_plus_minute_types_as_duration_and_runs_to_nanosecond_result() {
    let value = run("pub fn run(): Duration = 1.hour + 30.minute;\n");
    match value {
        Ok(Value::Duration {
            seconds,
            nanosecond,
        }) => {
            assert_eq!(seconds.to_string(), "5400");
            assert_eq!(nanosecond, 0);
        }
        other => panic!("expected a 5400 second Duration, got {other:?}"),
    }
}

#[test]
fn data_size_unit_literal_types_clean_through_core_units() {
    let program = lower_typed("pub fn run(): Int { 1.MiB; 0 }\n");
    let diagnostics = user_diagnostics(&program);
    assert!(
        diagnostics.is_empty(),
        "a `core.units` literal must type clean: {diagnostics:?}"
    );
}
