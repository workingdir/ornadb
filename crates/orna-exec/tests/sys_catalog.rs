//! The `ProgramCatalog` provider (hir-host.md A5): `sys.catalog.*` rows come from the lowered program, `sys.rt.info`
//! from the embedder's `HostInfo`, and every other `sys` member stays unlinked.

#![cfg(feature = "test-support")]

use num_bigint::BigInt;

use orna_exec::{
    ExecError, ExecLimits, HostInfo, HostRuntimeMode, MemoryStore, ProgramCatalog, Value,
    run_entry_with_host,
};
use orna_hir::{DefId, DefKind, ModulePath, Name, Program, SourceId, SourceUnit, lower, typeck};
use orna_syntax::parse_module_with_file;

const LIMITS: ExecLimits = ExecLimits {
    max_steps: 100_000,
    max_depth: 64,
    max_collection_items: 2_048,
    max_string_bytes: 1 << 20,
    max_integer_digits: 1_024,
};

/// Two tables: `Note` keyed by `id`, and `Tag` with an optional column.
const SOURCE: &str = "pub table Note(id: Int) {\n    text: Str,\n}\n\npub table Tag(code: Str) {\n    label: Str?,\n}\n";

fn lower_source(source: &str) -> Program {
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

fn entry_def(program: &Program, name: &str) -> DefId {
    program
        .modules
        .iter()
        .flat_map(|module| module.items.iter().copied())
        .find(|def| {
            matches!(program.defs[*def].kind, DefKind::Fn(_))
                && program.defs[*def].name.as_str() == name
        })
        .expect("the source defines the entry")
}

fn info() -> HostInfo {
    HostInfo {
        language_version: "1.1".to_owned(),
        sys_version: "1.1".to_owned(),
        canonical_orna_codec_version: "1".to_owned(),
        repository_layout_version: "1".to_owned(),
        storage_manifest_version: "1".to_owned(),
        presentation_protocol_version: "1".to_owned(),
        implementation_name: "orna-exec".to_owned(),
        implementation_version: "test".to_owned(),
        build_id: "test-build".to_owned(),
        supported_profiles: vec!["core".to_owned()],
        mode: HostRuntimeMode::Run,
        read_only: true,
        unicode_version: "15.1".to_owned(),
        timezone_database_version: None,
    }
}

/// Runs `run` with a `ProgramCatalog` over `source`, returning the entry's result.
fn run_with_catalog(source: &str, expression: &str) -> Result<Value, ExecError> {
    let program = lower_source(&format!("{source}\npub fn run(): Int = {expression};\n"));
    let mut host = ProgramCatalog::new(&program, &info(), Box::new(MemoryStore::new()));
    run_entry_with_host(&program, entry_def(&program, "run"), LIMITS, &mut host).result
}

#[test]
fn the_catalogue_lists_every_table_of_the_program() {
    assert_eq!(
        run_with_catalog(SOURCE, "sys.catalog.tables.count()"),
        Ok(Value::Int(BigInt::from(2)))
    );
}

#[test]
fn the_catalogue_lists_every_field_of_every_table() {
    // `Note` has `id` and `text`; `Tag` has `code` and `label`.
    assert_eq!(
        run_with_catalog(SOURCE, "sys.catalog.columns.count()"),
        Ok(Value::Int(BigInt::from(4)))
    );
}

#[test]
fn an_optional_field_is_marked_optional() {
    assert_eq!(
        run_with_catalog(
            SOURCE,
            "sys.catalog.columns.filter(column => column.optional).count()"
        ),
        Ok(Value::Int(BigInt::from(1)))
    );
}

#[test]
fn runtime_info_reads_the_embedder_record() {
    let program = lower_source("pub fn run(): Bool = sys.rt.info().read_only;\n");
    let mut host = ProgramCatalog::new(&program, &info(), Box::new(MemoryStore::new()));
    assert_eq!(
        run_entry_with_host(&program, entry_def(&program, "run"), LIMITS, &mut host).result,
        Ok(Value::Bool(true))
    );
}

#[test]
fn an_unlisted_sys_member_stays_unlinked() {
    assert_eq!(
        run_with_catalog(SOURCE, "sys.Storage.count()"),
        Err(ExecError::Code("ORNA-EVAL-UNSUPPORTED"))
    );
}
