//! Committed table read byte bound (ornadb-03ol, Orna-1.1.0 10-execution.md).
//! The committed read admits each row's key and stored-tuple bytes against the
//! byte bound before the row is retained, and refuses the first row past it.
//! These tests drive the production `committed_tables` over a real committed
//! format-3 table with an injected byte limit.

use std::{
    collections::BTreeMap,
    path::Path,
    process::{Command, Output},
};

use orna_application::{ApplicationAuthority, CommittedTableIdentity};
use orna_check::{Catalogue, TableSchema};
use orna_evaluator::Limits;
use orna_project::{CommittedReadError, CommittedReadLimits, committed_tables};
use orna_repository::Repository;
use orna_runtime::TableObjectId;

use orna_cli_testkit::format3;

const SOURCE: &str = "pub table music(id: Str) { title: Str, artist: Str, }\n\
pub fn main() = music | count();\n";

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

#[test]
fn committed_read_refuses_the_row_past_the_byte_limit_before_it_is_retained() {
    let (directory, _repository, relation_id) =
        format3::format3_repository_for_table(SOURCE, "music");
    for row in [
        "main.music.insert({ id: \"london\", title: \"Original\", artist: \"Kieran\" })",
        "main.music.insert({ id: \"paris\", title: \"Second\", artist: \"Kieran\" })",
        "main.music.insert({ id: \"rome\", title: \"Third\", artist: \"Kieran\" })",
    ] {
        assert_success(&orna(directory.path(), &["repl", row]), "REPL seed insert");
    }
    assert_success(
        &orna(directory.path(), &["publish", "--message", "seed"]),
        "publish seeded rows",
    );

    let repository = Repository::discover(directory.path()).expect("the project repository");
    let commit = repository
        .head()
        .expect("HEAD is readable")
        .expect("HEAD names a commit");
    let format = repository
        .open_pinned_format_context(commit.as_str())
        .expect("the committed format context");
    let object_id = TableObjectId::new(relation_id);
    let admitted = ApplicationAuthority::new(Catalogue::authoritative_core(), Limits::default())
        .admit_module_with_committed_table_metadata(
            "main.orna",
            SOURCE,
            "main",
            [CommittedTableIdentity::new("music", object_id)],
        )
        .expect("the committed table admits");
    let schemas: BTreeMap<String, TableSchema> = admitted
        .table_schemas()
        .iter()
        .map(|(name, table)| (name.clone(), table.schema().clone()))
        .collect();
    let table_ids = BTreeMap::from([("music".to_owned(), object_id)]);
    let read = |max_bytes: usize| {
        committed_tables(
            &repository,
            &format,
            &commit,
            &schemas,
            &table_ids,
            CommittedReadLimits { max_bytes },
        )
    };

    // At no byte limit, all three rows are admitted in key order.
    let tables = read(usize::MAX).expect("three rows fit an unbounded read");
    assert_eq!(tables["music"].rows.len(), 3);
    // A one-byte bound refuses the first row before any row is retained.
    let Err(error) = read(1) else {
        panic!("a one-byte bound refuses the first row");
    };
    assert_eq!(error, CommittedReadError::Limit { rows_seen: 1 });
}
