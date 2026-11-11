//! Top-level `let` bindings as session globals: evaluated once, read by later runs from the cache.
//!
//! A `let` definition is an implementation `static` property (`Type.name`), so the fixtures below read them through
//! that path.

use orna_exec::{
    ExecError, ExecLimits, Globals, MemoryStore, Value, evaluate_global, measure_entry_in,
};
use orna_hir::{DefId, DefKind, ModulePath, Name, Program, SourceId, SourceUnit, lower};
use orna_syntax::parse_module_with_file;

const LIMITS: ExecLimits = ExecLimits {
    max_steps: 100_000,
    max_depth: 64,
    max_collection_items: 2_048,
    max_string_bytes: 1 << 20,
    max_integer_digits: 1_024,
};

const SOURCE: &str = "pub protocol Currency {
    static code: Str;
    static minor_digits: Int;
}
pub type Gbp {
    impl Currency {
        static code = \"GBP\";
        static minor_digits = 2;
    }
}
pub fn label(): Str = Gbp.code;
pub fn digits_plus(extra: Int): Int = Gbp.minor_digits + extra;
";

fn lower_source(source: &str) -> Program {
    let parsed = parse_module_with_file(source, "session.orna");
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

fn fn_named(program: &Program, name: &str) -> DefId {
    program
        .modules
        .iter()
        .flat_map(|module| module.items.iter().copied())
        .find(|def| {
            matches!(program.defs[*def].kind, DefKind::Fn(_))
                && program.defs[*def].name.as_str() == name
        })
        .unwrap_or_else(|| panic!("the source defines `{name}`"))
}

fn let_named(program: &Program, name: &str) -> DefId {
    program
        .defs
        .iter_enumerated()
        .find(|(_, def)| matches!(def.kind, DefKind::Let(_)) && def.name.as_str() == name)
        .map(|(id, _)| id)
        .unwrap_or_else(|| panic!("the source declares the static `{name}`"))
}

fn int(value: &str) -> Value {
    Value::Int(value.parse().expect("integer literal"))
}

fn text(value: &str) -> Value {
    Value::Text(value.to_owned())
}

#[test]
fn let_is_evaluated_once_and_cached() {
    let program = lower_source(SOURCE);
    let code = let_named(&program, "code");

    let mut globals = Globals::new();
    let (result, store) = evaluate_global(
        &program,
        code,
        LIMITS,
        Box::new(MemoryStore::new()),
        &mut globals,
    );
    assert_eq!(result, Ok(text("GBP")));
    assert_eq!(globals.get(code), Some(&text("GBP")));

    let (result, _) = evaluate_global(&program, code, LIMITS, store, &mut globals);
    assert_eq!(result, Ok(text("GBP")));
}

#[test]
fn later_run_reads_let_from_session_cache() {
    let program = lower_source(SOURCE);
    let label = fn_named(&program, "label");
    let code = let_named(&program, "code");

    let mut globals = Globals::new();
    let (result, store) = measure_entry_in(
        &program,
        label,
        Vec::new(),
        LIMITS,
        Box::new(MemoryStore::new()),
        &mut globals,
    );
    assert_eq!(result.map(|measured| measured.value), Ok(text("GBP")));
    assert_eq!(
        globals.get(code),
        Some(&text("GBP")),
        "the run caches the let it read"
    );

    let (result, _) = measure_entry_in(&program, label, Vec::new(), LIMITS, store, &mut globals);
    assert_eq!(result.map(|measured| measured.value), Ok(text("GBP")));
}

#[test]
fn let_value_feeds_arithmetic_with_a_parameter() {
    let program = lower_source(SOURCE);
    let digits_plus = fn_named(&program, "digits_plus");

    let mut globals = Globals::new();
    let (result, _) = measure_entry_in(
        &program,
        digits_plus,
        vec![int("40")],
        LIMITS,
        Box::new(MemoryStore::new()),
        &mut globals,
    );
    assert_eq!(result.map(|measured| measured.value), Ok(int("42")));
}

#[test]
fn evaluating_a_function_as_a_global_is_refused() {
    let program = lower_source(SOURCE);
    let label = fn_named(&program, "label");

    let mut globals = Globals::new();
    let (result, _) = evaluate_global(
        &program,
        label,
        LIMITS,
        Box::new(MemoryStore::new()),
        &mut globals,
    );
    assert_eq!(result, Err(ExecError::NotYet("global is not a let")));
}
