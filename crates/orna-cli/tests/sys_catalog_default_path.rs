//! The default path serves the linked `sys.catalog.*` members (`tables`,
//! `columns`) from the admitted table schemas of a project, through both
//! `orna run` and `orna repl`. A catalogue member with no linked provider is
//! refused at admission, not evaluated. The rows carry typed references over
//! the committed identities (ORNA-SYS-111, ORNA-SYS-112): a project with no
//! commit has no such identity, so its catalogue is refused, never served with
//! null references.

use std::{
    io::Write,
    path::Path,
    process::{Command, Output, Stdio},
};

use tempfile::TempDir;

/// The implementation `table_id` of every `sys.TableRef`
/// (`orna_value::relations::TABLE`).
const TABLE_RELATION: &str = "3ef4b8a0-cd58-49f6-a081-dbc6948fa81e";

const CATALOGUE_SOURCE: &str = r#"pub table Item(id: Str) {
    name: Str,
}

pub fn name_column(): Str {
    sys.catalog.columns | filter(c => c.name == "name") | map(c => c.name) | first() ?? "none"
}

pub fn columns_found_by_table_reference(): Int {
    sys.catalog.columns | flat_map(c => sys.catalog.tables | filter(t => t.reference == c.table) | map(t => c.name)) | count()
}
"#;

fn orna(directory: &Path, arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_orna"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(["--db", directory.to_str().expect("UTF-8 path")])
        .args(arguments)
        .output()
        .expect("CLI process")
}

