//! `insert` and `delete` over lowered source, against the old evaluator's
//! outcomes: `ORNA-EVAL-TABLE-DUPLICATE-KEY` for a key already holding a row,
//! `ORNA-EVAL-TABLE-MISSING-ROW` for a delete of an absent key, and
//! `ORNA-EVAL-TABLE-ROW` for a row that omits a stored field.

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

const NOTE: &str = "pub table Note(id: Int) {\n    text: Str,\n}\n";

#[test]
fn a_second_insert_at_the_same_key_fails_as_a_duplicate() {
    let source = format!(
        "{NOTE}pub fn run() {{\n    Note.insert({{ id: 1, text: \"first\" }});\n    Note.insert({{ id: 1, text: \"second\" }});\n}}\n"
    );
    assert_eq!(
        run(&source),
        Err(ExecError::Code("ORNA-EVAL-TABLE-DUPLICATE-KEY"))
    );
}

#[test]
fn deleting_an_absent_key_fails_as_a_missing_row() {
    let source = format!("{NOTE}pub fn run() {{\n    Note.delete(7);\n}}\n");
    assert_eq!(
        run(&source),
        Err(ExecError::Code("ORNA-EVAL-TABLE-MISSING-ROW"))
    );
}

#[test]
fn an_insert_then_a_delete_then_a_reinsert_at_one_key_succeeds() {
    let source = format!(
        "{NOTE}pub fn run() {{\n    Note.insert({{ id: 1, text: \"a\" }});\n    Note.delete(1);\n    Note.insert({{ id: 1, text: \"b\" }});\n}}\n"
    );
    assert_eq!(run(&source), Ok(Value::Unit));
}

#[test]
fn a_row_that_omits_a_stored_field_is_a_table_row_error() {
    let source = format!("{NOTE}pub fn run() {{\n    Note.insert({{ id: 1 }});\n}}\n");
    assert_eq!(run(&source), Err(ExecError::Code("ORNA-EVAL-TABLE-ROW")));
}

#[test]
fn a_field_default_is_deferred_to_the_next_stage() {
    let source = "pub table Event(id: Int) {\n    label: Str = \"default label\",\n}\n\
                  pub fn run() {\n    Event.insert({ id: 1 });\n}\n";
    assert_eq!(
        run(source),
        Err(ExecError::NotYet("table field default (H6 slice B3)"))
    );
}
