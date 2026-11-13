//! A `sys` relation a pipeline reads from the host one row at a time (hir-host.md A4): the stages see the rows the host
//! answers, and a `sys` relation the host does not register is `ORNA-EVAL-UNSUPPORTED` (A3).

#![cfg(feature = "test-support")]

use num_bigint::BigInt;

use orna_exec::{ExecError, ExecLimits, FixtureHost, MemoryStore, Value, run_entry_with_host};
use orna_hir::{DefId, DefKind, ModulePath, Name, Program, SourceId, SourceUnit, lower, typeck};
use orna_syntax::parse_module_with_file;
use orna_value::{OPTION_TAG, Raw};

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
    typeck(&mut program);
    assert_eq!(program.validate(), Ok(()), "lowered program must validate");
    program
}

fn entry_def(program: &Program) -> DefId {
    program
        .modules
        .iter()
        .flat_map(|module| module.items.iter().copied())
        .find(|def| {
            matches!(program.defs[*def].kind, DefKind::Fn(_))
                && program.defs[*def].name.as_str() == "run"
        })
        .expect("the source defines `run`")
}
/// An absent `Int?` field: the canonical option none, `Tag(OPTION_TAG, [0])`.
fn absent_int() -> Raw {
    Raw::Tag(
        OPTION_TAG,
        Box::new(Raw::Array(vec![Raw::Int(BigInt::from(0))])),
    )
}

/// One `sys.Storage` row, as the host answers it: a record of its fields.
fn storage_row(pending_rows: i64, rows_exact: bool) -> Raw {
    let field = |name: &str, value: Raw| (Raw::Text(name.to_owned()), value);
    Raw::Map(vec![
        field("reference", Raw::Text("storage:a".to_owned())),
        field("object", Raw::Text("object:a".to_owned())),
        field("profile", Raw::Text("local-durable".to_owned())),
        field("rows", absent_int()),
        field("rows_exact", Raw::Bool(rows_exact)),
        field("bytes", absent_int()),
        field("pending_rows", Raw::Int(BigInt::from(pending_rows))),
    ])
}

fn run_on(host: &mut FixtureHost, expression: &str) -> Result<Value, ExecError> {
    let program = lower_one(&format!("pub fn run(): Int = {expression};\n"));
    run_entry_with_host(&program, entry_def(&program), LIMITS, host).result
}

fn registered_host() -> FixtureHost {
    FixtureHost::new(Box::new(MemoryStore::new())).with_relation(
        "sys.Storage",
        vec![storage_row(3, true), storage_row(0, false)],
    )
}

#[test]
fn a_pipeline_counts_the_rows_the_host_answers() {
    assert_eq!(
        run_on(&mut registered_host(), "sys.Storage.count()"),
        Ok(Value::Int(BigInt::from(2)))
    );
}

#[test]
fn a_filter_keeps_only_the_rows_its_predicate_admits() {
    assert_eq!(
        run_on(
            &mut registered_host(),
            "sys.Storage.filter(row => row.rows_exact).count()"
        ),
        Ok(Value::Int(BigInt::from(1)))
    );
}

#[test]
fn a_map_then_take_reads_the_projected_rows_and_stops() {
    assert_eq!(
        run_on(
            &mut registered_host(),
            "sys.Storage.map(row => row.pending_rows).take(1).count()"
        ),
        Ok(Value::Int(BigInt::from(1)))
    );
}

#[test]
fn an_unregistered_sys_relation_is_unsupported() {
    let mut host = FixtureHost::new(Box::new(MemoryStore::new()));
    assert_eq!(
        run_on(&mut host, "sys.Storage.count()"),
        Err(ExecError::Code("ORNA-EVAL-UNSUPPORTED"))
    );
}