fn git(directory: &Path, arguments: &[&str]) {
    let status = Command::new("git")
        .current_dir(directory)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args([
            "-c",
            "user.name=orna-test",
            "-c",
            "user.email=orna-test@example.invalid",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(arguments)
        .status()
        .expect("git process");
    assert!(status.success(), "git {arguments:?}: {status:?}");
}

/// An initialised project with `source` staged and no commit.
fn uncommitted_project(source: &str) -> TempDir {
    let directory = tempfile::tempdir().expect("project directory");
    let init = Command::new(env!("CARGO_BIN_EXE_orna"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(["init", directory.path().to_str().expect("UTF-8 path")])
        .output()
        .expect("CLI initialization");
    assert!(init.status.success(), "init: {init:?}");
    std::fs::write(directory.path().join("main.orna"), source).expect("project source");
    git(directory.path(), &["init", "-q"]);
    git(directory.path(), &["config", "user.name", "Orna Test"]);
    git(
        directory.path(),
        &["config", "user.email", "orna-test@example.invalid"],
    );
    git(directory.path(), &["add", "-A"]);
    directory
}

/// A project with an admitted table, published with `orna commit` so the
/// REPL and table reads have committed identities to serve.
fn committed_project(source: &str) -> TempDir {
    let directory = uncommitted_project(source);
    let committed = orna(directory.path(), &["commit", "-m", "publish catalogue"]);
    assert!(committed.status.success(), "commit: {committed:?}");
    directory
}

/// The `database_id` the project's repository metadata records.
fn database_id(directory: &Path) -> String {
    let metadata = std::fs::read_to_string(directory.join(".orna").join("database.orna"))
        .expect("repository metadata");
    metadata
        .lines()
        .find_map(|line| {
            line.trim()
                .strip_prefix("database_id: \"")
                .and_then(|rest| rest.strip_suffix("\","))
        })
        .expect("the metadata records a database id")
        .to_owned()
}

fn repl(directory: &Path, input: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_orna"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(["--db", directory.to_str().expect("UTF-8 path"), "repl"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the REPL starts");
    child
        .stdin
        .take()
        .expect("REPL stdin")
        .write_all(format!("{input}\n:quit\n").as_bytes())
        .expect("write REPL input");
    child.wait_with_output().expect("the REPL exits")
}

fn combined(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[test]
// Specified: ORNA-SYS-111; `sys.Column.table` is a `sys.TableRef`, not the table's name.
fn repl_names_the_owning_table_of_a_column_by_reference() {
    let project = committed_project(CATALOGUE_SOURCE);
    let output = repl(
        project.path(),
        r#"sys.catalog.columns | filter(c => c.name == "name") | map(c => c.table)"#,
    );
    let text = combined(&output);
    assert!(
        !text.contains("error["),
        "the REPL refused the read: {text}"
    );
    let database = database_id(project.path());
    assert!(
        text.contains(&format!("RowRef([{database}, {TABLE_RELATION},")),
        "the column's owning table is a sys.TableRef of the project's database: {text}"
    );
    assert!(
        !text.contains("\"Item\""),
        "the owning table is not served as text: {text}"
    );
}

#[test]
// Specified: ORNA-SYS-111, ORNA-SYS-112; a row reference is the key of its row,
// so the references a table lists for its columns are the columns' own.
fn repl_column_references_match_the_columns_a_table_lists() {
    let project = committed_project(CATALOGUE_SOURCE);
    let output = repl(
        project.path(),
        "sys.catalog.columns | flat_map(c => sys.catalog.tables | flat_map(t => t.columns | filter(n => n.reference == c.reference) | map(n => n.name)))",
    );
    let text = combined(&output);
    assert!(
        !text.contains("error["),
        "the REPL refused the read: {text}"
    );
    assert!(
        text.contains("\"id\"") && text.contains("\"name\""),
        "each column's reference is found among the table's columns: {text}"
    );
}

#[test]
fn repl_nested_table_columns_carry_the_declared_column_fields() {
    let project = committed_project(CATALOGUE_SOURCE);
    let output = repl(project.path(), "sys.catalog.tables | map(t => t.columns)");
    let text = combined(&output);
    assert!(
        !text.contains("error["),
        "the REPL refused the read: {text}"
    );
    assert!(
        text.contains("type"),
        "nested columns lack the declared `type` field: {text}"
    );
    assert!(
        !text.contains("\"ty\""),
        "nested columns still serve the undeclared `ty` field: {text}"
    );
}

#[test]
fn repl_refuses_an_unlinked_catalogue_member_at_admission() {
    // `sys.catalog.functions` has no linked provider (ORNA-SYS-008). A relation
    // pipeline over it is refused by intrinsic inference (`ORNA-S022-UNSUPPORTED`)
    // before evaluation, so no rows are served. The spec's `sys.version.incompatible`
    // code is produced only on the call-form path; this test pins the refusal, not the code.
    let project = committed_project(CATALOGUE_SOURCE);
    let output = repl(project.path(), "sys.catalog.functions | map(f => f.name)");
    let text = combined(&output);
    assert!(
        text.contains("error["),
        "an unlinked catalogue member must be refused at admission: {text}"
    );
}

#[test]
fn orna_run_reads_the_declared_column_catalogue() {
    let project = committed_project(CATALOGUE_SOURCE);
    let output = orna(project.path(), &["run", "main.name_column"]);
    let text = combined(&output);
    assert!(output.status.success(), "{text}");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "\"name\" : Str\n",
        "{text}"
    );
}

#[test]
// Specified: ORNA-SYS-111, ORNA-SYS-112; `orna run` serves the same references.
fn orna_run_finds_each_column_by_its_tables_reference() {
    let project = committed_project(CATALOGUE_SOURCE);
    let output = orna(
        project.path(),
        &["run", "main.columns_found_by_table_reference"],
    );
    let text = combined(&output);
    assert!(output.status.success(), "{text}");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "2 : Int\n",
        "{text}"
    );
}

#[test]
// Specified: ORNA-SYS-111. A table with no committed identity has no
// spec-correct reference and a null one breaches the declared type, so the
// catalogue of a project with no commit is refused (`sys.snapshot.incomplete`:
// the required snapshot objects are unavailable), not served with null
// references. A run that reads no catalogue still executes.
fn orna_run_refuses_the_catalogue_of_a_project_with_no_commit() {
    let project = uncommitted_project(CATALOGUE_SOURCE);
    let output = orna(project.path(), &["run", "main.name_column"]);
    let text = combined(&output);
    assert!(
        !output.status.success(),
        "the catalogue of a project with no committed identities is not served: {text}"
    );
    assert!(
        text.contains("sys.catalog.columns"),
        "the refusal points at the catalogue read: {text}"
    );
    assert!(
        !text.contains("\"name\" : Str"),
        "no row was served: {text}"
    );
}
