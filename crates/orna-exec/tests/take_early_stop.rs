//! `take` over a large table stops the scan at its count: the run reads only the rows it returns.

use orna_exec::{ExecLimits, MemoryStore, TableStore, Value, measure_entry_with_store};
use orna_hir::{DefId, DefKind, ModulePath, Name, Program, SourceId, SourceUnit, lower, typeck};
use orna_syntax::parse_module_with_file;

const LIMITS: ExecLimits = ExecLimits {
    max_steps: 100_000,
    max_depth: 64,
    max_collection_items: 2_048,
    max_string_bytes: 1 << 20,
    max_integer_digits: 1_024,
};

const ROWS: i32 = 100_000;

const SOURCE: &str = "pub table Note(id: Int) {\n    text: Str,\n}\n\npub fn put(id: Int) {\n    Note.insert({ id: id, text: \"a\" });\n}\n\npub fn run() {\n    Note.take(10)\n}\n";

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

fn def_named(program: &Program, name: &str) -> DefId {
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

#[test]
fn take_ten_over_a_hundred_thousand_rows_reads_ten_rows() {
    let program = lower_one(SOURCE);
    let put = def_named(&program, "put");
    let run = def_named(&program, "run");

    let mut store: Box<dyn TableStore> = Box::new(MemoryStore::new());
    for id in 1..=ROWS {
        let (result, back) =
            measure_entry_with_store(&program, put, vec![Value::Int(id.into())], LIMITS, store);
        result.expect("insert one row");
        store = back;
    }

    let (result, _) = measure_entry_with_store(&program, run, Vec::new(), LIMITS, store);
    let measured = result.expect("take ten rows");
    let Value::List(rows) = &measured.value else {
        panic!("take returns a list");
    };
    assert_eq!(rows.len(), 10, "take returns exactly its count");
    assert_eq!(
        measured.rows_read, 10,
        "the scan stops at the tenth row, leaving the other 99,990 unread"
    );
}
