//! Relation pipelines over a table source: stages collect into a list in ascending key order, and the terminals
//! `every` and `exists` stop at the row that decides them.

use std::rc::Rc;

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

fn lower_one(source: &str) -> Program {
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
    assert_eq!(program.validate(), Ok(()), "lowered program must validate");
    typeck(&mut program);
    assert_eq!(program.validate(), Ok(()), "typed program must validate");
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

/// Three rows inserted out of key order, so the scan must sort them.
const THREE: &str = "    Note.insert({ id: 2, text: \"b\" });\n    Note.insert({ id: 3, text: \"c\" });\n    Note.insert({ id: 1, text: \"a\" });\n";

fn note_row(id: i32, text: &str) -> Value {
    Value::Record(
        vec![
            (Name::new("id"), Value::Int(id.into())),
            (Name::new("text"), Value::Text(text.to_owned())),
        ]
        .into(),
    )
}

fn list(rows: Vec<Value>) -> Value {
    Value::List(Rc::from(rows))
}

#[test]
fn a_filter_over_a_table_collects_the_matching_rows_in_key_order() {
    let source = format!("{NOTE}pub fn run() {{\n{THREE}    Note.filter(row => row.id > 1)\n}}\n");
    assert_eq!(
        run(&source),
        Ok(list(vec![note_row(2, "b"), note_row(3, "c")]))
    );
}

#[test]
fn a_map_over_a_table_collects_the_transformed_rows() {
    let source = format!("{NOTE}pub fn run() {{\n{THREE}    Note.map(row => row.text)\n}}\n");
    assert_eq!(
        run(&source),
        Ok(list(vec![
            Value::Text("a".to_owned()),
            Value::Text("b".to_owned()),
            Value::Text("c".to_owned()),
        ]))
    );
}

#[test]
fn take_and_drop_count_rows_in_key_order() {
    let take = format!("{NOTE}pub fn run() {{\n{THREE}    Note.take(2)\n}}\n");
    assert_eq!(
        run(&take),
        Ok(list(vec![note_row(1, "a"), note_row(2, "b")]))
    );
    let drop = format!("{NOTE}pub fn run() {{\n{THREE}    Note.drop(2)\n}}\n");
    assert_eq!(run(&drop), Ok(list(vec![note_row(3, "c")])));
}

#[test]
fn take_zero_yields_no_rows() {
    let source = format!("{NOTE}pub fn run() {{\n{THREE}    Note.take(0)\n}}\n");
    assert_eq!(run(&source), Ok(list(vec![])));
}

#[test]
fn exists_is_true_when_a_row_satisfies_the_predicate() {
    let source = format!("{NOTE}pub fn run() {{\n{THREE}    Note.exists(row => row.id == 3)\n}}\n");
    assert_eq!(run(&source), Ok(Value::Bool(true)));
}

#[test]
fn exists_is_false_when_no_row_satisfies_the_predicate() {
    let source = format!("{NOTE}pub fn run() {{\n{THREE}    Note.exists(row => row.id > 5)\n}}\n");
    assert_eq!(run(&source), Ok(Value::Bool(false)));
}

#[test]
fn every_is_false_at_the_first_failing_row_and_true_when_all_hold() {
    let all = format!("{NOTE}pub fn run() {{\n{THREE}    Note.every(row => row.id > 0)\n}}\n");
    assert_eq!(run(&all), Ok(Value::Bool(true)));
    let some = format!("{NOTE}pub fn run() {{\n{THREE}    Note.every(row => row.id > 1)\n}}\n");
    assert_eq!(run(&some), Ok(Value::Bool(false)));
}

#[test]
fn a_stage_over_an_empty_table_yields_no_rows() {
    let source = format!("{NOTE}pub fn run() {{\n    Note.filter(row => row.id > 0)\n}}\n");
    assert_eq!(run(&source), Ok(list(vec![])));
}

/// Rows whose text order differs from their key order, so a sort that did nothing would fail.
const SORT_ROWS: &str = "    Note.insert({ id: 1, text: \"z\" });\n    Note.insert({ id: 2, text: \"a\" });\n    Note.insert({ id: 3, text: \"m\" });\n";

#[test]
fn sort_by_orders_the_table_rows_by_the_key() {
    let source =
        format!("{NOTE}pub fn run() {{\n{SORT_ROWS}    Note.sort_by(row => row.text)\n}}\n");
    assert_eq!(
        run(&source),
        Ok(list(vec![
            note_row(2, "a"),
            note_row(3, "m"),
            note_row(1, "z"),
        ]))
    );
}

#[test]
fn sort_by_is_stable_for_equal_keys() {
    let source = format!(
        "{NOTE}pub fn run() {{\n    Note.insert({{ id: 1, text: \"x\" }});\n    Note.insert({{ id: 2, text: \"x\" }});\n    Note.insert({{ id: 3, text: \"a\" }});\n    Note.sort_by(row => row.text)\n}}\n"
    );
    assert_eq!(
        run(&source),
        Ok(list(vec![
            note_row(3, "a"),
            note_row(1, "x"),
            note_row(2, "x"),
        ]))
    );
}

#[test]
fn a_filter_before_sort_sorts_only_the_matching_rows() {
    let source = format!(
        "{NOTE}pub fn run() {{\n{SORT_ROWS}    Note.filter(row => row.id > 1).sort_by(row => row.text)\n}}\n"
    );
    assert_eq!(
        run(&source),
        Ok(list(vec![note_row(2, "a"), note_row(3, "m")]))
    );
}

#[test]
fn a_stage_after_sort_runs_over_the_sorted_rows() {
    let source = format!(
        "{NOTE}pub fn run() {{\n{SORT_ROWS}    Note.sort_by(row => row.text).take(2)\n}}\n"
    );
    assert_eq!(
        run(&source),
        Ok(list(vec![note_row(2, "a"), note_row(3, "m")]))
    );
}
