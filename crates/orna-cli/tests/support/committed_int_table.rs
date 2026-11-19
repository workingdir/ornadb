//! A committed `Item(id: Int)` table for the table-store tests. The rows enter
//! the table through the `orna` binary (`run main.seed`, then `publish`), and
//! they are read back through [`CommittedTableStore`], the production committed
//! read. Included by a test with `#[path]`, next to `format3`.

use std::{
    collections::BTreeMap,
    fmt::Write,
    path::Path,
    process::{Command, Output},
};

use orna_application::{ApplicationAuthority, CommittedTableIdentity};
use orna_check::{Catalogue, TableSchema};
use orna_evaluator::Limits;
use orna_exec::TableId;
use orna_hir::DefId;
use orna_project::{CommittedTableStore, committed_tables};
use orna_repository::{GitCommitRef, Repository, RepositoryFormatContext};
use orna_runtime::TableObjectId;
use tempfile::TempDir;

use crate::format3;

/// The committed table's name.
pub const TABLE: &str = "Item";

/// The table id the committed store serves [`TABLE`] under.
pub fn table_id() -> TableId {
    DefId::new(1)
}

/// Buffer quota for reverse scans; forward scans hold one page at a time.
const MAX_BUFFERED_ROWS: usize = 4_096;

/// A committed `Item` table and the project directory that holds it. The store
/// and the inputs are declared before the directory, so they drop before the
/// directory they read from.
pub struct CommittedIntTable {
    pub store: CommittedTableStore,
    inputs: CommittedInputs,
    _directory: TempDir,
}

/// The inputs that the committed reads of the `Item` table take: the repository,
/// its HEAD commit, the commit's format context, and the table's schema and id.
struct CommittedInputs {
    repository: Repository,
    format: RepositoryFormatContext,
    commit: GitCommitRef,
    schemas: BTreeMap<String, TableSchema>,
    table_ids: BTreeMap<String, TableObjectId>,
}

impl CommittedIntTable {
    /// The `(runtime key, stored tuple)` rows of `Item` that the production
    /// committed read materialises, in ascending runtime-key order.
    pub fn production_rows(&self) -> Vec<(Vec<u8>, Vec<u8>)> {
        let inputs = &self.inputs;
        let mut tables = committed_tables(
            &inputs.repository,
            &inputs.format,
            &inputs.commit,
            &inputs.schemas,
            &inputs.table_ids,
        )
        .expect("the committed tables read");
        let table = tables.remove(TABLE).expect("the Item table is committed");
        table.rows
    }
}

/// Rows each seed run inserts. One `orna run` is charged against the
/// evaluator's per-run step budget, so a large table is seeded in batches.
const BATCH_ROWS: u32 = 2_000;

/// The start of each seed batch for the rows `first..end`. An empty range still
/// yields one batch, which inserts nothing.
fn batch_starts(first: u32, end: u32) -> Vec<u32> {
    (first..end.max(first + 1))
        .step_by(BATCH_ROWS as usize)
        .collect()
}

/// The Orna module with one `seed_<start>` function per batch, each inserting
/// its rows `start..min(start + BATCH_ROWS, end)`.
pub fn seed_source(first: u32, end: u32) -> String {
    let mut source = format!("pub table {TABLE}(id: Int) {{\n    label: Str,\n}}\n\n");
    for start in batch_starts(first, end) {
        let batch_end = (start + BATCH_ROWS).min(end);
        writeln!(
            source,
            "pub fn seed_{start}() {{\n    let cursor = {start};\n    while cursor < {batch_end} {{\n        \
             {TABLE}.insert({{ id: cursor, label: \"row\" }});\n        cursor += 1;\n    }};\n}}\n"
        )
        .expect("writing to a String cannot fail");
    }
    source
}

/// The canonical runtime key of `Item` row `index`, as the committed store
/// serves it.
pub fn runtime_key(index: u32) -> Vec<u8> {
    orna_value::Value::int(index.into())
        .encode()
        .expect("an Int key encodes")
}

/// Commits a project whose `Item` table holds the rows `first..end` and opens
/// it as a committed store. `first == end` commits an empty table.
pub fn committed_int_table(first: u32, end: u32) -> CommittedIntTable {
    assert!(first <= end, "the seeded range must not be inverted");
    let source = seed_source(first, end);
    let (directory, _repository, object_id) = format3::format3_repository_for_table(&source, TABLE);
    for start in batch_starts(first, end) {
        assert_success(
            &orna(directory.path(), &["run", &format!("main.seed_{start}")]),
            "seed run",
        );
    }
    assert_success(
        &orna(directory.path(), &["publish", "--message", "seed"]),
        "publish seeded rows",
    );
    let inputs = committed_inputs(directory.path(), object_id, &source, first);
    let store = open_committed(&inputs);
    CommittedIntTable {
        store,
        inputs,
        _directory: directory,
    }
}

/// Reads the committed inputs of the `Item` table at HEAD of the project in
/// `directory`, admitting `source` as the module that seeds `seed_{first}`.
fn committed_inputs(
    directory: &Path,
    object_id: [u8; 16],
    source: &str,
    first: u32,
) -> CommittedInputs {
    let repository = Repository::discover(directory).expect("the project repository");
    let commit = repository
        .head()
        .expect("HEAD is readable")
        .expect("HEAD names a commit");
    let format = repository
        .open_pinned_format_context(commit.as_str())
        .expect("the committed format context");
    let object_id = TableObjectId::new(object_id);
    let admitted = ApplicationAuthority::new(Catalogue::authoritative_core(), Limits::default())
        .admit_module_with_committed_table_metadata(
            "main.orna",
            source,
            format!("seed_{first}"),
            [CommittedTableIdentity::new(TABLE, object_id)],
        )
        .expect("the committed table admits");
    let schemas: BTreeMap<String, TableSchema> = admitted
        .table_schemas()
        .iter()
        .map(|(name, table)| (name.clone(), table.schema().clone()))
        .collect();
    let table_ids = BTreeMap::from([(TABLE.to_owned(), object_id)]);
    CommittedInputs {
        repository,
        format,
        commit,
        schemas,
        table_ids,
    }
}

/// Opens the committed store over `inputs`, serving the `Item` table under
/// [`table_id`].
fn open_committed(inputs: &CommittedInputs) -> CommittedTableStore {
    CommittedTableStore::open_committed(
        &inputs.repository,
        &inputs.format,
        &inputs.commit,
        &inputs.schemas,
        &inputs.table_ids,
        [(table_id(), TABLE.to_owned())],
        MAX_BUFFERED_ROWS,
    )
    .expect("the committed store opens")
}

fn orna(directory: &Path, arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_orna"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(["--db", directory.to_str().expect("UTF-8 path")])
        .args(arguments)
        .output()
        .expect("orna CLI process")
}

fn assert_success(output: &Output, label: &str) {
    assert_eq!(
        output.status.code(),
        Some(0),
        "{label} failed: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
