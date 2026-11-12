//! Table writes over lowered source, against the old evaluator's outcomes:
//! `ORNA-EVAL-TABLE-MISSING-ROW` for a delete, update or rekey of an absent key in a run whose store starts empty;
//! a run over a caller's store declines the same write as NotYet until H5 binds the activation snapshot.
//! `ORNA-EVAL-TABLE-ROW` for a row that omits a stored field or writes a computed or key field it may not.

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
fn deleting_an_absent_key_fails_as_missing_row() {
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
fn a_field_default_fills_the_field_an_insert_omits() {
    let source = "pub table Event(id: Int) {\n    label: Str = \"default label\",\n}\n\
                  pub fn run() {\n    Event.insert({ id: 1 })\n}\n";
    let expected = Value::Record(
        vec![
            (Name::new("id"), Value::Int(1.into())),
            (Name::new("label"), Value::Text("default label".to_owned())),
        ]
        .into(),
    );
    assert_eq!(run(source), Ok(expected));
}

fn note_row(id: i32, text: &str) -> Value {
    Value::Record(
        vec![
            (Name::new("id"), Value::Int(id.into())),
            (Name::new("text"), Value::Text(text.to_owned())),
        ]
        .into(),
    )
}

#[test]
fn an_upsert_inserts_an_absent_key_and_merges_into_a_present_one() {
    let source = format!(
        "{NOTE}pub fn run() {{\n    Note.upsert({{ id: 1, text: \"a\" }});\n    Note.upsert({{ id: 1, text: \"b\" }})\n}}\n"
    );
    assert_eq!(run(&source), Ok(note_row(1, "b")));
}

#[test]
fn an_update_rewrites_the_named_fields_of_a_present_row() {
    let source = format!(
        "{NOTE}pub fn run() {{\n    Note.insert({{ id: 1, text: \"a\" }});\n    Note.update(1, {{ text: \"b\" }})\n}}\n"
    );
    assert_eq!(run(&source), Ok(note_row(1, "b")));
}

#[test]
fn an_update_of_an_absent_key_fails_as_missing_row() {
    let source = format!("{NOTE}pub fn run() {{\n    Note.update(9, {{ text: \"b\" }});\n}}\n");
    assert_eq!(
        run(&source),
        Err(ExecError::Code("ORNA-EVAL-TABLE-MISSING-ROW"))
    );
}

#[test]
fn an_update_that_names_a_key_field_is_a_table_row_error() {
    let source = format!(
        "{NOTE}pub fn run() {{\n    Note.insert({{ id: 1, text: \"a\" }});\n    Note.update(1, {{ id: 2 }});\n}}\n"
    );
    assert_eq!(run(&source), Err(ExecError::Code("ORNA-EVAL-TABLE-ROW")));
}

#[test]
fn a_rekey_moves_the_row_to_the_new_key() {
    let source = format!(
        "{NOTE}pub fn run() {{\n    Note.insert({{ id: 1, text: \"a\" }});\n    Note.rekey(1, 2);\n    Note.delete(2)\n}}\n"
    );
    assert_eq!(run(&source), Ok(Value::Unit));
}

#[test]
fn a_rekey_leaves_no_row_at_the_old_key() {
    let source = format!(
        "{NOTE}pub fn run() {{\n    Note.insert({{ id: 1, text: \"a\" }});\n    Note.rekey(1, 2);\n    Note.delete(1);\n}}\n"
    );
    assert_eq!(
        run(&source),
        Err(ExecError::Code("ORNA-EVAL-TABLE-MISSING-ROW"))
    );
}

#[test]
fn a_rekey_onto_an_existing_key_fails_as_a_duplicate() {
    let source = format!(
        "{NOTE}pub fn run() {{\n    Note.insert({{ id: 1, text: \"a\" }});\n    Note.insert({{ id: 2, text: \"b\" }});\n    Note.rekey(1, 2);\n}}\n"
    );
    assert_eq!(
        run(&source),
        Err(ExecError::Code("ORNA-EVAL-TABLE-DUPLICATE-KEY"))
    );
}

#[test]
fn count_of_an_empty_table_is_zero() {
    let source = format!("{NOTE}pub fn run() {{\n    Note.count()\n}}\n");
    assert_eq!(run(&source), Ok(Value::Int(0.into())));
}

#[test]
fn count_is_the_number_of_stored_rows() {
    let source = format!(
        "{NOTE}pub fn run() {{\n    Note.insert({{ id: 1, text: \"a\" }});\n    Note.insert({{ id: 2, text: \"b\" }});\n    Note.count()\n}}\n"
    );
    assert_eq!(run(&source), Ok(Value::Int(2.into())));
}

#[test]
fn first_of_an_empty_table_is_none() {
    let source = format!("{NOTE}pub fn run() {{\n    Note.first()\n}}\n");
    assert_eq!(run(&source), Ok(Value::Option(None)));
}

#[test]
fn first_is_the_row_with_the_least_key() {
    let source = format!(
        "{NOTE}pub fn run() {{\n    Note.insert({{ id: 2, text: \"b\" }});\n    Note.insert({{ id: 1, text: \"a\" }});\n    Note.first()\n}}\n"
    );
    assert_eq!(
        run(&source),
        Ok(Value::Option(Some(std::rc::Rc::new(note_row(1, "a")))))
    );
}

#[test]
fn one_of_an_empty_table_fails_as_zero_rows() {
    let source = format!("{NOTE}pub fn run() {{\n    Note.one()\n}}\n");
    assert_eq!(
        run(&source),
        Err(ExecError::Code("ORNA-EVAL-RELATION-ONE-ZERO"))
    );
}

#[test]
fn one_of_a_single_row_is_that_row() {
    let source = format!(
        "{NOTE}pub fn run() {{\n    Note.insert({{ id: 7, text: \"x\" }});\n    Note.one()\n}}\n"
    );
    assert_eq!(run(&source), Ok(note_row(7, "x")));
}

#[test]
fn one_of_two_rows_fails_as_many_rows() {
    let source = format!(
        "{NOTE}pub fn run() {{\n    Note.insert({{ id: 1, text: \"a\" }});\n    Note.insert({{ id: 2, text: \"b\" }});\n    Note.one()\n}}\n"
    );
    assert_eq!(
        run(&source),
        Err(ExecError::Code("ORNA-EVAL-RELATION-ONE-MULTIPLE"))
    );
}
